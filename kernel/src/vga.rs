use core::arch::asm;

use crate::font_8x16;

/// Максимальный размер виртуальной консоли.
pub const MAX_WIDTH: usize = 240;
pub const MAX_HEIGHT: usize = 67;
/// Обычный цвет текста: белый на чёрном.
pub const COLOR_NORMAL: u8 = 0x0f;
/// Инверсный цвет для заголовка и строк состояния.
pub const COLOR_INVERSE: u8 = 0x70;

const GLYPH_WIDTH: usize = 8;
const GLYPH_HEIGHT: usize = 16;
const BYTES_PER_PIXEL: usize = 4;

const VGA: *mut u8 = 0xb8000 as *mut u8;
const FONT_RAM: *mut u8 = 0xa0000 as *mut u8;

const SEQUENCER_INDEX: u16 = 0x3c4;
const SEQUENCER_DATA: u16 = 0x3c5;
const GRAPHICS_INDEX: u16 = 0x3ce;
const GRAPHICS_DATA: u16 = 0x3cf;

/// Информация о линейном framebuffer из Multiboot.
pub struct FramebufferInfo {
    pub address: u64,
    pub pitch: u32,
    pub width: u32,
    pub height: u32,
    pub bpp: u8,
    pub kind: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Text,
    Framebuffer,
}

static mut MODE: Mode = Mode::Text;
static mut SCREEN_WIDTH: usize = 80;
static mut SCREEN_HEIGHT: usize = 25;
static mut FRAMEBUFFER: *mut u8 = core::ptr::null_mut();
static mut PITCH: usize = 0;
static mut PIXEL_HEIGHT: usize = 0;
static mut CURSOR: Option<(usize, usize)> = None;
static mut CONSOLE: [u8; MAX_WIDTH * MAX_HEIGHT * 2] =
    [0; MAX_WIDTH * MAX_HEIGHT * 2];

/// Стандартная палитра VGA в формате 0x00RRGGBB.
const PALETTE: [u32; 16] = [
    0x000000, 0x0000aa, 0x00aa00, 0x00aaaa,
    0xaa0000, 0xaa00aa, 0xaa5500, 0xaaaaaa,
    0x555555, 0x5555ff, 0x55ff55, 0x55ffff,
    0xff5555, 0xff55ff, 0xffff55, 0xffffff,
];

/// Инициализирует консоль. При наличии VBE использует framebuffer,
/// иначе остаётся в стандартном текстовом режиме 80x25.
pub fn init(info: Option<FramebufferInfo>) {
    if let Some(info) = info {
        if info.kind == 1
            && info.bpp == 32
            && info.address != 0
            && info.address <= u32::MAX as u64
        {
            let columns = info.width as usize / GLYPH_WIDTH;
            let rows = info.height as usize / GLYPH_HEIGHT;

            if columns >= 80 && rows >= 25 {
                unsafe {
                    SCREEN_WIDTH = columns.min(MAX_WIDTH);
                    SCREEN_HEIGHT = rows
                        .min(MAX_HEIGHT)
                        .saturating_sub(if is_virtualbox() { 3 } else { 0 })
                        .max(25);
                    FRAMEBUFFER = info.address as *mut u8;
                    PITCH = info.pitch as usize;
                    PIXEL_HEIGHT = info.height as usize;
                    MODE = Mode::Framebuffer;
                    CURSOR = None;
                }

                clear_screen();
                return;
            }
        }
    }

    unsafe {
        MODE = Mode::Text;
        SCREEN_WIDTH = 80;
        SCREEN_HEIGHT = 25;
        CURSOR = None;
    }
}

pub fn width() -> usize {
    unsafe { SCREEN_WIDTH }
}

pub fn height() -> usize {
    unsafe { SCREEN_HEIGHT }
}

fn mode() -> Mode {
    unsafe { MODE }
}

/// Копирует `count` 32-битных слов с перекрытием вперёд (src > dst).
fn copy_dwords(source: usize, destination: usize, count: usize) {
    unsafe {
        core::arch::asm!(
            "push esi",
            "push edi",
            "mov esi, eax",
            "mov edi, edx",
            "cld",
            "rep movsd",
            "pop edi",
            "pop esi",
            in("eax") source as u32,
            in("edx") destination as u32,
            inout("ecx") count as u32 => _,
        );
    }
}

/// Заполняет `count` 32-битных слов нулями.
fn clear_dwords(destination: usize, count: usize) {
    unsafe {
        core::arch::asm!(
            "push eax",
            "push edi",
            "mov edi, eax",
            "xor eax, eax",
            "cld",
            "rep stosd",
            "pop edi",
            "pop eax",
            in("eax") destination as u32,
            inout("ecx") count as u32 => _,
        );
    }
}

fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let mut eax = leaf;
    let mut ebx = 0u32;
    let mut ecx = 0u32;
    let mut edx = 0u32;

    unsafe {
        core::arch::asm!(
            "cpuid",
            inout("eax") eax,
            inout("ebx") ebx,
            inout("ecx") ecx,
            inout("edx") edx,
        );
    }

    (eax, ebx, ecx, edx)
}

/// VirtualBox рисует меню и статусную строку поверх/вокруг гостевого экрана,
/// поэтому нижние строки консоли могут оказаться за краем окна.
fn is_virtualbox() -> bool {
    let (_, ebx, ecx, edx) = cpuid(0x4000_0000);
    let mut vendor = [0u8; 12];

    vendor[0..4].copy_from_slice(&ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&ecx.to_le_bytes());
    vendor[8..12].copy_from_slice(&edx.to_le_bytes());

    &vendor == b"VBoxVBoxVBox"
}

fn fg_color(attribute: u8) -> u32 {
    PALETTE[(attribute & 0x0f) as usize]
}

fn bg_color(attribute: u8) -> u32 {
    PALETTE[((attribute >> 4) & 0x0f) as usize]
}

fn console_store(row: usize, col: usize, byte: u8, color: u8) {
    let width = width();
    let index = (row * width + col) * 2;

    unsafe {
        CONSOLE[index] = byte;
        CONSOLE[index + 1] = color;
    }
}

fn console_load(row: usize, col: usize) -> (u8, u8) {
    let width = width();
    let index = (row * width + col) * 2;

    unsafe { (CONSOLE[index], CONSOLE[index + 1]) }
}

fn fb_pixel(x: usize, y: usize, color: u32) {
    let pitch = unsafe { PITCH };
    let framebuffer = unsafe { FRAMEBUFFER };

    if framebuffer.is_null() {
        return;
    }

    let offset = y * pitch + x * BYTES_PER_PIXEL;

    unsafe {
        (framebuffer.add(offset) as *mut u32).write_volatile(color);
    }
}

fn draw_cell(row: usize, col: usize, byte: u8, color: u8) {
    if mode() != Mode::Framebuffer {
        return;
    }

    let glyph = &font_8x16::GLYPHS[byte as usize];
    let foreground = fg_color(color);
    let background = bg_color(color);
    let x = col * GLYPH_WIDTH;
    let y = row * GLYPH_HEIGHT;

    for (row_index, bits) in glyph.iter().enumerate() {
        for col_index in 0..GLYPH_WIDTH {
            let on = bits & (0x80 >> col_index) != 0;
            let pixel = if on { foreground } else { background };
            fb_pixel(x + col_index, y + row_index, pixel);
        }
    }
}

fn draw_cursor(row: usize, col: usize) {
    let (_, color) = console_load(row, col);
    let background = bg_color(color);
    let cursor_color = if background == 0xffffff { 0x000000 } else { 0xffffff };
    let x = col * GLYPH_WIDTH;
    let y = row * GLYPH_HEIGHT + GLYPH_HEIGHT - 2;

    for row_index in 0..2 {
        for col_index in 0..GLYPH_WIDTH {
            fb_pixel(x + col_index, y + row_index, cursor_color);
        }
    }
}

fn put_byte_color(row: usize, col: usize, byte: u8, color: u8) {
    if row >= height() || col >= width() {
        return;
    }

    match mode() {
        Mode::Text => {
            let offset = (row * width() + col) * 2;

            unsafe {
                VGA.add(offset).write_volatile(byte);
                VGA.add(offset + 1).write_volatile(color);
            }
        }
        Mode::Framebuffer => {
            let (old_byte, old_color) = console_load(row, col);

            if old_byte == byte && old_color == color {
                return;
            }

            if let Some((cursor_row, cursor_col)) = unsafe { CURSOR } {
                if cursor_row == row && cursor_col == col {
                    let (old_byte, old_color) = console_load(row, col);
                    draw_cell(row, col, old_byte, old_color);
                }
            }

            console_store(row, col, byte, color);
            draw_cell(row, col, byte, color);

            if let Some((cursor_row, cursor_col)) = unsafe { CURSOR } {
                if cursor_row == row && cursor_col == col {
                    draw_cursor(row, col);
                }
            }
        }
    }
}

