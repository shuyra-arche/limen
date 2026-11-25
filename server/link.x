OUTPUT_ARCH(riscv)
ENTRY(_start)
EXTERN(_start)

PHDRS
{
  code PT_LOAD FLAGS(5);
  read_only PT_LOAD FLAGS(4);
  writable PT_LOAD FLAGS(6);
}

SECTIONS
{
  ASSERT(DEFINED(KERNEL_LOAD_ADDRESS), "image must provide KERNEL_LOAD_ADDRESS")
  ASSERT(DEFINED(DTB_WINDOW_START) && DEFINED(DTB_WINDOW_END), "image must provide the readable DTB window")
  ASSERT(DTB_WINDOW_START > 0 && DTB_WINDOW_END > DTB_WINDOW_START, "invalid readable DTB window")
  . = KERNEL_LOAD_ADDRESS;
  _kernel_base = .;

  .text : {
    KEEP(*(.text._start))
    *(.text .text.*)
  } :code

  .rodata ALIGN(0x1000) : {
    *(.rodata .rodata.* .srodata .srodata.*)
    *(.eh_frame_hdr .eh_frame)
  } :read_only

  .data ALIGN(0x1000) : {
    PROVIDE(__global_pointer$ = . + 0x800);
    *(.data .data.* .sdata .sdata.*)
    *(.got .got.*)
  } :writable

  .bss (NOLOAD) : ALIGN(8) {
    _bss_start = .;
    *(.bss .bss.* .sbss .sbss.*)
    *(COMMON)
    . = ALIGN(8);
    _bss_end = .;
  } :writable

  _kernel_edge = .;
}
