//! GUI-редактор `tedit` в стиле «Блокнота» Windows.
//!
//! Окно повторяет привычный вид: строка меню, белое поле текста, строка
//! состояния и вертикальная полоса прокрутки. Горячие клавиши сохранены
//! от консольного `tedit`: Ctrl+O — сохранить, Ctrl+S — сохранить как,
//! Ctrl+X — выйти, Ctrl+K — вырезать строку, Ctrl+U — вставить, Ctrl+G —
//! подсказка, Ctrl+C — очистить строку состояния.

use crate::window::Rect;
use crate::{push_decimal, push_text, Key, Platform};

/// Ширина клиентской области окна.
pub const CONTENT_WIDTH: i32 = 732;
/// Высота клиентской области окна.
pub const CONTENT_HEIGHT: i32 = 556;

/// Максимальный размер редактируемого текста (как в консольном tedit).
pub const MAX_TEXT: usize = 8192;
/// Ёмкость абсолютного пути к файлу.
pub const PATH_CAPACITY: usize = 512;

const CUT_CAPACITY: usize = 256;
const STATUS_CAPACITY: usize = 160;

const CHAR_WIDTH: i32 = 8;
const CHAR_HEIGHT: i32 = 16;

const MENU_HEIGHT: i32 = 22;
const STATUS_HEIGHT: i32 = 22;
const TEXT_LEFT: i32 = 8;
const TEXT_RIGHT: i32 = 8;
const SCROLLBAR_WIDTH: i32 = 12;
const TEXT_TOP: i32 = MENU_HEIGHT;

/// Сколько знакомест текста помещается по горизонтали.
pub const COLUMNS: usize =
    ((CONTENT_WIDTH - TEXT_LEFT - TEXT_RIGHT - SCROLLBAR_WIDTH) / CHAR_WIDTH) as usize;
/// Сколько строк текста помещается по вертикали.
pub const ROWS: usize =
    ((CONTENT_HEIGHT - MENU_HEIGHT - STATUS_HEIGHT) / CHAR_HEIGHT) as usize;

const PAPER: u32 = 0x00ffffff;
const INK: u32 = 0x00000000;
const MENU_BG: u32 = 0x00f0f0f0;
const MENU_TEXT: u32 = 0x00202020;
const MENU_BORDER: u32 = 0x00c8c8c8;
const STATUS_BG: u32 = 0x00f0f0f0;
const STATUS_TEXT: u32 = 0x00404040;
const STATUS_ERROR: u32 = 0x00b00000;
const SCROLL_TRACK: u32 = 0x00f0f0f0;
const SCROLL_THUMB: u32 = 0x00b8b8b8;

/// Режим работы редактора.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Edit,
    /// Ctrl+X при несохранённых изменениях: ждём Y/N/Ctrl+C.
    ConfirmExit,
    /// Save As: ввод пути к новому файлу.
    SaveAs,
}

/// Открытое выпадающее меню строки меню.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    File,
    Edit,
    Format,
    View,
    Help,
}

/// Пункты строки меню: меню, подпись и смещение от левого края области.
const MENU_BUTTONS: [(Menu, &[u8], i32); 5] = [
    (Menu::File, b"File", 8),
    (Menu::Edit, b"Edit", 56),
    (Menu::Format, b"Format", 104),
    (Menu::View, b"View", 168),
    (Menu::Help, b"Help", 216),
];

const MENU_BUTTON_PAD: i32 = 6;
const MENU_PANEL_WIDTH: i32 = 190;
const MENU_ROW_HEIGHT: i32 = 20;
const MENU_PANEL_PAD: i32 = 4;
const MENU_PANEL_BG: u32 = 0x00f8f8f8;
const MENU_PANEL_BORDER: u32 = 0x00a0a0a0;

/// Что делать оболочке после клика мышью.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClickOutcome {
    /// Клик вне редактора или по пустому месту.
    Ignored,
    /// Нужно перерисовать окно.
    Redraw,
    /// Редактор просит закрыть окно.
    Close,
}

/// Что делать оболочке после обработки клавиши.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorOutcome {
    /// Клавиша ничего не изменила.
    Ignored,
    /// Нужно перерисовать окно.
    Redraw,
    /// Редактор просит закрыть окно.
    Close,
}

/// Состояние GUI-редактора.
pub struct Editor {
    data: [u8; MAX_TEXT],
    len: usize,
    cursor: usize,
    top_offset: usize,
    /// Абсолютный путь к файлу (для dock-запуска — `untitled`).
    name: [u8; PATH_CAPACITY],
    name_len: usize,
    dirty: bool,
    existed: bool,
    truncated: bool,
    mode: Mode,
    status: [u8; STATUS_CAPACITY],
    status_len: usize,
    status_error: bool,
    cut: [u8; CUT_CAPACITY],
    cut_len: usize,
    /// Открытое меню (None — все закрыты).
    menu: Option<Menu>,
    /// Мягкий перенос длинных строк.
    wrap: bool,
    /// Показывать строку состояния.
    show_status: bool,
    /// Путь, введённый в диалоге Save As.
    save_as_path: [u8; PATH_CAPACITY],
    save_as_len: usize,
}

