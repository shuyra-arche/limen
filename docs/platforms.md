# Platform contracts

Limen builds one kernel library per architecture. The trusted image supplies
board resources and retains the storage needed for the common handoff.
[CPU boot](boot.md) · [UEFI test images](../server/tests/target/entry/platforms/)

## RV64 virt with OpenSBI

An OpenSBI image supplies `KERNEL_LOAD_ADDRESS` and `limen_platform_ready`.
The ELF linker provides the final image bounds and reserves the major and
emergency stacks outside BSS. Firmware enters `_start` on the sole boot hart
in supervisor mode with Bare translation, the hart ID in `a0` and the physical
DTB address in `a1`. Metal masks supervisor interrupts, disables floating-point
and vector state, installs the direct trap vector and clears BSS.

The DTB must remain readable, immutable and disjoint from the image. Limen
checks its version-17-compatible header, the nonempty structure block, and the
extents of the structure, strings and first reservation entry. The platform
validates block contents. Admission also requires the SBI debug console and
system reset extensions. The image receives the hart ID and admitted tree
extent. A minor starts through SBI HSM with its resident record in `a1`.
[RV64 entry](../server/src/metal/riscv/boot/) · [SBI](../server/src/metal/generic/firmware/sbi/)

## UEFI loader

The two UEFI test images share a loader. OVMF loads `EFI/BOOT/BOOTX64.EFI` on
q35 and AAVMF loads `EFI/BOOT/BOOTAA64.EFI` on virt. The loader captures board
inputs, exits Boot Services with the final memory map and calls
`entry::boot::major`. It keeps the handoff, firmware allocations, loader stack
and identity mappings reserved. It supplies optional post-exit output and
shutdown callbacks. [UEFI adapter](../server/src/metal/generic/firmware/uefi/)

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
test image. [x86_64 entry](../server/src/metal/x86_64/boot/)

### AArch64 virt

The test profile uses Cortex-A57, non-secure EL1, GICv3 and a firmware DTB.
The loader finds the DTB in the UEFI configuration table, validates its extent
and copies it into loader-owned pages before exit. It discovers the target CPU,
PL011 address and PSCI conduit from the tree.

Metal masks DAIF, selects `SP_EL1`, switches stacks and installs the aligned
EL1 vector table. Admission verifies that the DTB matches its declared extent
and fits in retained loader data in the final map. The minor starts through
PSCI `CPU_ON` with a launch record as context, restores the major's EL1
translation controls and installs its stack and vectors. QEMU verifies the
HVC conduit. [AArch64 entry](../server/src/metal/aarch64/boot/) · [PSCI](../server/src/metal/generic/firmware/psci.rs)

The verified scope on x86 and Arm covers major and minor boot. CPU hotplug,
general topology admission, interrupt routing and Unit scheduling on these
targets require separate implementation and verification.
