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
    --out-dir "$scratch" server/tests/basis/sync/contracts.rs)
"${compile[@]}"

reject() {
    local contract=$1 code=$2
    if "${compile[@]}" --cfg "contract=\"$contract\"" 2>"$scratch/errors.jsonl"; then
        printf 'Sync contract %s unexpectedly compiled\n' "$contract" >&2
        return 1
    fi
    if ! jq -es --arg code "$code" 'any(.[]; .level == "error" and .code.code == $code)' \
        "$scratch/errors.jsonl" >/dev/null; then
        cat "$scratch/errors.jsonl" >&2
        return 1
    fi
}

reject escape E0521
reject alias E0499
reject send E0277
reject sync E0277
printf 'Sync compiler contracts passed on %s\n' "$target"