impl Editor {
    pub const fn new() -> Self {
        Self {
            data: [0; MAX_TEXT],
            len: 0,
            cursor: 0,
            top_offset: 0,
            name: [0; PATH_CAPACITY],
            name_len: 0,
            dirty: false,
            existed: false,
            truncated: false,
            mode: Mode::Edit,
            status: [0; STATUS_CAPACITY],
            status_len: 0,
            status_error: false,
            cut: [0; CUT_CAPACITY],
            cut_len: 0,
            menu: None,
            wrap: true,
            show_status: true,
            save_as_path: [0; PATH_CAPACITY],
            save_as_len: 0,
        }
    }

    /// Начинает новую пустую сессию без имени.
    ///
    /// Используется, когда домашний каталог недоступен. В обычном сеансе
    /// новый документ открывается с путём `/home/<имя>/untitled`.
    pub fn reset(&mut self) {
        self.len = 0;
        self.cursor = 0;
        self.top_offset = 0;
        self.name_len = 0;
        self.dirty = false;
        self.existed = false;
        self.truncated = false;
        self.mode = Mode::Edit;
        self.cut_len = 0;
        self.menu = None;
        self.save_as_len = 0;
        self.clear_status();
    }

    /// Открывает файл с уже загруженным текстом.
    pub fn open(
        &mut self,
        path: &[u8],
        text: &[u8],
        existed: bool,
        truncated: bool,
    ) {
        self.len = text.len().min(MAX_TEXT);
        self.data[..self.len].copy_from_slice(&text[..self.len]);
        self.name_len = path.len().min(PATH_CAPACITY);
        self.name[..self.name_len].copy_from_slice(&path[..self.name_len]);
        self.cursor = 0;
        self.top_offset = 0;
        self.dirty = false;
        self.existed = existed;
        self.truncated = truncated;
        self.mode = Mode::Edit;
        self.menu = None;
        self.save_as_len = 0;
        self.clear_status();

        if truncated {
            self.set_status(b"File truncated to 8192 bytes", true);
        } else if !existed {
            self.set_status(b"New file", false);
        }
    }

    /// Есть ли несохранённые изменения.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Ширина «виртуальной строки» для расчёта переносов.
    ///
    /// При выключенном переносе строка не разрывается, поэтому длина
    /// логической строки не ограничена видимой шириной окна.
    fn columns(&self) -> usize {
        if self.wrap {
            COLUMNS
        } else {
            MAX_TEXT * 2
        }
    }

    /// Просит закрыть редактор: при изменениях сначала показывает вопрос.
    ///
    /// Возвращает `true`, если окно можно закрывать сразу.
    pub fn close_requested(&mut self) -> bool {
        matches!(self.request_close(), EditorOutcome::Close)
    }

    fn request_close(&mut self) -> EditorOutcome {
        if self.dirty {
            self.mode = Mode::ConfirmExit;
            self.menu = None;
            self.set_status(b"Save modified buffer? Y=yes N=no ^C=cancel", false);
            EditorOutcome::Redraw
        } else {
            EditorOutcome::Close
        }
    }

    /// Открывает диалог Save As. Поле заранее заполняется домашним
    /// каталогом пользователя, чтобы новые файлы сохранялись туда.
    fn request_save_as(&mut self, platform: &mut dyn Platform) {
        self.mode = Mode::SaveAs;
        self.menu = None;
        self.save_as_len = 0;

        let mut home = [0u8; PATH_CAPACITY];
        let length = platform.home_dir(&mut home).min(PATH_CAPACITY);

        if length > 0 {
            self.save_as_path[..length].copy_from_slice(&home[..length]);
            self.save_as_len = length;

            if self.save_as_path[self.save_as_len - 1] != b'/'
                && self.save_as_len < PATH_CAPACITY
            {
                self.save_as_path[self.save_as_len] = b'/';
                self.save_as_len += 1;
            }
        }

        self.set_status(b"Save as: enter a file name", false);
    }

    /// Ввод пути в диалоге Save As.
    fn handle_save_as_key(
        &mut self,
        platform: &mut dyn Platform,
        key: Key,
    ) -> EditorOutcome {
        match key {
            Key::Char(ch) if ch >= 0x20 => {
                if self.save_as_len < self.save_as_path.len() {
                    self.save_as_path[self.save_as_len] = ch;
                    self.save_as_len += 1;
                }

                EditorOutcome::Redraw
            }
            Key::Backspace => {
                if self.save_as_len > 0 {
                    self.save_as_len -= 1;
                }

                EditorOutcome::Redraw
            }
            Key::Enter => {
                if self.save_as_len == 0 {
                    self.set_status(b"Save failed: no file name", true);
                    return EditorOutcome::Redraw;
                }

                let mut path = [0u8; PATH_CAPACITY];
                path[..self.save_as_len].copy_from_slice(&self.save_as_path[..self.save_as_len]);

                if self.save_as(platform, &path[..self.save_as_len]) {
                    self.mode = Mode::Edit;
                }

                EditorOutcome::Redraw
            }
            Key::Escape | Key::Control(b'c') => {
                self.mode = Mode::Edit;
                self.clear_status();
                EditorOutcome::Redraw
            }
            Key::Control(b'o') => {
                self.save(platform);
                EditorOutcome::Redraw
            }
            _ => EditorOutcome::Ignored,
        }
    }

