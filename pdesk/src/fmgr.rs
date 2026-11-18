//! fmgr — графический файловый менеджер в духе Thunar.
//!
//! Слева расположены разделы (`/dsk/a1`, `/dsk/a2`, `/dsk/b1`), справа —
//! список файлов. Поддерживаются навигация, открытие текста в tedit,
//! создание и удаление каталогов, переименование, копирование и перенос.

use crate::window;
use crate::{DiskEntry, DirEntry, Key, Platform, FILE_NAME_CAPACITY};

pub const CONTENT_WIDTH: i32 = 620;
pub const CONTENT_HEIGHT: i32 = 420;
pub const PATH_CAPACITY: usize = 512;

const MAX_ENTRIES: usize = 160;
const MAX_DISKS: usize = 32;
const STATUS_CAPACITY: usize = 128;
const TOOLBAR_HEIGHT: i32 = 28;
const PATH_HEIGHT: i32 = 22;
const STATUS_HEIGHT: i32 = 22;
const SIDEBAR_WIDTH: i32 = 132;
const ROW_HEIGHT: i32 = 18;
const HEADER_HEIGHT: i32 = 18;
const DISK_ROW_HEIGHT: i32 = 20;
const BUTTON_GAP: i32 = 5;

const BACKGROUND: u32 = 0x00101620;
const TOOLBAR_COLOR: u32 = 0x001a2430;
const PATH_COLOR: u32 = 0x000c141c;
const SIDEBAR_COLOR: u32 = 0x00121c26;
const ROW_COLOR: u32 = 0x00141e28;
const ROW_ALT_COLOR: u32 = 0x00101822;
const SELECTED_COLOR: u32 = 0x002c5a7c;
const HEADER_COLOR: u32 = 0x00203040;
const BORDER_COLOR: u32 = 0x005a6a7a;
const TEXT_COLOR: u32 = 0x00d0d0d0;
const MUTED_COLOR: u32 = 0x008098a8;
const BUTTON_COLOR: u32 = 0x00203040;
const FOLDER_COLOR: u32 = 0x00e0b040;
const FILE_COLOR: u32 = 0x00a0b0c0;
const STATUS_COLOR: u32 = 0x001a2430;

const BUTTONS: [(&[u8], i32); 8] = [
    (b"Up", 30),
    (b"New", 36),
    (b"Rename", 52),
    (b"Delete", 50),
    (b"Copy", 40),
    (b"Cut", 32),
    (b"Paste", 44),
    (b"Open", 40),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Browse,
    NewFolder,
    Rename,
    Delete,
}

/// Результат обработки события менеджером.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ignored,
    Redraw,
    OpenEditor,
}

pub struct Fmgr {
    current: [u8; PATH_CAPACITY],
    current_len: usize,
    entries: [DirEntry; MAX_ENTRIES],
    count: usize,
    selected: usize,
    scroll: usize,
    disks: [DiskEntry; MAX_DISKS],
    disk_count: usize,
    needs_refresh: bool,
    status: [u8; STATUS_CAPACITY],
    status_len: usize,
    input: [u8; FILE_NAME_CAPACITY],
    input_len: usize,
    mode: Mode,
    clipboard: [u8; PATH_CAPACITY],
    clipboard_len: usize,
    cut: bool,
    open_path: [u8; PATH_CAPACITY],
    open_len: usize,
    last_click: Option<(usize, u64)>,
}

impl Fmgr {
    pub fn new() -> Self {
        Self {
            current: [0; PATH_CAPACITY],
            current_len: 0,
            entries: [DirEntry::EMPTY; MAX_ENTRIES],
            count: 0,
            selected: 0,
            scroll: 0,
            disks: [DiskEntry::EMPTY; MAX_DISKS],
            disk_count: 0,
            needs_refresh: true,
            status: [0; STATUS_CAPACITY],
            status_len: 0,
            input: [0; FILE_NAME_CAPACITY],
            input_len: 0,
            mode: Mode::Browse,
            clipboard: [0; PATH_CAPACITY],
            clipboard_len: 0,
            cut: false,
            open_path: [0; PATH_CAPACITY],
            open_len: 0,
            last_click: None,
        }
    }

