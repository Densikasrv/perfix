BITS 32

section .text
global gdt_flush

gdt_flush:
    mov eax, [esp + 4]
    lgdt [eax]

    ; Reload data segment registers.
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    ; Reload CS with a far jump.
    jmp 0x08:.reload_cs

.reload_cs:
    ret