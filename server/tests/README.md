# Limen tests

The workspace defaults to the native Host target. Ordinary contracts use Rust's
native harness. Owner-local assertions live under `tests/entry`, `tests/basis`
and `tests/metal`. Private unit tests are declared by the production owner;
public Host contracts enter through `server/tests/contracts.rs`.

Run from the repository root to select the pinned toolchain.

```sh
RUSTFLAGS=-Dwarnings cargo test --locked --workspace
cargo test --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
```

Use Cargo's module-path filters for Host contracts:

```sh
cargo test --locked -p kernel --test contracts -- metal::firmware::acpi
cargo test --locked -p kernel --test contracts -- metal::firmware::devicetree
```

The Identity compiler fixture and independent Module ELF Host package retain
their own compilation boundaries. They are not QEMU scenarios.

## Native guests

The target workspace shares one lockfile and has three genuine build
boundaries: portable guest code (`kernel-guest`), guest code that requires
one specific ISA (`riscv64-guest`, `aarch64-guest`, `x86_64-guest`), and
independently built ELF images (`module-guest`). Cargo discovers integration
targets within each execution package's `tests/` directory. Test names
express the behavior, not a compile filter. Adding a test to an existing
package needs no manifest entry or CI change.

The `guest-support` library supplies the machine declaration, standard
startup, compiler-collected harness, panic handler and completion transport.
Standard guest roots use `#![test_runner(harness)]` and
`#![reexport_test_harness_main = "collected"]`. The entry macro supplies the
harness and completion path according to the declared behavior:
`test: collected,` for independent `#[test_case]` functions,
`test: <Boot callback>,` for a stateful scenario, and
`bench: <Boot callback>,` for measurement. Scenario images reject
nonempty collections; collected suites reject empty collections.
The shared entry invokes the compiler-generated `collected` exactly
once, then invokes the selected callback if one is provided. Returning
normally completes the guest; a terminal callback retains its explicit
completion responsibility. A custom loader must enforce the same collection
and completion contract. Each image chooses its platform and optional
firmware preparation:

```rust
guest::entry! {
    platform: platform::Platform {
        interrupt: platform::Interrupt::Aplic,
        aclint: true,
        sstc: platform::Setting::Off,
        ..platform::Platform::native(1)
    },
    test: scenario::check,
}
```

Use a separate image when initial machine state, hardware topology or an
unretired resource requires isolation. Keep ordered preconditions, mutations
and assertions together in the scenario; do not turn dependent steps into
collected cases. Cases within a collected suite must be order-independent and
restore the state they change. The shared Boot, migration and synchronization
images compile their real backend with Rust's target configuration. Special
loaders can use the library's declaration and completion without adopting its
standard entry.
The strict-NX Module retention case owns such a loader. The completion
transport fixture explicitly uses `test: collected => check,` because it
verifies both compiler collection and terminal behavior. Owner-local
assertions remain in Entry, Basis and Metal. One explicit path to an
owner's test implementation is preferable to a registry of scenarios.

Shared behavior is not automatically execution infrastructure. Boot's
foundation contract and RAM/Pool preparation live under `entry/boot/`;
the latter supplies both Boot and Basis lifecycle scenarios. The migration
contract and its SBI/UEFI admission adapters live together under
`basis/execution/migration/`. Native clock observations and interrupt
consumer contracts belong to Chrono and Interrupt respectively.
The target workspace retains compilation entries, the guest declaration,
startup and completion, and reusable machine fixtures.

The linked `.testenv` record describes the selected execution purpose
independently of the machine configuration (firmware, interrupt controller,
Hart count, serial topology, IOMMU and PCI devices). The Runner rejects a
missing, malformed or unsupported declaration, including a mismatch between
the requested purpose and the compiled callback, before starting QEMU. The kernel still discovers real hardware
through the original firmware inputs. Debug and Release use the same rules.

Install the exact QEMU version from [qemu.version](qemu.version), the required
firmware, and the selected Rust guest target. Only `module-guest` needs the
additional bare target for its independent AArch64 or x86_64 ELF images;
ordinary UEFI guests can compile without it.

```sh
rustup target add riscv64gc-unknown-none-elf aarch64-unknown-uefi x86_64-unknown-uefi
# Only for Module guests on the corresponding UEFI architecture:
rustup target add aarch64-unknown-none x86_64-unknown-none
cargo build --locked --manifest-path server/tests/runner/Cargo.toml \
    -p kernel-verification --target x86_64-unknown-linux-gnu
runner="$PWD/server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify"
```

Run a single test with native Cargo selection and a fresh evidence root:

