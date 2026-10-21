#![no_std]
#![no_main]

use core::arch::asm;
use core::ffi::c_void;
use core::panic::PanicInfo;

mod version;
mod vga;
mod keyboard;
mod shell;
mod gdt;
mod idt;
mod paging;
mod ata;
mod exfat;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(
dest: *mut c_void,
value: i32,
count: usize,
) -> *mut c_void {
let dest = dest as *mut u8;

for i in 0..count {
    unsafe {
        dest.add(i).write(value as u8);
    }
}

dest as *mut c_void

}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(
dest: *mut c_void,
src: *const c_void,
count: usize,
) -> *mut c_void {
let dest = dest as *mut u8;
let src = src as *const u8;

for i in 0..count {
    unsafe {
        dest.add(i).write(src.add(i).read());
    }
}

dest as *mut c_void

}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(
a: *const c_void,
b: *const c_void,
count: usize,
) -> i32 {
let a = a as *const u8;
let b = b as *const u8;

for i in 0..count {
    let av = unsafe { a.add(i).read() };
    let bv = unsafe { b.add(i).read() };

    if av != bv {
        return av as i32 - bv as i32;
    }
}

0

}

#[unsafe(no_mangle)]
pub extern "C" fn perfix_main() -> ! {
unsafe {
gdt::init();
idt::init();
paging::init();
}

shell::show_screen(None);

let mut input = [0u8; 64];
let mut len = 0usize;
let mut row = 6usize;
let mut col = 8usize;

loop {
    match keyboard::read_key() {
        
keyboard::Key::Enter => {
    shell::execute(&input[..len], &mut row);

    input = [0u8; 64];
    len = 0;
    col = 8;

    // shell::execute() уже вывела приглашение.
    // Следующая строка после него — строка ввода.

    if row >= 25 {
        vga::scroll_screen();
        row = 24;
    }
}


        keyboard::Key::Backspace => {
            if len > 0 {
                len -= 1;
                col = col.saturating_sub(1);
                vga::print_at(row, col, b" ");
            }
        }

        keyboard::Key::Character(ch) => {
            if len < input.len() && col < 80 {
                input[len] = ch;
                len += 1;

                let one_char = [ch];
                vga::print_at(row, col, &one_char);
                col += 1;
            }
        }

        keyboard::Key::None => {
            unsafe {
                asm!("pause", options(nomem, nostack, preserves_flags));
            }
        }
    }
}

}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
loop {
unsafe {
asm!("cli", "hlt", options(nomem, nostack));
}
}
}