    /// Имя файла без каталога.
    fn base_name(&self) -> &[u8] {
        if self.name_len == 0 {
            return b"untitled";
        }

        let name = &self.name[..self.name_len];

        match name.iter().rposition(|&byte| byte == b'/') {
            Some(index) if index + 1 < name.len() => &name[index + 1..],
            _ => name,
        }
    }

    /// Заголовок окна: `файл - tedit`, со звёздочкой при изменениях.
    pub fn title(&self, out: &mut [u8]) -> usize {
        let mut length = push_text(out, 0, self.base_name());

        if self.dirty {
            length = push_text(out, length, b" *");
        }

        push_text(out, length, b" - tedit")
    }

    fn clear_status(&mut self) {
        self.status_len = 0;
        self.status_error = false;
    }

    fn set_status(&mut self, text: &[u8], error: bool) {
        let length = text.len().min(self.status.len());
        self.status[..length].copy_from_slice(&text[..length]);
        self.status_len = length;
        self.status_error = error;
    }

    fn set_error(&mut self, prefix: &[u8], message: &[u8]) {
        let mut buffer = [0u8; STATUS_CAPACITY];
        let mut length = push_text(&mut buffer, 0, prefix);
        length = push_text(&mut buffer, length, message);
        let length = length.min(self.status.len());
        self.status[..length].copy_from_slice(&buffer[..length]);
        self.status_len = length;
        self.status_error = true;
    }

    /// Сохраняет файл. Возвращает `true` при успехе.
    fn save(&mut self, platform: &mut dyn Platform) -> bool {
        if self.name_len == 0 {
            self.set_status(b"Save failed: no file name", true);
            return false;
        }

        match platform.save_text_file(&self.name[..self.name_len], &self.data[..self.len]) {
            Ok(bytes) => {
                self.dirty = false;
                self.existed = true;

                let mut line = [0u8; STATUS_CAPACITY];
                let mut length = push_text(&mut line, 0, b"Saved: ");
                length = push_text(&mut line, length, self.base_name());
                length = push_text(&mut line, length, b" (");
                length = push_decimal(&mut line, length, bytes as u32);
                length = push_text(&mut line, length, b" bytes)");
                self.set_status(&line[..length], false);

                true
            }
            Err(message) => {
                self.set_error(b"Save failed: ", message);
                false
            }
        }
    }

    /// Сохраняет текст по введённому в Save As пути.
    fn save_as(&mut self, platform: &mut dyn Platform, path: &[u8]) -> bool {
        match platform.save_text_file(path, &self.data[..self.len]) {
            Ok(bytes) => {
                self.name_len = path.len().min(PATH_CAPACITY);
                self.name[..self.name_len].copy_from_slice(&path[..self.name_len]);
                self.dirty = false;
                self.existed = true;

                let mut line = [0u8; STATUS_CAPACITY];
                let mut length = push_text(&mut line, 0, b"Saved: ");
                length = push_text(&mut line, length, self.base_name());
                length = push_text(&mut line, length, b" (");
                length = push_decimal(&mut line, length, bytes as u32);
                length = push_text(&mut line, length, b" bytes)");
                self.set_status(&line[..length], false);

                true
            }
            Err(message) => {
                self.set_error(b"Save failed: ", message);
                false
            }
        }
    }

    /// Обрабатывает клавишу редактора.
    pub fn handle_key(&mut self, platform: &mut dyn Platform, key: Key) -> EditorOutcome {
        // Любая клавиша закрывает открытое меню.
        self.menu = None;

        if self.mode == Mode::ConfirmExit {
            return self.handle_confirm_key(platform, key);
        }

        if self.mode == Mode::SaveAs {
            return self.handle_save_as_key(platform, key);
        }

        match key {
            Key::Char(ch) => {
                self.clear_status();
                self.insert(&[ch]);
                EditorOutcome::Redraw
            }
            Key::Enter => {
                self.clear_status();
                self.insert(b"\n");
                EditorOutcome::Redraw
            }
            Key::Tab => {
                self.clear_status();
                self.insert(b"    ");
                EditorOutcome::Redraw
            }
            Key::Backspace => {
                self.clear_status();
                self.delete_before();
                EditorOutcome::Redraw
            }
            Key::Delete => {
                self.clear_status();
                self.delete_at();
                EditorOutcome::Redraw
            }
            Key::ArrowLeft => {
                self.clear_status();
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                EditorOutcome::Redraw
            }
            Key::ArrowRight => {
                self.clear_status();
                if self.cursor < self.len {
                    self.cursor += 1;
                }
                EditorOutcome::Redraw
            }
            Key::ArrowUp => {
                self.clear_status();
                self.move_up();
                EditorOutcome::Redraw
            }
            Key::ArrowDown => {
                self.clear_status();
                self.move_down();
                EditorOutcome::Redraw
            }
            Key::Home => {
                self.clear_status();
                self.cursor = line_start(&self.data[..self.len], self.cursor);
                EditorOutcome::Redraw
            }
            Key::End => {
                self.clear_status();
                self.cursor = line_end(&self.data[..self.len], self.cursor);
                EditorOutcome::Redraw
            }
            Key::PageUp => {
                self.clear_status();
                for _ in 0..ROWS.saturating_sub(1) {
                    self.move_up();
                }
                EditorOutcome::Redraw
            }
            Key::PageDown => {
                self.clear_status();
                for _ in 0..ROWS.saturating_sub(1) {
                    self.move_down();
                }
                EditorOutcome::Redraw
            }
            Key::Control(b'o') => {
                self.save(platform);
                EditorOutcome::Redraw
            }
            Key::Control(b's') => {
                self.request_save_as(platform);
                EditorOutcome::Redraw
            }
            Key::Control(b'x') => self.request_close(),
            Key::Control(b'k') => {
                self.cut_line();
                EditorOutcome::Redraw
            }
            Key::Control(b'u') => {
                self.paste();
                EditorOutcome::Redraw
            }
            Key::Control(b'g') => {
                self.set_status(
                    b"Arrows/Home/End/PgUp/PgDn move  ^O save  ^S save as  ^X exit  ^K cut  ^U paste",
                    false,
                );
                EditorOutcome::Redraw
            }
            Key::Control(b'c') => {
                self.clear_status();
                EditorOutcome::Redraw
            }
            Key::LayoutChanged | Key::Escape | Key::Control(_) => {
                self.clear_status();
                EditorOutcome::Redraw
            }
            Key::Quit => EditorOutcome::Ignored,
        }
    }