```sh
target=riscv64gc-unknown-none-elf
config="target.$target.runner=['$runner','native','--evidence-root','/tmp/limen-guest']"
cargo -Z panic-abort-tests --config "$config" test --locked \
    --manifest-path server/tests/target/Cargo.toml -p riscv64-guest \
    --target "$target" --test aplic
```

Guest selection is at the image boundary (`-p`, `--test`, `--tests`). Unlike
Host libtest, the bare-metal harness does not implement name filters for
individual cases; selecting an image runs its entire contract. Do not pass
libtest case filters after `--` to the native runner.

Add `--release` for the optimized image and select a fresh evidence root.
For independent Module images select `-p module-guest --test lifecycle`.
For all ordinary guests, select `-p kernel-guest -p riscv64-guest --tests`
(or the actual ISA-specific package). Target CI uses `--tests` for the
selected execution packages. Check verifies package coverage with Cargo
metadata; it never classifies individual test names. Each architecture/profile
job builds the runner once. Host observers, ordinary guests and Module
guests run as separate required steps with Cargo's `--no-fail-fast`.
A failing stage does not suppress another stage whose prerequisites are
available; any required failure still fails the job. A Cargo build failure
can prevent execution of targets in that invocation. On UEFI, only Module
requires the additional bare target, installed after the ordinary guest step.
The artifact retains per-stage Cargo logs and Actions step outcomes under
`.integration/stages/` (not a substitute for individual guest verdicts),
ordinary guest evidence under `.integration/guests/`, Module evidence under
`.integration/module/`, and the observer-owned records. A skipped or
cancelled stage is not evidence of a pass.
Ordinary completion requires QEMU exit 97 and successful process settlement.
The Runner records process facts separately from the native verdict. Guest
panic, ordinary shutdown, reset and trap are failures; deadline expiry is a
timeout, and incomplete execution or retirement evidence is inconclusive.
Serial output is diagnostic, never a generic success oracle.

The Runner uses the default OpenSBI, OVMF and AAVMF locations or the
`QEMU_OPENSBI`, `OVMF_DIR` and `AAVMF_DIR` overrides. The strict-NX Module
images explicitly choose the corresponding firmware variant. No fallback is
allowed if that firmware is absent. Only run trusted test images: native
semihosting is enabled to report completion.

## Hardware and resource contracts

| Owner | Native test entry / contract |
| --- | --- |
| Boot | `kernel-guest:boot-handoff`: original SBI/UEFI handoff, major/minor stacks, firmware reservation, memory admission and Foundation |
| Execution | `kernel-guest:execution-lifecycle`: creation, admission, cancellation, exact departure, return, reap and stack reuse on all architectures |
| Migration | `kernel-guest:execution-migration`: cross-Hart continuation, stack, ABI and stale receipts |
| Sync | `kernel-guest:sync`: collected cases and native synchronization contracts, plus the riscv64 enabled-interrupt contract |
| Participation | `riscv64-guest:execution-participation`: independent occupancies, exact switch-boundary integer registers, Turn/Snapshot, three admitted participants, fourth rejected |
| Interrupt | `riscv64-guest:aplic,plic-sstc,ipi`, `aarch64-guest:gic`, `x86_64-guest:apic-failure,apic-timer,ioapic,ipi`: real sources, claims, timer effects and launch failures |
| CPU translation | `aarch64-guest:paging`, `x86_64-guest:paging`, `module-guest:lifecycle` on riscv64: retained roots, exact context and actual translation |
| IO translation | `riscv64-guest:iommu`, `aarch64-guest:smmu`, `x86_64-guest:amdvi,vtd`: actual EDU DMA, binding, withdrawal and completion |
| Module | `module-guest:lifecycle`: UEFI strict-NX independent ELF, use draining and retention without withdrawal; riscv64 Sv39/Sv48/Sv57, two Harts, dependency-ordered retirement and reuse |

Host models and compiler contracts do not establish native hardware authority.
Hardware fixtures exercise actual observations and retained resources. They
do not replace the future runtime Trap or driver Sign lifecycle.

## Host-assisted observations

The `machine-observer` package in `runner/observe/` uses ordinary Host
libtest and the shared runner library. Its tests require a selected guest
architecture, profile and machine tools. The default runner Host tests do
not select this package. Cargo discovers all its integration targets with
`--tests`. A small build script derives the observed ISA from the actual
`LIMEN_TEST_TARGET` triple, independently of the Host compiler target.
The Boot Fault suite compiles the common observation and diagnostic tests on
every Host. Its riscv64 backend is compiled only for a selected riscv64 guest
on Unix; other guest architectures do not register that case. This is a
firmware-entry-specific BSS contract, not proof of UEFI early fault behavior.
The full-duration timeout probe remains explicitly ignored in its own test
definition because of its cost; CI names neither case. The outcome Guest
owns transport and nested compiler-collection probes; ordinary synchronization
tests do not contain injected harness failures.

