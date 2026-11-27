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

change server/src/metal/platform/boot/riscv64/linker.ld
expect_suites boot-handoff kernel-execution kernel-execution-harts riscv-module-execution \
    rv64-interrupt rv64-iommu rv64-ipi rv64-plic-sstc
[[ $(boards) == rv64 ]]
bash "$checks" . "$base" | jq -e '.rv64 and ((.host or .runner) | not)' >/dev/null

change server/src/metal/processor/executor/x86_64/native.rs
[[ $(boards) == q35 ]]
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/processor/executor/aarch64/native.rs
[[ $(boards) == virt ]]
change server/src/metal/processor/executor/riscv64/native.rs
[[ $(boards) == rv64 ]]
for source in server/src/metal/processor/executor/mod.rs server/src/metal/processor/treaty/mod.rs; do
    change "$source"
    [[ $(bash "$discover" . "$base") == "$full" ]]
done
change server/src/metal/processor/executor/local/node.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/processor/executor/local/protection.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/sync/turn/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
for source in server/src/metal/processor/execution/cooperative/mod.rs \
              server/src/metal/processor/treaty/cooperative.rs; do
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
for source in server/src/metal/identity/mod.rs server/src/metal/identity/treaty/mod.rs; do
    change "$source"
    expect_suites boot-handoff kernel-execution kernel-execution-harts platform-boot platform-execution rv64-interrupt rv64-iommu
    bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
done

change server/tests/target/support/uefi/io.rs
[[ $(boards) == q35 ]]
change server/tests/target/support/uefi/virt.rs
[[ $(boards) == virt ]]
change server/tests/target/support/uefi/q35.rs server/src/metal/platform/boot/aarch64/mod.rs
[[ $(boards) == 'q35 virt' ]]
for source in server/tests/target/support/uefi/lib.rs \
              server/tests/target/Cargo.toml server/tests/target/Cargo.lock; do
    change "$source"
    [[ $(boards) == 'q35 virt' ]]
    expect_suites aarch64-gic aarch64-paging aarch64-smmu platform-boot platform-execution \
        x86-64-amdvi x86-64-ioapic x86-64-local-apic-failure x86-64-local-apic-ipi x86-64-local-apic-timer x86-64-paging x86-64-vtd
done
change server/tests/target/entry/platforms/main.rs
expect_suites platform-boot
[[ $(boards) == 'q35 virt' ]]
change server/tests/target/metal/interrupt/aarch64/gic/main.rs
expect_suites aarch64-gic
[[ $(boards) == virt ]]
change server/tests/target/metal/memory/mmu/x86_64/amdvi/Cargo.toml
expect_suites x86-64-amdvi
[[ $(boards) == q35 ]]
change server/tests/target/support/paging/mod.rs
expect_suites aarch64-paging x86-64-paging
[[ $(boards) == 'q35 virt' ]]
change server/tests/target/support/paging/installation.rs
expect_suites aarch64-paging riscv-module-execution x86-64-paging
[[ $(boards) == 'q35 rv64 virt' ]]
change server/tests/target/support/uefi/interrupt.rs
expect_suites x86-64-ioapic x86-64-local-apic-ipi x86-64-local-apic-timer
[[ $(boards) == q35 ]]
change server/tests/target/metal/interrupt/x86_64/apic-timer/main.rs
expect_suites x86-64-local-apic-timer
[[ $(boards) == q35 ]]
change server/src/basis/execution.rs
expect_suites kernel-execution kernel-execution-harts platform-execution
change server/tests/basis/execution/migration.rs
expect_suites platform-execution
[[ $(boards) == 'q35 virt' ]]
change server/tests/metal/processor/execution/registers/x86_64.rs
expect_suites platform-execution
[[ $(boards) == q35 ]]
change server/tests/metal/processor/execution/registers/aarch64.rs
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
expect_suites platform-boot platform-execution x86-64-amdvi x86-64-ioapic x86-64-local-apic-failure x86-64-local-apic-ipi x86-64-local-apic-timer x86-64-paging x86-64-vtd
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
change server/src/metal/platform/description/acpi/dmar.rs
expect_suites x86-64-vtd
change server/src/metal/platform/description/acpi/ivrs.rs
expect_suites x86-64-amdvi
for source in server/src/metal/platform/firmware/uefi/map.rs \
              server/src/metal/platform/firmware/treaty/map.rs; do
    change "$source"
    [[ $(bash "$discover" . "$base") == "$full" ]]
    bash "$checks" . "$base" | jq -e '.host' >/dev/null
