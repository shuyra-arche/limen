# Limen tests

Host suites under `entry/boot/` and `metal/` exercise DTB header validation,
SBI extension discovery, the bounded allocation cursor and RV64 page-table
entry encoding and atomic operations. Each suite keeps its own
`host/Cargo.toml`. The root workspace discovers these packages.

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

At the firmware entry address the test image writes a nonzero BSS sentinel
and jumps to the production `_start` with the original firmware arguments.
The callback checks that the sentinel was cleared. The runner requires the
complete serial assertions, a guest-originated QMP `guest-shutdown` event
and a clean QEMU exit. The serial log, QMP transcript and verdict are
preserved with the candidate revision. The Target workflow
discovers owner-local `suite.json` declarations and uploads their evidence.
Hardware execution and later runtime contracts require separate verification.
