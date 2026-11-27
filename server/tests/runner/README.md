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
hash, firmware hashes, tool identity, process output, cleanup and result
receipts. UEFI variable files are copied before use. CI records the checked-out
source revision and compiler. No test changes the checked-out source.
Use a fresh evidence root for a repeated run.

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
