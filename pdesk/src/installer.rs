//! GUI-установщик Perfix на разделы ext4.
//!
//! Окно показывает только разделы ext4: остальные файловые системы
//! установщик не принимает. Выбор раздела — стрелками или мышью,
//! установка — Enter или кнопкой Install.

use crate::window::Rect;
use crate::{push_decimal, push_text, InstallTarget, Key, Platform, TEXT_COLOR, WINDOW_COLOR};

/// Ширина клиентской области окна.
pub const CONTENT_WIDTH: i32 = 620;
/// Высота клиентской области окна.
pub const CONTENT_HEIGHT: i32 = 480;

const MAX_TARGETS: usize = 8;
const LIST_TOP: i32 = 68;
const ROW_HEIGHT: i32 = 24;
const MAX_ROWS: usize = 5;
const BUTTON_WIDTH: i32 = 200;
const BUTTON_HEIGHT: i32 = 30;
const BUTTON_BOTTOM: i32 = 60;

/// Поля учётных записей.
const FIELD_TOP: i32 = 200;
const FIELD_HEIGHT: i32 = 22;
const FIELD_GAP: i32 = 30;
const USERNAME_MAX: usize = 16;
const PASSWORD_MAX: usize = 64;
/// Ни одно поле не выбрано: работают обычные горячие клавиши.
const FOCUS_NONE: usize = 3;

const HEADER_COLOR: u32 = 0x00ffffff;
const HINT_COLOR: u32 = 0x0090a0b0;
const SELECTED_COLOR: u32 = 0x002c5a7c;
const ROW_BORDER: u32 = 0x00304050;
const BUTTON_COLOR: u32 = 0x002c5a7c;
const BUTTON_BORDER: u32 = 0x0060e080;
const SUCCESS_COLOR: u32 = 0x0060e080;
const ERROR_COLOR: u32 = 0x00f07070;

/// Состояние окна установщика.
pub struct Installer {
    targets: [InstallTarget; MAX_TARGETS],
    count: usize,
    selected: usize,
    /// Форматировать раздел перед установкой.
    format: bool,
    loaded: bool,
    status: [u8; 200],
    status_len: usize,
    status_error: bool,
    username: [u8; USERNAME_MAX],
    username_len: usize,
    password: [u8; PASSWORD_MAX],
    password_len: usize,
    super_password: [u8; PASSWORD_MAX],
    super_password_len: usize,
    /// 0 — имя, 1 — пароль, 2 — пароль суперпользователя, 3 — ничего.
    focus: usize,
}

impl Installer {
    pub const fn new() -> Self {
        Self {
            targets: [InstallTarget {
                number: 0,
                sectors: 0,
                ext4: false,
            }; MAX_TARGETS],
            count: 0,
            selected: 0,
            format: false,
            loaded: false,
            status: [0; 200],
            status_len: 0,
            status_error: false,
            username: [0; USERNAME_MAX],
            username_len: 0,
            password: [0; PASSWORD_MAX],
            password_len: 0,
            super_password: [0; PASSWORD_MAX],
            super_password_len: 0,
            focus: FOCUS_NONE,
        }
    }

    fn field_rect(&self, area: Rect, index: usize) -> Rect {
        Rect {
            x: area.x + 16,
            y: area.y + FIELD_TOP + index as i32 * FIELD_GAP,
            width: area.width - 32,
            height: FIELD_HEIGHT,
        }
    }

    /// Проверяет учётные данные перед установкой.
    fn credentials_error(&self) -> Option<&'static [u8]> {
        if self.username_len == 0 {
            return Some(b"Enter a user name (1-16 characters).");
        }

