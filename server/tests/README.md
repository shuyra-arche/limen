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

All hardware tests use the same `kernel-guest` package in `target/`.
Cargo discovers the images in `target/tests/`; adding an image does not
require a Cargo manifest entry. Owner-local code contains the assertions.
A guest entry composes those assertions with the original Boot handoff.

A target name begins with `common-`, the target architecture, or `uefi-`
where the image requires the UEFI ABI. These names restrict compilation, not
QEMU hardware configuration. Every image declares its own platform in Rust:

```rust
platform::declare!(platform::Platform {
    interrupt: platform::Interrupt::Aplic,
    aclint: true,
    sstc: platform::Setting::Off,
    ..platform::Platform::native(1)
});
```

The linked `.testenv` record selects firmware, interrupt controller, Hart
count, serial topology, IOMMU and PCI devices. The Runner rejects a missing,
malformed or unsupported declaration. The kernel still discovers real hardware
through the original firmware inputs. Debug and Release use the same rules.

Install the exact QEMU version from [qemu.version](qemu.version), the required
firmware, and the Rust targets. AArch64 and x86_64 UEFI guests also require
their bare target to build the independent module ELF.

```sh
rustup target add riscv64gc-unknown-none-elf aarch64-unknown-uefi \
    aarch64-unknown-none x86_64-unknown-uefi x86_64-unknown-none
cargo build --locked --manifest-path server/tests/runner/Cargo.toml \
    --target x86_64-unknown-linux-gnu
runner="$PWD/server/tests/runner/target/x86_64-unknown-linux-gnu/debug/verify"
```

Run a single test with native Cargo selection and a fresh evidence root:

```sh
target=riscv64gc-unknown-none-elf
config="target.$target.runner=['$runner','native','--evidence-root','/tmp/limen-guest']"
cargo -Z panic-abort-tests --config "$config" test --locked \
    --manifest-path server/tests/target/Cargo.toml -p kernel-guest \
    --target "$target" --test riscv64-aplic
```

Add `--release` for the optimized image and select a fresh evidence root.
The Target workflow runs each architecture and profile, using Cargo's
`--test 'common-*'`, architecture and UEFI filters, not a list of test names.
Check validates the compilation groups of all Cargo-discovered guest targets;
an unclassified target is an error rather than a silently skipped test.
Ordinary completion requires QEMU exit 97 and successful process settlement.
Guest panic, ordinary shutdown, reset, trap and timeout are failures.
Serial output is diagnostic, never a generic success oracle.

The Runner uses the default OpenSBI, OVMF and AAVMF locations or the
`QEMU_OPENSBI`, `OVMF_DIR` and `AAVMF_DIR` overrides. The strict-NX Module
images explicitly choose the corresponding firmware variant. No fallback is
allowed if that firmware is absent. Only run trusted test images: native
semihosting is enabled to report completion.

## Hardware and resource contracts

| Owner | Native test entry / contract |
| --- | --- |
| Boot | `riscv64-boot-handoff`, `uefi-boot`: original handoff, major/minor stacks, firmware reservation, memory admission and Foundation |
| Execution | `common-execution-lifecycle`: creation, admission, cancellation, exact departure, return, reap and stack reuse on all architectures |
| Migration | `riscv64-execution-migration`, `uefi-execution-migration`: cross-Hart continuation, stack, ABI and stale receipts |
| Participation | `riscv64-execution-participation`: independent occupancies, exact switch-boundary integer registers, Turn/Snapshot, three admitted participants, fourth rejected |
| Interrupt | `riscv64-aplic`, `riscv64-plic-sstc`, `riscv64-ipi`, `aarch64-gic`, `x86_64-apic-*`, `x86_64-ioapic`: real sources, claims, timer effects and launch failures |
| CPU translation | `aarch64-paging`, `x86_64-paging`, `riscv64-module-retirement`: retained roots, exact context and actual translation |
| IO translation | `riscv64-iommu`, `aarch64-smmu`, `x86_64-amdvi`, `x86_64-vtd`: actual EDU DMA, binding, withdrawal and completion |
| Module | `uefi-module-retention`: strict-NX independent ELF, use draining and retention without withdrawal; `riscv64-module-retirement`: Sv39/Sv48/Sv57, two Harts, dependency-ordered retirement and reuse |

Host models and compiler contracts do not establish native hardware authority.
Hardware fixtures exercise actual observations and retained resources. They
do not replace the future runtime Trap or driver Sign lifecycle.

## Terminal observations

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
LIMEN_TEST_PROFILE=debug cargo test --locked \
    --manifest-path server/tests/runner/Cargo.toml \
    --target x86_64-unknown-linux-gnu --test contracts boot_fault:: \
    -- --ignored --test-threads=1
```

The unchanged ELF's hash, machine observation, controller transcript and run
receipts establish the fault test. It does not match instruction encodings,
modify production startup code or produce a patched ELF. Unlike ordinary
guests, this test asserts external terminal evidence because its expected
outcome prevents in-guest completion.

See [Runner](runner/README.md) for the execution and evidence boundary.
