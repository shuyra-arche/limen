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
change server/src/metal/boot/x86_64/mod.rs
[[ $(boards) == q35 ]]
bash "$discover" . "$base" | jq -e 'map(select(.id == "platform-boot").profile) | sort == ["debug","release"]' >/dev/null
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null

change server/src/metal/hart/x86_64.rs
[[ $(boards) == q35 ]]
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/hart/aarch64.rs
[[ $(boards) == virt ]]
change server/src/metal/hart/riscv.rs
[[ $(boards) == rv64 ]]
change server/src/metal/hart/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/hart/node.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/hart/protection.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/sync/turn.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
for source in server/src/metal/execution/mod.rs server/src/metal/execution/cooperative.rs; do
    change "$source"
    bash "$discover" . "$base" | jq -e 'map(.id) | unique == ["boot-handoff","kernel-execution","kernel-execution-harts","riscv-module-execution"]' >/dev/null
done
change server/src/metal/identity/mod.rs
[[ $(bash "$discover" . "$base") == "$(jq -c 'map(select(.id != "module-execution" and .id != "riscv-module-execution"))' <<<"$full")" ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null

change server/tests/target/entry/platforms/io.rs
[[ $(boards) == q35 ]]
change server/tests/target/entry/platforms/virt.rs
[[ $(boards) == virt ]]
change server/tests/target/entry/platforms/q35.rs server/src/metal/boot/aarch64/mod.rs
[[ $(boards) == 'q35 virt' ]]
change server/tests/target/entry/platforms/main.rs
[[ $(boards) == 'q35 virt' ]]
change server/src/basis/execution.rs
bash "$discover" . "$base" | jq -e 'map(.id) | unique == ["kernel-execution","kernel-execution-harts"]' >/dev/null
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
change server/src/metal/firmware/uefi/mod.rs
[[ $(boards) == 'q35 virt' ]]
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/firmware/uefi/loader.rs
[[ $(boards) == 'q35 virt' ]]
bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
change server/src/metal/firmware/acpi/madt.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
change server/src/metal/firmware/map.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/entry/boot/handoff.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/trap.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/major.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/minor.rs
bash "$discover" . "$base" | jq -e 'map(.id) | unique == ["kernel-execution-harts","platform-boot","riscv-module-execution"]' >/dev/null
change server/src/metal/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/firmware/psci.rs
[[ $(boards) == virt ]]
bash "$checks" . "$base" | jq -e '((.host or .runner or .rv64) | not)' >/dev/null
change server/src/metal/firmware/sbi/probe.rs
[[ $(boards) == rv64 ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/boot/console.rs
bash "$checks" . "$base" | jq -e '.host' >/dev/null
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/services.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/firmware/devicetree/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
for source in server/src/metal/hart/description.rs \
              server/src/metal/execution/riscv/description.rs \
              server/src/metal/time/mod.rs; do
    change "$source"
    [[ $(boards) == rv64 ]]
    bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
done
change server/src/metal/mmu/internal/riscv/description.rs
[[ $(boards) == rv64 ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
for source in treaty/src/model/address.rs treaty/src/model/geometry.rs; do
    change "$source"
    [[ $(bash "$discover" . "$base") == "$full" ]]
    bash "$checks" . "$base" | jq -e '.host and .runner and .rv64' >/dev/null
done
change server/src/metal/hart/stack.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/tests/qemu.version
[[ $(bash "$discover" . "$base") == "$full" ]]
change rust-toolchain.toml
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .runner and .rv64' >/dev/null
change server/src/metal/mmu/internal/access.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null

for source in server/src/metal/module/elf.rs server/tests/metal/module/probe.rs; do
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
for source in server/src/metal/mmu/internal/riscv/root/mod.rs \
              server/src/metal/mmu/internal/riscv/root/binding.rs \
              server/src/metal/mmu/internal/riscv/mode.rs \
              server/src/metal/mmu/internal/riscv/context/mod.rs \
              server/src/metal/mmu/internal/riscv/context/satp.rs \
              server/tests/metal/module/riscv_root.rs \
              server/tests/metal/module/riscv_fixture.rs; do
    change "$source"
    bash "$discover" . "$base" | jq -e 'map([.id, .platform, .profile]) | sort ==
        [["riscv-module-execution", "rv64", "debug"], ["riscv-module-execution", "rv64", "release"]]' >/dev/null
done
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/mmu/internal/riscv/root/binding.rs
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/allocator/storage.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/allocator/bump.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.host' >/dev/null
change server/src/metal/boot/x86_64/example.rs
previous=$(git rev-parse HEAD)
mkdir -p server/src/metal/boot/aarch64
git mv server/src/metal/boot/{x86_64,aarch64}/example.rs
git commit -qm rename
[[ $(bash "$discover" . "$previous" | jq -r 'map(.platform)|unique|join(" ")') == 'q35 virt' ]]
change server/src/metal/fdt.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.host and .rv64 and (.runner | not)' >/dev/null
change server/src/metal/hart/hardware.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/tests/metal/allocator/source.rs
bash "$checks" . "$base" | jq -e '.host and ((.runner or .rv64) | not)' >/dev/null
# Host integration roots and new owner modules use the same host selection,
# without requiring a new manifest or path-specific CI entry.
for source in server/tests/contracts.rs \
              server/tests/metal/mod.rs \
              server/tests/metal/firmware/mod.rs \
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
