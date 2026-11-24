# Target runner

`verify run --suite <project-directory> --kernel <repository> --output <fresh-directory>`
executes a project discovered from `suite.json`. Target projects own the
platform image, input and observable assertions. The runner manages source
capture, builds, QEMU or Cargo test execution, timeouts, process cleanup and
evidence.

A QEMU suite declares its platform binary and the complete serial lines required
after its assertions. It can also declare imported test helpers, Cargo features,
environment and distinct capture evidence. The runner builds the platform from
the suite's locked dependency graph. It negotiates QMP before starting the
guest and records the monitor transcript in `qmp.jsonl` alongside the serial
log. Success requires complete serial assertions, one guest-originated
`guest-shutdown` receipt and a clean process exit. Results distinguish
`pass`, `fail`, `timeout` and `inconclusive`.
A Cargo test suite names its package or own manifest and test target.
Adding a new project does not change Runner code.

`composition/` contains only the shared patch application mechanism. Source
patches are stored inside the test projects and protected by exact preimage
SHA-256 contracts. The disposable provider is never the working source tree.

On Linux, child processes run in isolated process groups. Leftover descendants
invalidate success; process output, build failures and cleanup receipts are
retained for diagnosis. See `../README.md` for invocation.
