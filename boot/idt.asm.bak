
BITS 32

section .text

extern idt_exception_handler
global isr_stub_table

%macro ISR_NOERR 1
global isr%1
isr%1:
    push dword 0
    push dword %1
    jmp isr_common_stub
%endmacro

%macro ISR_ERR 1
global isr%1
isr%1:
    push dword %1
    jmp isr_common_stub
%endmacro

ISR_NOERR 0
ISR_NOERR 1
ISR_NOERR 2
ISR_NOERR 3
ISR_NOERR 4
ISR_NOERR 5
ISR_NOERR 6
ISR_NOERR 7
ISR_ERR   8
ISR_NOERR 9
ISR_ERR   10
ISR_ERR   11
ISR_ERR   12
ISR_ERR   13
ISR_ERR   14
ISR_NOERR 15
ISR_NOERR 16
ISR_ERR   17
ISR_NOERR 18
ISR_NOERR 19
ISR_NOERR 20
ISR_ERR   21
ISR_NOERR 22
ISR_NOERR 23
ISR_NOERR 24
ISR_NOERR 25
ISR_NOERR 26
ISR_NOERR 27
ISR_NOERR 28
ISR_ERR   29
ISR_ERR   30
ISR_NOERR 31

isr_common_stub:
    cld
    pusha

    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    ; После pusha:
    ; [esp + 32] = vector
    ; [esp + 36] = error code
    ; [esp + 40] = EIP
    mov eax, [esp + 32]
    mov edx, [esp + 36]
    mov ecx, [esp + 40]

    push ecx        ; EIP
    push edx        ; error
    push eax        ; vector
    call idt_exception_handler

.hang:
    cli
    hlt
    jmp .hang

section .rodata
align 4

isr_stub_table:
    dd isr0,  isr1,  isr2,  isr3
    dd isr4,  isr5,  isr6,  isr7
    dd isr8,  isr9,  isr10, isr11
    dd isr12, isr13, isr14, isr15
    dd isr16, isr17, isr18, isr19
    dd isr20, isr21, isr22, isr23
    dd isr24, isr25, isr26, isr27
    dd isr28, isr29, isr30, isr31
