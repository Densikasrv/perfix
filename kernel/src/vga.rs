const WIDTH: usize = 80;
const HEIGHT: usize = 25;
const VGA: *mut u8 = 0xb8000 as *mut u8;
const COLOR: u8 = 0x0f;

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