    fn handle_confirm_key(
        &mut self,
        platform: &mut dyn Platform,
        key: Key,
    ) -> EditorOutcome {
        match key {
            Key::Char(b'y') | Key::Char(b'Y') => {
                if self.save(platform) {
                    EditorOutcome::Close
                } else {
                    self.mode = Mode::Edit;
                    EditorOutcome::Redraw
                }
            }
            Key::Char(b'n') | Key::Char(b'N') => EditorOutcome::Close,
            Key::Escape | Key::Control(b'c') => {
                self.mode = Mode::Edit;
                self.clear_status();
                EditorOutcome::Redraw
            }
            Key::Control(b'o') => {
                self.save(platform);
                EditorOutcome::Redraw
            }
            _ => EditorOutcome::Ignored,
        }
    }

    fn insert(&mut self, bytes: &[u8]) {
        if bytes.is_empty() || self.len + bytes.len() > self.data.len() {
            if !bytes.is_empty() {
                self.set_status(b"Buffer full: file is too large", true);
            }
            return;
        }

        let at = self.cursor.min(self.len);
        let extra = bytes.len();

        let mut index = self.len;
        while index > at {
            self.data[index + extra - 1] = self.data[index - 1];
            index -= 1;
        }

        self.data[at..at + extra].copy_from_slice(bytes);
        self.len += extra;
        self.cursor = at + extra;
        self.dirty = true;
    }

    fn delete_range(&mut self, at: usize, count: usize) {
        let at = at.min(self.len);
        let count = count.min(self.len - at);

        if count == 0 {
            return;
        }

        let mut index = at;
        while index + count < self.len {
            self.data[index] = self.data[index + count];
            index += 1;
        }

        self.len -= count;
    }

    fn delete_before(&mut self) {
        if self.cursor > 0 {
            self.delete_range(self.cursor - 1, 1);
            self.cursor -= 1;
            self.dirty = true;
        }
    }

    fn delete_at(&mut self) {
        if self.cursor < self.len {
            self.delete_range(self.cursor, 1);
            self.dirty = true;
        }
    }

    fn move_up(&mut self) {
        let data = &self.data[..self.len];
        let mut cursor = self.cursor;
        move_up(data, &mut cursor);
        self.cursor = cursor;
    }

    fn move_down(&mut self) {
        let data = &self.data[..self.len];
        let mut cursor = self.cursor;
        move_down(data, &mut cursor);
        self.cursor = cursor;
    }

    fn cut_line(&mut self) {
        let data = &self.data[..self.len];
        let start = line_start(data, self.cursor);
        let end = line_end(data, self.cursor);
        let has_newline = end < self.len;
        let content_len = end - start;
        let cut_len = content_len + if has_newline { 1 } else { 0 };

        if cut_len > CUT_CAPACITY {
            self.set_status(b"Line is too long for the cut buffer", true);
            return;
        }

        self.cut[..content_len].copy_from_slice(&self.data[start..end]);
        self.cut_len = content_len;

        if has_newline {
            self.cut[content_len] = b'\n';
            self.cut_len += 1;
        }

        if self.cut_len == 0 {
            self.set_status(b"Nothing to cut", true);
            return;
        }

        self.delete_range(start, cut_len);
        self.cursor = start.min(self.len);
        self.dirty = true;
        self.set_status(b"Cut current line  (^U to paste)", false);
    }

    fn paste(&mut self) {
        if self.cut_len == 0 {
            self.set_status(b"Nothing to paste", true);
            return;
        }

        if self.len + self.cut_len > self.data.len() {
            self.set_status(b"Buffer full: cannot paste", true);
            return;
        }

        let cut = self.cut;
        let cut_len = self.cut_len;
        self.insert(&cut[..cut_len]);
    }

