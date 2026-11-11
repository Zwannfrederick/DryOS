/* SPDX-License-Identifier: MIT OR GPL-2.0-only */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

PHDRS
{
    text    PT_LOAD FLAGS(5); /* PF_R | PF_X (W^X enforced) */
    rodata  PT_LOAD FLAGS(4); /* PF_R */
    data    PT_LOAD FLAGS(6); /* PF_R | PF_W */
}

SECTIONS
{
    . = 0xffffffff80000000;
    __kernel_virt_start = .;

    .text ALIGN(0x1000) : {
        *(.text .text.*)
    } :text

    .rodata ALIGN(0x1000) : {
        *(.rodata .rodata.*)
    } :rodata

    .data ALIGN(0x1000) : {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
        *(.data .data.*)
    } :data

    .bss ALIGN(0x1000) : {
        *(COMMON)
        *(.bss .bss.*)
    } :data

    __kernel_virt_end = ALIGN(0x1000);

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
        *(.comment)
    }
}
