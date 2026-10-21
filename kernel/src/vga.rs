use core::arch::asm;

const WIDTH: usize = 80;
const HEIGHT: usize = 25;
const VGA: *mut u8 = 0xb8000 as *mut u8;
const COLOR: u8 = 0x0f;

/// Область знакогенератора в плоскости 2 видеопамяти.
const FONT_RAM: *mut u8 = 0xa0000 as *mut u8;

/// Порт индекса/данных Sequencer.
const SEQUENCER_INDEX: u16 = 0x3c4;
const SEQUENCER_DATA: u16 = 0x3c5;

/// Порт индекса/данных Graphics Controller.
const GRAPHICS_INDEX: u16 = 0x3ce;
const GRAPHICS_DATA: u16 = 0x3cf;


fn put_byte(row: usize, col: usize, byte: u8) {
    if row >= HEIGHT || col >= WIDTH {
        return;
    }

    let offset = (row * WIDTH + col) * 2;

    unsafe {
        VGA.add(offset).write_volatile(byte);
        VGA.add(offset + 1).write_volatile(COLOR);
    }
}

pub fn print_at(row: usize, col: usize, text: &[u8]) {
    for (i, &byte) in text.iter().enumerate() {
        if col + i >= WIDTH {
            break;
        }

        put_byte(row, col + i, byte);
    }
}

pub fn clear_screen() {
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            put_byte(row, col, b' ');
        }
    }
}

pub fn scroll_screen() {
    for row in 1..HEIGHT {
        for col in 0..WIDTH {
            let src = (row * WIDTH + col) * 2;
            let dst = ((row - 1) * WIDTH + col) * 2;

            unsafe {
                let character = VGA.add(src).read_volatile();
                let color = VGA.add(src + 1).read_volatile();

                VGA.add(dst).write_volatile(character);
                VGA.add(dst + 1).write_volatile(color);
            }
        }
    }

    for col in 0..WIDTH {
        put_byte(HEIGHT - 1, col, b' ');
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

/// Загружает глифы 8x16 в знакогенератор VGA (плоскость 2).
///
/// Стандартный шрифт содержит только CP437, поэтому коды, которых в нём
/// нет (например, кириллица CP866), нужно предварительно загрузить.
/// Каждый глиф занимает 32 байта: `код * 32`, по одной строке на байт.
/// Высота знакоместа берётся из CRTC и может быть 8, 16 или 32 строки.
pub fn load_glyphs(glyphs: &[(u8, [u8; 16])]) {
    if glyphs.is_empty() {
        return;
    }

    let cell_height = crtc_cell_height();

    // Временный режим: плоскость 2 открыта на запись целиком.
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x02, 0x04);
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x04, 0x07);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x04, 0x02);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x05, 0x00);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x06, 0x0c);

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

    // Возвращаем стандартный текстовый режим 80x25. Значения фиксированные:
    // читать эти регистры обратно можно не на всех видеокартах.
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

