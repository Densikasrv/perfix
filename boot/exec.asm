; Переключение стека при запуске ELF-программы.
;
; Программа выполняется в ring 0, но со своим стеком. Перед переходом в
; точку входа сохраняются callee-saved регистры ядра, чтобы после возврата
; Rust-код не получил испорченное состояние.

BITS 32

section .text

global elf_enter
global elf_restore_kernel

; void elf_enter(u32 entry, u32 stack_top)
elf_enter:
    mov eax, [esp + 4]      ; entry
    mov ecx, [esp + 8]      ; stack_top

    push ebp
    push ebx
    push esi
    push edi

    mov [elf_saved_esp], esp

    mov esp, ecx
    xor ebp, ebp
    cld

    mov dx, 0x10
    mov ds, dx
    mov es, dx
    mov fs, dx
    mov gs, dx

    ; На входе статической программы EDX — это rtld_fini. glibc
    ; регистрирует его как destructor через __cxa_atexit, поэтому
    ; мусор в EDX приводит к вызову случайного адреса при exit().
    ; Остальные регистры тоже обнуляем для чистого начального состояния.
    xor edx, edx
    xor ebx, ebx
    xor ecx, ecx
    xor esi, esi
    xor edi, edi

    jmp eax

; Возврат в ядро. Вызывается из обработчика int 0x80 (exit) и из
; обработчика исключений. Возвращается туда, откуда вызывали elf_enter.
elf_restore_kernel:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    cld

    mov esp, [elf_saved_esp]
    pop edi
    pop esi
    pop ebx
    pop ebp
    ret

section .bss
align 4

elf_saved_esp:
    resd 1