    /// Путь, который нужно открыть в GUI-редакторе.
    pub fn open_path(&self, out: &mut [u8]) -> usize {
        let length = self.open_len.min(out.len());
        out[..length].copy_from_slice(&self.open_path[..length]);
        length
    }

    fn set_status(&mut self, text: &[u8]) {
        self.status_len = text.len().min(STATUS_CAPACITY);
        self.status[..self.status_len].copy_from_slice(&text[..self.status_len]);
    }

    fn clear_status(&mut self) {
        self.status_len = 0;
    }

    fn set_current(&mut self, path: &[u8]) {
        self.current_len = path.len().min(PATH_CAPACITY);
        self.current[..self.current_len].copy_from_slice(&path[..self.current_len]);
        self.selected = 0;
        self.scroll = 0;
        self.needs_refresh = true;
    }

    fn disk_root(disk: &DiskEntry, out: &mut [u8]) -> usize {
        let mut length = 0usize;

        for &byte in b"/dsk/" {
            out[length] = byte;
            length += 1;
        }

        for &byte in disk.name() {
            out[length] = byte;
            length += 1;
        }

        length
    }

    /// Перечитывает список раздела.
    fn refresh(&mut self, platform: &mut dyn Platform) {
        self.needs_refresh = false;
        self.disk_count = platform.disk_list(&mut self.disks).min(MAX_DISKS);

        if self.current_len == 0 {
            if self.disk_count > 0 {
                let mut path = [0u8; PATH_CAPACITY];
                let length = Self::disk_root(&self.disks[0], &mut path);
                self.current_len = length;
                self.current[..length].copy_from_slice(&path[..length]);
            } else {
                self.set_status(b"No supported partitions found");
                self.count = 0;
                return;
            }
        }

        match platform.list_directory(&self.current[..self.current_len], &mut self.entries) {
            Ok(count) => {
                self.count = count.min(MAX_ENTRIES);

                if self.selected >= self.count {
                    self.selected = self.count.saturating_sub(1);
                }

                if self.status_len == 0 {
                    let mut text = [0u8; STATUS_CAPACITY];
                    let mut length = 0usize;
                    length = push_text(&mut text, length, b"Items: ");
                    length = push_decimal(&mut text, length, self.count as u32);
                    self.set_status(&text[..length]);
                }
            }
            Err(message) => {
                self.count = 0;
                self.set_status(message);
            }
        }
    }

    fn entry_path(&self, index: usize, out: &mut [u8; PATH_CAPACITY]) -> usize {
        let mut length = self.current_len.min(PATH_CAPACITY - 1);
        out[..length].copy_from_slice(&self.current[..length]);

        if length > 0 && out[length - 1] != b'/' && length < out.len() {
            out[length] = b'/';
            length += 1;
        }

        if index < self.count {
            let name = self.entries[index].name();
            let copy = name.len().min(out.len() - length);
            out[length..length + copy].copy_from_slice(&name[..copy]);
            length += copy;
        }

        length
    }

    fn join_current(&self, name: &[u8], out: &mut [u8; PATH_CAPACITY]) -> usize {
        let mut length = self.current_len.min(PATH_CAPACITY - 1);
        out[..length].copy_from_slice(&self.current[..length]);

        if length > 0 && out[length - 1] != b'/' && length < out.len() {
            out[length] = b'/';
            length += 1;
        }

        let copy = name.len().min(out.len() - length);
        out[length..length + copy].copy_from_slice(&name[..copy]);
        length + copy
    }

    fn parent_path(&self, out: &mut [u8; PATH_CAPACITY]) -> usize {
        // Корень раздела (`/dsk/a1`) — выше него не поднимаемся.
        let mut length = self.current_len;

        while length > 0 && self.current[length - 1] != b'/' {
            length -= 1;
        }

        if length > 0 {
            length -= 1;
        }

        if length < 6 {
            length = self.current_len.min(6);
        }

        out[..length].copy_from_slice(&self.current[..length]);
        length
    }

    fn activate(&mut self, platform: &mut dyn Platform) -> Outcome {
        if self.selected >= self.count {
            return Outcome::Ignored;
        }

        let mut path = [0u8; PATH_CAPACITY];
        let length = self.entry_path(self.selected, &mut path);

        if self.entries[self.selected].is_dir {
            self.set_current(&path[..length]);
            self.clear_status();
            self.refresh(platform);
            Outcome::Redraw
        } else {
            self.open_len = length;
            self.open_path[..length].copy_from_slice(&path[..length]);
            Outcome::OpenEditor
        }
    }

