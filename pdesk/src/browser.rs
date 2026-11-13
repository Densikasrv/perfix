//! Perfbrowse — графический текстовый браузер pdesk.
//!
//! Страницу загружает ядро через [`Platform::browse`], а окно рисует готовый
//! текст с переносом по словам и прокруткой. Сеть, DNS и HTTP остаются в
//! ядре; pdesk по-прежнему не знает внутренностей Perfix.
//!
//! Адреса `https://` ядро загружает по TLS 1.3 с проверкой сертификатов;
//! ошибки (нет сети, отказ в доверии, таймаут) приходят текстом в статус.

use crate::{Key, PROMPT_COLOR, Platform, TEXT_COLOR, WINDOW_COLOR};

/// Ширина клиентской области окна.
pub const CONTENT_WIDTH: i32 = 720;
/// Высота клиентской области окна.
pub const CONTENT_HEIGHT: i32 = 480;

const CHAR_WIDTH: i32 = 8;
const CHAR_HEIGHT: i32 = 16;
const URL_HEIGHT: i32 = 24;
const STATUS_HEIGHT: i32 = 16;
const TEXT_TOP: i32 = URL_HEIGHT + STATUS_HEIGHT;
const URL_CAPACITY: usize = 160;
const STATUS_CAPACITY: usize = 96;
const PAGE_CAPACITY: usize = 32 * 1024;
const MAX_LINE: usize = 240;
const BAR_COLOR: u32 = 0x00161c22;

/// Сколько символов помещается в строку страницы.
const COLS: usize = CONTENT_WIDTH as usize / CHAR_WIDTH as usize - 2;
/// Сколько строк страницы помещается в окне.
const ROWS: usize = (CONTENT_HEIGHT - TEXT_TOP) as usize / CHAR_HEIGHT as usize;

/// Текст последней загруженной страницы.
///
/// Буфер статический, чтобы не увеличивать стек [`crate::run`].
static mut PAGE: [u8; PAGE_CAPACITY] = [0; PAGE_CAPACITY];

fn page_buffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(PAGE) }
}

/// Состояние окна Perfbrowse.
pub struct Browser {
    url: [u8; URL_CAPACITY],
    url_len: usize,
    status: [u8; STATUS_CAPACITY],
    status_len: usize,
    page_len: usize,
    total_lines: usize,
    scroll: usize,
    /// Ожидает загрузки, чтобы успеть нарисовать «Loading...».
    pending: bool,
    delay: u8,
}

impl Browser {
    pub fn new() -> Self {
        Self {
            url: [0; URL_CAPACITY],
            url_len: 0,
            status: [0; STATUS_CAPACITY],
            status_len: 0,
            page_len: 0,
            total_lines: 0,
            scroll: 0,
            pending: false,
            delay: 0,
        }
    }

    /// Возвращает окно в исходное состояние.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Заполняет адресную строку и запускает загрузку.
    pub fn open(&mut self, url: &[u8]) {
        let length = url.len().min(URL_CAPACITY);
        self.url[..length].copy_from_slice(&url[..length]);
        self.url_len = length;
        self.page_len = 0;
        self.total_lines = 0;
        self.scroll = 0;
        self.request_load();
    }

    /// Загружается ли страница прямо сейчас.
    pub fn pending(&self) -> bool {
        self.pending
    }

    fn set_status(&mut self, text: &[u8]) {
        let length = text.len().min(STATUS_CAPACITY);
        self.status[..length].copy_from_slice(&text[..length]);
        self.status_len = length;
    }

    fn request_load(&mut self) {
        if self.url_len == 0 {
            self.set_status(b"Enter a URL");
            return;
        }

        // Схема https:// поддерживается так же, как http://: запрос уходит в
        // ядро, где TLS 1.3 проверяет цепочку сертификатов. Ошибки (нет сети,
        // отказ в доверии, таймаут) ядро возвращает текстом в статус окна.
        self.pending = true;
        self.delay = 2;
        self.set_status(b"Loading...");
    }

    /// Обрабатывает клавишу. Возвращает `true`, если нужна перерисовка.
    pub fn handle_key(&mut self, key: Key) -> bool {
        if self.pending {
            return false;
        }

        match key {
            Key::Char(byte) => {
                if (32..127).contains(&byte) && self.url_len < URL_CAPACITY {
                    self.url[self.url_len] = byte;
                    self.url_len += 1;
                    true
                } else {
                    false
                }
            }
            Key::Backspace => {
                if self.url_len > 0 {
                    self.url_len -= 1;
                    true
                } else {
                    false
                }
            }
            Key::Enter => {
                self.request_load();
                true
            }
            Key::ArrowUp => self.scroll_by(-1),
            Key::ArrowDown => self.scroll_by(1),
            Key::PageUp => self.scroll_by(-(ROWS as isize)),
            Key::PageDown => self.scroll_by(ROWS as isize),
            Key::Home => {
                self.scroll = 0;
                true
            }
            Key::End => {
                self.scroll = self.total_lines.saturating_sub(ROWS);
                true
            }
            Key::Control(b'l') => {
                self.url_len = 0;
                self.page_len = 0;
                self.total_lines = 0;
                self.scroll = 0;
                self.set_status(b"");
                true
            }
            _ => false,
        }
    }

