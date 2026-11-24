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
with the debug console and system reset extensions. The cross-Hart scenario
also uses the hart state management extension.

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
complete serial assertions, one QMP `SHUTDOWN` event with `guest=true` and
`reason=guest-shutdown`, and a clean QEMU exit. The serial log, QMP transcript
and verdict are preserved with the candidate revision.

The Target workflow runs affected owner-local `suite.json` scenarios at their
declared Hart counts and uploads their evidence. Manual runs select one or four
Harts and fail when the selected count has no matching suite.

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
also exercises rejection before occupancy, Busy, stale identity and dormant
cancellation. A cancelled Unit releases its slot under a new generation
without executing its entry.

Run [target/entry/execution-harts/](target/entry/execution-harts/) to exercise
two participating Harts on a four-Hart QEMU machine.

```sh
server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify run \
  --suite server/tests/target/entry/execution-harts --kernel "$PWD" \
  --output /tmp/limen-execution-harts --profile debug --harts 4
```

The guest starts the secondary through OpenSBI and coordinates the two Harts
through a real contention and handoff. It checks that the idle secondary skips
a Ready Unit pinned to the primary. It then verifies independent occupancies,
rejection before the original leave is settled, continuation migration after
settlement, and rejection of a late original occupancy after re-entry. A
test-only linker wrapper sets distinct `s0`–`s11` values for each saved
frame and checks their restoration at the production switch boundary,
including the call that resumes on the other Hart.