    fn begin_input(&mut self, mode: Mode) {
        self.mode = mode;
        self.input_len = 0;
        self.clear_status();

        if mode == Mode::Rename && self.selected < self.count {
            let name = self.entries[self.selected].name();
            self.input_len = name.len().min(FILE_NAME_CAPACITY);
            self.input[..self.input_len].copy_from_slice(&name[..self.input_len]);
        }
    }

    fn valid_name(&self) -> bool {
        self.input_len > 0
            && self.input_len <= FILE_NAME_CAPACITY
            && !self.input[..self.input_len].contains(&b'/')
            && self.input[..self.input_len] != *b"."
            && self.input[..self.input_len] != *b".."
    }

    fn commit_input(&mut self, platform: &mut dyn Platform) -> Outcome {
        if !self.valid_name() {
            self.set_status(b"Invalid file name");
            return Outcome::Redraw;
        }

        let mut destination = [0u8; PATH_CAPACITY];
        let destination_len = self.join_current(&self.input[..self.input_len], &mut destination);

        let creating = self.mode == Mode::NewFolder;
        let result = match self.mode {
            Mode::NewFolder => platform.create_directory(&destination[..destination_len]),
            Mode::Rename => {
                if self.selected >= self.count {
                    Err(b"nothing selected" as &'static [u8])
                } else {
                    let mut source = [0u8; PATH_CAPACITY];
                    let source_len = self.entry_path(self.selected, &mut source);
                    platform.rename_path(&source[..source_len], &destination[..destination_len])
                }
            }
            _ => Ok(()),
        };

        self.mode = Mode::Browse;
        self.input_len = 0;

        match result {
            Ok(()) => {
                self.set_status(if creating {
                    b"Folder created"
                } else {
                    b"Renamed"
                });
                self.needs_refresh = true;
            }
            Err(message) => self.set_status(message),
        }

        if self.needs_refresh {
            self.refresh(platform);
        }

        let _ = destination;
        Outcome::Redraw
    }

    fn delete_selected(&mut self, platform: &mut dyn Platform) -> Outcome {
        if self.selected >= self.count {
            return Outcome::Ignored;
        }

        let mut path = [0u8; PATH_CAPACITY];
        let length = self.entry_path(self.selected, &mut path);

        match platform.remove_path(&path[..length]) {
            Ok(()) => {
                self.set_status(b"Deleted");
                self.needs_refresh = true;
                self.refresh(platform);
            }
            Err(message) => self.set_status(message),
        }

        Outcome::Redraw
    }

    fn copy_selected(&mut self, cut: bool) {
        if self.selected >= self.count {
            return;
        }

        let mut path = [0u8; PATH_CAPACITY];
        let length = self.entry_path(self.selected, &mut path);
        self.clipboard_len = length;
        self.clipboard[..length].copy_from_slice(&path[..length]);
        self.cut = cut;
        self.set_status(if cut { b"Cut" } else { b"Copied" });
    }

    fn base_name(path: &[u8]) -> &[u8] {
        let mut start = 0usize;

        for (index, &byte) in path.iter().enumerate() {
            if byte == b'/' {
                start = index + 1;
            }
        }

        &path[start..]
    }

    fn paste(&mut self, platform: &mut dyn Platform) -> Outcome {
        if self.clipboard_len == 0 {
            self.set_status(b"Clipboard is empty");
            return Outcome::Redraw;
        }

        let name = Self::base_name(&self.clipboard[..self.clipboard_len]);
        let mut destination = [0u8; PATH_CAPACITY];
        let destination_len = self.join_current(name, &mut destination);

        match platform.copy_path(
            &self.clipboard[..self.clipboard_len],
            &destination[..destination_len],
            self.cut,
        ) {
            Ok(()) => {
                self.set_status(if self.cut { b"Moved" } else { b"Copied" });

                if self.cut {
                    self.clipboard_len = 0;
                    self.cut = false;
                }

                self.needs_refresh = true;
                self.refresh(platform);
            }
            Err(message) => self.set_status(message),
        }

        Outcome::Redraw
    }

    pub fn handle_key(&mut self, platform: &mut dyn Platform, key: Key) -> Outcome {
        if self.needs_refresh {
            self.refresh(platform);
        }

        match self.mode {
            Mode::NewFolder | Mode::Rename => match key {
                Key::Char(ch) => {
                    if self.input_len < FILE_NAME_CAPACITY {
                        self.input[self.input_len] = ch;
                        self.input_len += 1;
                    }
                    Outcome::Redraw
                }
                Key::Backspace => {
                    self.input_len = self.input_len.saturating_sub(1);
                    Outcome::Redraw
                }
                Key::Enter => self.commit_input(platform),
                Key::Escape => {
                    self.mode = Mode::Browse;
                    self.input_len = 0;
                    self.clear_status();
                    Outcome::Redraw
                }
                _ => Outcome::Ignored,
            },
            Mode::Delete => match key {
                Key::Char(b'y') | Key::Char(b'Y') => {
                    self.mode = Mode::Browse;
                    self.delete_selected(platform)
                }
                Key::Char(b'n') | Key::Char(b'N') | Key::Escape => {
                    self.mode = Mode::Browse;
                    self.clear_status();
                    Outcome::Redraw
                }
                _ => Outcome::Ignored,
            },
            Mode::Browse => match key {
                Key::ArrowUp => {
                    self.selected = self.selected.saturating_sub(1);
                    Outcome::Redraw
                }
                Key::ArrowDown => {
                    if self.selected + 1 < self.count {
                        self.selected += 1;
                    }
                    Outcome::Redraw
                }
                Key::PageUp => {
                    self.selected = self.selected.saturating_sub(10);
                    Outcome::Redraw
                }
                Key::PageDown => {
                    self.selected = (self.selected + 10).min(self.count.saturating_sub(1));
                    Outcome::Redraw
                }
                Key::Home => {
                    self.selected = 0;
                    Outcome::Redraw
                }
                Key::End => {
                    self.selected = self.count.saturating_sub(1);
                    Outcome::Redraw
                }
                Key::Enter => self.activate(platform),
                Key::Backspace => {
                    let mut path = [0u8; PATH_CAPACITY];
                    let length = self.parent_path(&mut path);

                    if length != self.current_len {
                        self.set_current(&path[..length]);
                        self.clear_status();
                        self.refresh(platform);
                    }

                    Outcome::Redraw
                }
                Key::Delete | Key::Char(b'd') | Key::Char(b'D') => {
                    if self.selected < self.count {
                        self.mode = Mode::Delete;
                    }
                    Outcome::Redraw
                }
                Key::Char(b'n') | Key::Char(b'N') => {
                    self.begin_input(Mode::NewFolder);
                    Outcome::Redraw
                }
                Key::Char(b'r') | Key::Char(b'R') => {
                    if self.selected < self.count {
                        self.begin_input(Mode::Rename);
                    }
                    Outcome::Redraw
                }
                Key::Char(b'c') | Key::Char(b'C') => {
                    self.copy_selected(false);
                    Outcome::Redraw
                }
                Key::Char(b'x') | Key::Char(b'X') => {
                    self.copy_selected(true);
                    Outcome::Redraw
                }
                Key::Char(b'v') | Key::Char(b'V') => self.paste(platform),
                Key::Char(b'o') | Key::Char(b'O') => self.activate(platform),
                Key::Char(b'u') | Key::Char(b'U') => {
                    let mut path = [0u8; PATH_CAPACITY];
                    let length = self.parent_path(&mut path);

                    if length != self.current_len {
                        self.set_current(&path[..length]);
                        self.clear_status();
                        self.refresh(platform);
                    }

                    Outcome::Redraw
                }
                Key::Escape => {
                    self.clear_status();
                    Outcome::Redraw
                }
                _ => Outcome::Ignored,
            },
        }
    }

    pub fn handle_click(
        &mut self,
        platform: &mut dyn Platform,
        area: window::Rect,
        x: i32,
        y: i32,
        ticks: u64,
    ) -> Outcome {
        if self.needs_refresh {
            self.refresh(platform);
        }

        // Кнопки панели инструментов.
        for index in 0..BUTTONS.len() {
            if button_rect(area, index).contains(x, y) {
                return self.toolbar_action(platform, index);
            }
        }

        let main_y = area.y + TOOLBAR_HEIGHT + PATH_HEIGHT;
        let main_height = area.height - TOOLBAR_HEIGHT - PATH_HEIGHT - STATUS_HEIGHT;

        // Боковая панель: разделы.
        if x >= area.x && x < area.x + SIDEBAR_WIDTH && y >= main_y && y < main_y + main_height {
            let row = (y - main_y - HEADER_HEIGHT) / DISK_ROW_HEIGHT;

            if y >= main_y + HEADER_HEIGHT && row >= 0 && (row as usize) < self.disk_count {
                let mut path = [0u8; PATH_CAPACITY];
                let length = Self::disk_root(&self.disks[row as usize], &mut path);
                self.set_current(&path[..length]);
                self.clear_status();
                self.refresh(platform);
                return Outcome::Redraw;
            }

            return Outcome::Ignored;
        }

        // Список файлов.
        let list_x = area.x + SIDEBAR_WIDTH;
        let list_y = main_y + HEADER_HEIGHT;

        if x >= list_x && x < area.x + area.width && y >= list_y && y < main_y + main_height {
            let row = ((y - list_y) / ROW_HEIGHT) as usize;
            let index = self.scroll + row;

            if index < self.count {
                let previous = self.last_click;
                self.last_click = Some((index, ticks));

                if self.selected == index {
                    if let Some((last_index, last_ticks)) = previous {
                        if last_index == index && ticks.saturating_sub(last_ticks) < 500 {
                            return self.activate(platform);
                        }
                    }
                }

                self.selected = index;
                self.clear_status();
            }

            return Outcome::Redraw;
        }

        Outcome::Ignored
    }

    fn toolbar_action(&mut self, platform: &mut dyn Platform, index: usize) -> Outcome {
        match index {
            0 => {
                let mut path = [0u8; PATH_CAPACITY];
                let length = self.parent_path(&mut path);

                if length != self.current_len {
                    self.set_current(&path[..length]);
                    self.clear_status();
                    self.refresh(platform);
                }

                Outcome::Redraw
            }
            1 => {
                self.begin_input(Mode::NewFolder);
                Outcome::Redraw
            }
            2 => {
                if self.selected < self.count {
                    self.begin_input(Mode::Rename);
                }
                Outcome::Redraw
            }
            3 => {
                if self.selected < self.count {
                    self.mode = Mode::Delete;
                }
                Outcome::Redraw
            }
            4 => {
                self.copy_selected(false);
                Outcome::Redraw
            }
            5 => {
                self.copy_selected(true);
                Outcome::Redraw
            }
            6 => self.paste(platform),
            _ => self.activate(platform),
        }
    }

    pub fn paint(&mut self, platform: &mut dyn Platform, area: window::Rect) {
        if self.needs_refresh {
            self.refresh(platform);
        }

        platform.fill_rect(area.x, area.y, area.width, area.height, BACKGROUND);

        let main_y = area.y + TOOLBAR_HEIGHT + PATH_HEIGHT;
        let main_height = area.height - TOOLBAR_HEIGHT - PATH_HEIGHT - STATUS_HEIGHT;

        self.paint_toolbar(platform, area);
        self.paint_path(platform, area);

        self.paint_sidebar(platform, area, main_y, main_height);
        self.paint_files(platform, area, main_y, main_height);
        self.paint_status(platform, area);
    }

    fn paint_toolbar(&self, platform: &mut dyn Platform, area: window::Rect) {
        platform.fill_rect(area.x, area.y, area.width, TOOLBAR_HEIGHT, TOOLBAR_COLOR);

        for index in 0..BUTTONS.len() {
            let rect = button_rect(area, index);
            platform.fill_rect(rect.x, rect.y, rect.width, rect.height, BUTTON_COLOR);
            platform.draw_rect(rect.x, rect.y, rect.width, rect.height, BORDER_COLOR);
            platform.draw_text(
                rect.x + 6,
                rect.y + (rect.height - 16) / 2,
                BUTTONS[index].0,
                TEXT_COLOR,
                BUTTON_COLOR,
            );
        }
    }

    fn paint_path(&self, platform: &mut dyn Platform, area: window::Rect) {
        let y = area.y + TOOLBAR_HEIGHT;
        platform.fill_rect(area.x, y, area.width, PATH_HEIGHT, PATH_COLOR);
        platform.draw_text(
            area.x + 6,
            y + 3,
            &self.current[..self.current_len],
            TEXT_COLOR,
            PATH_COLOR,
        );
    }

    fn paint_sidebar(
        &self,
        platform: &mut dyn Platform,
        area: window::Rect,
        main_y: i32,
        main_height: i32,
    ) {
        platform.fill_rect(area.x, main_y, SIDEBAR_WIDTH, main_height, SIDEBAR_COLOR);
        platform.draw_text(
            area.x + 6,
            main_y + 1,
            b"Disks",
            MUTED_COLOR,
            SIDEBAR_COLOR,
        );

        for index in 0..self.disk_count {
            let y = main_y + HEADER_HEIGHT + index as i32 * DISK_ROW_HEIGHT;
            let disk = &self.disks[index];
            let mut name = [0u8; 16];
            let length = Self::disk_root(disk, &mut name);
            let active = self.current_len >= length && self.current[..length] == name[..length];
            let background = if active { SELECTED_COLOR } else { SIDEBAR_COLOR };

            if active {
                platform.fill_rect(
                    area.x + 2,
                    y,
                    SIDEBAR_WIDTH - 4,
                    DISK_ROW_HEIGHT - 1,
                    background,
                );
            }

            platform.draw_text(
                area.x + 6,
                y + 2,
                &name[..length],
                if active { TEXT_COLOR } else { MUTED_COLOR },
                background,
            );

            let label: &[u8] = match disk.kind {
                1 => b"exFAT",
                2 => b"ext4",
                _ => b"other",
            };
            platform.draw_text(
                area.x + 70,
                y + 2,
                label,
                if active { TEXT_COLOR } else { MUTED_COLOR },
                background,
            );
        }
    }

    fn visible_rows(area: window::Rect) -> usize {
        let main_height = area.height - TOOLBAR_HEIGHT - PATH_HEIGHT - STATUS_HEIGHT;
        ((main_height - HEADER_HEIGHT) / ROW_HEIGHT).max(1) as usize
    }

    fn paint_files(
        &mut self,
        platform: &mut dyn Platform,
        area: window::Rect,
        main_y: i32,
        main_height: i32,
    ) {
        let list_x = area.x + SIDEBAR_WIDTH;
        let width = area.width - SIDEBAR_WIDTH;
        let rows = Self::visible_rows(area);

        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }

        platform.fill_rect(list_x, main_y, width, main_height, BACKGROUND);
        platform.fill_rect(list_x, main_y, width, HEADER_HEIGHT, HEADER_COLOR);
        platform.draw_text(list_x + 22, main_y + 1, b"Name", MUTED_COLOR, HEADER_COLOR);
        platform.draw_text(
            list_x + width - 70,
            main_y + 1,
            b"Size",
            MUTED_COLOR,
            HEADER_COLOR,
        );

        let list_y = main_y + HEADER_HEIGHT;

        for row in 0..rows {
            let index = self.scroll + row;
            let y = list_y + row as i32 * ROW_HEIGHT;
            let background = if index % 2 == 0 { ROW_COLOR } else { ROW_ALT_COLOR };

            if index >= self.count {
                platform.fill_rect(list_x, y, width, ROW_HEIGHT, BACKGROUND);
                continue;
            }

            let selected = index == self.selected;
            let background = if selected { SELECTED_COLOR } else { background };
            platform.fill_rect(list_x, y, width, ROW_HEIGHT, background);

            let entry = &self.entries[index];

            if entry.is_dir {
                platform.fill_rect(list_x + 5, y + 4, 12, 9, FOLDER_COLOR);
                platform.fill_rect(list_x + 7, y + 2, 6, 3, FOLDER_COLOR);
            } else {
                platform.fill_rect(list_x + 5, y + 3, 10, 12, FILE_COLOR);
                platform.fill_rect(list_x + 8, y + 5, 6, 2, background);
                platform.fill_rect(list_x + 8, y + 8, 6, 2, background);
                platform.fill_rect(list_x + 8, y + 11, 4, 1, background);
            }

            let name = entry.name();
            let name_x = list_x + 22;
            let max_name = ((width - 100) / 8).max(1) as usize;
            let show = name.len().min(max_name);
            platform.draw_text(name_x, y + 1, &name[..show], TEXT_COLOR, background);

            let mut size = [0u8; 16];
            let size_len = if entry.is_dir {
                push_text(&mut size, 0, b"DIR")
            } else {
                push_size(&mut size, 0, entry.size)
            };
            platform.draw_text(
                list_x + width - 8 - size_len as i32 * 8,
                y + 1,
                &size[..size_len],
                MUTED_COLOR,
                background,
            );
        }

        // Полоса прокрутки.
        if self.count > rows {
            let track_y = list_y;
            let track_height = main_height - HEADER_HEIGHT;
            let bar_x = area.x + area.width - 5;
            platform.fill_rect(bar_x, track_y, 3, track_height, PATH_COLOR);
            let thumb_height = (track_height * rows as i32 / self.count as i32).max(10);
            let max_scroll = self.count.saturating_sub(rows).max(1);
            let offset = (track_height - thumb_height) * self.scroll as i32 / max_scroll as i32;
            platform.fill_rect(bar_x, track_y + offset, 3, thumb_height, MUTED_COLOR);
        }
    }