done
change server/src/entry/boot/handoff.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/boot/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
for source in server/src/metal/processor/execution/mod.rs \
              server/src/metal/processor/execution/terminal/trap.rs \
              server/src/metal/processor/executor/entry/mod.rs; do
    change "$source"
    [[ $(bash "$discover" . "$base") == "$full" ]]
    bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
done
for pair in 'riscv64 rv64' 'aarch64 virt' 'x86_64 q35'; do
    read -r arch board <<<"$pair"
    for owner in execution/terminal executor/entry; do
        change "server/src/metal/processor/$owner/$arch.rs"
        [[ $(boards) == "$board" ]]
        bash "$checks" . "$base" | jq -e --arg board "$board" '
            if $board == "rv64" then .rv64 and (.runner | not)
            else ((.host or .runner or .rv64) | not) end' >/dev/null
    done
done
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
change server/src/metal/processor/executor/launch/mod.rs
expect_suites aarch64-gic kernel-execution-harts platform-boot platform-execution riscv-module-execution rv64-ipi x86-64-local-apic-failure x86-64-local-apic-ipi
change server/src/metal/processor/executor/launch/aarch64.rs
expect_suites aarch64-gic platform-boot platform-execution
[[ $(boards) == virt ]]
change server/src/metal/processor/executor/launch/x86_64.rs
expect_suites platform-boot platform-execution x86-64-local-apic-failure x86-64-local-apic-ipi
[[ $(boards) == q35 ]]
change server/src/metal/processor/executor/launch/riscv64.rs
expect_suites kernel-execution-harts riscv-module-execution rv64-ipi
[[ $(boards) == rv64 ]]
change server/src/metal/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
for source in server/src/metal/platform/firmware/psci/native.rs server/src/metal/interrupt/aarch64/gic/cpu_interface/native.rs server/src/metal/chrono/aarch64/counter.rs; do
    change "$source"
    [[ $(boards) == virt ]]
    bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
done
for source in server/src/metal/processor/description/aarch64/cpu.rs \
              server/src/metal/platform/firmware/psci/mod.rs \
              server/src/metal/platform/firmware/treaty/psci.rs \
              server/src/metal/interrupt/aarch64/gic/mod.rs \
              server/src/metal/interrupt/aarch64/timer/mod.rs \
              server/src/metal/chrono/aarch64/description.rs; do
    change "$source"
    [[ $(boards) == virt ]]
    bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
done
change server/src/metal/interrupt/mod.rs
[[ $(boards) == 'q35 rv64 virt' ]]
for source in server/src/metal/interrupt/x86_64/internal/apic/{mod,registers,launch}.rs; do
    change "$source"
    [[ $(boards) == q35 ]]
    expect_suites module-execution platform-boot platform-execution x86-64-ioapic x86-64-local-apic-failure x86-64-local-apic-ipi x86-64-local-apic-timer
    bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
done
change server/src/metal/interrupt/x86_64/internal/apic/native.rs
[[ $(boards) == q35 ]]
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
for source in server/src/metal/interrupt/riscv64/external/aplic/{mod,description,domain,encoding,registers,source}.rs \
              server/src/metal/interrupt/riscv64/external/aplic/direct/{mod,context,encoding,reception}.rs \
              server/src/metal/interrupt/riscv64/internal/aclint/mod.rs; do
    change "$source"
    expect_suites rv64-interrupt
done
change server/src/metal/interrupt/riscv64/internal/timer/mod.rs
expect_suites rv64-interrupt rv64-plic-sstc
for owner in mod binding command directory queue registers translation; do
    change "server/src/metal/memory/mmu/riscv64/external/iommu/$owner.rs"
    expect_suites rv64-iommu
done
for owner in mod binding command directory fault translation; do
    change "server/src/metal/memory/mmu/x86_64/external/vtd/$owner.rs"
    expect_suites x86-64-vtd
