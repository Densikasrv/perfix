
use core::arch::asm;

use crate::vga;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    zero: u8,
    flags: u8,
    offset_high: u16,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            zero: 0,
            flags: 0,
            offset_high: 0,
        }
    }

    fn new(handler: u32) -> Self {
        Self {
            offset_low: handler as u16,
            selector: 0x08,
            zero: 0,
            flags: 0x8E,
            offset_high: (handler >> 16) as u16,
        }
    }
}

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u32,
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::missing(); 256];

unsafe extern "C" {
    static isr_stub_table: [u32; 32];
    fn isr128();
}

pub unsafe fn init() {
    let idt = core::ptr::addr_of_mut!(IDT).cast::<IdtEntry>();
    let stubs = core::ptr::addr_of!(isr_stub_table).cast::<u32>();

    for i in 0..32 {
        let handler = unsafe { stubs.add(i).read() };

        unsafe {
            idt.add(i).write(IdtEntry::new(handler));
        }
    }

    // Системный вызов для запущенных ELF-программ.
    unsafe {
        idt.add(0x80)
            .write(IdtEntry::new(isr128 as *const () as usize as u32));
    }

    let pointer = IdtPointer {
        limit: (core::mem::size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: core::ptr::addr_of!(IDT) as u32,
    };

    unsafe {
        asm!(
            "lidt [{}]",
            in(reg) &pointer,
            options(nostack, preserves_flags)
        );
    }
}

/// Обработчик `int 0x80`.
///
/// `frame` указывает на сохранённые `pusha` регистры; запись в `frame[7]`
/// (EAX) задаёт значение, которое получит программа после `iret`.
#[unsafe(no_mangle)]
pub extern "C" fn syscall_handler(frame: *mut u32) {
    if frame.is_null() {
        return;
    }

    let registers = unsafe { &mut *(frame as *mut [u32; 8]) };
    let number = registers[7];

    // Номера вызовов совпадают с Linux i386: так сторонние статические
    // программы, собранные под Linux, работают без пересборки.
    match number {
        // exit / exit_group
        0 | 1 | 252 => crate::exec::exit_program(registers[4]),
        // write(fd, buf, count)
        4 => {
            crate::exec::write_output(registers[6], registers[5]);
            registers[7] = 0;
        }
        // writev(fd, iovec *, iovcnt)
        146 => {
            registers[7] = crate::exec::writev_output(registers[6], registers[5]);
        }
        // uname(struct utsname *)
        122 => {
            registers[7] = crate::exec::uname_output(registers[4]) as u32;
        }
        // mprotect(addr, len, prot): страницы уже доступны на запись.
        125 => {
            registers[7] = 0;
        }
        // getpid
        20 => {
            registers[7] = 1;
        }
        // getuid / getgid / geteuid / getegid
        24 | 47 | 49 | 50 => {
            registers[7] = 0;
        }
        // brk(addr)
        45 => {
            registers[7] = crate::exec::set_break(registers[4]);
        }
        // set_thread_area(user_desc *)
        243 => {
            registers[7] = crate::gdt::set_thread_area(registers[4]) as u32;
        }
        _ => {
            registers[7] = u32::MAX;
        }
    }
}

fn exception_name(vector: u32) -> &'static [u8] {
    match vector {
        0 => b"Divide Error",
        1 => b"Debug",
        2 => b"Non-Maskable Interrupt",
        3 => b"Breakpoint",
        4 => b"Overflow",
        5 => b"Bound Range Exceeded",
        6 => b"Invalid Opcode",
        7 => b"Device Not Available",
        8 => b"Double Fault",
        9 => b"Coprocessor Segment Overrun",
        10 => b"Invalid TSS",
        11 => b"Segment Not Present",
        12 => b"Stack-Segment Fault",
        13 => b"General Protection Fault",
        14 => b"Page Fault",
        16 => b"x87 Floating-Point Exception",
        17 => b"Alignment Check",
        18 => b"Machine Check",
        19 => b"SIMD Floating-Point Exception",
        20 => b"Virtualization Exception",
        21 => b"Control Protection Exception",
        29 => b"VMM Communication Exception",
        30 => b"Security Exception",
        _ => b"Reserved CPU Exception",
    }
}

fn print_hex(row: usize, col: usize, value: u32) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    for i in 0..8 {
        let shift = 28 - i * 4;
        let digit = HEX[((value >> shift) & 0xF) as usize];
        vga::print_at(row, col + i, &[digit]);
    }
}


#[unsafe(no_mangle)]
pub extern "C" fn idt_exception_handler(
    vector: u32,
    error: u32,
    eip: u32,
) -> ! {
    // Ошибку загруженной программы не показываем как панику ядра:
    // сообщаем и возвращаем управление оболочке.
    if crate::exec::active() {
        crate::exec::handle_exception(vector, error, eip);
    }

    vga::clear_screen();

    vga::print_at(0, 0, b"Perfix - CPU Exception");
    vga::print_at(2, 0, b"Exception:");
    vga::print_at(2, 11, exception_name(vector));

    vga::print_at(4, 0, b"Vector: 0x");
    print_hex(4, 10, vector);

    vga::print_at(5, 0, b"Error:  0x");
    print_hex(5, 10, error);

    vga::print_at(6, 0, b"EIP:    0x");
    print_hex(6, 10, eip);

    if vector == 14 {
        let cr2: u32;

        unsafe {
            asm!(
                "mov {0:e}, cr2",
                out(reg) cr2,
                options(nomem, nostack, preserves_flags)
            );
        }

        vga::print_at(7, 0, b"CR2:    0x");
        print_hex(7, 10, cr2);
    }

    vga::print_at(9, 0, b"System halted.");

    loop {
        unsafe {
            asm!("cli", "hlt", options(nomem, nostack));
        }
    }
}

pub fn test_divzero() -> ! {
    unsafe {
        asm!(
            "xor edx, edx",
            "mov eax, 1",
            "xor ecx, ecx",
            "div ecx",
            options(noreturn)
        );
    }
}