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

Limen masks supervisor interrupts, establishes a direct trap vector and an
emergency stack, then clears BSS. The trusted boot entry checks the supervisor
CSRs and SBI handoff before reading the DTB header. A fault reports its cause,
program counter, trap value and interrupted stack pointer, then requests SBI
system failure shutdown. A trap during that failure path parks the hart.

The platform callback receives the hart ID and the described DTB extent. Board
topology, physical RAM admission, page-table activation, Unit scheduling and
user calls are later integration work. The
[boot verification guide](../server/tests/README.md) describes the exercised
end-to-end path and its evidence.
