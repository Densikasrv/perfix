//! Полноэкранный текстовый редактор `tedit` в духе GNU nano.
//!
//! Редактор использует всю консоль (240x67 в framebuffer-режиме):
//! строка заголовка, область текста, строка состояния и подсказка по
//! клавишам. Текст хранится в статическом буфере CP866 и при сохранении
//! конвертируется в UTF-8.

use crate::exfat::{self, FsError};
use crate::keyboard::{self, Key};
use crate::vga;

/// Максимальный размер редактируемого текста.
const MAX_TEXT: usize = 8192;
/// UTF-8 представление не может быть длиннее двух байт на символ CP866.
const MAX_UTF8: usize = MAX_TEXT * 2;

const TITLE_ROW: usize = 0;
const TEXT_TOP: usize = 1;

const MAX_WIDTH: usize = vga::MAX_WIDTH;
const CUT_CAPACITY: usize = 256;

fn width() -> usize {
    vga::width()
}

fn text_rows() -> usize {
    vga::height().saturating_sub(3)
}

fn status_row() -> usize {
    vga::height().saturating_sub(2)
}

fn help_row() -> usize {
    vga::height() - 1
}

/// Буфер редактируемого текста.
struct Buffer {
    data: [u8; MAX_TEXT],
    len: usize,
}

impl Buffer {
    const fn new() -> Buffer {
        Buffer {
            data: [0; MAX_TEXT],
            len: 0,
        }
    }

    fn insert(&mut self, at: usize, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }

        if self.len + bytes.len() > self.data.len() {
            return false;
        }

        let at = at.min(self.len);
        let extra = bytes.len();

        let mut index = self.len;
        while index > at {
            self.data[index + extra - 1] = self.data[index - 1];
            index -= 1;
        }

        self.data[at..at + extra].copy_from_slice(bytes);
        self.len += extra;

        true
    }

    fn delete(&mut self, at: usize, count: usize) {
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
}

static mut BUFFER: Buffer = Buffer::new();
static mut UTF8: [u8; MAX_UTF8] = [0; MAX_UTF8];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Edit,
    ConfirmExit,
}

struct Editor<'a> {
    name: &'a [u8],
    cursor: usize,
    top_offset: usize,
    dirty: bool,
    saved: bool,
    new_file: bool,
    truncated: bool,
    mode: Mode,
    status: [u8; MAX_WIDTH],
    status_len: usize,
    cut: [u8; CUT_CAPACITY],
    cut_len: usize,
}

impl<'a> Editor<'a> {
    fn new(name: &'a [u8]) -> Editor<'a> {
        Editor {
            name,
            cursor: 0,
            top_offset: 0,
            dirty: false,
            saved: false,
            new_file: false,
            truncated: false,
            mode: Mode::Edit,
            status: [b' '; MAX_WIDTH],
            status_len: 0,
            cut: [0; CUT_CAPACITY],
            cut_len: 0,
        }
    }

    fn set_status(&mut self, text: &[u8]) {
        let len = text.len().min(width());
        self.status[..len].copy_from_slice(&text[..len]);
        self.status_len = len;
    }

    fn clear_status(&mut self) {
        self.status_len = 0;
    }
}

/// Результат работы редактора.
pub struct Outcome {
    pub saved: bool,
}

fn load_ext4_file(
    path: &[u8],
    output: &mut [u8],
) -> Result<(usize, bool), FsError> {
    use crate::fs::ext4::{AtaDevice, Ext4};

    let device = AtaDevice;
    let ext4 = Ext4::mount(
        &device,
        crate::disk::selected_lba(),
        crate::disk::selected_sectors(),
    )
    .map_err(|_| FsError::BadGeometry)?;

    let mut inode_number = 2u32;

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        inode_number = ext4
            .find_directory_entry(inode_number, component)
            .map_err(|_| FsError::BadGeometry)?
            .ok_or(FsError::PathNotFound)?;
    }

    let inode = ext4
        .read_inode(inode_number)
        .map_err(|_| FsError::BadGeometry)?;

    if inode.is_directory || inode.mode & 0xF000 != 0x8000 {
        return Err(FsError::NotAFile);
    }

