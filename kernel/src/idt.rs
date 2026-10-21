
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