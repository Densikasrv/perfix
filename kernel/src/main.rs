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
mod tedit;
mod fetch;
mod cpu;
mod sysinfo;
mod font_cp866;
mod font_8x16;
mod rtc;
mod utf8;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(
dest: *mut c_void,
value: i32,
count: usize,
) -> *mut c_void {
    unsafe {
        core::arch::asm!(
            "cld",
            "rep stosb",
            inout("edi") dest as usize => _,
            inout("ecx") count => _,
            in("al") value as u8,
            options(nostack, preserves_flags)
        );
    }

    dest
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

fn mbi_u32(multiboot_info: u32, offset: usize) -> u32 {
    unsafe {
        (multiboot_info as *const u8)
            .add(offset)
            .cast::<u32>()
            .read_unaligned()
    }
}

fn mbi_u8(multiboot_info: u32, offset: usize) -> u8 {
    unsafe { (multiboot_info as *const u8).add(offset).read_volatile() }
}

fn mbi_u64(multiboot_info: u32, offset: usize) -> u64 {
    unsafe {
        (multiboot_info as *const u8)
            .add(offset)
            .cast::<u64>()
            .read_unaligned()
    }
}

fn framebuffer_info(multiboot_info: u32) -> Option<vga::FramebufferInfo> {
    const FLAG_FRAMEBUFFER: u32 = 1 << 12;

    if multiboot_info == 0 {
        return None;
    }

    let flags = mbi_u32(multiboot_info, 0);

    if flags & FLAG_FRAMEBUFFER == 0 {
        return None;
    }

    Some(vga::FramebufferInfo {
        address: mbi_u64(multiboot_info, 88),
        pitch: mbi_u32(multiboot_info, 96),
        width: mbi_u32(multiboot_info, 100),
        height: mbi_u32(multiboot_info, 104),
        bpp: mbi_u8(multiboot_info, 108),
        kind: mbi_u8(multiboot_info, 109),
    })
}

/// Перерисовывает область ввода после выбора команды из истории.
fn redraw_input(
    input: &[u8],
    prompt_row: &mut usize,
    row: &mut usize,
    col: &mut usize,
) {
    if *row >= *prompt_row {
        for line in *prompt_row..=*row {
            vga::fill_row(line, vga::COLOR_NORMAL);
        }
    }

    shell::prompt(*prompt_row);
    *row = *prompt_row;
    *col = shell::prompt_width();

    for &byte in input {
        if *col >= vga::width() {
            *row += 1;

            if *row >= vga::height() {
                vga::scroll_screen();
                *row = vga::height() - 1;
                *prompt_row = prompt_row.saturating_sub(1);
            }

            *col = 0;
        }

        vga::print_at(*row, *col, &[byte]);
        *col += 1;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn perfix_main(multiboot_info: u32) -> ! {
sysinfo::init(multiboot_info);

let framebuffer = framebuffer_info(multiboot_info);

let (framebuffer_address, framebuffer_size) = match &framebuffer {
    Some(info) => (
        info.address,
        info.pitch as u64 * info.height as u64,
    ),
    None => (0, 0),
};

unsafe {
gdt::init();
idt::init();
paging::init(framebuffer_address, framebuffer_size);
}

vga::init(framebuffer);

// В текстовом режиме стандартный шрифт VGA не содержит кириллицы,
// поэтому догружаем глифы CP866. В framebuffer-режиме используется
// полная таблица font_8x16.
vga::load_glyphs(&font_cp866::GLYPHS);

shell::show_screen(None);

const INPUT_CAPACITY: usize = shell::INPUT_CAPACITY;

let mut input = [0u8; INPUT_CAPACITY];
let mut draft = [0u8; INPUT_CAPACITY];
let mut draft_len = 0usize;
let mut history_pos: Option<usize> = None;
let mut len = 0usize;
let mut row = 6usize;
// Строка, с которой начинается текущий ввод: Backspace не должен
// стирать приглашение и предыдущий вывод.
let mut prompt_row = row;
let mut col = shell::prompt_width();

loop {
    match keyboard::read_key() {
        keyboard::Key::Enter => {
            shell::execute(&input[..len], &mut row);

            if len > 0 {
                shell::history_add(&input[..len]);
            }

            history_pos = None;
            draft_len = 0;

            input = [0u8; INPUT_CAPACITY];
            len = 0;
            prompt_row = row;
            col = shell::prompt_width();

            // shell::execute() уже вывела приглашение,
            // следующая строка после него — строка ввода.
        }

        keyboard::Key::LayoutChanged => {
            // Приглашение показывает раскладку, поэтому перерисовываем его.
            shell::prompt(prompt_row);
        }

        keyboard::Key::ArrowUp => {
            let count = shell::history_len();

            if count > 0 {
                let target = match history_pos {
                    Some(position) => position.saturating_sub(1),
                    None => {
                        draft[..len].copy_from_slice(&input[..len]);
                        draft_len = len;
                        count - 1
                    }
                };

                if let Some(entry) = shell::history_get(target) {
                    let entry_len = entry.len().min(INPUT_CAPACITY);

                    input = [0u8; INPUT_CAPACITY];
                    input[..entry_len].copy_from_slice(&entry[..entry_len]);
                    len = entry_len;
                    history_pos = Some(target);

                    redraw_input(
                        &input[..len],
                        &mut prompt_row,
                        &mut row,
                        &mut col,
                    );
                }
            }
        }

        keyboard::Key::ArrowDown => {
            if let Some(position) = history_pos {
                let count = shell::history_len();

                if position + 1 < count {
                    let target = position + 1;

                    if let Some(entry) = shell::history_get(target) {
                        let entry_len = entry.len().min(INPUT_CAPACITY);

                        input = [0u8; INPUT_CAPACITY];
                        input[..entry_len].copy_from_slice(&entry[..entry_len]);
                        len = entry_len;
                        history_pos = Some(target);

                        redraw_input(
                            &input[..len],
                            &mut prompt_row,
                            &mut row,
                            &mut col,
                        );
                    }
                } else {
                    // Возвращаемся к строке, которую набирали до истории.
                    history_pos = None;
                    let draft_length = draft_len.min(INPUT_CAPACITY);

                    input = [0u8; INPUT_CAPACITY];
                    input[..draft_length]
                        .copy_from_slice(&draft[..draft_length]);
                    len = draft_length;

                    redraw_input(
                        &input[..len],
                        &mut prompt_row,
                        &mut row,
                        &mut col,
                    );
                }
            }
        }

        keyboard::Key::Backspace => {
            if len == 0 {
                continue;
            }

            len -= 1;

            if col > 0 && !(row == prompt_row && col == shell::prompt_width()) {
                col -= 1;
                vga::print_at(row, col, b" ");
            } else if row > prompt_row {
                // Курсор в начале строки — возвращаемся на предыдущую.
                row -= 1;
                col = vga::width() - 1;
                vga::print_at(row, col, b" ");
            } else {
                // Левее приглашения стирать нечего.
                len += 1;
            }
        }

        keyboard::Key::Character(ch) => {
            if len < input.len() {
                // Длинная команда переносится на следующую строку,
                // а при заполнении экрана — прокручивается.
                if col >= vga::width() {
                    row += 1;

                    if row >= vga::height() {
                        vga::scroll_screen();
                        row = vga::height() - 1;
                        prompt_row = prompt_row.saturating_sub(1);
                    }

                    col = 0;
                }

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

        // Специальные клавиши игнорируются в командной строке.
        _ => {}
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