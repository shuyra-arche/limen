# Target runner

`verify run --suite <project-directory> --kernel <repository> --output <fresh-directory>`
executes a project discovered from `suite.json`. Target projects own the
platform image, input and observable assertions. The runner manages source
capture, builds, QEMU or Cargo test execution, timeouts, process cleanup and
evidence.

A QEMU suite declares its platform binary and the complete serial lines required
after its assertions. It can also declare imported test helpers, Cargo features,
environment and distinct capture evidence. The runner builds the platform from
the suite's locked dependency graph and requires both complete assertions and
QEMU completion. Results distinguish `pass`, `fail`, `timeout` and `inconclusive`.
A Cargo test suite names its package or own manifest and test target.
Adding a new project does not change Runner code.

`composition/` contains only the shared patch application mechanism. Source
patches are stored inside the test projects and protected by exact preimage
SHA-256 contracts. The disposable provider is never the working source tree.

On Linux, child processes run in isolated process groups. Leftover descendants
invalidate success; process output, build failures and cleanup receipts are
retained for diagnosis. See `../README.md` for invocation.
