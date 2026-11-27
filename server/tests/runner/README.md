# Target runner

`verify run --suite <project-directory> --kernel <repository> --output <fresh-directory>`
executes a project discovered from `suite.json`. Target projects own the
platform image, input and observable assertions. The runner manages source
capture, builds, QEMU or Cargo test execution, timeouts, process cleanup and
evidence. QEMU boots use four virtual CPUs. Each suite checks the number of
CPUs that participate in its scenario through the guest's observations.

A QEMU suite declares its platform binary and the complete literal serial
lines required after its assertions. It can also declare imported test
helpers, Cargo features, environment, distinct capture evidence and hardware.
`run.hardware` accepts optional `machine`, `cpu` and `devices` entries.
Each component has a `name` and optional string `options` map. A suite with
no hardware settings boots RV64 `virt` with the `rv64` CPU and no extra devices.
The runner records the exact QEMU component arguments. It builds the locked
dependency graph, negotiates QMP before starting the guest, and saves `qmp.jsonl`
alongside the serial log. Success requires complete serial assertions, one
guest-originated `guest-shutdown` receipt and a clean process exit.

A `board-boot` suite can declare a shared `run.launcher` script relative to
`server/tests/target/`. Its `run.launch` entries name the machine options,
devices, features and expected assertions for each selected board. The
launcher receives the board, profile, evidence directory, Hart count, suite
ID and archived suite manifest. It records `verdict.json` only after checking
the board's guest assertions and completion evidence. The runner requires a
clean launcher exit and a matching `pass` receipt for success.

Each run records `result.json` with its verdict and the results of individual
attempts. The batch preserves `pass`, `fail`, `timeout` and `inconclusive`.
A Cargo test suite names its package or own manifest and test target.
Adding a new project does not change Runner code.

`composition/` contains only the shared patch application mechanism. Source
patches are stored inside the test projects and protected by exact preimage
SHA-256 contracts. The disposable provider is never the working source tree.

On Linux, child processes run in isolated process groups. Leftover descendants
invalidate success; process output, build failures and cleanup receipts are
retained for diagnosis. See `../README.md` for invocation.
