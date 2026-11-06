//! Тетрис в окне pdesk.
//!
//! Классическое поле 10x20. Падение фигур по таймеру запускает оболочка
//! (см. `Desktop::tick`), поэтому игра не зависит от частоты кадров.
//! Управление: стрелки — движение и поворот, пробел/Enter — сброс,
//! после проигрыша Enter начинает заново.

use crate::{Key, Platform, WINDOW_COLOR};

/// Ширина поля в клетках.
const COLS: usize = 10;
/// Высота поля в клетках.
const ROWS: usize = 20;
/// Размер клетки в пикселях.
const CELL: i32 = 16;
/// Высота строки счёта над полем.
const HUD_HEIGHT: i32 = 24;
/// Ширина панели с информацией справа.
const PANEL_WIDTH: i32 = 120;

/// Размер содержимого окна игры.
pub const CONTENT_WIDTH: i32 = COLS as i32 * CELL + 12 + PANEL_WIDTH;
pub const CONTENT_HEIGHT: i32 = HUD_HEIGHT + ROWS as i32 * CELL;

const FIELD_COLOR: u32 = 0x000b0b0b;
const BORDER_COLOR: u32 = 0x005a6a7a;
const TEXT_COLOR: u32 = 0x00d0d0d0;
const LABEL_COLOR: u32 = 0x0070d0ff;
const OVER_COLOR: u32 = 0x00ff6060;

/// Цвета фигур по номеру в поле (0 — пусто).
const COLORS: [u32; 8] = [
    0x00000000,
    0x0038d8f0, // I
    0x003050f0, // J
    0x00f0a020, // L
    0x00f0d020, // O
    0x0040d040, // S
    0x00b060e0, // T
    0x00f05050, // Z
];

/// Формы фигур: 7 фигур по 4 поворота, маска 4x4 (по 4 бита на строку).
const SHAPES: [[u16; 4]; 7] = [
    [0x0f00, 0x2222, 0x00f0, 0x4444], // I
    [0x44c0, 0x8e00, 0x6440, 0x0e20], // J
    [0x4460, 0x0e80, 0xc440, 0x2e00], // L
    [0xcc00, 0xcc00, 0xcc00, 0xcc00], // O
    [0x06c0, 0x8c40, 0x6c00, 0x4620], // S
    [0x0e40, 0x4c40, 0x4e00, 0x4640], // T
    [0xc600, 0x2640, 0x0c60, 0x4c80], // Z
];

/// Состояние игры.
pub struct Tetris {
    board: [[u8; COLS]; ROWS],
    piece: usize,
    next: usize,
    rotation: usize,
    x: i32,
    y: i32,
    score: u32,
    lines: u32,
    game_over: bool,
    seed: u32,
}

impl Tetris {
    pub fn new() -> Self {
        let mut tetris = Self {
            board: [[0; COLS]; ROWS],
            piece: 0,
            next: 0,
            rotation: 0,
            x: 3,
            y: 0,
            score: 0,
            lines: 0,
            game_over: false,
            seed: 0x1234_5678,
        };

        tetris.reset();
        tetris
    }

    /// Начинает новую игру.
    pub fn reset(&mut self) {
        self.board = [[0; COLS]; ROWS];
        self.score = 0;
        self.lines = 0;
        self.game_over = false;
        self.piece = self.random_piece();
        self.next = self.random_piece();
        self.spawn();
    }

    fn random_piece(&mut self) -> usize {
        // xorshift: состояния в несколько байт достаточно для фигур.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;

        (self.seed % 7) as usize
    }

    fn spawn(&mut self) {
        self.rotation = 0;
        self.x = 3;
        self.y = 0;

        if self.collides(self.piece, 0, self.x, self.y) {
            self.game_over = true;
        }
    }

    fn collides(&self, piece: usize, rotation: usize, px: i32, py: i32) -> bool {
        let mask = SHAPES[piece][rotation & 3];

        for row in 0..4i32 {
            for col in 0..4i32 {
                if mask & (1u16 << (row * 4 + col) as u16) == 0 {
                    continue;
                }

                let x = px + col;
                let y = py + row;

                if x < 0 || x >= COLS as i32 || y >= ROWS as i32 {
                    return true;
                }

                if y >= 0 && self.board[y as usize][x as usize] != 0 {
                    return true;
                }
            }
        }

        false
    }

    fn store(&mut self, piece: usize, rotation: usize, px: i32, py: i32) {
        let mask = SHAPES[piece][rotation & 3];
        let color = (piece + 1) as u8;

        for row in 0..4i32 {
            for col in 0..4i32 {
                if mask & (1u16 << (row * 4 + col) as u16) == 0 {
                    continue;
                }

                let x = px + col;
                let y = py + row;

                if x >= 0 && x < COLS as i32 && y >= 0 && y < ROWS as i32 {
                    self.board[y as usize][x as usize] = color;
                }
            }
        }
    }
}


impl Tetris {
    /// Один шаг гравитации. Возвращает `true`, если что-то изменилось.
    pub fn step(&mut self) -> bool {
        if self.game_over {
            return false;
        }

        if !self.collides(self.piece, self.rotation, self.x, self.y + 1) {
            self.y += 1;
            return true;
        }

        self.lock_piece();
        true
    }

    fn lock_piece(&mut self) {
        self.store(self.piece, self.rotation, self.x, self.y);
        self.clear_lines();
        self.piece = self.next;
        self.next = self.random_piece();
        self.spawn();
    }