    let utf8 = unsafe { &mut *core::ptr::addr_of_mut!(UTF8) };
    let mut offset = 0u64;

    while offset < inode.size && (offset as usize) < utf8.len() {
        let count = ext4
            .read_file(
                inode_number,
                offset,
                &mut utf8[offset as usize..],
            )
            .map_err(|_| FsError::BadGeometry)?;

        if count == 0 {
            break;
        }

        offset += count as u64;
    }

    let mut decoder = crate::utf8::Decoder::new();
    let mut decoded = [0u8; 4];
    let mut output_len = 0usize;

    for &byte in &utf8[..offset as usize] {
        let count = decoder.feed(
            byte,
            &mut decoded,
            crate::fs::exfat::fs::utf16_to_cp866,
        );

        if output_len + count > output.len() {
            break;
        }

        output[output_len..output_len + count]
            .copy_from_slice(&decoded[..count]);

        output_len += count;
    }

    let truncated = offset < inode.size || output_len == output.len();

    Ok((output_len, truncated))
}

/// Загружает текст файла с выбранного тома.
///
/// Отсутствующий файл возвращается как `FsError::PathNotFound`.
pub fn load_text(path: &[u8], output: &mut [u8]) -> Result<(usize, bool), FsError> {
    match crate::disk::selected_kind() {
        crate::disk::FsKind::Exfat => exfat::load_text_file(path, output),
        crate::disk::FsKind::Ext4 => load_ext4_file(path, output),
        crate::disk::FsKind::Other => Err(FsError::NotExfat),
    }
}

