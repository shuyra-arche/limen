#!/usr/bin/env bash
# Exercise discovery against actual owner manifests and committed Git diffs.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cd "$root"
git ls-files -z 'server/tests/**/suite.json' 'server/tests/**/host/Cargo.toml' |
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
bash "$discover" . "$base" | jq -e 'map(.profile) | sort == ["debug","release"]' >/dev/null
bash "$checks" . "$base" | jq -e '(.full or .runner or .rv64) == false and .manifests == []' >/dev/null

change server/src/metal/hart/x86_64.rs
[[ $(boards) == q35 ]]
bash "$checks" . "$base" | jq -e '(.full or .runner or .rv64) == false and .manifests == []' >/dev/null
change server/src/metal/hart/aarch64.rs
[[ $(boards) == virt ]]
change server/src/metal/hart/riscv.rs
[[ $(boards) == rv64 ]]
change server/src/metal/hart/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/hart/node.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '(.manifests | sort) == ["server/tests/entry/boot/host/Cargo.toml", "server/tests/metal/sync/host/Cargo.toml"]' >/dev/null
change server/src/metal/hart/protection.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/sync/turn.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.rv64 and (.full or .runner) == false and (.manifests | sort) == ["server/tests/entry/boot/host/Cargo.toml", "server/tests/metal/sync/host/Cargo.toml"]' >/dev/null
change server/src/metal/execution/mod.rs
bash "$discover" . "$base" | jq -e 'map(.id) | unique == ["kernel-execution","kernel-execution-harts"]' >/dev/null

change server/tests/target/entry/platforms/io.rs
[[ $(boards) == q35 ]]
change server/tests/target/entry/platforms/virt.rs
[[ $(boards) == virt ]]
change server/tests/target/entry/platforms/q35.rs server/src/metal/boot/aarch64/mod.rs
[[ $(boards) == 'q35 virt' ]]
change server/tests/target/entry/platforms/main.rs
[[ $(boards) == 'q35 virt' ]]
change server/src/basis/room.rs
bash "$discover" . "$base" | jq -e 'map(.id) | unique == ["kernel-execution","kernel-execution-harts"]' >/dev/null
change server/tests/metal/sync/contracts.rs
bash "$checks" . "$base" | jq -e '.rv64 and (.full or .runner) == false and .manifests == ["server/tests/metal/sync/host/Cargo.toml"]' >/dev/null
[[ $(bash "$discover" . "$base") == '[]' ]]
change server/src/basis/pend.rs
bash "$checks" . "$base" | jq -e '.rv64 and (.full or .runner) == false and .manifests == ["server/tests/entry/boot/host/Cargo.toml"]' >/dev/null
bash "$discover" . "$base" | jq -e 'map(select(.id == "boot-handoff" or .id == "platform-boot") | [.platform, .profile]) | sort == [["q35","debug"],["q35","release"],["rv64","debug"],["rv64","release"],["virt","debug"],["virt","release"]]' >/dev/null
change server/tests/target/entry/foundation.rs
[[ $(boards) == 'q35 rv64 virt' ]]
change server/tests/entry/boot/contracts.rs
bash "$checks" . "$base" | jq -e '.rv64 and (.full or .runner) == false and .manifests == ["server/tests/entry/boot/host/Cargo.toml"]' >/dev/null
change server/src/entry/boot/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/firmware/uefi/mod.rs
[[ $(boards) == 'q35 virt' ]]
bash "$checks" . "$base" | jq -e '.rv64 == false and .manifests == []' >/dev/null
change server/src/metal/firmware/uefi/loader.rs
[[ $(boards) == 'q35 virt' ]]
bash "$checks" . "$base" | jq -e '.rv64 == false and .manifests == ["server/tests/entry/boot/host/Cargo.toml"]' >/dev/null
change server/src/metal/firmware/map.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.manifests == ["server/tests/entry/boot/host/Cargo.toml"]' >/dev/null
change server/src/entry/boot/handoff.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/trap.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/major.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/minor.rs
bash "$discover" . "$base" | jq -e 'map(.id) | unique == ["kernel-execution-harts","platform-boot"]' >/dev/null
change server/src/metal/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/firmware/psci.rs
[[ $(boards) == virt ]]
bash "$checks" . "$base" | jq -e '(.full or .runner or .rv64) == false' >/dev/null
change server/src/metal/firmware/sbi/probe.rs
[[ $(boards) == rv64 ]]
bash "$checks" . "$base" | jq -e '.manifests == ["server/tests/metal/firmware/sbi/host/Cargo.toml"]' >/dev/null
change server/src/metal/boot/console.rs
bash "$checks" . "$base" | jq -e '.manifests == ["server/tests/entry/boot/host/Cargo.toml"]' >/dev/null
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/boot/services.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/firmware/devicetree/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.manifests == ["server/tests/entry/boot/host/Cargo.toml"]' >/dev/null
change server/src/metal/hart/stack.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/tests/qemu.version
[[ $(bash "$discover" . "$base") == "$full" ]]
change rust-toolchain.toml
[[ $(bash "$discover" . "$base") == "$full" ]]
bash "$checks" . "$base" | jq -e '.full and .runner and .rv64' >/dev/null
change server/src/metal/mmu/internal/access.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '.rv64 and .manifests == ["server/tests/entry/boot/host/Cargo.toml"]' >/dev/null
change server/src/metal/allocator/storage.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '(.manifests | sort) == ["server/tests/entry/boot/host/Cargo.toml", "server/tests/metal/allocator/host/Cargo.toml"]' >/dev/null
change server/src/metal/allocator/bump.rs
[[ $(boards) == 'q35 rv64 virt' ]]
bash "$checks" . "$base" | jq -e '(.manifests | sort) == ["server/tests/entry/boot/host/Cargo.toml", "server/tests/metal/allocator/host/Cargo.toml"]' >/dev/null
change server/src/metal/boot/x86_64/example.rs
previous=$(git rev-parse HEAD)
mkdir -p server/src/metal/boot/aarch64
git mv server/src/metal/boot/{x86_64,aarch64}/example.rs
git commit -qm rename
[[ $(bash "$discover" . "$previous" | jq -r 'map(.platform)|unique|join(" ")') == 'q35 virt' ]]
change docs/README.md
[[ $(bash "$discover" . "$base") == '[]' ]]
echo 'change selection contracts passed'