    fn clear_lines(&mut self) {
        let mut row = ROWS;

        while row > 0 {
            row -= 1;

            if self.board[row].iter().all(|&value| value != 0) {
                for current in (1..=row).rev() {
                    self.board[current] = self.board[current - 1];
                }

                self.board[0] = [0; COLS];
                self.lines += 1;
                self.score += 100;
                // Проверяем эту же строку ещё раз: сверху приехала новая.
                row += 1;
            }
        }
    }

    fn try_move(&mut self, dx: i32) -> bool {
        if self.game_over {
            return false;
        }

        if self.collides(self.piece, self.rotation, self.x + dx, self.y) {
            return false;
        }

        self.x += dx;
        true
    }

    fn try_rotate(&mut self) -> bool {
        if self.game_over {
            return false;
        }

        let rotation = (self.rotation + 1) & 3;

        // Простые "пинки" от стен: пробуем сдвинуть фигуру в сторону.
        for offset in [0, -1, 1, -2, 2] {
            if !self.collides(self.piece, rotation, self.x + offset, self.y) {
                self.rotation = rotation;
                self.x += offset;
                return true;
            }
        }

        false
    }

    fn hard_drop(&mut self) {
        if self.game_over {
            return;
        }

        while !self.collides(self.piece, self.rotation, self.x, self.y + 1) {
            self.y += 1;
        }

        self.lock_piece();
    }

    /// Обрабатывает клавишу. Возвращает `true`, если нужна перерисовка.
    pub fn handle_key(&mut self, key: Key) -> bool {
        if self.game_over {
            if key == Key::Enter {
                self.reset();
                return true;
            }

            return false;
        }

        match key {
            Key::ArrowLeft => self.try_move(-1),
            Key::ArrowRight => self.try_move(1),
            Key::ArrowDown => self.step(),
            Key::ArrowUp => self.try_rotate(),
            Key::Enter | Key::Char(b' ') => {
                self.hard_drop();
                true
            }
            _ => false,
        }
    }
}



impl Tetris {
    /// Рисует игру в прямоугольнике содержимого окна.
    pub fn paint(&self, platform: &mut dyn Platform, x: i32, y: i32) {
        let field_x = x;
        let field_y = y + HUD_HEIGHT;
        let field_width = COLS as i32 * CELL;
        let field_height = ROWS as i32 * CELL;

        // Счёт и число линий.
        let mut buffer = [0u8; 24];
        let mut length = crate::push_text(&mut buffer, 0, b"SCORE ");
        length = crate::push_decimal(&mut buffer, length, self.score);
        platform.draw_text(x, y + 4, &buffer[..length], TEXT_COLOR, WINDOW_COLOR);

        let mut buffer = [0u8; 24];
        let mut length = crate::push_text(&mut buffer, 0, b"LINES ");
        length = crate::push_decimal(&mut buffer, length, self.lines);
        platform.draw_text(x + 150, y + 4, &buffer[..length], TEXT_COLOR, WINDOW_COLOR);

        // Поле и рамка.
        platform.fill_rect(field_x, field_y, field_width, field_height, FIELD_COLOR);
        platform.draw_rect(
            field_x - 1,
            field_y - 1,
            field_width + 2,
            field_height + 2,
            BORDER_COLOR,
        );

        for row in 0..ROWS {
            for col in 0..COLS {
                let value = self.board[row][col];

                if value != 0 {
                    draw_cell(
                        platform,
                        field_x + col as i32 * CELL,
                        field_y + row as i32 * CELL,
                        COLORS[value as usize],
                    );
                }
            }
        }

        // Текущая фигура.
        let mask = SHAPES[self.piece][self.rotation & 3];

        for row in 0..4i32 {
            for col in 0..4i32 {
                if mask & (1u16 << (row * 4 + col) as u16) == 0 {
                    continue;
                }

                let cell_x = field_x + (self.x + col) * CELL;
                let cell_y = field_y + (self.y + row) * CELL;

                if cell_y >= field_y {
                    draw_cell(platform, cell_x, cell_y, COLORS[self.piece + 1]);
                }
            }
        }

        // Панель: следующая фигура.
        let panel_x = field_x + field_width + 12;
        platform.draw_text(panel_x, field_y + 4, b"NEXT", LABEL_COLOR, WINDOW_COLOR);

        let next_mask = SHAPES[self.next][0];

        for row in 0..4i32 {
            for col in 0..4i32 {
                if next_mask & (1u16 << (row * 4 + col) as u16) == 0 {
                    continue;
                }

                draw_cell(
                    platform,
                    panel_x + col * CELL,
                    field_y + 28 + row * CELL,
                    COLORS[self.next + 1],
                );
            }
        }

        if self.game_over {
            platform.draw_text(
                field_x + 16,
                field_y + field_height / 2 - 16,
                b"GAME OVER",
                OVER_COLOR,
                FIELD_COLOR,
            );
            platform.draw_text(
                field_x + 8,
                field_y + field_height / 2 + 4,
                b"ENTER - RESTART",
                TEXT_COLOR,
                FIELD_COLOR,
            );
        }
    }
}

/// Рисует клетку фигуры с небольшим зазором, чтобы поле читалось.
fn draw_cell(platform: &mut dyn Platform, x: i32, y: i32, color: u32) {
    platform.fill_rect(x + 1, y + 1, CELL - 2, CELL - 2, color);
}
