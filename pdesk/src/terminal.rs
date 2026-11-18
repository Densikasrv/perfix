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

/// Сколько последних команд хранит история (как в консольной оболочке).
const HISTORY_DEPTH: usize = 16;

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
    /// История команд: 0 — самая старая, `history_count - 1` — последняя.
    history: [[u8; COLS]; HISTORY_DEPTH],
    history_lengths: [usize; HISTORY_DEPTH],
    history_count: usize,
    /// Позиция просмотра истории; `None` — редактируется текущий ввод.
    history_pos: Option<usize>,
    /// Неоконченная строка, сохранённая при входе в историю.
    draft: [u8; COLS],
    draft_len: usize,
    /// Вводится пароль: символы не показываются, строка подписана `password:`.
    secret: bool,
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
            history: [[0; COLS]; HISTORY_DEPTH],
            history_lengths: [0; HISTORY_DEPTH],
            history_count: 0,
            history_pos: None,
            draft: [0; COLS],
            draft_len: 0,
            secret: false,
        }
    }

    /// Переключает ввод пароля (символы маскируются).
    pub fn set_secret(&mut self, secret: bool) {
        self.secret = secret;
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
        self.history_pos = None;
        self.draft_len = 0;
        length
    }

    /// Добавляет выполненную команду в историю.
    pub fn history_add(&mut self, command: &[u8]) {
        if command.is_empty() {
            return;
        }

        let length = command.len().min(COLS);

        // Две одинаковые команды подряд не дублируются.
        if self.history_count > 0 {
            let last = self.history_count - 1;

            if self.history_lengths[last] == length
                && self.history[last][..length] == command[..length]
            {
                return;
            }
        }

        let slot = if self.history_count < HISTORY_DEPTH {
            self.history_count
        } else {
            // Сдвигаем историю влево, освобождая последний слот.
            for index in 1..HISTORY_DEPTH {
                self.history[index - 1] = self.history[index];
                self.history_lengths[index - 1] = self.history_lengths[index];
            }

            HISTORY_DEPTH - 1
        };

        self.history[slot][..length].copy_from_slice(&command[..length]);
        self.history_lengths[slot] = length;
        self.history_count = (self.history_count + 1).min(HISTORY_DEPTH);
        self.history_pos = None;
        self.draft_len = 0;
    }

    /// Подставляет в строку ввода команду истории.
    fn load_history(&mut self, index: usize) {
        let length = self.history_lengths[index];

        self.input = [0; COLS];
        self.input[..length].copy_from_slice(&self.history[index][..length]);
        self.input_len = length;
        self.history_pos = Some(index);
    }

    /// Листает историю к предыдущей (более старой) команде.
    fn history_previous(&mut self) -> bool {
        if self.history_count == 0 {
            return false;
        }

        let target = match self.history_pos {
            Some(position) => position.saturating_sub(1),
            None => {
                // Запоминаем неоконченный ввод, чтобы вернуть его вниз.
                self.draft[..self.input_len]
                    .copy_from_slice(&self.input[..self.input_len]);
                self.draft_len = self.input_len;
                self.history_count - 1
            }
        };

        self.load_history(target);
        true
    }

    /// Листает историю к следующей (более новой) команде.
    fn history_next(&mut self) -> bool {
        let Some(position) = self.history_pos else {
            return false;
        };

        if position + 1 < self.history_count {
            self.load_history(position + 1);
            return true;
        }

        // После самой новой команды возвращаем неоконченный ввод.
        let length = self.draft_len.min(COLS);

        self.input = [0; COLS];
        self.input[..length].copy_from_slice(&self.draft[..length]);
        self.input_len = length;
        self.history_pos = None;
        true
    }

    /// Очищает экран терминала.
    pub fn clear(&mut self) {
        self.count = 0;
    }

    /// Начинает новую сессию: очищает экран и печатает приветствие.
    pub fn reset(&mut self) {
        self.clear();
        self.input_len = 0;
        self.history_pos = None;
        self.draft_len = 0;
        self.secret = false;

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

            Key::ArrowUp => self.history_previous(),
            Key::ArrowDown => self.history_next(),

            _ => false,
        }
    }

    /// Рисует содержимое терминала в клиентской области окна.
    ///
    /// Размер области может отличаться от 80x40: окно подстраивается под
    /// разрешение экрана и тянется мышью, поэтому показываем столько
    /// столбцов и строк, сколько помещается, прижимая вывод к нижней
    /// строке ввода.
    pub fn paint(&self, platform: &mut dyn Platform, area: crate::window::Rect) {
        platform.fill_rect(area.x, area.y, area.width, area.height, WINDOW_COLOR);

        let columns = (area.width / CHAR_WIDTH).max(1) as usize;
        let rows = (area.height / CHAR_HEIGHT).max(1) as usize;
        let scroll_rows = rows.saturating_sub(1).max(1);
        let start = self.count.saturating_sub(scroll_rows);

        for (index, row) in (start..self.count).enumerate() {
            let length = self.lengths[row].min(columns);

            platform.draw_text(
                area.x,
                area.y + index as i32 * CHAR_HEIGHT,
                &self.lines[row][..length],
                TEXT_COLOR,
                WINDOW_COLOR,
            );
        }

        let input_y = area.y + (rows - 1) as i32 * CHAR_HEIGHT;
        let caret_x;

        if self.secret {
            // Пароль не показываем: только подпись и звёздочки.
            let label = b"password: ";
            let label_len = label.len().min(columns);
            let stars_len = self
                .input_len
                .min(columns.saturating_sub(label_len));

            platform.draw_text(
                area.x,
                input_y,
                &label[..label_len],
                PROMPT_COLOR,
                WINDOW_COLOR,
            );

            let stars = [b'*'; COLS];
            platform.draw_text(
                area.x + label_len as i32 * CHAR_WIDTH,
                input_y,
                &stars[..stars_len],
                TEXT_COLOR,
                WINDOW_COLOR,
            );

            caret_x = area.x + (label_len + stars_len) as i32 * CHAR_WIDTH;
        } else {
            let prompt_len = self.prompt_len.min(columns);
            let input_len = self.input_len.min(columns - prompt_len);

            platform.draw_text(
                area.x,
                input_y,
                &self.prompt[..prompt_len],
                PROMPT_COLOR,
                WINDOW_COLOR,
            );
            platform.draw_text(
                area.x + prompt_len as i32 * CHAR_WIDTH,
                input_y,
                &self.input[..input_len],
                TEXT_COLOR,
                WINDOW_COLOR,
            );

            caret_x = area.x + (prompt_len + input_len) as i32 * CHAR_WIDTH;
        }

        // Каретка ввода подчёркивает позицию следующего символа.
        platform.fill_rect(
            caret_x,
            input_y + CHAR_HEIGHT - 2,
            CHAR_WIDTH,
            2,
            PROMPT_COLOR,
        );
    }
}
