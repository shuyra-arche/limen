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
change server/src/metal/x86_64/mod.rs
[[ $(boards) == q35 ]]
bash "$discover" . "$base" | jq -e 'map(.profile) | sort == ["debug","release"]' >/dev/null
bash "$checks" . "$base" | jq -e '(.full or .runner or .rv64) == false and .manifests == []' >/dev/null

change server/tests/target/entry/platforms/virt.rs
[[ $(boards) == virt ]]
change server/tests/target/entry/platforms/q35.rs server/src/metal/aarch64/mod.rs
[[ $(boards) == 'q35 virt' ]]
change server/tests/target/entry/platforms/main.rs
[[ $(boards) == 'q35 virt' ]]
change server/src/basis/room.rs
bash "$discover" . "$base" | jq -e 'map(.id) | unique == ["kernel-execution","kernel-execution-harts"]' >/dev/null
change server/src/entry/boot/mod.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/console.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/stack.rs
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/tests/qemu.version
[[ $(bash "$discover" . "$base") == "$full" ]]
change server/src/metal/allocator/bump.rs
[[ $(boards) == "" ]]
bash "$checks" . "$base" | jq -e '.manifests == ["server/tests/metal/allocator/host/Cargo.toml"]' >/dev/null
change server/src/metal/x86_64/example.rs
previous=$(git rev-parse HEAD)
mkdir -p server/src/metal/aarch64
git mv server/src/metal/{x86_64,aarch64}/example.rs
git commit -qm rename
[[ $(bash "$discover" . "$previous" | jq -r 'map(.platform)|unique|join(" ")') == 'q35 virt' ]]
change docs/README.md
[[ $(bash "$discover" . "$base") == '[]' ]]
echo 'change selection contracts passed'
