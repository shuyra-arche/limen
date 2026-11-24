# Limen

Limen is the Shuyra kernel. This development revision exposes the trusted RV64
boot prefix for OpenSBI. It establishes a boot stack and an emergency trap
stack, clears BSS, checks the supervisor handoff and device-tree header, and
passes the firmware facts to the trusted platform image. The image owns board
discovery and supplies resident stacks for cooperative Kernel Unit execution.
On the Boot Hart, a Unit can yield, resume its continuation and return an end
value. Room releases the execution occupancy after Metal confirms the leave.

Read [document-limen](https://github.com/shuyra-arche/document-limen) for the
design and [Limen documentation](docs/README.md) for the implemented boundary.
The `kernel` package lives in [server/](server/). Its [test guide](server/tests/README.md)
contains the QEMU boot and execution scenarios.

## Build and test

Run from the repository root with Rustup and an x86-64 Linux host C linker.

```sh
rustup toolchain install nightly --profile minimal --component rustfmt,clippy --target riscv64gc-unknown-none-elf
cargo +nightly fmt --all -- --check
cargo +nightly build --locked --workspace --target riscv64gc-unknown-none-elf
cargo +nightly build --locked --workspace --release --target riscv64gc-unknown-none-elf
RUSTFLAGS=-Dwarnings cargo +nightly test --locked --workspace --target x86_64-unknown-linux-gnu
cargo +nightly test --locked --manifest-path server/tests/runner/Cargo.toml --target x86_64-unknown-linux-gnu
bash tools/check-layout.sh
bash tools/check-tooling.sh
```

## Compatibility

V0 replaces ABI 14, its Client and the old platform integration. Consumers of
those interfaces, including existing Vanllia images, need to stay on their
existing [release tag](https://github.com/shuyra-arche/limen/releases) or full
revision until their provider requirements are available on the new path.
The new user ABI is a design contract under development.

See [Contributing](CONTRIBUTING.md) for verification and review requirements.

Kernel and its previous API derive from [anreonyr/sqware](https://github.com/anreonyr/sqware)
at `672d8d2deafc8067c94b6af340a5f89c6a765c9c`. Historical Client mechanisms
derive from [Execution](https://github.com/shuyra-arche/execution) at
`1b333a28bc4c34beaea3f7986a9bb6e8ab2db414`. The internal SBI module derives
from [SBI](https://github.com/shuyra-arche/sbi) at
`2dc10bafb919eb2e8f6ca37c95cfd56f9a05831d`, originally extracted from sqware's
`crates/sbi` at `c970afaf7dae1145633278253351be729b625717`.
Git history preserves original authorship and migration records. These
checkpoints and this repository provide no license grant. Preserve
third-party notices with their code.
