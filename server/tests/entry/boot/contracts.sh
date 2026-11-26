#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

target=${1:-riscv64gc-unknown-none-elf}
artifact=$(cargo build --locked -p kernel --target "$target" --message-format json |
    jq -er 'select(.reason == "compiler-artifact" and .target.name == "kernel")
        | .filenames[] | select(endswith(".rmeta"))')
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

compile=(rustc --edition 2024 --crate-type lib --emit metadata --error-format json
    --target "$target" -D warnings -C panic=abort --extern "kernel=$artifact"
    --out-dir "$scratch" server/tests/entry/boot/contracts.rs)
"${compile[@]}"
for contract in receiver role mapping material mapping-move material-move; do
    expected=E0616
    case "$contract" in
        *-move) expected=E0382 ;;
    esac
    if "${compile[@]}" --cfg "contract=\"$contract\"" 2>"$scratch/errors.jsonl"; then
        printf 'Boot contract %s unexpectedly compiled\n' "$contract" >&2
        exit 1
    fi
    if ! jq -es --arg expected "$expected" 'any(.[]; .level == "error" and .code.code == $expected)' "$scratch/errors.jsonl" >/dev/null; then
        cat "$scratch/errors.jsonl" >&2
        exit 1
    fi
done
printf 'Boot compiler contracts passed on %s\n' "$target"