/// Загружает файл для GUI-редактора.
///
/// Возвращает `(длина, существовал, обрезан, длина пути)`, а в `resolved`
/// пишет абсолютный путь, по которому файл можно сохранить позже.
pub fn gui_load(
    name: &[u8],
    output: &mut [u8],
    resolved: &mut [u8],
) -> Result<(usize, bool, bool, usize), &'static [u8]> {
    let mut path = [0u8; 512];
    let Some(path_len) = crate::shell::resolve_path(name, &mut path) else {
        return Err(b"path is too long");
    };

    if crate::shell::reserved_path(&path[..path_len]) {
        return Err(b"reserved path");
    }

    let copy = path_len.min(resolved.len());
    resolved[..copy].copy_from_slice(&path[..copy]);

    match load_text(&path[..path_len], output) {
        Ok((length, truncated)) => Ok((length, true, truncated, copy)),
        Err(FsError::PathNotFound) => Ok((0, false, false, copy)),
        Err(error) => Err(error.message()),
    }
}
/// Запускает редактор для файла `path`.
///
/// Отсутствующий файл считается новым. Возвращает ошибку только если
/// том или файл нельзя прочитать.
pub fn run(name: &[u8]) -> Result<Outcome, FsError> {
    if name.is_empty() {
        return Err(FsError::InvalidName);
    }

    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER) };
    buffer.len = 0;

    let mut editor = Editor::new(name);

    let is_exfat = matches!(
        crate::disk::selected_kind(),
        crate::disk::FsKind::Exfat
    );
    let is_ext4 = matches!(
        crate::disk::selected_kind(),
        crate::disk::FsKind::Ext4
    );

    let loaded = load_text(name, &mut buffer.data);

    match loaded {
        Ok((len, truncated)) => {
            buffer.len = len;
            editor.truncated = truncated;
        }
        Err(FsError::PathNotFound) if is_exfat || is_ext4 => {
            editor.new_file = true;
        }
        Err(error) => return Err(error),
    }

    if editor.truncated {
        editor.set_status(b"File truncated to 8192 bytes");
    }

    let mut needs_render = true;

    loop {
        ensure_visible(&buffer.data[..buffer.len], &mut editor);

        if needs_render {
            render(buffer, &mut editor);
            needs_render = false;
        }

        let key = keyboard::read_key();

        if editor.mode == Mode::ConfirmExit {
            match key {
                Key::Character(b'y') | Key::Character(b'Y') => {
                    if save(buffer, &mut editor) {
                        return Ok(Outcome { saved: true });
                    }

                    editor.mode = Mode::Edit;
                    needs_render = true;
                }
                Key::Character(b'n') | Key::Character(b'N') => {
                    return Ok(Outcome {
                        saved: editor.saved,
                    });
                }
                Key::Escape | Key::Control(b'c') => {
                    editor.mode = Mode::Edit;
                    editor.clear_status();
                    needs_render = true;
                }
                Key::None => pause(),
                _ => {}
            }

            continue;
        }

        match key {
            Key::None => pause(),

            Key::Character(ch) => {
                editor.clear_status();
                insert_bytes(buffer, &mut editor, &[ch]);
                needs_render = true;
            }

            Key::Enter => {
                editor.clear_status();
                insert_bytes(buffer, &mut editor, b"\n");
                needs_render = true;
            }

            Key::Tab => {
                editor.clear_status();
                insert_bytes(buffer, &mut editor, b"    ");
                needs_render = true;
            }

            Key::Backspace => {
                editor.clear_status();
                delete_before(buffer, &mut editor);
                needs_render = true;
            }

            Key::Delete => {
                editor.clear_status();
                delete_at(buffer, &mut editor);
                needs_render = true;
            }

            Key::ArrowLeft => {
                editor.clear_status();
                if editor.cursor > 0 {
                    editor.cursor -= 1;
                }
                needs_render = true;
            }

            Key::ArrowRight => {
                editor.clear_status();
                if editor.cursor < buffer.len {
                    editor.cursor += 1;
                }
                needs_render = true;
            }

            Key::ArrowUp => {
                editor.clear_status();
                move_up(&buffer.data[..buffer.len], &mut editor);
                needs_render = true;
            }

            Key::ArrowDown => {
                editor.clear_status();
                move_down(&buffer.data[..buffer.len], &mut editor);
                needs_render = true;
            }

            Key::Home => {
                editor.clear_status();
                editor.cursor = line_start(&buffer.data[..buffer.len], editor.cursor);
                needs_render = true;
            }

            Key::End => {
                editor.clear_status();
                editor.cursor = line_end(&buffer.data[..buffer.len], editor.cursor);
                needs_render = true;
            }

            Key::PageUp => {
                editor.clear_status();
                for _ in 0..text_rows() - 1 {
                    move_up(&buffer.data[..buffer.len], &mut editor);
                }
                needs_render = true;
            }

            Key::PageDown => {
                editor.clear_status();
                for _ in 0..text_rows() - 1 {
                    move_down(&buffer.data[..buffer.len], &mut editor);
                }
                needs_render = true;
            }

            Key::Control(b'o') => {
                save(buffer, &mut editor);
                needs_render = true;
            }

            Key::Control(b'x') => {
                if editor.dirty {
                    editor.mode = Mode::ConfirmExit;
                    editor.set_status(b"Save modified buffer? Y=yes N=no ^C=cancel");
                    needs_render = true;
                } else {
                    return Ok(Outcome {
                        saved: editor.saved,
                    });
                }
            }

            Key::Control(b'k') => {
                cut_line(buffer, &mut editor);
                needs_render = true;
            }

            Key::Control(b'u') => {
                paste(buffer, &mut editor);
                needs_render = true;
            }

            Key::Control(b'g') => {
                editor.set_status(
                    b"Arrows/Home/End/PgUp/PgDn move  ^O save  ^X exit  ^K cut  ^U paste",
                );
                needs_render = true;
            }

            Key::Control(b'c') => {
                editor.clear_status();
                needs_render = true;
            }

            Key::LayoutChanged | Key::Escape | Key::Control(_) => {
                editor.clear_status();
                needs_render = true;
            }
        }
    }
}

fn pause() {
    unsafe {
        core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
    }
}

fn insert_bytes(buffer: &mut Buffer, editor: &mut Editor, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }

    if buffer.insert(editor.cursor, bytes) {
        editor.cursor += bytes.len();
        editor.dirty = true;
    } else {
        editor.set_status(b"Buffer full: file is too large");
    }
}

fn delete_before(buffer: &mut Buffer, editor: &mut Editor) {
    if editor.cursor > 0 {
        buffer.delete(editor.cursor - 1, 1);
        editor.cursor -= 1;
        editor.dirty = true;
    }
}

