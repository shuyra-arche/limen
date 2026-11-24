# Limen tests

Test source follows its semantic owner. Host suites under `metal/` exercise the
retained bounded allocation cursor and RV64 page-table entry encoding and
atomic operations without executing privileged instructions. Each suite keeps
its own `host/Cargo.toml`. The root workspace discovers these packages.

```sh
RUSTFLAGS=-Dwarnings cargo +nightly test --locked --workspace --target x86_64-unknown-linux-gnu
cargo +nightly test --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
```

## RV64 boot

The Boot owner keeps its platform image and assertions in [target/entry/boot/](target/entry/boot/).
The target runner archives the candidate Git revision. Commit changes before
starting verification so the report identifies the source it actually ran.
Set `QEMU_RISCV64` to QEMU 10.0.x and `QEMU_OPENSBI` to OpenSBI firmware
with the debug console and system reset extensions.

```sh
bash tools/tests/discover.sh "$PWD" "" 1
cargo +nightly build --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/boot --kernel "$PWD" \
  --output /tmp/limen-boot --profile debug --harts 1
```

The scenario follows the production firmware entry and waits for the platform
image's boot assertions and actual QEMU termination. Serial output and the
runner verdict are preserved with the candidate revision. The Target workflow
discovers owner-local `suite.json` declarations and uploads their evidence.
Hardware execution and later runtime contracts require separate verification.

## RV64 Kernel Unit execution

Run [target/entry/execution/](target/entry/execution/) with the same runner and
firmware. Use a fresh output path for each profile.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/execution --kernel "$PWD" \
  --output /tmp/limen-execution --profile debug --harts 1
```

The guest verifies separate stacks in the shared Kernel Nest, Yield and resume,
exact leave receipts, ending and reaping, and reuse with a new identity. It
also exercises rejected dispatch, Busy and stale identity paths on the same
production entry and firmware.