A terminal boot fault cannot execute a normal guest test assertion. The
Boot-owned Host test compiles the ordinary RV64 boot guest via Cargo's JSON
artifact messages and verifies that image succeeds. A second run starts
paused, using GDB over a private Unix socket. GDB stops at the actual linked
entry, writes initialization witnesses into the first and last BSS words,
and watches the first word. Once it has been cleared while the last word
still holds its witness, the test injects an illegal instruction at the
current PC in guest memory. It observes the actual exception registers at the
installed vector, then detaches so the original terminal handler runs.
The Host test compares the complete terminal diagnostic with that independent
machine observation and requires a guest-originated QMP shutdown. The pinned
OpenSBI syscon path does not independently expose the SBI reset reason.
Install `gdb-multiarch` (or set `GDB_MULTIARCH`) for this test.

```sh
LIMEN_TEST_TARGET=riscv64gc-unknown-none-elf LIMEN_TEST_PROFILE=debug \
    cargo test --locked --manifest-path server/tests/runner/Cargo.toml \
    -p machine-observer --target x86_64-unknown-linux-gnu \
    --test boot-fault -- --test-threads=1
```

The unchanged ELF's hash, machine observation, controller transcript and run
receipts establish the fault test. It does not match instruction encodings,
modify production startup code or produce a patched ELF. Unlike ordinary
guests, this test asserts external terminal evidence because its expected
outcome prevents in-guest completion.

## Native benchmarks

Benchmark bodies live in the owning domain under `server/benches/`. The
existing target-only Guest packages supply dedicated entry points when no
matching Test image exists. A common entry can instead declare one platform
and distinct `test:` and `bench:` behaviors. The package's `bench` feature
selects measurement only where the entry declares it and its optional
`#[cfg(...)]` condition holds. A shared entry groups each behavior's
callback or collection declaration and its ordinary Rust items in one
block. Only the selected block contributes items at crate scope. The
capability condition therefore governs its dependencies as well as its
behavior. Single-purpose entries retain the direct callback or collection
form, with ordinary module declarations at crate scope. Sync uses:

```rust
guest::entry! {
    platform: platform::Platform::native(4),

    test: {
        collected;
        mod contracts;
    },

    #[cfg(target_arch = "riscv64")]
    bench: {
        measurement::check;
        #[path = "../../../../benches/metal/sync/shared/mod.rs"]
        mod measurement;
    },
}
```

On other architectures the same entry continues to declare and compile
its Test behavior. Both use the same linker, startup, machine preparation,
process settlement and completion contract. They execute in separate boots.
No separate benchmark workspace or lockfile is required.
The first target, `riscv64-guest:clock`, establishes a measurement transport
over the admitted RISC-V timebase. The Sync owner uses two machine topologies:

| Image | Active Harts | Measurement series |
| --- | --- | --- |
| `sync-local` | 1 | `leaf-local`, `turn-local` |
| `kernel-guest:sync` | 4 | `leaf-shared`, `turn-shared` |
| `kernel-guest:sync` | 2 of 4 | `atomic-alternating`, `leaf-alternating`, `turn-alternating` |

The local workload increments a protected word on a one-Hart machine.
The shared workload starts three actual minor Harts, then measures batches
of equal work on all four Harts. Every batch must retire all participating
operations and match the exact aggregate count. The alternating workload
passes an explicit atomic baton between two Harts; the other two Harts
remain inactive in the fixture. These durations include the baton and
synchronization costs, not an isolated measurement of MCS queue handoff.
The atomic alternating series provides a comparison, not a value to
subtract mechanically.

```sh
target=riscv64gc-unknown-none-elf
config="target.$target.runner=['$runner','bench','--evidence-root','/tmp/limen-bench']"
cargo -Z panic-abort-tests --config "$config" bench --locked \
    --manifest-path server/tests/target/Cargo.toml -p riscv64-guest \
    --target "$target" --bench clock
```

Use a fresh evidence root. Replace `--bench clock` with `--bench sync-local`
or `--benches` to select dedicated measurement targets.

The shared four-Hart Sync entry retains its ordinary Test contract. To
select its measurement callback on riscv64, use:

