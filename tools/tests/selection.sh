#!/usr/bin/env bash
# Exercise discovery against actual owner manifests and committed Git diffs.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cd "$root"
git ls-files -z 'server/tests/**/suite.json' |
    tar --null -T - -cf - | tar -C "$scratch" -xf -
cd "$scratch"
git init -q
git config user.name test
git config user.email test@localhost
git add .
git commit -qm baseline
base=$(git rev-parse HEAD)
discover="$root/tools/tests/discover.sh"
checks="$root/tools/tests/checks.sh"
full=$(bash "$discover" .)
change() {
    git reset --hard -q "$base"
    for path in "$@"; do
        mkdir -p "$(dirname "$path")"
        echo >>"$path"
    done
    git add .
    git commit -qm change
}
boards() {
    bash "$discover" . "$base" | jq -r 'map(.platform) | unique | join(" ")'
}
suites() {
    bash "$discover" . "$base" | jq -r 'map(.id) | unique | join(" ")'
}
expect_suites() {
    local actual
    actual=$(suites)
    if [[ $actual != "$*" ]]; then
        printf 'expected suites: %s\nactual suites: %s\n' "$*" "$actual" >&2
        return 1
    fi
}
change server/src/metal/platform/boot/x86_64/mod.rs
[[ $(boards) == q35 ]]
bash "$discover" . "$base" | jq -e 'map(select(.id == "platform-boot").profile) | sort == ["debug","release"]' >/dev/null
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null

change server/src/metal/processor/hart/x86_64/native.rs
[[ $(boards) == q35 ]]
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/processor/hart/aarch64/native.rs
[[ $(boards) == virt ]]
change server/src/metal/processor/hart/riscv64/native.rs
[[ $(boards) == rv64 ]]
change server/src/metal/processor/hart/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/processor/hart/local/node.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/processor/hart/local/protection.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/sync/turn/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
for source in server/src/metal/processor/execution/mod.rs server/src/metal/processor/execution/cooperative/mod.rs; do
    change "$source"
    expect_suites boot-handoff kernel-execution kernel-execution-harts platform-execution riscv-module-execution
done
change server/src/metal/processor/execution/cooperative/x86_64/mod.rs
[[ $(boards) == q35 ]]
expect_suites platform-execution
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/processor/execution/cooperative/aarch64/mod.rs
[[ $(boards) == virt ]]
expect_suites platform-execution
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/identity/mod.rs
expect_suites boot-handoff kernel-execution kernel-execution-harts platform-boot platform-execution rv64-interrupt rv64-iommu
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null

