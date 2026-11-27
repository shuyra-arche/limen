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
A stateful scenario uses `#![test_runner(guest::scenario)]` and an explicit
Boot callback. A collected suite uses `#![test_runner(guest::run)]` with
independent `#[test_case]` functions. Both use Cargo's discovered test targets.
The shared entry invokes the compiler-generated `collected` function exactly
once before the Boot callback; a custom entry must do the same. Scenario
images reject nonempty collections instead of silently skipping cases;
collected suites reject empty collections. Each image chooses its platform and
optional firmware preparation:

```rust
guest::entry! {
    platform: platform::Platform {
        interrupt: platform::Interrupt::Aplic,
        aclint: true,
        sstc: platform::Setting::Off,
        ..platform::Platform::native(1)
    },
    run: |boot| {
        scenario::check(boot);
        guest::finish()
    }
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
The strict-NX Module retention case owns such a loader. Owner-local
assertions remain in Entry, Basis and Metal. One explicit path to an
owner's test implementation is preferable to a registry of scenarios.

The linked `.testenv` record selects firmware, interrupt controller, Hart
count, serial topology, IOMMU and PCI devices. The Runner rejects a missing,
malformed or unsupported declaration. The kernel still discovers real hardware
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
metadata; it never classifies individual test names. On UEFI, ordinary
guests run before the extra bare target required by Module is installed.
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
The machine-specific fault test uses libtest's native ignore attribute on
other guest architectures. The full-duration timeout probe is explicitly
ignored in its own test definition; CI names neither case. The outcome Guest
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

See [Runner](runner/README.md) for the execution and evidence boundary.