done
for owner in mod binding directory queue; do
    change "server/src/metal/memory/mmu/x86_64/external/amdvi/$owner.rs"
    expect_suites x86-64-amdvi
done
for owner in mod binding command directory queue registers translation; do
    change "server/src/metal/memory/mmu/aarch64/external/smmu/$owner.rs"
    expect_suites aarch64-smmu
done
change server/src/metal/memory/mmu/x86_64/internal/installation.rs
expect_suites x86-64-paging
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/memory/mmu/treaty/installation.rs
expect_suites aarch64-paging riscv-module-execution x86-64-paging
[[ $(boards) == 'q35 rv64 virt' ]]
change server/src/metal/memory/mmu/aarch64/internal/installation.rs
expect_suites aarch64-paging
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
for source in server/src/metal/interrupt/x86_64/external/ioapic/{mod,pin,port,route}.rs; do
    change "$source"
    expect_suites x86-64-ioapic
done
change server/src/metal/interrupt/riscv64/external/plic/context.rs
expect_suites rv64-plic-sstc
change server/tests/metal/interrupt/x86_64/internal/apic/native_failure.rs
expect_suites x86-64-local-apic-failure
change server/tests/metal/interrupt/riscv64/external/native_plic.rs
expect_suites rv64-plic-sstc
change server/tests/target/support/riscv64/reception.rs
expect_suites rv64-interrupt rv64-ipi rv64-plic-sstc
change server/src/metal/interrupt/x86_64/internal/apic/ipi.rs
expect_suites module-execution platform-boot platform-execution x86-64-ioapic x86-64-local-apic-failure x86-64-local-apic-ipi x86-64-local-apic-timer
for source in server/src/metal/interrupt/riscv64/internal/ipi/{mod,native}.rs; do
    change "$source"
    expect_suites rv64-ipi
done
for source in server/src/metal/interrupt/treaty/ipi.rs server/tests/target/support/ipi.rs; do
    change "$source"
    expect_suites rv64-ipi x86-64-local-apic-ipi
done
change server/tests/target/support/clock.rs
expect_suites aarch64-gic rv64-plic-sstc
for source in server/src/metal/chrono/{counter/mod,counter/rate,treaty/clock,treaty/mod}.rs; do
    change "$source"
    expect_suites aarch64-gic rv64-interrupt rv64-ipi rv64-plic-sstc x86-64-local-apic-timer
done
change server/src/metal/chrono/riscv64/time.rs
expect_suites rv64-interrupt rv64-ipi rv64-plic-sstc
change server/src/metal/chrono/x86_64/tsc.rs
expect_suites x86-64-local-apic-timer
change server/tests/target/support/timer.rs
expect_suites aarch64-gic rv64-plic-sstc
change server/src/metal/interrupt/treaty/absolute.rs
expect_suites aarch64-gic rv64-interrupt rv64-plic-sstc
for source in server/src/metal/interrupt/treaty/route.rs server/tests/target/support/route.rs; do
    change "$source"
    expect_suites aarch64-gic rv64-plic-sstc x86-64-ioapic
    [[ $(boards) == 'q35 rv64 virt' ]]
done
change server/tests/target/support/interrupt.rs
expect_suites aarch64-gic rv64-interrupt rv64-plic-sstc x86-64-ioapic
change server/tests/target/support/claim.rs
expect_suites aarch64-gic rv64-interrupt rv64-plic-sstc
change server/src/metal/interrupt/treaty/claim.rs
expect_suites aarch64-gic rv64-interrupt rv64-plic-sstc x86-64-ioapic
change server/src/metal/interrupt/treaty/mod.rs
expect_suites aarch64-gic rv64-interrupt rv64-ipi rv64-plic-sstc x86-64-ioapic x86-64-local-apic-ipi
for source in server/src/metal/memory/mmu/treaty/binding.rs server/tests/target/support/paging/binding.rs; do
    change "$source"
    expect_suites aarch64-smmu rv64-iommu x86-64-amdvi x86-64-vtd
