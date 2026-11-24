/* OpenSBI enters the lowest load address on QEMU virt. */
SECTIONS {
  .boot_probe KERNEL_LOAD_ADDRESS : { KEEP(*(.limen_test_entry)) } :code
}
INSERT BEFORE .text;

/* Test-only bounds of the code the image actually linked. */
limen_test_text_start = ADDR(.text);
limen_test_text_end = ADDR(.text) + SIZEOF(.text);
