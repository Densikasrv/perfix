; Perfix boot entry
; Architecture: 32-bit x86
; Boot protocol: Multiboot 1

BITS 32

; Multiboot 1 header must be aligned to 4 bytes
; and located within the first 8192 bytes of the ELF file.
section .multiboot
align 4

    dd 0x1BADB002
    dd 0x00000003
    dd -(0x1BADB002 + 0x00000003)

; Reserve 16 KiB for the initial kernel stack.
section .bss
align 16

stack_bottom:
    resb 16384
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

    ; Show S: reached the assembly entry point.
    mov dword [0xB8000], 0x0F530F53

    ; Show R: about to call Rust.
    mov dword [0xB8004], 0x0F520F52

    call perfix_main

.hang:
    cli
    hlt
    jmp .hang
