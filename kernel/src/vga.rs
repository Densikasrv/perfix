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

    // Прокрутка стирает нижнюю строку — возвращаем на место индикатор.
    print_status(crate::keyboard::is_russian());
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

fn read_register(index_port: u16, data_port: u16, index: u8) -> u8 {
    outb(index_port, index);
    inb(data_port)
}

fn write_register(index_port: u16, data_port: u16, index: u8, value: u8) {
    outb(index_port, index);
    outb(data_port, value);
}

/// Загружает глифы 8x16 в знакогенератор VGA (плоскость 2).
///
/// Стандартный шрифт содержит только CP437, поэтому коды, которых в нём
/// нет (например, кириллица CP866), нужно предварительно загрузить.
/// Каждый глиф занимает 32 байта: `код * 32`, 16 строк по 8 пикселей.
pub fn load_glyphs(glyphs: &[(u8, [u8; 16])]) {
    if glyphs.is_empty() {
        return;
    }

    // Сохраняем регистры текстового режима, чтобы вернуть их после записи.
    let saved_map_mask = read_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x02);
    let saved_memory_mode = read_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x04);
    let saved_read_map = read_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x04);
    let saved_graphics_mode = read_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x05);
    let saved_misc = read_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x06);

    // Открываем плоскость 2 на запись.
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x02, 0x04);
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x04, 0x07);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x04, 0x02);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x05, 0x00);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x06, 0x0c);

    for &(code, bitmap) in glyphs {
        let offset = code as usize * 32;

        for (row, &byte) in bitmap.iter().enumerate() {
            unsafe {
                FONT_RAM.add(offset + row).write_volatile(byte);
            }
        }
    }

    // Возвращаем режим, в котором работает текстовый вывод.
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x02, saved_map_mask);
    write_register(SEQUENCER_INDEX, SEQUENCER_DATA, 0x04, saved_memory_mode);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x04, saved_read_map);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x05, saved_graphics_mode);
    write_register(GRAPHICS_INDEX, GRAPHICS_DATA, 0x06, saved_misc);
}

/// Показывает текущую раскладку в правом нижнем углу экрана.
pub fn print_status(russian: bool) {
    print_at(HEIGHT - 1, WIDTH - 3, if russian { b"RU" } else { b"EN" });
}