fn delete_at(buffer: &mut Buffer, editor: &mut Editor) {
    if editor.cursor < buffer.len {
        buffer.delete(editor.cursor, 1);
        editor.dirty = true;
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

fn move_up(data: &[u8], editor: &mut Editor) {
    let start = line_start(data, editor.cursor);

    if start == 0 {
        return;
    }

    let previous_end = start - 1;
    let previous_start = line_start(data, previous_end);
    let column = editor.cursor - start;
    let previous_len = previous_end - previous_start;

    editor.cursor = previous_start + column.min(previous_len);
}

fn move_down(data: &[u8], editor: &mut Editor) {
    let end = line_end(data, editor.cursor);

    if end >= data.len() {
        return;
    }

    let start = line_start(data, editor.cursor);
    let column = editor.cursor - start;
    let next_start = end + 1;
    let next_end = line_end(data, next_start);

    editor.cursor = next_start + column.min(next_end - next_start);
}

fn cut_line(buffer: &mut Buffer, editor: &mut Editor) {
    let start = line_start(&buffer.data[..buffer.len], editor.cursor);
    let end = line_end(&buffer.data[..buffer.len], editor.cursor);
    let has_newline = end < buffer.len;
    let content_len = end - start;
    let cut_len = content_len + if has_newline { 1 } else { 0 };

    if cut_len > CUT_CAPACITY {
        editor.set_status(b"Line is too long for the cut buffer");
        return;
    }

    editor.cut[..content_len].copy_from_slice(&buffer.data[start..end]);
    editor.cut_len = content_len;

    if has_newline {
        editor.cut[content_len] = b'\n';
        editor.cut_len += 1;
    }

    if editor.cut_len == 0 {
        editor.set_status(b"Nothing to cut");
        return;
    }

    buffer.delete(start, cut_len);
    editor.cursor = start.min(buffer.len);
    editor.dirty = true;
    editor.set_status(b"Cut current line  (^U to paste)");
}

fn paste(buffer: &mut Buffer, editor: &mut Editor) {
    if editor.cut_len == 0 {
        editor.set_status(b"Nothing to paste");
        return;
    }

    if buffer.insert(editor.cursor, &editor.cut[..editor.cut_len]) {
        editor.cursor += editor.cut_len;
        editor.dirty = true;
    } else {
        editor.set_status(b"Buffer full: cannot paste");
    }
}

fn save_ext4_file(path: &[u8], text: &[u8]) -> Result<(), &'static [u8]> {
    use crate::fs::ext4::{AtaDevice, Ext4, Ext4Error};

    let device = AtaDevice;
    let ext4 = Ext4::mount_read_write(
        &device,
        crate::disk::selected_lba(),
        crate::disk::selected_sectors(),
    )
    .map_err(|error| match error {
        Ext4Error::UnsafeWrite => {
            b"ext4 write disabled: unsafe features" as &'static [u8]
        }
        Ext4Error::ReadOnly => b"ext4 is read-only" as &'static [u8],
        _ => b"ext4 mount failed" as &'static [u8],
    })?;

    match ext4.write_file_path(path, text) {
        Ok(_) => Ok(()),
        Err(Ext4Error::NoSpace) => Err(b"ext4: no free space"),
        Err(Ext4Error::UnsupportedFeature) => {
            Err(b"ext4: operation not supported")
        }
        Err(Ext4Error::ReadOnly) => Err(b"ext4 is read-only"),
        Err(_) => Err(b"ext4 write failed"),
    }
}

/// Сохраняет текст (CP866) по указанному пути.
///
/// Возвращает размер записанного UTF-8 текста в байтах.
pub fn save_text(path: &[u8], text: &[u8]) -> Result<usize, &'static [u8]> {
    let utf8 = unsafe { &mut *core::ptr::addr_of_mut!(UTF8) };
    let utf8_len = exfat::cp866_to_utf8(text, utf8);

    match crate::disk::selected_kind() {
        crate::disk::FsKind::Exfat => {
            exfat::save_file(path, &utf8[..utf8_len])
                .map_err(|error| error.message())?;
        }
        crate::disk::FsKind::Ext4 => {
            save_ext4_file(path, &utf8[..utf8_len])?;
        }
        crate::disk::FsKind::Other => {
            return Err(b"unsupported filesystem");
        }
    }

    Ok(utf8_len)
}