        for &byte in &self.username[..self.username_len] {
            if !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') {
                return Some(b"User name may contain only letters, digits, '_' and '-'.");
            }
        }

        if self.password_len == 0 {
            return Some(b"Enter a user password.");
        }

        if self.super_password_len == 0 {
            return Some(b"Enter a superuser password.");
        }

        None
    }

    /// Добавляет символ в выбранное поле.
    fn insert_field_char(&mut self, ch: u8) {
        if ch < 0x20 {
            return;
        }

        match self.focus {
            0 => {
                let allowed = ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-';

                if allowed && self.username_len < USERNAME_MAX {
                    self.username[self.username_len] = ch;
                    self.username_len += 1;
                }
            }
            1 => {
                if self.password_len < PASSWORD_MAX {
                    self.password[self.password_len] = ch;
                    self.password_len += 1;
                }
            }
            2 => {
                if self.super_password_len < PASSWORD_MAX {
                    self.super_password[self.super_password_len] = ch;
                    self.super_password_len += 1;
                }
            }
            _ => {}
        }
    }

    fn backspace_field(&mut self) {
        match self.focus {
            0 => {
                if self.username_len > 0 {
                    self.username_len -= 1;
                }
            }
            1 => {
                if self.password_len > 0 {
                    self.password_len -= 1;
                }
            }
            2 => {
                if self.super_password_len > 0 {
                    self.super_password_len -= 1;
                }
            }
            _ => {}
        }
    }

    fn set_status(&mut self, text: &[u8], error: bool) {
        let length = text.len().min(self.status.len());
        self.status[..length].copy_from_slice(&text[..length]);
        self.status_len = length;
        self.status_error = error;
    }

    fn set_error(&mut self, prefix: &[u8], message: &[u8]) {
        let mut buffer = [0u8; 200];
        let mut length = 0usize;
        length = push_text(&mut buffer, length, prefix);
        length = push_text(&mut buffer, length, message);
        let length = length.min(self.status.len());
        self.status[..length].copy_from_slice(&buffer[..length]);
        self.status_len = length;
        self.status_error = true;
    }

    /// Загружает список целей при первом показе окна.
    fn ensure_targets(&mut self, platform: &mut dyn Platform) {
        if !self.loaded {
            self.refresh(platform);
        }
    }

    fn refresh(&mut self, platform: &mut dyn Platform) {
        let mut list = [InstallTarget {
            number: 0,
            sectors: 0,
            ext4: false,
        }; MAX_TARGETS];
        let count = platform.install_targets(&mut list).min(MAX_TARGETS);

        self.targets = list;
        self.count = count;
        self.selected = 0;
        self.loaded = true;

        if count == 0 {
            self.set_status(b"No partitions found.", true);
        } else {
            self.set_status(
                b"Select a partition. Only ext4 installs without formatting.",
                false,
            );
        }
    }

    fn install(&mut self, platform: &mut dyn Platform) {
        if self.count == 0 || self.selected >= self.count {
            return;
        }

        if let Some(message) = self.credentials_error() {
            self.set_status(message, true);
            return;
        }

        let target = self.targets[self.selected];

        if !target.ext4 && !self.format {
            self.set_status(
                b"Partition is not ext4. Enable 'Format as ext4' to erase it.",
                true,
            );
            return;
        }

        let number = target.number as u32;

        match platform.install_perfix(
            number,
            self.format,
            &self.username[..self.username_len],
            &self.password[..self.password_len],
            &self.super_password[..self.super_password_len],
        ) {
            Ok(()) => {
                if self.format {
                    self.set_status(
                        b"Partition formatted as ext4, Perfix and GRUB installed.",
                        false,
                    );
                } else {
                    self.set_status(
                        b"Installed /perfix.elf and GRUB. Reboot to start Perfix.",
                        false,
                    );
                }
                // После форматирования файловая система стала ext4.
                if self.format {
                    self.targets[self.selected].ext4 = true;
                }
            }
            Err(message) => self.set_error(b"Install failed: ", message),
        }
    }

    /// Обрабатывает клавиши окна. Возвращает `true`, если нужна перерисовка.
    pub fn handle_key(&mut self, platform: &mut dyn Platform, key: Key) -> bool {
        self.ensure_targets(platform);

        if self.focus != FOCUS_NONE {
            match key {
                Key::Char(ch) => {
                    self.insert_field_char(ch);
                    return true;
                }
                Key::Backspace => {
                    self.backspace_field();
                    return true;
                }
                Key::Tab => {
                    self.focus = (self.focus + 1) % (FOCUS_NONE + 1);
                    return true;
                }
                Key::Enter => {
                    self.focus = FOCUS_NONE;
                    self.install(platform);
                    return true;
                }
                Key::Escape => {
                    self.focus = FOCUS_NONE;
                    return true;
                }
                _ => return false,
            }
        }

        match key {
            Key::Tab => {
                self.focus = 0;
                true
            }
            Key::ArrowUp if self.count > 0 => {
                self.selected = if self.selected == 0 {
                    self.count - 1
                } else {
                    self.selected - 1
                };
                true
            }
            Key::ArrowDown if self.count > 0 => {
                self.selected = (self.selected + 1) % self.count;
                true
            }
            Key::Home if self.count > 0 => {
                self.selected = 0;
                true
            }
            Key::End if self.count > 0 => {
                self.selected = self.count - 1;
                true
            }
            Key::Enter => {
                self.install(platform);
                true
            }
            Key::Char(b' ') | Key::Char(b'f') | Key::Char(b'F') => {
                self.format = !self.format;
                if self.format {
                    self.set_status(
                        b"Format enabled: the selected partition will be erased.",
                        false,
                    );
                } else {
                    self.set_status(
                        b"Format disabled: only existing ext4 can be installed.",
                        false,
                    );
                }
                true
            }
            Key::Char(b'r') | Key::Char(b'R') => {
                self.refresh(platform);
                true
            }
            _ => false,
        }
    }

    fn button_rect(&self, area: Rect) -> Rect {
        Rect {
            x: area.x + 16,
            y: area.y + area.height - BUTTON_BOTTOM,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        }
    }

    /// Прямоугольник переключателя форматирования.
    fn checkbox_rect(&self, area: Rect) -> Rect {
        Rect {
            x: area.x + 16,
            y: area.y + area.height - 100,
            width: area.width - 32,
            height: 22,
        }
    }

    /// Обрабатывает щелчок мышью внутри клиентской области.
    pub fn handle_click(
        &mut self,
        platform: &mut dyn Platform,
        area: Rect,
        x: i32,
        y: i32,
    ) -> bool {
        self.ensure_targets(platform);

        for index in 0..3 {
            if self.field_rect(area, index).contains(x, y) {
                self.focus = index;
                return true;
            }
        }

        if self.checkbox_rect(area).contains(x, y) {
            self.format = !self.format;

            if self.format {
                self.set_status(
                    b"Format enabled: the selected partition will be erased.",
                    false,
                );
            } else {
                self.set_status(
                    b"Format disabled: only existing ext4 can be installed.",
                    false,
                );
            }

            return true;
        }

        if self.button_rect(area).contains(x, y) {
            self.install(platform);
            return true;
        }

        let list_top = area.y + LIST_TOP;

        if x >= area.x + 16
            && x < area.x + area.width - 16
            && y >= list_top
            && y < list_top + self.count as i32 * ROW_HEIGHT
        {
            let index = ((y - list_top) / ROW_HEIGHT) as usize;

            if index < self.count {
                self.selected = index;
                self.focus = FOCUS_NONE;
                return true;
            }
        }

        false
    }

    /// Рисует окно установщика.
    pub fn paint(&mut self, platform: &mut dyn Platform, area: Rect) {
        self.ensure_targets(platform);

        platform.fill_rect(area.x, area.y, area.width, area.height, WINDOW_COLOR);

        platform.draw_text(
            area.x + 16,
            area.y + 12,
            b"Install Perfix on an ext4 partition",
            HEADER_COLOR,
            WINDOW_COLOR,
        );
        platform.draw_text(
            area.x + 16,
            area.y + 38,
            b"Only ext4 installs without formatting. The disk must use an MBR table.",
            HINT_COLOR,
            WINDOW_COLOR,
        );

        let list_top = area.y + LIST_TOP;

        if self.count == 0 {
            platform.draw_text(
                area.x + 16,
                list_top + 8,
                b"No partitions found.",
                ERROR_COLOR,
                WINDOW_COLOR,
            );
        }

        for index in 0..self.count.min(MAX_ROWS) {
            let row = Rect {
                x: area.x + 16,
                y: list_top + index as i32 * ROW_HEIGHT,
                width: area.width - 32,
                height: ROW_HEIGHT - 2,
            };

            if index == self.selected {
                platform.fill_rect(row.x, row.y, row.width, row.height, SELECTED_COLOR);
            }

            platform.draw_rect(row.x, row.y, row.width, row.height, ROW_BORDER);

            let target = self.targets[index];
            let mut line = [b' '; 64];
            let mut length = 0usize;

            length = push_text(&mut line, length, b"Partition ");
            length = push_decimal(&mut line, length, target.number as u32);
            length = push_text(
                &mut line,
                length,
                if target.ext4 { b"   ext4   " } else { b"   other  " },
            );
            length = push_decimal(
                &mut line,
                length,
                (target.sectors / 2048) as u32,
            );
            length = push_text(&mut line, length, b" MiB");

            platform.draw_text(
                row.x + 10,
                row.y + (ROW_HEIGHT - 16) / 2 - 1,
                &line[..length],
                TEXT_COLOR,
                if index == self.selected {
                    SELECTED_COLOR
                } else {
                    WINDOW_COLOR
                },
            );
        }

        let labels: [&[u8]; 3] = [
            b"User name",
            b"User password",
            b"Superuser password",
        ];

        for index in 0..3 {
            let field = self.field_rect(area, index);
            let active = self.focus == index;
            let background = if active { SELECTED_COLOR } else { WINDOW_COLOR };

            platform.fill_rect(
                field.x,
                field.y,
                field.width,
                field.height,
                background,
            );
            platform.draw_rect(
                field.x,
                field.y,
                field.width,
                field.height,
                if active { BUTTON_BORDER } else { ROW_BORDER },
            );
            platform.draw_text(
                field.x + 8,
                field.y + (FIELD_HEIGHT - 16) / 2,
                labels[index],
                HINT_COLOR,
                background,
            );

            let value_x = field.x + 160;

            match index {
                0 => platform.draw_text(
                    value_x,
                    field.y + (FIELD_HEIGHT - 16) / 2,
                    &self.username[..self.username_len],
                    TEXT_COLOR,
                    background,
                ),
                1 => {
                    let masked = [b'*'; PASSWORD_MAX];
                    platform.draw_text(
                        value_x,
                        field.y + (FIELD_HEIGHT - 16) / 2,
                        &masked[..self.password_len],
                        TEXT_COLOR,
                        background,
                    );
                }
                _ => {
                    let masked = [b'*'; PASSWORD_MAX];
                    platform.draw_text(
                        value_x,
                        field.y + (FIELD_HEIGHT - 16) / 2,
                        &masked[..self.super_password_len],
                        TEXT_COLOR,
                        background,
                    );
                }
            }
        }

        platform.draw_text(
            area.x + 16,
            area.y + FIELD_TOP + 3 * FIELD_GAP - 4,
            b"Tab switches fields. Enter installs. Passwords are case-sensitive.",
            HINT_COLOR,
            WINDOW_COLOR,
        );

        let checkbox = self.checkbox_rect(area);
        let box_size = 16;

        platform.fill_rect(
            checkbox.x,
            checkbox.y + 2,
            box_size,
            box_size,
            if self.format { SELECTED_COLOR } else { WINDOW_COLOR },
        );
        platform.draw_rect(
            checkbox.x,
            checkbox.y + 2,
            box_size,
            box_size,
            if self.format { BUTTON_BORDER } else { ROW_BORDER },
        );
        platform.draw_text(
            checkbox.x + box_size + 8,
            checkbox.y + 4,
            b"Format as ext4 (erases partition data)  [Space]",
            if self.format { ERROR_COLOR } else { HINT_COLOR },
            WINDOW_COLOR,
        );

        let button = self.button_rect(area);
        platform.fill_rect(
            button.x,
            button.y,
            button.width,
            button.height,
            BUTTON_COLOR,
        );
        platform.draw_rect(
            button.x,
            button.y,
            button.width,
            button.height,
            BUTTON_BORDER,
        );
        platform.draw_text(
            button.x + 56,
            button.y + (BUTTON_HEIGHT - 16) / 2,
            b"Install",
            TEXT_COLOR,
            BUTTON_COLOR,
        );

        let status_y = area.y + area.height - 24;
        let status_color = if self.status_error {
            ERROR_COLOR
        } else {
            SUCCESS_COLOR
        };
        let max_chars = ((area.width - 32) / 8).max(0) as usize;
        let length = self.status_len.min(max_chars);

        platform.draw_text(
            area.x + 16,
            status_y,
            &self.status[..length],
            status_color,
            WINDOW_COLOR,
        );
    }
}