/// Записывает строку целиком, перерисовывая только изменившиеся ячейки.
///
/// Недостающие до ширины экрана символы заполняются пробелами.
pub fn write_row(row: usize, text: &[u8], color: u8) {
    if row >= height() {
        return;
    }

    let width = width();

    match mode() {
        Mode::Text => {
            for col in 0..width {
                let byte = text.get(col).copied().unwrap_or(b' ');
                put_byte_color(row, col, byte, color);
            }
        }
        Mode::Framebuffer => {
            for col in 0..width {
                let byte = text.get(col).copied().unwrap_or(b' ');
                let (old_byte, old_color) = console_load(row, col);

                if old_byte == byte && old_color == color {
                    continue;
                }

                console_store(row, col, byte, color);
                draw_cell(row, col, byte, color);
            }
        }
    }
}

fn put_byte(row: usize, col: usize, byte: u8) {
    put_byte_color(row, col, byte, COLOR_NORMAL);
}

pub fn print_at(row: usize, col: usize, text: &[u8]) {
    print_at_color(row, col, text, COLOR_NORMAL);
}

/// Выводит текст с заданным атрибутом цвета.
pub fn print_at_color(row: usize, col: usize, text: &[u8], color: u8) {
    let width = width();

    for (index, &byte) in text.iter().enumerate() {
        if col + index >= width {
            break;
        }

        put_byte_color(row, col + index, byte, color);
    }
}

/// Заполняет всю строку пробелами заданного цвета.
pub fn fill_row(row: usize, color: u8) {
    for col in 0..width() {
        put_byte_color(row, col, b' ', color);
    }
}

pub fn clear_screen() {
    match mode() {
        Mode::Text => {
            for row in 0..height() {
                for col in 0..width() {
                    put_byte(row, col, b' ');
                }
            }
        }
        Mode::Framebuffer => {
            let framebuffer = unsafe { FRAMEBUFFER };
            let size = unsafe { PITCH * PIXEL_HEIGHT };

            if !framebuffer.is_null() {
                unsafe {
                    core::ptr::write_bytes(framebuffer, 0, size);
                }
            }

            let width = width();
            let height = height();

            unsafe {
                for index in 0..width * height {
                    CONSOLE[index * 2] = b' ';
                    CONSOLE[index * 2 + 1] = COLOR_NORMAL;
                }

                CURSOR = None;
            }
        }
    }
}

/// Перерисовывает только непустые знакоместа (после очистки framebuffer).
fn repaint_nonblank() {
    let width = width();
    let height = height();

    for row in 0..height {
        for col in 0..width {
            let (byte, color) = console_load(row, col);

            if byte != b' ' || color != COLOR_NORMAL {
                draw_cell(row, col, byte, color);
            }
        }
    }
}

pub fn scroll_screen() {
    match mode() {
        Mode::Text => {
            let width = width();
            let height = height();

            for row in 1..height {
                for col in 0..width {
                    let src = (row * width + col) * 2;
                    let dst = ((row - 1) * width + col) * 2;

                    unsafe {
                        let character = VGA.add(src).read_volatile();
                        let color = VGA.add(src + 1).read_volatile();

                        VGA.add(dst).write_volatile(character);
                        VGA.add(dst + 1).write_volatile(color);
                    }
                }
            }

            for col in 0..width {
                put_byte(height - 1, col, b' ');
            }
        }
        Mode::Framebuffer => {
            let width = width();
            let height = height();

            if height == 0 || width == 0 {
                return;
            }

            let row_bytes = width * 2;
            let console = core::ptr::addr_of_mut!(CONSOLE) as *mut u8;

            unsafe {
                copy_dwords(
                    console.add(row_bytes) as usize,
                    console as usize,
                    ((height - 1) * row_bytes) / 4,
                );

                for col in 0..width {
                    let index = ((height - 1) * width + col) * 2;
                    CONSOLE[index] = b' ';
                    CONSOLE[index + 1] = COLOR_NORMAL;
                }

                CURSOR = None;
            }

            let framebuffer = unsafe { FRAMEBUFFER };
            let pitch = unsafe { PITCH };

            if !framebuffer.is_null() && pitch != 0 {
                let pixel_height = unsafe { PIXEL_HEIGHT };

                clear_dwords(
                    framebuffer as usize,
                    (pitch * pixel_height) / 4,
                );
            }

            repaint_nonblank();
        }
    }
}

