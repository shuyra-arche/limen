/* OpenSBI enters the lowest load address on QEMU virt. */
SECTIONS {
  .boot_probe KERNEL_LOAD_ADDRESS : { KEEP(*(.limen_test_entry)) } :code
}
INSERT BEFORE .text;