    /// Ставит курсор по координатам мыши или работает с меню.
    pub fn handle_click(
        &mut self,
        platform: &mut dyn Platform,
        area: Rect,
        x: i32,
        y: i32,
    ) -> ClickOutcome {
        // В диалоге Save As мышь не меняет курсор и меню.
        if self.mode == Mode::SaveAs {
            return ClickOutcome::Ignored;
        }

        // Строка меню.
        if y >= area.y && y < area.y + MENU_HEIGHT {
            for &(menu, label, offset) in MENU_BUTTONS.iter() {
                if menu_button_rect(area, offset, label).contains(x, y) {
                    self.menu = if self.menu == Some(menu) {
                        None
                    } else {
                        Some(menu)
                    };
                    return ClickOutcome::Redraw;
                }
            }

            if self.menu.is_some() {
                self.menu = None;
                return ClickOutcome::Redraw;
            }

            return ClickOutcome::Ignored;
        }

        // Открытое выпадающее меню.
        if let Some(menu) = self.menu {
            let panel = menu_panel_rect(area, menu);

            if panel.contains(x, y) {
                let index = ((y - panel.y - MENU_PANEL_PAD) / MENU_ROW_HEIGHT).max(0) as usize;

                if index < menu_len(menu) {
                    return self.activate_menu(platform, menu, index);
                }

                return ClickOutcome::Redraw;
            }

            // Клик мимо меню закрывает его и больше ничего не делает.
            self.menu = None;
            return ClickOutcome::Redraw;
        }

        let text_x = area.x + TEXT_LEFT;
        let text_y = area.y + TEXT_TOP;
        let text_width = area.width - TEXT_LEFT - TEXT_RIGHT - SCROLLBAR_WIDTH;
        let text_height = ROWS as i32 * CHAR_HEIGHT;

        if x < text_x || x >= text_x + text_width || y < text_y || y >= text_y + text_height {
            return ClickOutcome::Ignored;
        }

        let row = ((y - text_y) / CHAR_HEIGHT) as usize;
        let col = ((x - text_x) / CHAR_WIDTH) as usize;
        let data = &self.data[..self.len];

        self.cursor = offset_from_visual(
            data,
            self.top_offset,
            row,
            col.min(COLUMNS - 1),
            self.columns(),
        );
        self.mode = Mode::Edit;
        self.clear_status();
        self.ensure_visible();

        ClickOutcome::Redraw
    }

    /// Выполняет пункт выпадающего меню.
    fn activate_menu(
        &mut self,
        platform: &mut dyn Platform,
        menu: Menu,
        index: usize,
    ) -> ClickOutcome {
        self.menu = None;

        match (menu, index) {
            (Menu::File, 0) => {
                self.save(platform);
                ClickOutcome::Redraw
            }
            (Menu::File, 1) => {
                self.request_save_as(platform);
                ClickOutcome::Redraw
            }
            (Menu::File, _) => match self.request_close() {
                EditorOutcome::Close => ClickOutcome::Close,
                _ => ClickOutcome::Redraw,
            },
            (Menu::Edit, 0) => {
                self.cut_line();
                ClickOutcome::Redraw
            }
            (Menu::Edit, _) => {
                self.paste();
                ClickOutcome::Redraw
            }
            (Menu::Format, _) => {
                self.wrap = !self.wrap;
                self.ensure_visible();
                ClickOutcome::Redraw
            }
            (Menu::View, _) => {
                self.show_status = !self.show_status;
                ClickOutcome::Redraw
            }
            (Menu::Help, 0) => {
                self.set_status(
                    b"Arrows/Home/End/PgUp/PgDn move  ^O save  ^X exit  ^K cut  ^U paste",
                    false,
                );
                ClickOutcome::Redraw
            }
            (Menu::Help, _) => {
                self.set_status(b"tedit 0.2.6 - Perfix Notepad-style editor", false);
                ClickOutcome::Redraw
            }
        }
    }

    fn ensure_visible(&mut self) {
        let data = &self.data[..self.len];
        let columns = self.columns();
        let mut cursor = self.cursor;
        let mut top = self.top_offset;
        ensure_visible_in(data, &mut cursor, &mut top, columns);
        self.cursor = cursor;
        self.top_offset = top;
    }

