# Platform contracts

Limen builds one kernel library per architecture. The trusted image supplies
board resources and retains the storage needed for the common handoff.
[CPU boot](boot.md) · [UEFI test images](../server/tests/target/entry/platforms/)

## RV64 virt with OpenSBI

An OpenSBI image supplies `KERNEL_LOAD_ADDRESS`, `DTB_WINDOW_START`,
`DTB_WINDOW_END` and `limen_platform_ready`.
The ELF linker provides the final image and BSS bounds. The Rust-owned stacks
live in ordinary writable image storage. Firmware enters `_start` on the sole
boot hart in supervisor mode with Bare translation, the hart ID in `a0` and the
physical DTB address in `a1`. Metal masks supervisor interrupts, disables
floating-point and vector state, installs the direct trap vector and clears BSS.

The image defines `DTB_WINDOW_START..DTB_WINDOW_END` as a half-open identity
window whose storage is readable in supervisor mode. Supply it from the trusted
board configuration. The OpenSBI pointer names a stable, initialized DTB in
that window. Keep that storage immutable and disjoint from the image. The
128 MiB QEMU fixtures use `0x80200000..0x88000000`, excluding the firmware
prefix. Limen checks the header read and the complete declared extent against
the configured window before reading the rest of the tree.

`Boot::dtb_memory` returns the parsed memory description on this path. It
exposes RAM from enabled root memory nodes and reservations
from both the FDT reservation block and static `/reserved-memory` children.
It preserves `no-map` and `reusable` and rejects an enabled reservation whose
address must still be allocated. The parser accepts one- or two-cell addresses
and sizes, at most 32 levels, 16 RAM ranges and 64 reservations. OpenSBI
admits at most 256 KiB of DTB bytes from its readable window.
Capacity, malformed geometry and unsupported memory descriptions
fail before the handoff. Later startup stages compare the descriptions with
live image and hardware exclusions before admitting any range to Allocator.

Admission also requires the SBI debug console and system reset extensions.
A minor starts through SBI HSM with its resident record in `a1`.
[RV64 entry](../server/src/metal/boot/riscv/) · [SBI](../server/src/metal/firmware/sbi/)

## UEFI loader

The two UEFI test images share a loader. OVMF loads `EFI/BOOT/BOOTX64.EFI` on
q35 and AAVMF loads `EFI/BOOT/BOOTAA64.EFI` on virt. The loader captures board
inputs, exits Boot Services with the final memory map and calls
`entry::boot::major`. It keeps the handoff, firmware allocations, loader stack
and identity mappings reserved. It supplies optional post-exit output and
shutdown callbacks. The loader supplies the actual descriptor bytes, stride,
version and a configured descriptor-count limit. After entry, the common
UEFI adapter checks every physical range, rejects zero page counts, arithmetic
overflow and overlapping descriptors, and exposes an immutable FirmwareMap.
The original kind and attributes are retained; a conventional-memory
descriptor is not an Allocation or proof of safe access. The check uses
bounded pairwise comparisons without ordinary allocation. The selected test
profiles use a limit of 512 descriptors; other images set their own validated
configuration bound.

Use `DeviceTree::Memory(bytes)` when the platform supplies the DTB as a
memory description. The adapter checks the retained copy and runs the same
bounded parser as OpenSBI. A retained memory description can occupy up to
2 MiB including firmware padding. The parser limits the combined structure
and strings blocks to 256 KiB. Use `DeviceTree::Description(bytes)` for a DTB
whose extent alone needs checking at this handoff, or `None` when the loader
has no DTB.

