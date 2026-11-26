#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

build=$(cargo build --locked -p limen-treaty --message-format json)
artifact=$(jq -er 'select(.reason == "compiler-artifact" and .target.name == "limen_treaty")
    | .filenames[] | select(endswith(".rmeta"))' <<<"$build")
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

compile=(rustc --edition 2024 --crate-type lib --emit metadata --error-format json
    -D warnings -C panic=abort --extern "limen_treaty=$artifact"
    --out-dir "$scratch" treaty/tests/compiler/coordinates.rs)
"${compile[@]}"
for contract in compare substitute measure range; do
    if "${compile[@]}" --cfg "contract=\"$contract\"" 2>"$scratch/errors.jsonl"; then
        printf 'Coordinate contract %s unexpectedly compiled\n' "$contract" >&2
        exit 1
    fi
    if ! jq -es 'any(.[]; .level == "error" and .code.code == "E0308")' "$scratch/errors.jsonl" >/dev/null; then
        cat "$scratch/errors.jsonl" >&2
        exit 1
    fi
done
printf 'Treaty coordinate compiler contracts passed\n'
