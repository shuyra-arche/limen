/* A guest may supply a firmware-facing prelude before the ordinary entry. */
SECTIONS {
  .boot_probe KERNEL_LOAD_ADDRESS : { KEEP(*(.limen_test_entry)) } :code
}
INSERT BEFORE .text;

/* Select the actual linked entry; an image without a prelude uses _start. */
limen_test_image_entry = DEFINED(limen_test_entry) ? limen_test_entry : _start;

/* The test may inspect the linked permission boundaries without inventing them. */
limen_test_text_start = ADDR(.text);
limen_test_text_end = ADDR(.text) + SIZEOF(.text);
limen_test_read_only = ADDR(.rodata);
limen_test_writable = ADDR(.data);

/* Only an image defining __wrap_limen_switch_context supplies a probe.
   Otherwise every wrapped reference resolves directly to the real address. */
PROVIDE(__wrap_limen_switch_context = __real_limen_switch_context);