change server/tests/target/support/uefi/io.rs
[[ $(boards) == q35 ]]
change server/tests/target/support/uefi/virt.rs
[[ $(boards) == virt ]]
change server/tests/target/support/uefi/q35.rs server/src/metal/platform/boot/aarch64/mod.rs
[[ $(boards) == 'q35 virt' ]]
change server/tests/target/support/uefi/main.rs
[[ $(boards) == 'q35 virt' ]]
change server/src/basis/execution.rs
expect_suites kernel-execution kernel-execution-harts platform-execution
change server/tests/target/basis/execution/platforms/scenario.rs
expect_suites platform-execution
[[ $(boards) == 'q35 virt' ]]
change server/tests/target/basis/execution/platforms/x86_64.rs
expect_suites platform-execution
[[ $(boards) == q35 ]]
change server/tests/target/basis/execution/platforms/aarch64.rs
expect_suites platform-execution
[[ $(boards) == virt ]]
change server/tests/metal/sync/contracts.rs
bash "$checks" . "$base" | jq -e '.rv64 and ((.host or .runner) | not)' >/dev/null
[[ $(bash "$discover" . "$base") == '[]' ]]
change server/src/basis/pend.rs
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
bash "$discover" . "$base" | jq -e 'map(select(.id == "boot-handoff" or .id == "platform-boot") | [.platform, .profile]) | sort == [["q35","debug"],["q35","release"],["rv64","debug"],["rv64","release"],["virt","debug"],["virt","release"]]' >/dev/null
change server/tests/target/entry/foundation.rs
[[ $(boards) == 'q35 rv64 virt' ]]
change server/tests/entry/boot/contracts.rs
bash "$checks" . "$base" | jq -e '.rv64 and ((.host or .runner) | not)' >/dev/null
change server/src/entry/boot/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/firmware/uefi/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
change server/src/metal/platform/firmware/uefi/loader.rs
[[ $(boards) == 'q35 virt' ]]
bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
change server/src/metal/platform/description/acpi/madt.rs
[[ $(boards) == q35 ]]
expect_suites platform-boot platform-execution x86-64-amdvi x86-64-ioapic x86-64-local-apic-failure x86-64-paging x86-64-vtd
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
change server/src/metal/platform/description/acpi/dmar.rs
expect_suites x86-64-vtd
change server/src/metal/platform/description/acpi/ivrs.rs
expect_suites x86-64-amdvi
change server/src/metal/platform/firmware/uefi/map.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/entry/boot/handoff.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/boot/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/boot/trap.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/boot/major.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/boot/handoff/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/boot/handoff/sbi.rs
[[ $(boards) == rv64 ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
change server/src/metal/platform/boot/handoff/uefi.rs
[[ $(boards) == 'q35 virt' ]]
bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
change server/src/metal/platform/boot/minor.rs
expect_suites kernel-execution-harts platform-boot platform-execution riscv-module-execution x86-64-local-apic-failure
change server/src/metal/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
for source in server/src/metal/platform/firmware/psci/native.rs server/src/metal/interrupt/aarch64/gic/native.rs server/src/metal/chrono/aarch64/native.rs; do
    change "$source"
    [[ $(boards) == virt ]]
    bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
done
for source in server/src/metal/processor/hart/aarch64/description.rs \
              server/src/metal/platform/firmware/psci/mod.rs \
              server/src/metal/interrupt/aarch64/gic/mod.rs \
              server/src/metal/interrupt/aarch64/internal/timer.rs \
              server/src/metal/chrono/aarch64/mod.rs; do
    change "$source"
    [[ $(boards) == virt ]]
    bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
done
change server/src/metal/interrupt/mod.rs
[[ $(boards) == 'q35 rv64 virt' ]]
change server/src/metal/interrupt/x86_64/internal/apic/mod.rs
[[ $(boards) == q35 ]]
expect_suites module-execution platform-boot platform-execution x86-64-ioapic x86-64-local-apic-failure
bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
change server/src/metal/interrupt/x86_64/internal/apic/native.rs
[[ $(boards) == q35 ]]
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
for source in server/src/metal/interrupt/riscv64/external/aplic/mod.rs \
              server/src/metal/interrupt/riscv64/external/aplic/direct.rs \
              server/src/metal/interrupt/riscv64/internal/aclint/mod.rs \
              server/src/metal/interrupt/riscv64/external/aplic/encoding.rs; do
    change "$source"
    expect_suites rv64-interrupt
done
change server/src/metal/interrupt/riscv64/internal/timer/mod.rs
expect_suites rv64-interrupt rv64-plic-sstc
change server/src/metal/memory/mmu/riscv64/external/iommu/queue.rs
expect_suites rv64-iommu
change server/src/metal/memory/mmu/x86_64/external/vtd.rs
expect_suites x86-64-vtd
change server/src/metal/memory/mmu/x86_64/external/amdvi/mod.rs
expect_suites x86-64-amdvi
change server/src/metal/memory/mmu/aarch64/external/smmu/mod.rs
expect_suites aarch64-smmu
change server/src/metal/memory/mmu/x86_64/internal/native.rs
expect_suites x86-64-paging
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/memory/mmu/aarch64/internal/native.rs
expect_suites aarch64-paging
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/interrupt/x86_64/external/ioapic/mod.rs
expect_suites x86-64-ioapic
change server/src/metal/interrupt/riscv64/external/plic/context.rs
expect_suites rv64-plic-sstc
change server/tests/target/metal/interrupt/x86_64/apic-failure/scenario.rs
expect_suites x86-64-local-apic-failure
change server/tests/target/metal/interrupt/riscv64/plic-sstc/plic.rs
expect_suites rv64-plic-sstc
change server/tests/target/support/riscv64/reception.rs
expect_suites rv64-interrupt rv64-plic-sstc
change server/tests/target/support/pci/edu.rs
expect_suites aarch64-smmu rv64-iommu x86-64-amdvi x86-64-vtd
change server/tests/target/support/pci/q35.rs
expect_suites x86-64-amdvi x86-64-vtd
change server/tests/target/support/uefi/run.sh
expect_suites aarch64-gic aarch64-paging aarch64-smmu platform-boot platform-execution x86-64-amdvi \
    x86-64-ioapic x86-64-local-apic-failure x86-64-paging x86-64-vtd
change server/tests/target/entry/boot/main.rs
expect_suites boot-handoff
change server/tests/metal/memory/mmu/riscv64/internal/activity/mod.rs
[[ $(bash "$discover" . "$base") == '[]' ]]
bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
change server/src/metal/platform/firmware/sbi/probe.rs
[[ $(boards) == rv64 ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/platform/boot/console.rs
bash "$checks" . "$base" | jq -e '.host' >/dev/null
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/boot/services.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/description/devicetree/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
for source in server/src/metal/processor/hart/riscv64/description.rs \
              server/src/metal/processor/execution/description/riscv64.rs \
              server/src/metal/chrono/riscv64/mod.rs; do
    change "$source"
    [[ $(boards) == rv64 ]]
    bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
done
change server/src/metal/memory/mmu/riscv64/internal/description.rs
[[ $(boards) == rv64 ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
for source in treaty/src/model/address.rs treaty/src/model/geometry.rs; do
    change "$source"
    [[ $(bash "$discover" . "$base") == "$full" ]]
    bash "$checks" . "$base" | jq -e '.host and .runner and .rv64' >/dev/null
done
change server/src/metal/processor/hart/local/stack.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/tests/qemu.version
[[ $(bash "$discover" . "$base") == "$full" ]]
change rust-toolchain.toml
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .runner and .rv64' >/dev/null
change server/src/metal/memory/mmu/access/ram.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null

for source in server/src/metal/module/image/elf.rs server/tests/metal/module/probe.rs; do
    change "$source"
    bash "$discover" . "$base" | jq -e 'map([.id, .platform, .profile]) | sort ==
        [["module-execution", "q35", "debug"], ["module-execution", "q35", "release"],
         ["module-execution", "virt", "debug"], ["module-execution", "virt", "release"],
         ["riscv-module-execution", "rv64", "debug"], ["riscv-module-execution", "rv64", "release"]]' >/dev/null
    bash "$checks" . "$base" | jq -e '.host' >/dev/null
done
change server/tests/target/entry/module/arm_mapping.rs
bash "$discover" . "$base" | jq -e 'map([.id, .platform, .profile]) | sort ==
    [["module-execution", "virt", "debug"], ["module-execution", "virt", "release"]]' >/dev/null
for source in server/src/metal/memory/mmu/riscv64/internal/encoder/mod.rs \
              server/src/metal/memory/mmu/riscv64/internal/encoder/binding.rs \
              server/tests/metal/module/riscv_root.rs \
              server/tests/metal/module/riscv_fixture.rs; do
    change "$source"
    bash "$discover" . "$base" | jq -e 'map([.id, .platform, .profile]) | sort ==
        [["riscv-module-execution", "rv64", "debug"], ["riscv-module-execution", "rv64", "release"]]' >/dev/null
done
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/memory/mmu/riscv64/internal/encoder/binding.rs
bash "$checks" . "$base" | jq -e '.host' >/dev/null

# Boot and cooperative admission now consume the actual SATP context and
# supervisor enable/status observations, not independent raw comparisons.
for source in server/src/metal/processor/hart/riscv64/status.rs \
              server/src/metal/interrupt/riscv64/internal/supervisor.rs \
              server/src/metal/memory/mmu/riscv64/sv/mod.rs \
              server/src/metal/memory/mmu/riscv64/internal/context/mod.rs \
              server/src/metal/memory/mmu/riscv64/internal/context/satp.rs; do
    change "$source"
    expect_suites boot-handoff kernel-execution kernel-execution-harts riscv-module-execution \
        rv64-interrupt rv64-iommu rv64-plic-sstc
    bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
done
for source in server/src/metal/processor/hart/aarch64/status.rs \
              server/src/metal/processor/execution/description/aarch64.rs; do
    change "$source"
    [[ $(boards) == virt ]]
    bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
done
change server/src/metal/processor/execution/description/aarch64.rs
expect_suites platform-execution
change server/src/metal/memory/allocator/allocation/storage.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/memory/allocator/bootstrap/bump.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/platform/boot/x86_64/example.rs
previous=$(git rev-parse HEAD)
mkdir -p server/src/metal/platform/boot/aarch64
git mv server/src/metal/platform/boot/{x86_64,aarch64}/example.rs
git commit -qm rename
[[ $(bash "$discover" . "$previous" | jq -r 'map(.platform)|unique|join(" ")') == 'q35 virt' ]]
change server/src/metal/processor/hart/local/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/tests/metal/memory/allocator/source.rs
bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
# Host integration roots and new owner modules use the same host selection,
# without requiring a new manifest or path-specific CI entry.
for source in server/tests/contracts.rs \
              server/tests/metal/mod.rs \
              server/tests/metal/platform/description/mod.rs \
              server/tests/basis/cell/mod.rs; do
    change "$source"
    bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
    [[ $(bash "$discover" . "$base") == '[]' ]]
done
change .cargo/config.toml
bash "$checks" . "$base" | jq -e '.host and .runner and .rv64' >/dev/null
[[ $(bash "$discover" . "$base") == "$full" ]]
change docs/README.md
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
[[ $(bash "$discover" . "$base") == '[]' ]]
echo 'change selection contracts passed'
