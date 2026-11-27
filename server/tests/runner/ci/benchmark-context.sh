#!/usr/bin/env bash
# Record comparable execution conditions, not a claim of identical host speed.
set -euo pipefail
export LC_ALL=C
: "${LIMEN_TEST_TARGET:?guest target required}"
mkdir -p .integration

# This fingerprint deliberately excludes Kernel implementation source and
# includes the workload, runner, fixtures, lockfiles and build configuration.
workload=$(git -c safe.directory="$PWD" ls-files --stage -- \
    .github/workflows/bench.yml .cargo rust-toolchain.toml Cargo.toml Cargo.lock \
    server/Cargo.toml server/build.rs \
    server/tests/qemu.version server/tests/runner server/tests/target/support \
    server/tests/target/Cargo.toml server/tests/target/Cargo.lock server/tests/target/build.rs \
    ':(glob)server/tests/target/**/Cargo.toml' \
    ':(glob)server/tests/target/**/build.rs' \
    ':(glob)server/tests/target/**/benches/**' \
    ':(glob)server/tests/metal/**/bench/**' | sha256sum | cut -d ' ' -f1)
cpu=$(awk -F: '/^(vendor_id|cpu family|model[[:space:]]|model name|stepping|microcode|flags)[[:space:]]*:/ {print}' /proc/cpuinfo | sort -u)
[[ -n "$cpu" ]]
qemu=$(command -v "qemu-system-${LIMEN_TEST_ARCH:?guest architecture required}")
qemu_hash=$(sha256sum "$qemu" | cut -d ' ' -f1)
quota=unavailable
if [[ -r /sys/fs/cgroup/cpu.max ]]; then quota=$(cat /sys/fs/cgroup/cpu.max); fi

jq -n \
    --arg target "$LIMEN_TEST_TARGET" \
    --arg profile bench \
    --arg workload_sha256 "$workload" \
    --arg qemu_sha256 "$qemu_hash" \
    --arg rustc "$(cat .integration/rustc.txt)" \
    --arg cargo "$(cat .integration/cargo.txt)" \
    --arg host_kernel "$(uname -srm)" \
    --arg host_os "$(cat /etc/os-release)" \
    --arg host_cpu "$cpu" \
    --arg host_cpus "$(nproc)" \
    --arg host_cpu_quota "$quota" \
    '{schema: 1, $target, $profile, $workload_sha256, $qemu_sha256, $rustc, $cargo,
      $host_kernel, $host_os, $host_cpu, $host_cpus, $host_cpu_quota}' \
    > .integration/benchmark-context.json
