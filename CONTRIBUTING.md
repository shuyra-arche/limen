# Contributing

Follow the [Shuyra contribution standard](https://github.com/shuyra-arche/.github/blob/main/CONTRIBUTING.md)
and its [engineering conventions](https://github.com/shuyra-arche/.github/tree/main/docs/engineering).
The root README documents the local toolchain and verification commands.

Use a short-lived branch from `main` and the organization PR template. Keep a PR
draft while implementation or required verification is in progress. Record the
toolchain, commands, results and skipped checks. Distinguish host tests, RV64
compilation and guest execution. Core API changes require explicit source and
ABI compatibility review. Merge through a reviewed PR, using squash.

The Server's `entry` owns trusted entry and sequencing. `metal` owns hardware
mechanisms. Add Basis and Kernel Unit owners as real contracts are implemented.
A service owns its Treaty, Client and Server together. A Client may depend on
the public contract and transport. Privileged implementation belongs on the
Server side of that boundary. Keep the workspace lockfile current and preserve
source attribution and third-party notices.

Rust modules with child files use `name/mod.rs`. Test suites follow the same
layout and declare their non-root test entry points under the owning package's
`tests/` directory.
