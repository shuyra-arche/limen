# Limen

Limen is the Shuyra kernel. A trusted platform image links the Rust `kernel`
library and supplies the firmware inputs and board resources. Limen takes over
the boot CPU, validates those inputs, and calls the image with a `Boot` value.
The boot CPU is the *major*. The image can then start a *minor* on its own stack.

## What runs today

| QEMU target | Firmware | Verified behavior |
| --- | --- | --- |
| RV64 virt | OpenSBI | Major and minor startup, cooperative Kernel Unit execution, yield and migration between two harts |
| x86_64 q35 | UEFI (OVMF) | Major and minor startup, final memory map, kernel IDT and xAPIC startup |
| AArch64 virt | UEFI (AAVMF) | Major and minor startup, retained device tree, EL1 vectors and PSCI startup |

Each target uses the same Rust boot handoff and terminal exception reporter.
The build selects a concrete boot protocol and the available machine mechanisms.
Retained firmware descriptions are independent of the selected instruction set;
ACPI and Device Tree absence is represented by the actual handoff, not by ISA.
The RV64 execution interface lets a trusted Unit yield, resume and return an
end value. User-mode calls and the new Treaty ABI are under development.
Consumers of ABI 14, including existing Vanllia images, should pin their
release tag or full revision. [Releases](https://github.com/shuyra-arche/limen/releases)

## Build and test

Use Rustup from the repository root on x86-64 Linux. The toolchain is fixed in
[`rust-toolchain.toml`](rust-toolchain.toml). The RV64 and AArch64 early exception
entry points currently need nightly function alignment. The host tests also
require a C linker. Ordinary Cargo commands use the host target; kernel and
QEMU guest builds must select their hardware target explicitly. The RV64 flags
in `.cargo/config.toml` still apply to explicit RV64 builds.

The supported kernel image profiles are `riscv64gc-unknown-none-elf` with the
SBI handoff, and `x86_64-unknown-uefi` or `aarch64-unknown-uefi` with the UEFI
handoff. Other targets select the portable library. The RV64 provider exports
its [ELF linker script](server/src/metal/platform/boot/riscv64/linker.ld) to
the image crate, which supplies the load address, readable DTB extent and
resident major CPU materials. UEFI images pass their own resident major
materials at the firmware handoff. [CPU boot](docs/boot.md)

```sh
rustc -Vv
rustup component add rustfmt clippy
rustup target add riscv64gc-unknown-none-elf x86_64-unknown-uefi aarch64-unknown-uefi
cargo build --locked --workspace --target riscv64gc-unknown-none-elf
cargo build --locked --workspace --release --target riscv64gc-unknown-none-elf
RUSTFLAGS=-Dwarnings cargo build --locked -p kernel --target x86_64-unknown-uefi
RUSTFLAGS=-Dwarnings cargo build --locked -p kernel --target aarch64-unknown-uefi
```

```sh
cargo fmt --all -- --check
RUSTFLAGS=-Dwarnings cargo test --locked --workspace
cargo test --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
bash tools/check-layout.sh
bash tools/check-tooling.sh
```

The target tests link the library into bootable guest images. The test guide
lists the pinned QEMU version, firmware and commands for each scenario.
[Run the QEMU tests](server/tests/README.md)

## Documentation

- [Implementation guide](docs/README.md) — boot, platform contracts, exceptions and Kernel Unit execution.
- [Document Limen](https://github.com/shuyra-arche/document-limen) — architecture and Treaty design.
- [Contributing](CONTRIBUTING.md) — project conventions, verification and review.
- [Source history](docs/history.md) — upstream revisions and attribution.
