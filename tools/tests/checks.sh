#!/usr/bin/env bash
# Select the workspace host suites, the verification runner, and RV64 contracts.
set -euo pipefail
root=$(cd "${1:-.}" && pwd)
source "$(dirname "$0")/changes.sh"
changed_paths "$root" "${2:-}"
host=false
runner=false
rv64=false
for path in "${changed[@]}"; do
    case "$path" in
        rust-toolchain.toml|Cargo.toml|Cargo.lock|treaty/*|.cargo/*|server/Cargo.toml|server/build.rs|tools/tests/checks.sh|tools/tests/changes.sh|.github/workflows/check.yml)
            full=true ;;
        server/src/metal/boot/x86_64/*|server/src/metal/boot/aarch64/*|server/src/metal/hart/x86_64.rs|server/src/metal/hart/aarch64.rs|server/src/metal/firmware/psci.rs|server/src/metal/firmware/uefi/mod.rs|server/src/metal/firmware/arm.rs)
            ;;
        server/src/metal/firmware/uefi/loader.rs|server/src/metal/firmware/acpi/*)
            host=true ;;
        server/src/*|server/link.x)
            host=true; rv64=true ;;
        server/tests/metal/sync/contracts.*|server/tests/entry/boot/contracts.*)
            rv64=true ;;
        server/tests/runner/*|server/tests/qemu.version|server/tests/target/*/suite.json)
            runner=true ;;
        server/tests/metal/*|server/tests/entry/*)
            host=true ;;
    esac
done
if [[ $full == true ]]; then host=true; runner=true; rv64=true; fi
jq -cn --argjson host "$host" --argjson runner "$runner" --argjson rv64 "$rv64" '
    {$host, $runner, $rv64}'
