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
The RV64 execution interface lets a trusted Unit yield, resume and return an
end value. User-mode calls and the new Treaty ABI are under development.
Consumers of ABI 14, including existing Vanllia images, should pin their
release tag or full revision. [Releases](https://github.com/shuyra-arche/limen/releases)

## Build and test

From the repository root on x86-64 Linux, install Rust nightly and the targets.
The host tests also require a C linker.

```sh
rustup toolchain install nightly --profile minimal --component rustfmt,clippy
rustup target add --toolchain nightly riscv64gc-unknown-none-elf x86_64-unknown-uefi aarch64-unknown-uefi
cargo +nightly build --locked --workspace --target riscv64gc-unknown-none-elf
cargo +nightly build --locked --workspace --release --target riscv64gc-unknown-none-elf
cargo +nightly build --locked -p kernel --target x86_64-unknown-uefi
cargo +nightly build --locked -p kernel --target aarch64-unknown-uefi
```

```sh
cargo +nightly fmt --all -- --check
RUSTFLAGS=-Dwarnings cargo +nightly test --locked --workspace --target x86_64-unknown-linux-gnu
cargo +nightly test --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
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