    fn paint_status(&self, platform: &mut dyn Platform, area: window::Rect) {
        let y = area.y + area.height - STATUS_HEIGHT;
        platform.fill_rect(area.x, y, area.width, STATUS_HEIGHT, STATUS_COLOR);

        let mut line = [0u8; STATUS_CAPACITY];
        let mut length = 0usize;

        match self.mode {
            Mode::NewFolder => {
                length = push_text(&mut line, length, b"New folder: ");
                length = push_text(&mut line, length, &self.input[..self.input_len]);
                length = push_text(&mut line, length, b"_");
            }
            Mode::Rename => {
                length = push_text(&mut line, length, b"Rename to: ");
                length = push_text(&mut line, length, &self.input[..self.input_len]);
                length = push_text(&mut line, length, b"_");
            }
            Mode::Delete => {
                length = push_text(&mut line, length, b"Delete ");
                if self.selected < self.count {
                    length = push_text(&mut line, length, self.entries[self.selected].name());
                }
                length = push_text(&mut line, length, b"? (y/n)");
            }
            Mode::Browse => {
                if self.status_len > 0 {
                    length = push_text(&mut line, length, &self.status[..self.status_len]);
                } else if self.selected < self.count {
                    let entry = &self.entries[self.selected];
                    length = push_text(&mut line, length, entry.name());

                    if !entry.is_dir {
                        length = push_text(&mut line, length, b"  ");
                        length = push_size(&mut line, length, entry.size);
                        length = push_text(&mut line, length, b" bytes");
                    }
                }
            }
        }

        platform.draw_text(area.x + 6, y + 3, &line[..length], TEXT_COLOR, STATUS_COLOR);
    }
}

