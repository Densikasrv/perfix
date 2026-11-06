//! pterm — терминал в окне pdesk.
//!
//! Команды выполняет консольная оболочка Perfix: ядро запускает их в
//! виртуальной консоли 80x40 и возвращает текст вывода (см.
//! `kernel/src/pterm.rs`). Здесь хранятся только прокручиваемый буфер
//! вывода, строка ввода и приглашение с раскладкой и каталогом.

use crate::{Key, Platform, PROMPT_COLOR, TEXT_COLOR, WINDOW_COLOR};

/// Ширина терминала в символах.
pub const COLS: usize = 80;
/// Высота терминала в символах (последняя строка — строка ввода).
pub const ROWS: usize = 40;
/// Ширина знакоместа шрифта 8x16.
pub const CHAR_WIDTH: i32 = 8;
/// Высота знакоместа шрифта 8x16.
pub const CHAR_HEIGHT: i32 = 16;
/// Предельная длина приглашения оболочки.
pub const PROMPT_CAPACITY: usize = 64;
/// Ёмкость буфера вывода одной команды (строки через `\n`).
pub const OUTPUT_CAPACITY: usize = COLS * ROWS + ROWS;

/// Сколько строк прокручиваемого вывода помещается в окне.
const SCROLL_ROWS: usize = ROWS - 1;

/// Состояние окна терминала.
pub struct Terminal {
    lines: [[u8; COLS]; SCROLL_ROWS],
    lengths: [usize; SCROLL_ROWS],
    count: usize,
    input: [u8; COLS],
    input_len: usize,
    prompt: [u8; PROMPT_CAPACITY],
    prompt_len: usize,
}

impl Terminal {
    pub fn new() -> Self {
        Self {
            lines: [[b' '; COLS]; SCROLL_ROWS],
            lengths: [0; SCROLL_ROWS],
            count: 0,
            input: [0; COLS],
            input_len: 0,
            prompt: [0; PROMPT_CAPACITY],
            prompt_len: 0,
        }
    }

    /// Устанавливает приглашение оболочки (раскладка и текущий каталог).
    pub fn set_prompt(&mut self, text: &[u8]) {
        self.prompt_len = text.len().min(PROMPT_CAPACITY);
        self.prompt[..self.prompt_len].copy_from_slice(&text[..self.prompt_len]);
    }

    /// Добавляет строку в буфер вывода с прокруткой.
    pub fn push_line(&mut self, text: &[u8]) {
        let text = &text[..text.len().min(COLS)];

        if self.count == SCROLL_ROWS {
            for row in 1..SCROLL_ROWS {
                self.lines[row - 1] = self.lines[row];
                self.lengths[row - 1] = self.lengths[row];
            }

            self.count -= 1;
        }

        let row = self.count;
        self.lines[row] = [b' '; COLS];
        self.lines[row][..text.len()].copy_from_slice(text);
        self.lengths[row] = text.len();
        self.count += 1;
    }

    /// Добавляет многострочный текст вывода команды.
    pub fn append_text(&mut self, text: &[u8]) {
        let mut start = 0usize;

        for (index, &byte) in text.iter().enumerate() {
            if byte == b'\n' {
                self.push_line(&text[start..index]);
                start = index + 1;
            }
        }

        if start < text.len() {
            self.push_line(&text[start..]);
        }
    }

    /// Выводит в буфер строку с приглашением и введённой командой.
    pub fn echo(&mut self, command: &[u8]) {
        let mut line = [b' '; COLS];
        let mut length = 0usize;

        for &byte in &self.prompt[..self.prompt_len] {
            if length >= COLS {
                break;
            }

            line[length] = byte;
            length += 1;
        }

        for &byte in command {
            if length >= COLS {
                break;
            }

            line[length] = byte;
            length += 1;
        }

        self.push_line(&line[..length]);
    }

    /// Забирает введённую строку и очищает строку ввода.
    pub fn take_input(&mut self, out: &mut [u8]) -> usize {
        let length = self.input_len.min(out.len());
        out[..length].copy_from_slice(&self.input[..length]);
        self.input_len = 0;
        length
    }

    /// Очищает экран терминала.
    pub fn clear(&mut self) {
        self.count = 0;
    }

    /// Начинает новую сессию: очищает экран и печатает приветствие.
    pub fn reset(&mut self) {
        self.clear();
        self.input_len = 0;

        self.push_line(b"Perfix pterm");
        self.push_line(b"Commands are executed by the Perfix shell.");
        self.push_line(b"Type help to list commands, exit to close pdesk.");
        self.push_line(b"");
    }

    /// Обрабатывает редактирование строки ввода.
    ///
    /// Возвращает `true`, если содержимое окна изменилось.
    pub fn handle_key(&mut self, key: Key) -> bool {
        match key {
            Key::Char(ch) => {
                // Оставляем место под приглашение и каретку.
                if self.input_len + self.prompt_len < COLS {
                    self.input[self.input_len] = ch;
                    self.input_len += 1;
                    true
                } else {
                    false
                }
            }

            Key::Backspace => {
                if self.input_len > 0 {
                    self.input_len -= 1;
                    true
                } else {
                    false
                }
            }

            _ => false,
        }
    }

    /// Рисует содержимое терминала в прямоугольнике с левым верхним углом
    /// `(x, y)`. Фон клиентской области уже залит вызывающей стороной.
    pub fn paint(&self, platform: &mut dyn Platform, x: i32, y: i32) {
        for row in 0..self.count {
            platform.draw_text(
                x,
                y + row as i32 * CHAR_HEIGHT,
                &self.lines[row][..self.lengths[row]],
                TEXT_COLOR,
                WINDOW_COLOR,
            );
        }

        let input_y = y + SCROLL_ROWS as i32 * CHAR_HEIGHT;

        platform.draw_text(
            x,
            input_y,
            &self.prompt[..self.prompt_len],
            PROMPT_COLOR,
            WINDOW_COLOR,
        );
        platform.draw_text(
            x + self.prompt_len as i32 * CHAR_WIDTH,
            input_y,
            &self.input[..self.input_len],
            TEXT_COLOR,
            WINDOW_COLOR,
        );

        // Каретка ввода подчёркивает позицию следующего символа.
        let caret_x = x + (self.prompt_len + self.input_len) as i32 * CHAR_WIDTH;
        platform.fill_rect(
            caret_x,
            input_y + CHAR_HEIGHT - 2,
            CHAR_WIDTH,
            2,
            PROMPT_COLOR,
        );
    }
}