    /// Рисует редактор в клиентской области окна.
    pub fn paint(&mut self, platform: &mut dyn Platform, area: Rect) {
        platform.fill_rect(area.x, area.y, area.width, area.height, PAPER);

        // Строка меню, как в «Блокноте».
        platform.fill_rect(area.x, area.y, area.width, MENU_HEIGHT, MENU_BG);

        for &(menu, label, offset) in MENU_BUTTONS.iter() {
            let button = menu_button_rect(area, offset, label);
            let background = if self.menu == Some(menu) {
                MENU_PANEL_BG
            } else {
                MENU_BG
            };

            if self.menu == Some(menu) {
                platform.fill_rect(
                    button.x,
                    button.y,
                    button.width,
                    button.height,
                    background,
                );
            }

            platform.draw_text(
                area.x + offset,
                area.y + 3,
                label,
                MENU_TEXT,
                background,
            );
        }

        platform.fill_rect(
            area.x,
            area.y + MENU_HEIGHT - 1,
            area.width,
            1,
            MENU_BORDER,
        );

        self.ensure_visible();

        let data = &self.data[..self.len];
        let columns = self.columns();
        let mut pos = self.top_offset.min(data.len());

        for row in 0..ROWS {
            let mut line = [b' '; COLUMNS];
            let mut col = 0usize;

            while col < COLUMNS && pos < data.len() && data[pos] != b'\n' {
                line[col] = match data[pos] {
                    b'\t' => b' ',
                    0x20..=0x7e | 0x80..=0xff => data[pos],
                    _ => b'.',
                };

                pos += 1;
                col += 1;
            }

            platform.draw_text(
                area.x + TEXT_LEFT,
                area.y + TEXT_TOP + row as i32 * CHAR_HEIGHT,
                &line,
                INK,
                PAPER,
            );

            if pos < data.len() && data[pos] == b'\n' {
                pos += 1;
            } else if !self.wrap {
                // Без переноса остаток длинной строки не показываем.
                while pos < data.len() && data[pos] != b'\n' {
                    pos += 1;
                }

                if pos < data.len() {
                    pos += 1;
                }
            }
        }

        self.paint_scrollbar(platform, area, data, columns);

        if self.show_status {
            self.paint_status(platform, area, data);
        }

        // Каретка — тонкая вертикальная черта.
        if self.mode != Mode::SaveAs {
            let (row, col) = cursor_screen(data, self.cursor, self.top_offset, columns);
            let col = col.min(COLUMNS - 1);

            platform.fill_rect(
                area.x + TEXT_LEFT + col as i32 * CHAR_WIDTH,
                area.y + TEXT_TOP + row as i32 * CHAR_HEIGHT,
                2,
                CHAR_HEIGHT,
                INK,
            );
        }

        // Открытое меню рисуется поверх текста.
        if let Some(menu) = self.menu {
            self.paint_menu(platform, area, menu);
        }

        // Диалог Save As — поверх всего остального.
        if self.mode == Mode::SaveAs {
            self.paint_save_dialog(platform, area);
        }
    }

    /// Рисует модальный диалог Save As с полем ввода пути.
    fn paint_save_dialog(&self, platform: &mut dyn Platform, area: Rect) {
        let width = 460.min(area.width - 40).max(200);
        let height = 108;
        let panel = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };

        // Затемнение под диалогом не нужно — панель с рамкой читается и так.
        platform.fill_rect(panel.x, panel.y, panel.width, panel.height, MENU_PANEL_BG);
        platform.draw_rect(
            panel.x,
            panel.y,
            panel.width,
            panel.height,
            MENU_PANEL_BORDER,
        );

        platform.draw_text(
            panel.x + 10,
            panel.y + 6,
            b"Save As",
            MENU_TEXT,
            MENU_PANEL_BG,
        );
        platform.draw_text(
            panel.x + 10,
            panel.y + 30,
            b"File name:",
            MENU_TEXT,
            MENU_PANEL_BG,
        );

        let input = Rect {
            x: panel.x + 10,
            y: panel.y + 48,
            width: panel.width - 20,
            height: 24,
        };

        platform.fill_rect(input.x, input.y, input.width, input.height, PAPER);
        platform.draw_rect(input.x, input.y, input.width, input.height, MENU_PANEL_BORDER);

        let max_chars = ((input.width - 8) / CHAR_WIDTH).max(1) as usize;
        let start = self.save_as_len.saturating_sub(max_chars);
        let text = &self.save_as_path[start..self.save_as_len];
        let text_x = input.x + 4;
        let text_y = input.y + 4;

        if self.save_as_len == 0 {
            platform.draw_text(
                text_x,
                text_y,
                b"/path/file.txt",
                STATUS_TEXT,
                PAPER,
            );
        } else {
            platform.draw_text(text_x, text_y, text, INK, PAPER);

            let caret_chars = self.save_as_len - start;
            platform.fill_rect(
                text_x + caret_chars as i32 * CHAR_WIDTH,
                text_y,
                2,
                CHAR_HEIGHT,
                INK,
            );
        }

