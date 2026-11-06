//! Указатель мыши.
//!
//! Указатель нельзя просто закрасить пиксели: под ним находится рабочий
//! стол и содержимое окна. Поэтому перед отрисовкой стрелки фон под ней
//! сохраняется, а при перемещении возвращается обратно.

use crate::Platform;

/// Ширина области указателя в пикселях.
pub const WIDTH: i32 = 12;
/// Высота области указателя в пикселях.
pub const HEIGHT: i32 = 20;

const AREA: usize = (WIDTH * HEIGHT) as usize;

const WHITE: u32 = 0x00ffffff;
const BLACK: u32 = 0x00000000;

/// Маска стрелки: `W` — белая заливка, `X` — чёрный контур,
/// точка — прозрачный пиксель (фон не меняется).
const ARROW: [&[u8]; 20] = [
    b"X...........",
    b"XX..........",
    b"XWX.........",
    b"XWWX........",
    b"XWWWX.......",
    b"XWWWWX......",
    b"XWWWWWX.....",
    b"XWWWWWWX....",
    b"XWWWWWWWX...",
    b"XWWWWWWWWX..",
    b"XWWWWWWWWWX.",
    b"XWWWWWWXXXXX",
    b"XWWWXWWX....",
    b"XWWX.XWWX...",
    b"XWX..XWWX...",
    b"XX....XWWX..",
    b"X.....XWWX..",
    b".......XWWX.",
    b".......XWWX.",
    b"........XX..",
];

/// Указатель мыши с сохранением фона под собой.
pub struct Cursor {
    pub x: i32,
    pub y: i32,
    /// Позиция, в которой снят сохранённый фон.
    saved_x: i32,
    saved_y: i32,
    saved: [u32; AREA],
    visible: bool,
}

impl Cursor {
    pub const fn new(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            saved_x: 0,
            saved_y: 0,
            saved: [0; AREA],
            visible: false,
        }
    }

    /// Убирает указатель с экрана, возвращая сохранённый фон.
    ///
    /// Восстановление идёт по позиции, в которой фон был снят, а не по
    /// текущей: указатель мог уже переместиться, и тогда старая стрелка
    /// осталась бы на экране (mouse trails).
    pub fn erase(&mut self, platform: &mut dyn Platform) {
        if !self.visible {
            return;
        }

        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                let color = self.saved[(row * WIDTH + column) as usize];
                platform.write_pixel(self.saved_x + column, self.saved_y + row, color);
            }
        }

        self.visible = false;
    }

    /// Запоминает фон под указателем и рисует стрелку.
    pub fn draw(&mut self, platform: &mut dyn Platform) {
        self.saved_x = self.x;
        self.saved_y = self.y;

        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                self.saved[(row * WIDTH + column) as usize] =
                    platform.read_pixel(self.saved_x + column, self.saved_y + row);
            }
        }

        for row in 0..HEIGHT {
            let mask = ARROW[row as usize];

            for column in 0..WIDTH {
                let color = match mask[column as usize] {
                    b'W' => WHITE,
                    b'X' => BLACK,
                    _ => continue,
                };

                platform.write_pixel(self.saved_x + column, self.saved_y + row, color);
            }
        }

        self.visible = true;
    }
}
