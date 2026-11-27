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
        server/src/metal/processor/execution/cooperative/x86_64/*|server/src/metal/processor/execution/terminal/x86_64.rs|server/src/metal/processor/execution/terminal/aarch64.rs|server/src/metal/processor/executor/entry/x86_64.rs|server/src/metal/processor/executor/entry/aarch64.rs|server/src/metal/processor/execution/cooperative/aarch64/*|server/src/metal/processor/executor/launch/x86_64.rs|server/src/metal/processor/executor/launch/aarch64.rs|server/src/metal/platform/boot/x86_64/*|server/src/metal/platform/boot/aarch64/*|server/src/metal/processor/executor/x86_64/native.rs|server/src/metal/processor/executor/aarch64/native.rs|server/src/metal/platform/firmware/psci/native.rs|server/src/metal/interrupt/x86_64/*/native.rs|server/src/metal/interrupt/aarch64/*/native.rs|server/src/metal/memory/mmu/x86_64/internal/installation.rs|server/src/metal/memory/mmu/aarch64/internal/installation.rs|server/src/metal/memory/mmu/x86_64/*/native.rs|server/src/metal/memory/mmu/aarch64/*/native.rs|server/src/metal/chrono/aarch64/counter.rs)
            ;;
        server/src/metal/platform/boot/handoff/uefi.rs|server/src/metal/platform/firmware/uefi/loader.rs|server/src/metal/processor/description/aarch64/*|server/src/metal/processor/description/x86_64/*|server/src/metal/processor/execution/profile/*|server/src/metal/platform/firmware/psci/mod.rs|server/src/metal/platform/firmware/treaty/psci.rs|server/src/metal/interrupt/x86_64/*|server/src/metal/interrupt/aarch64/*|server/src/metal/memory/mmu/x86_64/*|server/src/metal/memory/mmu/aarch64/*|server/src/metal/chrono/aarch64/description.rs)
            host=true ;;
        server/src/metal/platform/boot/riscv64/linker.ld)
            rv64=true ;;
        server/src/*)
            host=true; rv64=true ;;
        server/tests/metal/sync/contracts.*|server/tests/entry/boot/contracts.*)
            rv64=true ;;
        server/tests/runner/*|server/tests/qemu.version|server/tests/entry/boot/fault.rs|server/tests/target/support/platform.rs)
            runner=true ;;
        server/tests/target/*)
            ;;
        server/tests/*)
            host=true ;;
    esac
done
if [[ $full == true ]]; then host=true; runner=true; rv64=true; fi
jq -cn --argjson host "$host" --argjson runner "$runner" --argjson rv64 "$rv64" '
    {$host, $runner, $rv64}'