        platform.draw_text(
            panel.x + 10,
            panel.y + 82,
            b"Enter - save   Esc - cancel",
            STATUS_TEXT,
            MENU_PANEL_BG,
        );
    }

    /// Рисует выпадающее меню с пунктами и подсказками клавиш.
    fn paint_menu(&self, platform: &mut dyn Platform, area: Rect, menu: Menu) {
        let panel = menu_panel_rect(area, menu);

        platform.fill_rect(
            panel.x,
            panel.y,
            panel.width,
            panel.height,
            MENU_PANEL_BG,
        );
        platform.draw_rect(
            panel.x,
            panel.y,
            panel.width,
            panel.height,
            MENU_PANEL_BORDER,
        );

        for index in 0..menu_len(menu) {
            let row_y = panel.y + MENU_PANEL_PAD + index as i32 * MENU_ROW_HEIGHT;
            let label = menu_label(menu, index, self.wrap, self.show_status);

            platform.draw_text(
                panel.x + 10,
                row_y + 2,
                label,
                MENU_TEXT,
                MENU_PANEL_BG,
            );

            let shortcut = menu_shortcut(menu, index);

            if !shortcut.is_empty() {
                let shortcut_x = panel.x + panel.width
                    - 10
                    - shortcut.len() as i32 * CHAR_WIDTH;
                platform.draw_text(
                    shortcut_x,
                    row_y + 2,
                    shortcut,
                    STATUS_TEXT,
                    MENU_PANEL_BG,
                );
            }
        }
    }

    fn paint_scrollbar(
        &self,
        platform: &mut dyn Platform,
        area: Rect,
        data: &[u8],
        columns: usize,
    ) {
        let track_x = area.x + area.width - SCROLLBAR_WIDTH;
        let track_y = area.y + TEXT_TOP;
        let track_height = ROWS as i32 * CHAR_HEIGHT;

        platform.fill_rect(track_x, track_y, SCROLLBAR_WIDTH, track_height, SCROLL_TRACK);

        let total_rows = total_visual_rows(data, columns).max(1);
        let visible = ROWS;
        let thumb_height = (track_height * visible as i32 / total_rows as i32)
            .clamp(12, track_height);
        let top_row = visual_rows_before(data, self.top_offset, columns);
        let max_top = total_rows.saturating_sub(visible);
        let thumb_y = if max_top == 0 {
            0
        } else {
            (track_height - thumb_height) * top_row as i32 / max_top as i32
        };

        platform.fill_rect(
            track_x + 2,
            track_y + thumb_y,
            SCROLLBAR_WIDTH - 4,
            thumb_height,
            SCROLL_THUMB,
        );
    }

    fn paint_status(&self, platform: &mut dyn Platform, area: Rect, data: &[u8]) {
        let status_y = area.y + area.height - STATUS_HEIGHT;

        platform.fill_rect(area.x, status_y, area.width, STATUS_HEIGHT, STATUS_BG);
        platform.fill_rect(area.x, status_y, area.width, 1, MENU_BORDER);

        let mut left = [b' '; 96];
        let mut left_len = 0usize;

        if self.status_len > 0 {
            left_len = push_text(&mut left, 0, &self.status[..self.status_len]);
        } else {
            left_len = push_text(&mut left, left_len, self.base_name());

            if self.dirty {
                left_len = push_text(&mut left, left_len, b"  [Modified]");
            }

            if self.truncated {
                left_len = push_text(&mut left, left_len, b"  [Truncated]");
            }
        }

        let color = if self.status_error {
            STATUS_ERROR
        } else {
            STATUS_TEXT
        };

        platform.draw_text(
            area.x + 8,
            status_y + 3,
            &left[..left_len.min(left.len())],
            color,
            STATUS_BG,
        );

        let mut right = [0u8; 32];
        let mut right_len = push_text(&mut right, 0, b"Ln ");
        right_len = push_decimal(&mut right, right_len, line_number(data, self.cursor) as u32 + 1);
        right_len = push_text(&mut right, right_len, b", Col ");
        right_len = push_decimal(
            &mut right,
            right_len,
            (self.cursor - line_start(data, self.cursor)) as u32 + 1,
        );

        let right_x = area.x + area.width - 8 - right_len as i32 * CHAR_WIDTH;

        platform.draw_text(
            right_x,
            status_y + 3,
            &right[..right_len],
            STATUS_TEXT,
            STATUS_BG,
        );
    }
}

fn line_start(data: &[u8], pos: usize) -> usize {
    let mut index = pos.min(data.len());

    while index > 0 && data[index - 1] != b'\n' {
        index -= 1;
    }

    index
}

fn line_end(data: &[u8], pos: usize) -> usize {
    let mut index = pos.min(data.len());

    while index < data.len() && data[index] != b'\n' {
        index += 1;
    }

    index
}

fn line_number(data: &[u8], pos: usize) -> usize {
    data[..pos.min(data.len())]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count()
}

fn move_up(data: &[u8], cursor: &mut usize) {
    let start = line_start(data, *cursor);

    if start == 0 {
        return;
    }

    let previous_end = start - 1;
    let previous_start = line_start(data, previous_end);
    let column = *cursor - start;
    let previous_len = previous_end - previous_start;

    *cursor = previous_start + column.min(previous_len);
}

fn move_down(data: &[u8], cursor: &mut usize) {
    let end = line_end(data, *cursor);

    if end >= data.len() {
        return;
    }

    let start = line_start(data, *cursor);
    let column = *cursor - start;
    let next_start = end + 1;
    let next_end = line_end(data, next_start);

    *cursor = next_start + column.min(next_end - next_start);
}

fn visual_distance(data: &[u8], from: usize, to: usize, columns: usize) -> (usize, usize) {
    let mut row = 0usize;
    let mut col = 0usize;
    let mut pos = from;

    while pos < to {
        if data[pos] == b'\n' {
            row += 1;
            col = 0;
        } else {
            if col == columns {
                row += 1;
                col = 0;
            }

            col += 1;
        }

        pos += 1;
    }

    (row, col)
}

fn visual_row_start(data: &[u8], target: usize, columns: usize) -> usize {
    let target = target.min(data.len());
    let mut row_start = 0usize;
    let mut col = 0usize;
    let mut pos = 0usize;

    while pos < target {
        if data[pos] == b'\n' {
            pos += 1;
            row_start = pos;
            col = 0;
        } else {
            if col == columns {
                row_start = pos;
                col = 0;
            }

            pos += 1;
            col += 1;
        }
    }

    row_start
}

fn next_row_start(data: &[u8], offset: usize, columns: usize) -> usize {
    let mut pos = offset.min(data.len());
    let mut col = 0usize;

    while pos < data.len() {
        if data[pos] == b'\n' {
            return pos + 1;
        }

        pos += 1;
        col += 1;

        if col == columns {
            if pos < data.len() && data[pos] == b'\n' {
                return pos + 1;
            }

            return pos;
        }
    }

    data.len()
}