/// Сохраняет файл GUI-редактора.
///
/// Путь может быть относительным (тогда он считается от текущего каталога)
/// или абсолютным, как в команде `tedit`.
pub fn gui_save(path: &[u8], text: &[u8]) -> Result<usize, &'static [u8]> {
    let mut resolved = [0u8; 512];
    let Some(length) = crate::shell::resolve_path(path, &mut resolved) else {
        return Err(b"path is too long");
    };

    if crate::shell::reserved_path(&resolved[..length]) {
        return Err(b"reserved path");
    }

    save_text(&resolved[..length], text)
}

fn save(buffer: &Buffer, editor: &mut Editor) -> bool {
    match save_text(editor.name, &buffer.data[..buffer.len]) {
        Ok(utf8_len) => {
            editor.dirty = false;
            editor.saved = true;
            editor.new_file = false;

            let mut line = [b' '; MAX_WIDTH];
            let mut len = 0usize;
            push(&mut line, &mut len, b"Saved: ");
            push(&mut line, &mut len, editor.name);
            push(&mut line, &mut len, b" (");
            push_dec(&mut line, &mut len, utf8_len);
            push(&mut line, &mut len, b" bytes)");
            editor.set_status(&line[..len]);

            true
        }
        Err(error) => {
            let mut line = [b' '; MAX_WIDTH];
            let mut len = 0usize;
            push(&mut line, &mut len, b"Save failed: ");
            push(&mut line, &mut len, error);
            editor.set_status(&line[..len]);

            false
        }
    }
}


fn ensure_visible(data: &[u8], editor: &mut Editor) {
    if editor.cursor > data.len() {
        editor.cursor = data.len();
    }

    if editor.top_offset > data.len() {
        editor.top_offset = data.len();
    }

    if editor.cursor < editor.top_offset {
        editor.top_offset = visual_row_start(data, editor.cursor);
    }

    let mut guard = 0usize;

    loop {
        let (row, _) = cursor_screen(data, editor);

        if row < text_rows() {
            break;
        }

        let next = next_row_start(data, editor.top_offset);

        if next <= editor.top_offset {
            editor.top_offset = editor.cursor;
            break;
        }

        editor.top_offset = next;
        guard += 1;

        if guard > MAX_TEXT {
            break;
        }
    }
}

/// Позиция курсора на экране относительно начала области текста.
fn cursor_screen(data: &[u8], editor: &Editor) -> (usize, usize) {
    let from = editor.top_offset.min(data.len());
    let to = editor.cursor.min(data.len());

    if to < from {
        return (0, 0);
    }

    let (row, col) = visual_distance(data, from, to);

    // Позиция сразу после 80-го символа — это «ожидание переноса»:
    // показываем курсор на последнем знакоместе той же строки.
    if col == width() {
        (row, width() - 1)
    } else {
        (row, col)
    }
}

fn visual_distance(data: &[u8], from: usize, to: usize) -> (usize, usize) {
    let mut row = 0usize;
    let mut col = 0usize;
    let mut pos = from;

    while pos < to {
        if data[pos] == b'\n' {
            row += 1;
            col = 0;
        } else {
            if col == width() {
                row += 1;
                col = 0;
            }

            col += 1;
        }

        pos += 1;
    }

    (row, col)
}

/// Начало визуальной строки, в которой находится позиция `target`.
fn visual_row_start(data: &[u8], target: usize) -> usize {
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
            if col == width() {
                row_start = pos;
                col = 0;
            }

            pos += 1;
            col += 1;
        }
    }

    row_start
}

/// Начало следующей визуальной строки после `offset`.
fn next_row_start(data: &[u8], offset: usize) -> usize {
    let mut pos = offset.min(data.len());
    let mut col = 0usize;

    while pos < data.len() {
        if data[pos] == b'\n' {
            return pos + 1;
        }

        pos += 1;
        col += 1;

        if col == width() {
            if pos < data.len() && data[pos] == b'\n' {
                return pos + 1;
            }

            return pos;
        }
    }

    data.len()
}


