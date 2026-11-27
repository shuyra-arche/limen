#!/usr/bin/env bash
# Select the actual guest target using the repository's pinned Rust toolchain.
set -euo pipefail
architecture=${1:?guest architecture required}
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup.sh
sh /tmp/rustup.sh -y --profile minimal --default-toolchain none
source "$HOME/.cargo/env"
case "$architecture" in
  riscv64) target=riscv64gc-unknown-none-elf ;;
  aarch64) target=aarch64-unknown-uefi ;;
  x86_64) target=x86_64-unknown-uefi ;;
  *) echo "unsupported guest architecture: $architecture" >&2; exit 2 ;;
esac
rustup target add "$target"
echo "LIMEN_TEST_TARGET=$target" >> "$GITHUB_ENV"
echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
