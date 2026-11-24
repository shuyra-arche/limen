# Limen documentation

[Document Limen](https://github.com/shuyra-arche/document-limen) owns the
architecture and Treaty contracts. This repository documents the implementation
it currently provides.

All three boot paths are declared in [`entry::boot`](../server/src/entry/boot/mod.rs).
The module documents the prerequisites and the facts each continuation receives.
Metal first installs the stack and exception entry using the firmware ABI.
All three paths then call the same `entry::boot::start`. It checks that the
hardware stack pointer lies in the boot stack and asks Metal to read back the installed
CPU state. Only then can the platform validate its input and enter its
continuation. A rejected handoff uses the shared diagnostic and shutdown path.

One Metal stack definition backs `stack_range()` on all three targets. RV64
places its 16 KiB ordinary stack and separate 4 KiB trap stack outside BSS.
The linker still supplies the final ELF image bounds used for DTB overlap
checks. The UEFI and Arm entries use 32 KiB ordinary stacks.

The shared `boot::console()` returns an optional `core::fmt::Write` device.
RV64 probes the SBI debug-console and system-reset extensions. The test
images select explicit Cargo board profiles: `qemu-q35` requires debugcon at
0xe9 and isa-debug-exit at 0xf4; `qemu-virt` requires the identity-mapped
PL011 at 0x09000000 and PSCI/HVC. These devices and mappings must remain
available whenever early services are used. Without those profiles x86_64
and AArch64 provide no early console, and shutdown parks the CPU. PSCI
SYSTEM_OFF cannot encode an exit reason; the Arm verifier checks the guest
message and the shutdown receipt together.

## RV64 boot

An OpenSBI platform image links the `kernel` library and supplies
`KERNEL_LOAD_ADDRESS` and `limen_platform_ready`. The linker places the image and
both bounded stacks in the ELF's load ranges. Firmware enters `_start` on the
sole boot hart in supervisor mode with Bare translation, with the hart ID and
physical DTB address in `a0` and `a1`. The platform must provide immutable,
readable DTB storage disjoint from the image. The fixed version-17-compatible
header must describe a whole, nonempty structure block and disjoint extents
for structure, strings and the first reservation entry. The platform owns
validation of the block contents.

Limen masks supervisor interrupts, disables floating-point and vector state,
establishes a direct trap vector and an emergency stack, then clears BSS.
The trusted boot entry checks the supervisor CSRs and probes the SBI debug
console (DBCN) and system reset (SRST) before reading the DTB header. Admission
requires both extensions.

A rejected handoff or a trap reports through DBCN when available, requests
failure shutdown through SRST when available, and parks the Hart if shutdown
is unavailable or returns. The trap diagnostic includes the cause, program
counter, trap value and interrupted stack pointer. A trap during that failure
path parks the Hart.

The platform callback receives the hart ID and the described DTB extent.

## Kernel Unit execution

The trusted image consumes the Boot handoff to establish cooperative execution
on the Boot Hart. It supplies two resident 16 KiB stacks in the shared Bare
Kernel Nest. Each Unit reserves one stack and a generational identity.
`Execution::with_secondary` accepts a resident `Secondary` with its own
dispatcher and emergency stacks. The platform can use `start_hart` to start
one selected Hart through OpenSBI HSM. Room admits the secondary after its
trusted supervisor entry establishes its local execution state.
The 16 KiB secondary dispatcher stack holds the live startup frames, the
platform entry and its dispatcher-side callees.

Trusted Unit entries must fit their stack and run in supervisor mode with
interrupts and extended registers disabled. `create` pins the Unit to its
creating Hart. `create_migratable` requires a trusted entry that can resume
on either Hart and yields without retaining Hart-local resources.

Room admits a Unit and reserves an exact occupancy. Metal enters through the
Unit's stack and saves its integer continuation at the call boundary on Yield.
A normal return records the Unit's sole end value. The dispatcher resumes on
its own stack before Metal issues the leave receipt. Room consumes that
receipt to make the Unit ready again or release the admission. The shared
Room and Unit state uses a short try-lock to publish eligibility and
settle an exact occupancy. A yielded continuation becomes available to the
other Hart only after settlement. An Ending Unit keeps its binding until the
admission is released. Reaping the Ended Unit allows the slot and stack to be
reused with a new generation.

A trusted platform can stop a dormant Unit before its first submission.
Room admission and the cancellation decision share one protected scope.
`finish_end` retires the unused continuation, and `reap` returns the
cancellation outcome and releases the slot.

This execution path covers cooperative supervisor Units on up to two
resident Harts. The [verification guide](../server/tests/README.md) describes
the RV64 target scenarios. Hart detachment, preemption, hardware translation,
user traps and calls require separate implementation and verification.

## Platform boot CPU entries

Each architecture implements CPU startup under `metal/<arch>/boot/`.
The common gate uses `metal::boot` to check the selected implementation.
Declare the first CPU entry as a Rust naked function in each backend’s
`boot/start.rs`, with its firmware or loader ABI. Put the instructions and
operands directly in `naked_asm!`. Export fixed linker or vector symbols
from their Rust definitions and set hardware-required alignment on the
function. Use `sym` for Rust-owned
storage and `const` for sizes or layout offsets. The linker supplies final
image and section boundaries. Board-specific diagnostics live under
`metal/<arch>/board/` when selected by a board feature.

The q35 and AArch64 virt diagnostic images share a UEFI loader. OVMF loads
`EFI/BOOT/BOOTX64.EFI` on q35 and AAVMF loads `EFI/BOOT/BOOTAA64.EFI` on
virt. The loader captures board inputs, obtains the final memory map through
`ExitBootServices`, then calls `entry::boot::enter`. It keeps the handoff,
firmware allocations and loader stack reserved while Metal takes over the CPU.

On q35, the loader calls Metal using the System V ABI. Metal checks CPL0
and CR0.PG, masks interrupts, switches to its 32 KiB stack and installs 256
fail-stop interrupt gates using the current code selector. The loader's GDT
and page tables stay in use.

On virt, the test selects non-secure EL1, GICv3, PSCI/HVC and device-tree
firmware configuration. The loader obtains the DTB address from the UEFI
configuration table, validates its extent, and copies it into loader-owned
pages before exiting Boot Services. Metal masks DAIF, selects SP_EL1, switches to its 32 KiB
stack and installs an aligned fail-stop vector table. The callback verifies
that the DTB header and its declared extent fit within the retained loader-data descriptor
in the final memory map before reporting success.

Both entries require a single boot CPU and an exclusively reserved image.
The tests cover the CPU handoff. RAM admission, exception recovery, interrupt
controller operation, Manifest v1 and first Unit dispatch are later stages.
