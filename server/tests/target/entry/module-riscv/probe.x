/* Expose the section boundaries of the ordinary OpenSBI kernel image. */
limen_test_read_only = ADDR(.rodata);
limen_test_writable = ADDR(.data);
