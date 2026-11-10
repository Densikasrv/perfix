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

        ; Request a linear graphics mode (1 = Text mode preferred, but allows override)
    dd 0   ; Было 0 (чистая графика). Ставим 1 (текст по дефолту, но графика разрешена)
    dd 1920
    dd 1080
    dd 32

; Reserve 256 KiB for the initial kernel stack.
; Перед стеком оставлен защитный промежуток: при переполнении стек растёт
; вниз и затирает guard, а не статические переменные ядра (VGA, клавиатура
; и т. п.), которые лежат в .bss непосредственно перед ним.
section .bss
align 16

multiboot_info:
    resd 1

stack_guard:
    resb 65536
stack_bottom:
    resb 262144
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

    ; ВНИМАНИЕ: Запись в 0xB8000 безопасна ТОЛЬКО если GRUB остался в текстовом режиме.
    ; Если GRUB включит графику, эти строки могут не отобразиться или повредить память кадра.
    ; Для отладки графики лучше убрать или закомментировать их.
    ; mov dword [0xB8000], 0x0F530F53
    ; mov dword [0xB8004], 0x0F520F52

    push dword [multiboot_info]
    call perfix_main

.hang:
    cli
    hlt
    jmp .hang