fn ensure_visible_in(data: &[u8], cursor: &mut usize, top: &mut usize, columns: usize) {
    if *cursor > data.len() {
        *cursor = data.len();
    }

    if *top > data.len() {
        *top = data.len();
    }

    if *cursor < *top {
        *top = visual_row_start(data, *cursor, columns);
    }

    let mut guard = 0usize;

    loop {
        let (row, _) = cursor_screen(data, *cursor, *top, columns);

        if row < ROWS {
            break;
        }

        let next = next_row_start(data, *top, columns);

        if next <= *top {
            *top = *cursor;
            break;
        }

        *top = next;
        guard += 1;

        if guard > MAX_TEXT {
            break;
        }
    }
}

/// Позиция курсора на экране относительно начала области текста.
fn cursor_screen(
    data: &[u8],
    cursor: usize,
    top: usize,
    columns: usize,
) -> (usize, usize) {
    let from = top.min(data.len());
    let to = cursor.min(data.len());

    if to < from {
        return (0, 0);
    }

    let (row, col) = visual_distance(data, from, to, columns);

    // Позиция сразу после последнего столбца — «ожидание переноса».
    if col == columns {
        (row, columns - 1)
    } else {
        (row, col)
    }
}

fn total_visual_rows(data: &[u8], columns: usize) -> usize {
    let mut row = 0usize;
    let mut col = 0usize;

    for &byte in data {
        if byte == b'\n' {
            row += 1;
            col = 0;
        } else {
            if col == columns {
                row += 1;
                col = 0;
            }

            col += 1;
        }
    }

    row + 1
}

fn visual_rows_before(data: &[u8], offset: usize, columns: usize) -> usize {
    visual_distance(data, 0, offset.min(data.len()), columns).0
}

fn offset_from_visual(
    data: &[u8],
    from: usize,
    row: usize,
    col: usize,
    columns: usize,
) -> usize {
    let mut pos = from.min(data.len());
    let mut current_row = 0usize;
    let mut current_col = 0usize;

    while pos < data.len() && current_row < row {
        if data[pos] == b'\n' {
            current_row += 1;
            current_col = 0;
            pos += 1;
        } else {
            if current_col == columns {
                current_row += 1;
                current_col = 0;
            } else {
                current_col += 1;
                pos += 1;
            }
        }
    }

    let mut remaining = col;

    while pos < data.len() && remaining > 0 && data[pos] != b'\n' {
        pos += 1;
        remaining -= 1;
    }

    pos
}

/// Прямоугольник кнопки меню в строке меню.
fn menu_button_rect(area: Rect, offset: i32, label: &[u8]) -> Rect {
    Rect {
        x: area.x + offset - MENU_BUTTON_PAD,
        y: area.y,
        width: label.len() as i32 * CHAR_WIDTH + 2 * MENU_BUTTON_PAD,
        height: MENU_HEIGHT,
    }
}

/// Прямоугольник выпадающей панели меню.
fn menu_panel_rect(area: Rect, menu: Menu) -> Rect {
    let offset = MENU_BUTTONS
        .iter()
        .find(|(item, _, _)| *item == menu)
        .map(|(_, _, offset)| *offset)
        .unwrap_or(8);

    let width = MENU_PANEL_WIDTH.min(area.width - offset);
    let height = MENU_PANEL_PAD * 2 + menu_len(menu) as i32 * MENU_ROW_HEIGHT;

    Rect {
        x: area.x + offset - MENU_BUTTON_PAD,
        y: area.y + MENU_HEIGHT,
        width: width.max(40),
        height,
    }
}

/// Количество пунктов в меню.
fn menu_len(menu: Menu) -> usize {
    match menu {
        Menu::File => 3,
        Menu::Edit | Menu::Help => 2,
        Menu::Format | Menu::View => 1,
    }
}

/// Подпись пункта меню (зависит от текущих настроек).
fn menu_label(menu: Menu, index: usize, wrap: bool, show_status: bool) -> &'static [u8] {
    match (menu, index) {
        (Menu::File, 0) => b"Save",
        (Menu::File, 1) => b"Save As...",
        (Menu::File, _) => b"Exit",
        (Menu::Edit, 0) => b"Cut line",
        (Menu::Edit, _) => b"Paste",
        (Menu::Format, _) => {
            if wrap {
                b"Word wrap: on"
            } else {
                b"Word wrap: off"
            }
        }
        (Menu::View, _) => {
            if show_status {
                b"Status bar: on"
            } else {
                b"Status bar: off"
            }
        }
        (Menu::Help, 0) => b"Keyboard shortcuts",
        (Menu::Help, _) => b"About tedit",
    }
}

/// Подсказка о горячей клавише пункта меню.
fn menu_shortcut(menu: Menu, index: usize) -> &'static [u8] {
    match (menu, index) {
        (Menu::File, 0) => b"Ctrl+O",
        (Menu::File, 1) => b"Ctrl+S",
        (Menu::File, _) => b"Ctrl+X",
        (Menu::Edit, 0) => b"Ctrl+K",
        (Menu::Edit, _) => b"Ctrl+U",
        _ => b"",
    }
}