```sh
cargo -Z panic-abort-tests --config "$config" bench --locked \
    --manifest-path server/tests/target/Cargo.toml -p kernel-guest \
    --features bench --target "$target" --test sync
```

The feature is necessary because Cargo's custom harness does not supply
a separate compile-time benchmark configuration. Selecting a Test image
through the bench runner, or the reverse, is a purpose mismatch.
For the complete selected architecture, Cargo discovers and compiles both
integration and dedicated benchmark targets. The runner selects only images
whose compiled declaration specifies the benchmark purpose:

```sh
mkdir -p /tmp/limen-set
cargo -Z panic-abort-tests bench --locked --no-run --message-format=json \
    --manifest-path server/tests/target/Cargo.toml \
    -p kernel-guest -p riscv64-guest \
    --features kernel-guest/bench,riscv64-guest/bench \
    --target "$target" --tests --benches > /tmp/limen-set/artifacts.jsonl
"$runner" bench-set --artifacts /tmp/limen-set/artifacts.jsonl \
    --evidence-root /tmp/limen-set
```

The runner records the exact candidate set and image digests in
`selection.json` before starting any machine. Test-purpose integration
images are not selected, and produce no verdict. A dedicated benchmark
target declaring test purpose, an incomplete Cargo build, or an empty
benchmark selection is an error. All selected benchmarks are attempted
even if an earlier measurement fails. This discovery costs compilation of
the candidate Test images, but requires neither a target-name list nor
an additional source declaration.

The clock and local Sync benchmarks use 1,024 warmup iterations and nine
batches of 16,384 operations. Each shared Sync series uses 512 warmup
iterations and nine batches of 4,096 operations per participant.
The measurement record includes the observing Hart, admitted frequency,
number of series, active participant count, aggregate warmup and measured
operation counts, and raw batch durations in nanoseconds. All series must
be present, distinct and complete. Reporting occurs after measurement.

Duration includes loop and boundary ordering overhead. Shared durations
also include the start gate and completion wait. Divide the aggregate
operations by elapsed time for batch throughput; do not interpret that
quotient as individual acquisition latency or a physical hardware cycle
count. The clock smoke benchmark is not a calibrated estimate of one
instruction's cost.

The bench runner first requires native completion and clean process
retirement, then checks that the declared measurement series is complete.
Only then is `measurement.json` written. Missing or malformed measurements
fail the benchmark invocation even if the guest completion verdict is pass.
QEMU data are emulator observations, not physical hardware results; no
performance threshold is a correctness criterion. The independent Bench
workflow builds both kinds of Cargo targets and executes the discovered
benchmark-purpose images with Cargo's optimized bench profile on riscv64.
It has separate triggers, concurrency, job results and evidence artifacts.
It runs automatically on pull requests changing benchmark code, guest
integration entries (which can provide either purpose), or shared build
and execution facilities. Ordinary kernel changes and pushes do not
automatically run measurements; select a revision
with workflow dispatch once the workflow is on the default branch.
Target CI runs correctness tests only. Both workflows
share the pinned QEMU and Rust preparation scripts in `runner/ci/`, but
neither workflow depends on the other's verdict. Check still validates
shared infrastructure, including Host tests of measurement parsing.
Other architecture measurements require their own admitted or explicitly
calibrated clocks.

### Descriptive comparison

Each completed image writes `summary.json` alongside its raw measurement.
The Bench workflow records `benchmark-context.json` with shared execution
inputs, compiler, profile, target, QEMU binary digest, and observed Host
CPU and OS conditions. Each benchmark image records its own `workload.json`
from Cargo's Rust dependency-information file, including the source hashes
of that image's entry and owner-local measurement modules. Shared inputs
remain conservative, but unrelated benchmark images do not share a
workload identity. Record from the same unchanged checkout as the build.

Extract two successful Bench artifacts and run:

```sh
"$runner" compare /path/to/baseline /path/to/candidate
```

Comparison first requires complete artifact evidence for the exact selected
image set, including matching image digests, and compatible shared
execution context. It compares only matching series with equal local
workload inputs, firmware, platform, sample count and other series
definitions. Added, removed or changed workloads appear under
`uncompared` without a ratio. Series in the same image share an identity
because their execution can affect one another. No comparable series is
a failed comparison. Statistics are recomputed from raw measurements;
candidate-to-baseline median duration ratios remain descriptive.
Different source and image revisions are allowed and identified. Equal
recorded conditions do not establish identical host scheduling or CPU
frequency. These are descriptive observations, not statistical significance
or a performance gate.

See [Runner](runner/README.md) for the execution and evidence boundary.
