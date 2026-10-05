/* Raspberry Pi 3, 64-bit: the firmware (or QEMU) loads us at 0x80000. */
ENTRY(_start)

SECTIONS
{
    . = 0x80000;
    __image_start = .;
    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }
    .rodata : ALIGN(16) { *(.rodata .rodata.*) }
    .data : ALIGN(16) { *(.data .data.*) }
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }
    /* Boot stack: 4 MiB (the PDF interpreter recurses through forms). */
    . = ALIGN(4096);
    __stack_bottom = .;
    . += 4 * 1024 * 1024;
    __stack_top = .;
    /* The heap runs from here; size is set in main.rs. */
    . = ALIGN(2 * 1024 * 1024);
    __heap_start = .;
    /DISCARD/ : { *(.comment) *(.note .note.*) *(.eh_frame*) }
}
