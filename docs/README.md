# Limen documentation

[Document Limen](https://github.com/shuyra-arche/document-limen) owns the
architecture and Treaty contracts. This repository documents the implementation
it currently provides.

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

The q35 diagnostic image starts through OVMF. Its loader exits Boot Services
with the final memory map, keeps that map and its old stack reserved, and calls
`entry::boot::enter` using the System V ABI. The x86_64 backend checks CPL0
and CR0.PG, masks interrupts, switches to its 32 KiB stack, and installs 256
fail-stop interrupt gates using the current code selector. The loader's GDT
and page tables stay in use. No DTB is supplied by this test image.

The AArch64 virt image uses QEMU's Linux Image protocol with non-secure EL1,
GICv3 and PSCI/HVC. The entry preserves x0, checks CurrentEL, masks DAIF,
selects SP_EL1, switches to its 32 KiB stack, installs an aligned fail-stop
vector table, and enters the diagnostic callback. The image checks the DTB
header inside the RAM aperture reserved for this board configuration.

Both entries require a single boot CPU and an exclusively reserved image.
The tests cover the CPU handoff. RAM admission, exception recovery, interrupt
controller operation, Manifest v1 and first Unit dispatch are later stages.
