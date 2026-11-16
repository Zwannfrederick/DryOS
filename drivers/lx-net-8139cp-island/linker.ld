/* SPDX-License-Identifier: GPL-2.0-only */
/* Static lower-half ELF64 linker script for `dry-lx-net.elf` isolated driver island (`W^X` enforced). */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

PHDRS
{
    text    PT_LOAD FLAGS(5); /* PF_R | PF_X */
    rodata  PT_LOAD FLAGS(4); /* PF_R */
    data    PT_LOAD FLAGS(6); /* PF_R | PF_W */
}

SECTIONS
{
    . = 0x0000000000400000;

    .text ALIGN(0x1000) : {
        __user_text_start = .;
        *(.text._start)
        *(.text .text.*)
        . = ALIGN(0x1000);
        __user_text_end = .;
    } :text

    .rodata ALIGN(0x1000) : {
        __user_rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(0x1000);
        __user_rodata_end = .;
    } :rodata

    .data ALIGN(0x1000) : {
        __user_data_start = .;
        *(.data .data.*)
    } :data

    .bss ALIGN(0x1000) : {
        __user_bss_start = .;
        *(COMMON)
        *(.bss .bss.*)
        . = ALIGN(0x1000);
        __user_bss_end = .;
    } :data

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
        *(.comment)
    }
}
