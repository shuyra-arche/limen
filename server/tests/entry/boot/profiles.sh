#!/usr/bin/env bash
# Verify the build script's actual Cargo directives for each image profile.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

rustc --edition 2024 -D warnings "$root/server/build.rs" -o "$scratch/build"
build() {
    TARGET=$1 CARGO_CFG_TARGET_ARCH=$2 CARGO_CFG_TARGET_OS=$3 \
        CARGO_MANIFEST_DIR="$root/server" "$scratch/build"
}

directives() {
    awk '/^cargo::rustc-cfg=|^cargo::metadata=|^cargo::rerun-if-changed=src\//'
}

linker=src/metal/platform/boot/riscv64/linker.ld
[[ -f "$root/server/$linker" ]]
rv64=$(build riscv64gc-unknown-none-elf riscv64 none | directives)
expected=$(printf '%s\n' 'cargo::rustc-cfg=limen_target' 'cargo::rustc-cfg=limen_firmware="sbi"' \
    'cargo::rustc-cfg=limen_cooperative' "cargo::rerun-if-changed=$linker" \
    "cargo::metadata=linker_script=$root/server/$linker")
[[ $rv64 == "$expected" ]]

expected=$(printf '%s\n' 'cargo::rustc-cfg=limen_target' 'cargo::rustc-cfg=limen_firmware="uefi"' \
    'cargo::rustc-cfg=limen_cooperative')
[[ $(build x86_64-unknown-uefi x86_64 uefi | directives) == "$expected" ]]
[[ $(build aarch64-unknown-uefi aarch64 uefi | directives) == "$expected" ]]

[[ -z $(build x86_64-unknown-linux-gnu x86_64 linux | directives) ]]
[[ -z $(build aarch64-apple-darwin aarch64 macos | directives) ]]
[[ -z $(build x86_64-unknown-none x86_64 none | directives) ]]
[[ -z $(build aarch64-unknown-none aarch64 none | directives) ]]
[[ -z $(build riscv64imac-unknown-none-elf riscv64 none | directives) ]]
[[ -z $(build x86_64-unknown-uefi x86_64 none | directives) ]]
[[ -z $(build aarch64-unknown-uefi x86_64 uefi | directives) ]]
echo 'build profile contracts passed'