    fn scroll_by(&mut self, delta: isize) -> bool {
        let max = self.total_lines.saturating_sub(ROWS);

        self.scroll = if delta < 0 {
            self.scroll.saturating_sub((-delta) as usize)
        } else {
            (self.scroll + delta as usize).min(max)
        };

        true
    }

    /// Выполняет отложенную загрузку. Возвращает `true`, если страница готова.
    pub fn tick(&mut self, platform: &mut dyn Platform) -> bool {
        if !self.pending {
            return false;
        }

        if self.delay > 0 {
            self.delay -= 1;
            return false;
        }

        self.pending = false;
        let page = page_buffer();

        match platform.browse(&self.url[..self.url_len], page) {
            Ok(length) => {
                self.page_len = length.min(page.len());
                self.total_lines = count_lines(&page[..self.page_len], COLS);
                self.scroll = 0;
                self.set_status(b"");
            }
            Err(message) => {
                self.page_len = 0;
                self.total_lines = 0;
                self.scroll = 0;
                self.set_status(message);
            }
        }

        true
    }

    /// Рисует адресную строку, состояние и текст страницы.
    pub fn paint(&mut self, platform: &mut dyn Platform, area: crate::window::Rect) {
        platform.fill_rect(area.x, area.y, area.width, area.height, WINDOW_COLOR);

        // Адресная строка.
        platform.fill_rect(area.x, area.y, area.width, URL_HEIGHT, BAR_COLOR);
        platform.draw_text(area.x + 6, area.y + 4, b"URL", PROMPT_COLOR, BAR_COLOR);

        let visible = (area.width as usize / CHAR_WIDTH as usize).saturating_sub(6);
        let start = self.url_len.saturating_sub(visible);
        platform.draw_text(
            area.x + 6 + 4 * CHAR_WIDTH,
            area.y + 4,
            &self.url[..self.url_len][start..],
            TEXT_COLOR,
            BAR_COLOR,
        );

        // Строка состояния.
        if self.status_len > 0 {
            platform.draw_text(
                area.x + 6,
                area.y + URL_HEIGHT + 2,
                &self.status[..self.status_len],
                TEXT_COLOR,
                WINDOW_COLOR,
            );
        }

        let page = &page_buffer()[..self.page_len];
        let top = area.y + TEXT_TOP;

        if page.is_empty() {
            if !self.pending {
                platform.draw_text(
                    area.x + 6,
                    top,
                    b"Perfbrowse: type a URL and press Enter",
                    TEXT_COLOR,
                    WINDOW_COLOR,
                );
            }

            return;
        }

        let mut position = 0usize;
        let mut line = [b' '; MAX_LINE];
        let mut line_number = 0usize;
        let mut row = 0usize;

        while position < page.len() && row < ROWS {
            let length = take_line(page, &mut position, COLS, &mut line);

            if line_number >= self.scroll {
                platform.draw_text(
                    area.x + 6,
                    top + row as i32 * CHAR_HEIGHT,
                    &line[..length],
                    TEXT_COLOR,
                    WINDOW_COLOR,
                );
                row += 1;
            }

            line_number += 1;
        }
    }
}

/// Берёт из текста одну строку с переносом по словам.
fn take_line(text: &[u8], position: &mut usize, width: usize, out: &mut [u8]) -> usize {
    if *position >= text.len() {
        return 0;
    }

    if text[*position] == b'\n' {
        *position += 1;
        return 0;
    }

    let mut length = 0usize;

    while *position < text.len() {
        if text[*position] == b'\n' {
            *position += 1;
            break;
        }

        if text[*position] == b' ' {
            *position += 1;
            continue;
        }

        let start = *position;

        while *position < text.len() && text[*position] != b' ' && text[*position] != b'\n' {
            *position += 1;
        }

        let word = &text[start..*position];

        if word.len() > width {
            if length > 0 {
                *position = start;
                break;
            }

            let count = width.min(word.len());
            out[..count].copy_from_slice(&word[..count]);
            *position = start + count;
            return count;
        }

        if length > 0 && length + 1 + word.len() > width {
            *position = start;
            break;
        }

        if length > 0 {
            out[length] = b' ';
            length += 1;
        }

        out[length..length + word.len()].copy_from_slice(word);
        length += word.len();

        if *position < text.len() && text[*position] == b'\n' {
            *position += 1;
            break;
        }
    }

    length
}

/// Считает количество строк после переноса.
fn count_lines(text: &[u8], width: usize) -> usize {
    let mut position = 0usize;
    let mut line = [b' '; MAX_LINE];
    let mut count = 0usize;

    while position < text.len() {
        take_line(text, &mut position, width, &mut line);
        count += 1;
    }

    count
}