/// Показывает курсор консоли.
pub fn set_cursor(row: usize, col: usize) {
    if row >= height() || col >= width() {
        hide_cursor();
        return;
    }

    match mode() {
        Mode::Text => text_set_cursor(row, col),
        Mode::Framebuffer => unsafe {
            if let Some((old_row, old_col)) = CURSOR {
                let (byte, color) = console_load(old_row, old_col);
                draw_cell(old_row, old_col, byte, color);
            }

            CURSOR = Some((row, col));
            draw_cursor(row, col);
        },
    }
}

/// Скрывает курсор консоли.
pub fn hide_cursor() {
    match mode() {
        Mode::Text => text_hide_cursor(),
        Mode::Framebuffer => unsafe {
            if let Some((row, col)) = CURSOR {
                let (byte, color) = console_load(row, col);
                draw_cell(row, col, byte, color);
            }

            CURSOR = None;
        },
    }
}

fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn inb(port: u16) -> u8 {
    let value: u8;

    unsafe {
        asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }

    value
}

fn write_register(index_port: u16, data_port: u16, index: u8, value: u8) {
    outb(index_port, index);
    outb(data_port, value);
}

fn text_set_cursor(row: usize, col: usize) {
    let position = (row * width() + col) as u16;

    // Размер курсора: нижние две строки знакоместа 8x16.
    outb(0x3d4, 0x0a);
    outb(0x3d5, 0x0e);
    outb(0x3d4, 0x0b);
    outb(0x3d5, 0x0f);

    outb(0x3d4, 0x0e);
    outb(0x3d5, (position >> 8) as u8);
    outb(0x3d4, 0x0f);
    outb(0x3d5, position as u8);
}

fn text_hide_cursor() {
    outb(0x3d4, 0x0a);
    outb(0x3d5, 0x20);
}

/// Загружает глифы 8x16 в знакогенератор VGA (только текстовый режим).
///
/// В framebuffer-режиме используется полная таблица `font_8x16`, поэтому
/// загрузка в видеопамять не требуется.
pub fn load_glyphs(glyphs: &[(u8, [u8; 16])]) {
    if mode() == Mode::Framebuffer || glyphs.is_empty() {
        return;
    }

    let cell_height = crtc_cell_height();

    // Временный режим: плоскость 2 в окне A0000-BFFFF, odd/even выключен.
    // GR6: биты 2-3 задают Memory Map Select, 00 — окно 128K с A0000.
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x02, 0x04);
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x03, 0x00);
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x04, 0x07);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x04, 0x02);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x05, 0x00);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x06, 0x00);

    for &(code, bitmap) in glyphs {
        let offset = code as usize * 32;
        let rows = cell_height.min(32);
        let mut cell = [0u8; 32];

        match rows {
            // Отображаем 16 строк шрифта в 8-строчное знакоместо.
            0..=8 => {
                for row in 0..8 {
                    cell[row] = bitmap[row * 2] | bitmap[row * 2 + 1];
                }
            }
            16 => cell[..16].copy_from_slice(&bitmap),
            // Для знакомест большей высоты строки дублируются.
            _ => {
                for row in 0..rows {
                    cell[row] = bitmap[row * 16 / rows];
                }
            }
        }

        for row in 0..rows {
            unsafe {
                FONT_RAM.add(offset + row).write_volatile(cell[row]);
            }
        }
    }

    // Возвращаем стандартный текстовый режим 80x25.
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x02, 0x03);
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x04, 0x02);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x04, 0x00);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x05, 0x10);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x06, 0x0e);
}

/// Выводит кириллицу CP866 для проверки загруженного шрифта.
pub fn print_charset(row: usize) -> usize {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";

    print_at(row, 0, b"CP866 charset:");

    let starts: [u8; 4] = [0x80, 0x90, 0xA0, 0xE0];

    for (index, &start) in starts.iter().enumerate() {
        let mut line = [b' '; 80];
        let mut len = 0usize;

        line[len] = b'0';
        line[len + 1] = b'x';
        len += 2;
        line[len] = DIGITS[(start >> 4) as usize];
        len += 1;
        line[len] = DIGITS[(start & 0x0f) as usize];
        len += 1;
        line[len] = b':';
        line[len + 1] = b' ';
        len += 2;

        for code in start..start + 16 {
            line[len] = code;
            len += 1;
        }

        print_at(row + 1 + index, 0, &line[..len]);
    }

    let tail = [0xf0u8, b' ', 0xf1, b' ', 0xfc];
    print_at(row + 5, 0, &tail);

    6
}

/// Высота знакоместа в строках (CRTC Maximum Scan Line + 1).
fn crtc_cell_height() -> usize {
    outb(0x3d4, 0x09);

    (inb(0x3d5) & 0x1f) as usize + 1
}
