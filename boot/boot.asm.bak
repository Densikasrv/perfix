; Perfix boot entry
; Architecture: 32-bit x86
; Boot protocol: Multiboot 1

BITS 32

; Multiboot 1 header must be aligned to 4 bytes
; and located within the first 8192 bytes of the ELF file.
section .multiboot
align 4

    dd 0x1BADB002
    ; ALIGN | MEMINFO | VIDEO
    dd 0x00000007
    dd -(0x1BADB002 + 0x00000007)

    ; a.out kludge fields (not used)
    dd 0, 0, 0, 0, 0

    ; Request a linear graphics mode: 1920x1080x32.
    dd 0
    dd 1920
    dd 1080
    dd 32

; Reserve 64 KiB for the initial kernel stack.
; The stack sits in .bss directly below kernel statics, so it must have
; enough headroom for the deepest path: shell -> exFAT write -> bitmap.
section .bss
align 16

multiboot_info:
    resd 1

stack_bottom:
    resb 65536
stack_top:

; Actual kernel entry point.
section .text
global _start
extern perfix_main

_start:
    cli
    cld

    mov esp, stack_top
    xor ebp, ebp
    mov [multiboot_info], ebx

    ; Show S: reached the assembly entry point.
    mov dword [0xB8000], 0x0F530F53

    ; Show R: about to call Rust.
    mov dword [0xB8004], 0x0F520F52

    push dword [multiboot_info]
    call perfix_main

.hang:
    cli
    hlt
    jmp .hang