fn render(buffer: &Buffer, editor: &mut Editor) {
    let data = &buffer.data[..buffer.len];

    // Заголовок.
    let mut title = [b' '; MAX_WIDTH];
    let mut title_len = 0usize;
    push(&mut title, &mut title_len, b"  tedit  ");
    push(&mut title, &mut title_len, editor.name);

    if editor.new_file {
        push(&mut title, &mut title_len, b"  [New]");
    }

    if editor.dirty {
        push(&mut title, &mut title_len, b"  [Modified]");
    }

    vga::write_row(TITLE_ROW, &title, vga::COLOR_INVERSE);

    // Область текста: каждая экранная строка собирается целиком, поэтому
    // неизменившиеся ячейки не перерисовываются.
    let mut pos = editor.top_offset.min(data.len());

    for screen_row in 0..text_rows() {
        let mut line = [b' '; MAX_WIDTH];
        let mut col = 0usize;

        while col < width() && pos < data.len() && data[pos] != b'\n' {
            line[col] = match data[pos] {
                b'\t' => b' ',
                0x20..=0x7e | 0x80..=0xff => data[pos],
                _ => b'.',
            };

            pos += 1;
            col += 1;
        }

        vga::write_row(TEXT_TOP + screen_row, &line, vga::COLOR_NORMAL);

        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    // Строка состояния.
    let mut status = [b' '; MAX_WIDTH];
    let mut status_len = 0usize;

    if editor.status_len > 0 {
        push(
            &mut status,
            &mut status_len,
            &editor.status[..editor.status_len],
        );
    } else {
        let name = if editor.name.len() > 30 {
            &editor.name[..30]
        } else {
            editor.name
        };

        push(&mut status, &mut status_len, b" ");
        push(&mut status, &mut status_len, name);
        push(&mut status, &mut status_len, b"  Ln ");
        push_dec(
            &mut status,
            &mut status_len,
            line_number(data, editor.cursor) + 1,
        );
        push(&mut status, &mut status_len, b"/");
        push_dec(
            &mut status,
            &mut status_len,
            line_number(data, data.len()) + 1,
        );
        push(&mut status, &mut status_len, b", Col ");
        push_dec(
            &mut status,
            &mut status_len,
            editor.cursor - line_start(data, editor.cursor) + 1,
        );

        if editor.dirty {
            push(&mut status, &mut status_len, b"  [Modified]");
        }

        if editor.truncated {
            push(&mut status, &mut status_len, b"  [Truncated]");
        }
    }

    vga::write_row(status_row(), &status, vga::COLOR_INVERSE);

    // Подсказка по клавишам.
    let mut help = [b' '; MAX_WIDTH];
    let mut help_len = 0usize;
    push(&mut help, &mut help_len, b"^O Save   ^X Exit   ^K Cut   ^U Paste   ^G Help");
    vga::write_row(help_row(), &help, vga::COLOR_INVERSE);

    // Аппаратный курсор.
    let (cursor_row, cursor_col) = cursor_screen(data, editor);
    vga::set_cursor(TEXT_TOP + cursor_row.min(text_rows() - 1), cursor_col);
}


fn push<const N: usize>(out: &mut [u8; N], len: &mut usize, bytes: &[u8]) {
    for &byte in bytes {
        if *len >= out.len() {
            return;
        }

        out[*len] = byte;
        *len += 1;
    }
}

fn push_dec<const N: usize>(out: &mut [u8; N], len: &mut usize, value: usize) {
    let mut digits = [0u8; 20];
    let mut count = 0usize;
    let mut value = value;

    if value == 0 {
        digits[0] = b'0';
        count = 1;
    } else {
        while value > 0 && count < digits.len() {
            digits[count] = b'0' + (value % 10) as u8;
            value /= 10;
            count += 1;
        }
    }

    while count > 0 {
        count -= 1;
        push(out, len, &[digits[count]]);
    }
}


