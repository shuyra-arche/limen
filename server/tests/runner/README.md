# Native target runner

Cargo invokes `verify native --evidence-root <fresh-root> <image>` for each
selected executable. The Runner reads the image's bounded `.testenv`
description and the ELF/PE architecture. It constructs QEMU from those
inputs, without recognizing the test name or compiling the guest.

The exact QEMU version is checked. Ordinary success requires the dedicated
exit status 97 and settled child processes. The guest communicates assertion
failure through a distinct exit status. Ordinary firmware shutdown, panic,
timeout and reset are not success. Stdout is retained only as diagnostics.

Each image has its own fresh evidence directory with the platform and image
hash, firmware hashes, tool identity, and process output. The executor writes
`process.json` with the observed termination, execution error and cleanup
receipt. Only the native test boundary writes `result.json`, with a
`pass`, `fail`, `timeout` or `inconclusive` verdict and reason. Incomplete
execution or retirement evidence cannot become a pass. A reached deadline is
recorded as a timeout even if cleanup remains uncertain; inspect the process
receipt separately. Missing or malformed receipts are not successful runs.
Host observers own their assertions; a successful process exit alone is not
a guest verdict. UEFI variable files are copied before use. CI records the
checked-out source revision and compiler. No test changes the checked-out source.
Use a fresh evidence root for a repeated run.

## Responsibilities

- `cargo` decodes Cargo's successful JSON build stream into native harness
  artifacts; both benchmark selection and the Host observers consume these
  actual build products.
- `platform` decodes and admits the executable's machine declaration without
  preparing firmware or starting a process.
- `machine` prepares the admitted QEMU command and its per-run boot material;
  ordinary native tests and Host-controlled observers use the same preparation.
  Its boot module owns SBI and UEFI launch differences.
- `process` owns the deadline, child lifecycle and retirement receipt.
- `qmp` owns the control connection and its shutdown receipt or observation error.
- `native` interprets the ordinary guest completion contract. Host observers
  interpret their own contracts, not a generic exit status.
- `bench` reuses native completion and checks a bounded set of distinct,
  complete measurement series; it stores measurements separately from the
  execution verdict.
- `evidence` supplies shared storage operations without deciding receipt meaning.

`--observe` additionally records QMP over an isolated Unix socket. This does
not turn an unexpected exit into success; a Host test may explicitly inspect
the terminal receipts for a negative test. `verify symbolize <elf> <serial>`
resolves observed terminal PCs without changing a verdict.

Linux execution uses isolated process groups and a deadline. Unexpected
descendants invalidate success. Semihosting is enabled for trusted guest
completion; never run an untrusted test image with this runner.

See [tests](../README.md) for Cargo commands and test ownership.

An externally controlled observation negotiates QMP without issuing its own
continue command. The supplied controller alone starts the paused machine and
is supervised by the same bounded process executor.

The `kernel-verification` package owns the executable and the shared Host
library. The `machine-observer` workspace member depends on that library;
it owns the integration roots that observe actual guests. The observed
architecture is derived from the selected guest target triple, not from
the Host ISA or a test name. The observation package is deliberately not
a default workspace member: ordinary Host tests need no installed emulator.
Target CI runs all its integration targets with native libtest.

Cargo's benchmark invocation selects `verify bench --evidence-root <fresh-root>
<image> --bench`. A completed guest with incomplete measurements is a failed
benchmark invocation, not a usable timing result. The raw serial transcript
and the native execution receipt remain available for investigation.

The independent Bench workflow uses Cargo's `--tests --benches --no-run
--message-format=json` to discover candidate executables, then invokes
`verify bench-set` on the successful artifact stream. Only images declaring
benchmark purpose are selected. Test-only integration images receive no
execution verdict; malformed declarations, incompatible dedicated benchmark
targets and empty selections fail. The exact selected set is recorded in
`selection.json`, and individual failures do not stop other selected images.
Target CI selects ordinary `--tests` only. The Runner requires the requested
purpose to match the compiled declaration before preparing a machine.
The workflows share environment preparation but retain separate run receipts
and artifacts.

Benchmark CI runs automatically for changes to benchmark implementation,
candidate guest integration entries, or shared execution infrastructure,
not ordinary Kernel source changes or pushes. Select a revision manually to collect a performance baseline.

`summary.json` records descriptive statistics derived from the raw samples:
minimum, median, maximum, median absolute deviation, median normalized time
and aggregate throughput. No measurements or outliers are discarded.
`verify compare <baseline-artifact> <candidate-artifact>` accepts complete
Bench artifact roots. Shared execution context must match; workload identity
is local to the actual image's source dependencies in `workload.json`.
Incompatible individual series are explicitly reported without ratios.
