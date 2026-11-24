# Limen documentation

[Document Limen](https://github.com/shuyra-arche/document-limen) owns the
architecture and Treaty contracts. This repository documents the implementation
it currently provides.

All three boot paths are declared in [`entry::boot`](../server/src/entry/boot/mod.rs).
The module defines the major CPU handoff and the Boot facts delivered to the image.
Metal first installs the stack and exception entry using the firmware ABI.
All three paths then call the same `entry::boot::start`. It checks that the
hardware stack pointer lies in the boot stack and asks Metal to read back the installed
CPU state. It admits the firmware facts and calls the one image-defined
`limen_platform_ready(Boot)` on all three architectures. A rejected handoff uses the shared diagnostic and shutdown path.

One Metal stack definition backs `stack_range()` on all three targets. RV64
places its 16 KiB ordinary stack and separate 4 KiB trap stack outside BSS.
The linker still supplies the final ELF image bounds used for DTB overlap
checks. The x86 and Arm entries use 32 KiB ordinary stacks.

The active boot protocol supplies early services. OpenSBI images probe SBI debug console and system reset once during
admission and publish their callbacks for subsequent use. An early RV64 trap
probes directly because the vector is live before BSS initialization. UEFI images pass a static table of
post-exit output and shutdown operations to `boot::major`. Either operation
can be absent. The kernel stores the selected operations for use on the major
and minor CPUs. Boot Services calls must finish before ExitBootServices.

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

The Boot consumer receives the firmware Hart ID and the admitted DTB extent.
`Boot::device_tree()` and `Boot::firmware_map()` return optional facts.
OpenSBI supplies the tree. UEFI supplies its final map and, on virt, the tree.

## Kernel Unit execution

The trusted image consumes the Boot handoff to establish cooperative execution
on the Boot Hart. It supplies two resident 16 KiB stacks in the shared Bare
Kernel Nest. Each Unit reserves one stack and a generational identity.
`Execution::with_minor` accepts a resident `Minor` with its own
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

Each architecture implements major and minor CPU startup under `metal/<arch>/boot/`.
The common gate uses `metal::boot` to check the selected implementation.
Declare `major` in `boot/major.rs` and `minor` in `boot/minor.rs` as Rust naked functions, with its firmware or loader ABI. Put the instructions and
operands directly in `naked_asm!`. Export a fixed name when the external entry contract requires one, such as
RV64 ELF `_start`. Refer to private CPU entries and vectors by address and
apply their required alignment to the function. Use `sym` for Rust-owned
storage and `const` for sizes or layout offsets. The linker supplies final
image and section boundaries. The platform image supplies any board-specific diagnostics and startup
resources. The QEMU platform images live under `server/tests/target/entry/platforms/`.
The kernel builds once per target architecture. Metal implements CPU mechanisms
and firmware interfaces.
Shared stacks, minor launch state, device-tree header validation, diagnostic
formatting, boot service publication and allocator code live under
`metal/generic/`. Its `firmware/` directory owns UEFI handoff types, RV64 SBI
services and the AArch64 PSCI conduit. Each CPU backend explicitly selects its firmware adapter. The adapter supplies
early services and admits its input at the common gate. Keep architecture
selection on module declarations. Put register reads, exception-stack storage
and verification of installed CPU state in the selected backend.

The q35 and AArch64 virt diagnostic images share a UEFI loader. OVMF loads
`EFI/BOOT/BOOTX64.EFI` on q35 and AAVMF loads `EFI/BOOT/BOOTAA64.EFI` on
virt. The loader captures board inputs, obtains the final memory map through
`ExitBootServices`, then calls `entry::boot::major`. It keeps the handoff,
firmware allocations and loader stack reserved while Metal takes over the CPU.

On q35, the loader calls Metal using the System V ABI. Metal checks CPL0
and CR0.PG, masks interrupts, switches to its 32 KiB stack and installs 256
fail-stop interrupt gates using the current code selector. The loader's GDT
and page tables stay in use.

On virt, the test selects non-secure EL1, GICv3, PSCI/HVC and device-tree
firmware configuration. The loader obtains the DTB address from the UEFI
configuration table, validates its extent, and copies it into loader-owned
pages before exiting Boot Services. Metal masks DAIF, selects SP_EL1, switches to its 32 KiB
stack and installs an aligned fail-stop vector table. The shared Boot admission gate verifies
that the DTB header and its declared extent fit within the retained loader-data descriptor
in the final memory map before reporting success.

## Minor CPU startup

A major CPU receives Boot once. Each minor starts with an exclusive resident
`Minor` record and its own 16 KiB stack. The major publishes the launch record
before requesting startup. The target claims it and reads back its hardware
identity, active stack and exception entry before publishing Ready and calling
the supplied minor entry. `Minor::ready()` observes that publication. A
definitive rejection permits retry. Unconfirmed xAPIC delivery keeps the
record, trampoline, GDT and mappings reserved because the target may still enter.

| Profile | Startup request | State established on the minor |
| --- | --- | --- |
| RV64 / OpenSBI | SBI HSM hart start | Supervisor state, private ordinary and trap stacks, direct trap vector |
| x86_64 / q35 | xAPIC INIT and SIPI to a loader-reserved executable page below 1 MiB | Four-level paging, long mode, retained GDT, kernel IDT and private stack |
| AArch64 / virt | PSCI CPU_ON over HVC with the launch record as context | Non-secure EL1, identity-mapped translation controls, SP_EL1 and kernel vectors |

Before exiting Boot Services, the q35 test image enumerates healthy enabled APs
with UEFI MP Services, reserves a low executable page and calibrates the major
CPU’s TSC using UEFI Stall. It supplies that page and the post-exit delay
function to the xAPIC startup mechanism. Its QEMU debug ports belong to the
test image. The Arm image reads the PSCI conduit, enabled target CPU and
chosen PL011 address from the DTB provided by UEFI. Its PSCI calls after
ExitBootServices use that discovered conduit.

The q35 reset stub needs the page-table root, GDT base, entry and launch record
below 4 GiB. The loader keeps the GDT and identity mappings resident. The
virt profile uses Cortex-A57 cores and the major’s EL1 translation tables.
The caller selects a known inactive target. CPU hotplug, general-purpose topology admission,
general interrupt routing and Unit scheduling on x86 and Arm are outside the
verified boot path.
