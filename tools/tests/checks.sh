#!/usr/bin/env bash
set -euo pipefail
root=$(cd "${1:-.}" && pwd)
source "$(dirname "$0")/changes.sh"
changed_paths "$root" "${2:-}"
runner=false
rv64=false
for path in "${changed[@]}"; do
    case "$path" in
        rust-toolchain.toml) full=true ;;
        Cargo.toml|Cargo.lock|.cargo/*|server/Cargo.toml|server/build.rs|tools/tests/checks.sh|tools/tests/changes.sh|.github/workflows/check.yml) full=true ;;
        server/src/metal/boot/x86_64/*|server/src/metal/boot/aarch64/*|server/src/metal/hart/x86_64.rs|server/src/metal/hart/aarch64.rs|server/src/metal/firmware/psci.rs|server/src/metal/firmware/uefi/*) ;;
        server/src/*|server/link.x) rv64=true ;;
        server/tests/basis/sync/*) rv64=true ;;
        server/tests/runner/*|server/tests/qemu.version|server/tests/target/*/suite.json) runner=true ;;
    esac
done
manifests=()
while IFS= read -r -d '' manifest; do
    manifest=${manifest#"$root"/}
    owner=${manifest%/host/Cargo.toml}
    for path in "${changed[@]}"; do
        if [[ $path == "$owner/"* || $path == "${owner/server\/tests/server\/src}/"* ||
              ( $owner == server/tests/entry/boot && ( $path == server/src/metal/firmware/map.rs || $path == server/src/metal/firmware/uefi/loader.rs || $path == server/src/metal/firmware/devicetree/* || $path == server/src/metal/boot/console.rs || $path == server/src/metal/allocator/* || $path == server/src/metal/identity/* || $path == server/src/basis/sync/queue.rs || $path == server/src/basis/sync/turn.rs || $path == server/src/metal/hart/node.rs ) ) ||
              ( $owner == server/tests/basis/sync && $path == server/src/metal/hart/node.rs ) ]]; then
            manifests+=("$manifest")
            break
        fi
    done
done < <(find "$root/server/tests" -name target -type d -prune -o -path '*/host/Cargo.toml' -print0)
if [[ $full == true ]]; then runner=true; rv64=true; fi
printf '%s\n' "${manifests[@]}" | jq -Rsc --argjson full "$full" --argjson runner "$runner" --argjson rv64 "$rv64" '
    {full:$full, runner:$runner, rv64:$rv64, manifests:(split("\n") | map(select(length>0)))}'
