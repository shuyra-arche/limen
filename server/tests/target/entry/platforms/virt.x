ENTRY(image)
SECTIONS {
    . = 0x40080000;
    __image_start = .;
    .text : { KEEP(*(.text.header)) *(.text .text.*) }
    .rodata : { *(.rodata .rodata.*) }
    .data : { *(.data .data.* .got .got.*) }
    .bss (NOLOAD) : { *(.bss .bss.* COMMON) }
    . = ALIGN(16);
    __image_size = . - __image_start;
    /DISCARD/ : { *(.eh_frame*) }
}
