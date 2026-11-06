//! Змейка в стиле Nokia 3310.
//!
//! Поле 20x20, шаг игры запускает оболочка по таймеру. Управление:
//! стрелки, после проигрыша Enter начинает заново.

use crate::{Key, Platform, WINDOW_COLOR};

/// Ширина поля в клетках.
const COLS: i16 = 20;
/// Высота поля в клетках.
const ROWS: i16 = 20;
/// Размер клетки в пикселях.
const CELL: i32 = 16;
/// Высота строки счёта над полем.
const HUD_HEIGHT: i32 = 24;
/// Максимальная длина змейки.
const MAX_LENGTH: usize = (COLS as usize) * (ROWS as usize);

/// Размер содержимого окна игры.
pub const CONTENT_WIDTH: i32 = COLS as i32 * CELL;
pub const CONTENT_HEIGHT: i32 = HUD_HEIGHT + ROWS as i32 * CELL;

// Экран Nokia 3310: светло-зелёный LCD и тёмные пиксели.
const LCD_COLOR: u32 = 0x009bbc0f;
const PIXEL_COLOR: u32 = 0x000f380f;
const BORDER_COLOR: u32 = 0x005a6a7a;
const TEXT_COLOR: u32 = 0x00d0d0d0;
const OVER_COLOR: u32 = 0x00ff6060;

/// Состояние игры.
pub struct Snake {
    /// Тело: body[0] — голова, дальше хвост.
    body: [(i16, i16); MAX_LENGTH],
    length: usize,
    direction: (i16, i16),
    food: (i16, i16),
    score: u32,
    game_over: bool,
    /// Змейка начинает движение после первой стрелки.
    started: bool,
    seed: u32,
}

impl Snake {
    pub fn new() -> Self {
        let mut snake = Self {
            body: [(0, 0); MAX_LENGTH],
            length: 3,
            direction: (1, 0),
            food: (0, 0),
            score: 0,
            game_over: false,
            started: false,
            seed: 0x2545_f491,
        };

        snake.reset();
        snake
    }

    /// Начинает новую игру.
    pub fn reset(&mut self) {
        self.length = 3;
        self.body[0] = (2, 10);
        self.body[1] = (1, 10);
        self.body[2] = (0, 10);
        self.direction = (1, 0);
        self.score = 0;
        self.game_over = false;
        self.started = false;
        self.place_food();
    }

    fn random(&mut self) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed
    }

    fn on_body(&self, x: i16, y: i16) -> bool {
        self.body[..self.length].iter().any(|&cell| cell == (x, y))
    }

    fn place_food(&mut self) {
        for _ in 0..200 {
            let x = (self.random() % COLS as u32) as i16;
            let y = (self.random() % ROWS as u32) as i16;

            if !self.on_body(x, y) {
                self.food = (x, y);
                return;
            }
        }

        // Если не повезло — ищем первую свободную клетку.
        for y in 0..ROWS {
            for x in 0..COLS {
                if !self.on_body(x, y) {
                    self.food = (x, y);
                    return;
                }
            }
        }
    }

    /// Один шаг змейки. Возвращает `true`, если что-то изменилось.
    pub fn step(&mut self) -> bool {
        if self.game_over || !self.started {
            return false;
        }

        let (head_x, head_y) = self.body[0];
        let (step_x, step_y) = self.direction;
        let next = (head_x + step_x, head_y + step_y);

        if next.0 < 0 || next.0 >= COLS || next.1 < 0 || next.1 >= ROWS {
            self.game_over = true;
            return true;
        }

        let eating = next == self.food;
        // Хвост сдвинется, поэтому в него врезаться можно (если не едим).
        let limit = if eating {
            self.length
        } else {
            self.length.saturating_sub(1)
        };

        for index in 0..limit {
            if self.body[index] == next {
                self.game_over = true;
                return true;
            }
        }

        if eating && self.length < MAX_LENGTH {
            self.length += 1;
            self.score += 1;
        }

        for index in (1..self.length).rev() {
            self.body[index] = self.body[index - 1];
        }

        self.body[0] = next;

        if eating {
            self.place_food();
        }

        true
    }
}


impl Snake {
    /// Обрабатывает клавишу. Возвращает `true`, если нужна перерисовка.
    pub fn handle_key(&mut self, key: Key) -> bool {
        if self.game_over {
            if key == Key::Enter {
                self.reset();
                return true;
            }

            return false;
        }

        let next = match key {
            Key::ArrowUp => Some((0, -1)),
            Key::ArrowDown => Some((0, 1)),
            Key::ArrowLeft => Some((-1, 0)),
            Key::ArrowRight => Some((1, 0)),
            _ => None,
        };

        let next = match next {
            Some(direction) => direction,
            None => return false,
        };

        // Первая стрелка запускает движение.
        self.started = true;

        // Разворот на 180 градусов запрещён.
        if next.0 == -self.direction.0 && next.1 == -self.direction.1 {
            return false;
        }

        self.direction = next;
        true
    }

    /// Рисует игру в прямоугольнике содержимого окна.
    pub fn paint(&self, platform: &mut dyn Platform, x: i32, y: i32) {
        let mut buffer = [0u8; 24];
        let mut length = crate::push_text(&mut buffer, 0, b"SCORE ");
        length = crate::push_decimal(&mut buffer, length, self.score);
        platform.draw_text(x, y + 4, &buffer[..length], TEXT_COLOR, WINDOW_COLOR);

        let field_x = x;
        let field_y = y + HUD_HEIGHT;
        let field_width = COLS as i32 * CELL;
        let field_height = ROWS as i32 * CELL;

        // Экран "Nokia": светло-зелёный фон и тёмные клетки.
        platform.fill_rect(field_x, field_y, field_width, field_height, LCD_COLOR);
        platform.draw_rect(
            field_x - 1,
            field_y - 1,
            field_width + 2,
            field_height + 2,
            BORDER_COLOR,
        );

        // Еда.
        draw_block(
            platform,
            field_x + self.food.0 as i32 * CELL,
            field_y + self.food.1 as i32 * CELL,
        );

        for index in 0..self.length {
            let (cell_x, cell_y) = self.body[index];
            draw_block(
                platform,
                field_x + cell_x as i32 * CELL,
                field_y + cell_y as i32 * CELL,
            );
        }

        if !self.started && !self.game_over {
            platform.draw_text(
                field_x + 24,
                field_y + field_height / 2 - 8,
                b"PRESS ARROW",
                PIXEL_COLOR,
                LCD_COLOR,
            );
        }

        if self.game_over {
            platform.draw_text(
                field_x + 48,
                field_y + field_height / 2 - 16,
                b"GAME OVER",
                OVER_COLOR,
                LCD_COLOR,
            );
            platform.draw_text(
                field_x + 24,
                field_y + field_height / 2 + 4,
                b"ENTER - RESTART",
                PIXEL_COLOR,
                LCD_COLOR,
            );
        }
    }
}

/// Рисует сегмент змейки с зазором между клетками.
fn draw_block(platform: &mut dyn Platform, x: i32, y: i32) {
    platform.fill_rect(x + 1, y + 1, CELL - 2, CELL - 2, PIXEL_COLOR);
}