`Boot::conventional_ram` is present when both memory descriptions were supplied.
It walks their intersection without allocating, returning whole UEFI pages
that are conventional memory, are outside both kinds of DTB reservation
and have no UEFI runtime obligation. It preserves the UEFI descriptor kind
and attributes. The original DTB and UEFI descriptions remain available
through `Boot`. Initial RAM admission still requires the remaining platform
reservations, active image/stack/translation storage, verified access and old
device isolation.
[UEFI adapter](../server/src/metal/firmware/uefi/) ·
[Formal boot contract](https://github.com/shuyra-arche/document-limen/blob/246170c626ee2b5e316753e95f50f1669e8f47dd/server/entry/boot/platforms.md)

### x86_64 q35

The major enters Metal through the System V ABI in 64-bit ring 0 with paging
enabled. Metal checks CPL and CR0.PG, switches stacks and installs 256
diagnostic gates using the current code selector. The loader's GDT and page
tables remain active.

Before exiting Boot Services, the loader finds a healthy enabled AP using MP
Services, reserves an executable SIPI page below 1 MiB and uses UEFI Stall
to calibrate a TSC delay. It supplies the page and delay to the xAPIC
INIT/SIPI path. The reset stub expects a four-level identity map and addresses
below 4 GiB for the page-table root, GDT base, entry and launch record. The
minor enters long mode and loads the kernel IDT. The debug ports belong to the
test image. [x86_64 entry](../server/src/metal/boot/x86_64/)

### AArch64 virt

The test profile uses Cortex-A57, non-secure EL1, GICv3 and a firmware DTB.
The loader finds the DTB in the UEFI configuration table, validates its extent
and copies it into loader-owned pages before exit. It discovers the target CPU,
PL011 address and PSCI conduit from the tree.

Metal masks DAIF, selects `SP_EL1`, switches stacks and installs the aligned
EL1 vector table. Admission verifies that the DTB matches its declared extent
and fits in retained loader data in the final map. The platform supplies it
as `DeviceTree::Memory` to enable cross-checking. The minor starts through
PSCI `CPU_ON` with a launch record as context, restores the major's EL1
translation controls and installs its stack and vectors. QEMU verifies the
HVC conduit. [AArch64 entry](../server/src/metal/boot/aarch64/) · [PSCI](../server/src/metal/firmware/psci.rs)

The verified scope on x86 and Arm covers major and minor boot, and on Arm
also the DTB and UEFI memory cross-check. CPU hotplug,
general topology admission, interrupt routing and Unit scheduling on these
targets require separate implementation and verification.

## Early physical exclusions

RV64 supplies the resident image extent from its linker. The UEFI loader
supplies the physical image extent under the entry identity map. The test
loaders take it from UEFI Loaded Image before ExitBootServices. Common boot
checks that the image contains the major and early trap stacks before
delivering the handoff.

`Boot::exclusions` records the resident image, both kernel stacks, the
retained DTB and UEFI map bytes, and the original firmware reservations.
It walks the existing bounded descriptions without allocating a second range
table. Separate claims can overlap. An overlap with a live byte range retains
the queried physical range, including a whole page that contains only some
of those bytes. Image-owned minor stacks remain covered by the image.

`Exclusions::retains` keeps ranges reserved unless retirement evidence covers
the whole query. The permanent stack must be active, the relevant readers
and Harts must have left the old storage, and the owners must confirm old
CPU translations and page-table walks have retired and old device access
is isolated. The current boot backends provide no such combined evidence.
The image, kernel boot stacks, retained handoff bytes and non-reclaimable
firmware reservations remain excluded while this handoff owns them.
DTB reservation attributes stay available, including `reusable`. A device
pool described by a DTB keeps its reservation until its owner can hand it off.
ACPI reclaimable memory stays reserved until ACPI has been enabled.
Reclaimable firmware claims become eligible for later admission when all
their conditions hold. The caller still establishes RAM provenance,
granularity and actual access before handing a range to Allocator.

## Organizing early physical regions

`Boot::regions` writes a sorted table into fixed, caller-supplied storage.
It uses DTB RAM on OpenSBI, conventional UEFI memory when only that memory
description is supplied, and their checked intersection when both are
available. The table clips each input to the completed retirement scope,
excludes granules touched by fixed claims and keeps each surviving interval's
source and attributes. Invalid geometry or insufficient table space clears
the output. The table describes physical ranges for later handoff.

To reserve a local root, its owner supplies an exclusive `MappedRange` whose
writable byte slice corresponds to the indicated physical range. An existing
Bump cursor reserves the requested Rust `Layout` within that mapping. The
handoff verifies both address alignments, subtracts all granules touched by
the reservation and returns the exact uninitialized root bytes and their
physical provenance. A failed request retains the mapping and geometry.
Missing evidence leaves ranges held. The current QEMU boot paths produce
an empty table and deliver no root.