done
change server/src/metal/memory/mmu/treaty/mod.rs
expect_suites aarch64-paging aarch64-smmu module-execution riscv-module-execution rv64-iommu x86-64-amdvi x86-64-paging x86-64-vtd
change server/tests/target/support/pci/edu.rs
expect_suites aarch64-smmu rv64-iommu x86-64-amdvi x86-64-vtd
change server/tests/target/support/pci/q35.rs
expect_suites x86-64-amdvi x86-64-vtd
change server/tests/target/support/uefi/run.sh
expect_suites aarch64-gic aarch64-paging aarch64-smmu platform-boot platform-execution x86-64-amdvi \
    x86-64-ioapic x86-64-local-apic-failure x86-64-local-apic-ipi x86-64-local-apic-timer x86-64-paging x86-64-vtd
change server/tests/target/entry/boot/main.rs
expect_suites boot-handoff
change server/tests/metal/memory/mmu/riscv64/internal/activity/mod.rs
[[ $(bash "$discover" . "$base") == '[]' ]]
bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
for source in server/src/metal/platform/firmware/sbi/probe.rs \
              server/src/metal/platform/firmware/treaty/sbi.rs; do
    change "$source"
    [[ $(boards) == rv64 ]]
    bash "$checks" . "$base" | jq -e '.host' >/dev/null
done
change server/src/metal/processor/execution/terminal/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/platform/boot/treaty/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/processor/execution/terminal/console.rs
bash "$checks" . "$base" | jq -e '.host' >/dev/null
[[ $(bash "$discover" . "$base") == "$full" ]]
for source in server/src/metal/platform/description/devicetree/mod.rs \
              server/src/metal/platform/description/treaty/devicetree.rs; do
    change "$source"
    [[ $(bash "$discover" . "$base") == "$full" ]]
    bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
done
for source in server/src/metal/processor/description/riscv64/cpu.rs \
              server/src/metal/processor/description/riscv64/isa.rs \
              server/src/metal/chrono/riscv64/description.rs; do
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
change server/src/metal/processor/executor/local/stack.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/tests/qemu.version
[[ $(bash "$discover" . "$base") == "$full" ]]
change rust-toolchain.toml
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .runner and .rv64' >/dev/null
change server/src/metal/memory/access/treaty/ram.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null

for source in server/src/metal/module/image/elf.rs server/src/metal/module/treaty/mod.rs \
              server/tests/metal/module/probe.rs; do
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
for source in server/src/metal/processor/description/riscv64/status.rs \
              server/src/metal/interrupt/riscv64/internal/supervisor.rs \
              server/src/metal/memory/mmu/riscv64/sv/mod.rs \
              server/src/metal/memory/mmu/riscv64/internal/context.rs \
              server/src/metal/memory/mmu/riscv64/internal/format.rs \
              server/src/metal/memory/mmu/riscv64/internal/installation.rs; do
    change "$source"
    expect_suites boot-handoff kernel-execution kernel-execution-harts riscv-module-execution \
        rv64-interrupt rv64-iommu rv64-ipi rv64-plic-sstc
    bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
done
for source in server/src/metal/processor/description/aarch64/status.rs \
              server/src/metal/processor/execution/profile/aarch64.rs; do
    change "$source"
    [[ $(boards) == virt ]]
    bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
done
change server/src/metal/processor/execution/profile/aarch64.rs
expect_suites platform-execution
change server/src/metal/processor/execution/profile/x86_64.rs
[[ $(boards) == q35 ]]
expect_suites platform-execution
change server/src/metal/processor/execution/profile/mod.rs
[[ $(boards) == 'q35 virt' ]]
expect_suites platform-execution
for source in server/src/metal/memory/allocator/allocation/storage.rs \
              server/src/metal/memory/allocator/treaty/mod.rs; do
    change "$source"
    [[ $(boards) == 'q35 rv64 virt' ]]
    bash "$checks" . "$base" | jq -e '.host' >/dev/null
done
change server/src/metal/memory/allocator/bootstrap/bump.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/platform/boot/x86_64/example.rs
previous=$(git rev-parse HEAD)
mkdir -p server/src/metal/platform/boot/aarch64
git mv server/src/metal/platform/boot/{x86_64,aarch64}/example.rs
git commit -qm rename
[[ $(bash "$discover" . "$previous" | jq -r 'map(.platform)|unique|join(" ")') == 'q35 virt' ]]
change server/src/metal/processor/executor/local/mod.rs
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
