#!/usr/bin/env bash
# Exercise Host check scope, not target-test selection.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
git init -q
git config user.name test
git config user.email test@localhost
git commit --allow-empty -qm baseline
base=$(git rev-parse HEAD)
checks="$root/tools/tests/checks.sh"

scope() {
    local path=$1 predicate=$2
    git reset --hard -q "$base"
    mkdir -p "$(dirname "$path")"
    echo >>"$path"
    git add .
    git commit -qm change
    bash "$checks" . "$base" | jq -e "$predicate" >/dev/null
}
bash "$checks" . | jq -e '.host and .runner and .riscv64' >/dev/null
scope server/src/basis/execution.rs '.host and .riscv64 and (.runner | not)'
scope server/src/metal/platform/boot/riscv64/linker.ld '.riscv64 and ((.host or .runner) | not)'
scope server/src/metal/platform/boot/x86_64/mod.rs '((.host or .runner or .riscv64) | not)'
scope server/tests/target/tests/common-boot.rs '((.host or .runner or .riscv64) | not)'
scope server/tests/target/support/platform.rs '.runner and ((.host or .riscv64) | not)'
scope server/tests/entry/boot/fault.rs '.runner and ((.host or .riscv64) | not)'
scope treaty/src/model/address.rs '.host and .runner and .riscv64'
echo 'host check scope contracts passed'