fn button_rect(area: window::Rect, index: usize) -> window::Rect {
    let mut x = area.x + 6;

    for button_index in 0..index {
        x += BUTTONS[button_index].1 + BUTTON_GAP;
    }

    window::Rect {
        x,
        y: area.y + 4,
        width: BUTTONS[index].1,
        height: TOOLBAR_HEIGHT - 8,
    }
}

fn push_text(buffer: &mut [u8], mut length: usize, text: &[u8]) -> usize {
    for &byte in text {
        if length >= buffer.len() {
            break;
        }

        buffer[length] = byte;
        length += 1;
    }

    length
}

fn push_decimal(buffer: &mut [u8], length: usize, value: u32) -> usize {
    if value == 0 {
        return push_text(buffer, length, b"0");
    }

    let mut digits = [0u8; 10];
    let mut count = 0usize;
    let mut value = value;

    while value > 0 && count < digits.len() {
        digits[count] = b'0' + (value % 10) as u8;
        value /= 10;
        count += 1;
    }

    let mut length = length;

    while count > 0 {
        count -= 1;
        length = push_text(buffer, length, &[digits[count]]);
    }

    length
}

fn push_size(buffer: &mut [u8], length: usize, size: u64) -> usize {
    if size < 1024 {
        let length = push_decimal(buffer, length, size as u32);
        push_text(buffer, length, b" B")
    } else if size < 1024 * 1024 {
        let length = push_decimal(buffer, length, (size / 1024) as u32);
        push_text(buffer, length, b" K")
    } else {
        let length = push_decimal(buffer, length, (size / (1024 * 1024)) as u32);
        push_text(buffer, length, b" M")
    }
}
