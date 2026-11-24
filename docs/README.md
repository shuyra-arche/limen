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
readable DTB storage disjoint from the image. Headers compatible with version
17 are recognized.

Limen masks supervisor interrupts, disables floating-point and vector state,
establishes a direct trap vector and an
emergency stack, then clears BSS. The trusted boot entry checks the supervisor
CSRs and SBI handoff before reading the DTB header. A fault reports its cause,
program counter, trap value and interrupted stack pointer, then requests SBI
system failure shutdown. A trap during that failure path parks the hart.

The platform callback receives the hart ID and the described DTB extent.

## Kernel Unit execution

The trusted image consumes the Boot handoff to establish cooperative execution
on that Hart. It supplies two resident 16 KiB stacks in the shared Bare
Kernel Nest. Each Unit reserves one stack and a generational identity. The
trusted entry must fit the stack and run in supervisor mode with interrupts
and extended registers disabled.

Room admits a Unit and reserves an exact occupancy. Metal enters through the
Unit's stack and saves its integer continuation at the call boundary on Yield.
A normal return records the Unit's sole end value. The dispatcher resumes on
its own stack before Metal issues the leave receipt. Room consumes that
receipt to make the Unit ready again or release the admission. An Ending Unit
keeps its binding until the admission is released. Reaping the Ended Unit
allows the slot and stack to be reused with a new generation.

The [verification guide](../server/tests/README.md) describes the RV64 target
scenarios. This execution path covers the Boot Hart and trusted Kernel Units.
Additional Hart participation, hardware translation, user traps and calls
require their own implementation and verification.
