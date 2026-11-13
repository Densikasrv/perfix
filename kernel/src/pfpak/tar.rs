//! Потоковый разбор tar и извлечение пакета.
//!
//! Распакованный gzip-поток приходит кусками произвольной длины, поэтому
//! заголовки и данные собираются постепенно. Поддерживаются обычные файлы
//! и каталоги (ustar, GNU longname и pax `path=`). Символические ссылки,
//! жёсткие ссылки и специальные файлы запрещены: пакет не должен уметь
//! выйти за пределы разрешённых путей через ссылку.

use super::paths;
use crate::arena;

/// Ошибка разбора или извлечения.
pub type Error = &'static [u8];

/// Максимальный размер одного файла пакета.
const MAX_FILE_SIZE: usize = 2 * 1024 * 1024;
/// Максимальный суммарный объём извлечённых данных (защита от gzip-бомб).
const MAX_TOTAL_SIZE: u64 = 16 * 1024 * 1024;
/// Предел для pax-заголовка.
const MAX_PAX_SIZE: usize = 1024;
/// Предел для GNU longname.
const MAX_LONG_NAME: usize = 512;

/// Куда извлекать файлы пакета.
pub trait Target {
    fn directory(&mut self, path: &[u8]) -> Result<(), Error>;
    fn file(&mut self, path: &[u8], data: &[u8]) -> Result<(), Error>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Header,
    Data,
    Padding,
    Done,
}

/// Извлекатель tar-потока.
pub struct Extractor<'a> {
    target: &'a mut dyn Target,
    state: State,
    header: [u8; 512],
    header_len: usize,
    remaining: u64,
    padding: usize,
    total: u64,
    file: Option<&'static mut [u8]>,
    file_len: usize,
    file_mark: u32,
    long_name: [u8; MAX_LONG_NAME],
    long_name_len: usize,
    pax: [u8; MAX_PAX_SIZE],
    pax_len: usize,
}

impl<'a> Extractor<'a> {
    pub fn new(target: &'a mut dyn Target) -> Self {
        Self {
            target,
            state: State::Header,
            header: [0; 512],
            header_len: 0,
            remaining: 0,
            padding: 0,
            total: 0,
            file: None,
            file_len: 0,
            file_mark: 0,
            long_name: [0; MAX_LONG_NAME],
            long_name_len: 0,
            pax: [0; MAX_PAX_SIZE],
            pax_len: 0,
        }
    }

    /// Завершает извлечение и освобождает незавершённый файл.
    pub fn finish(&mut self) {
        self.release_file();
    }

    fn release_file(&mut self) {
        if self.file.is_some() {
            arena::release(self.file_mark);
            self.file = None;
        }

        self.file_len = 0;
    }

    fn reset_metadata(&mut self) {
        self.long_name_len = 0;
        self.pax_len = 0;
    }

    fn header_complete(&mut self) -> Result<(), Error> {
        if self.header.iter().all(|&byte| byte == 0) {
            self.release_file();
            self.state = State::Done;
            return Ok(());
        }

        verify_checksum(&self.header)?;

        let typeflag = self.header[156];
        let size = parse_number(&self.header[124..136])
            .ok_or(b"invalid tar size" as Error)?;

        match typeflag {
            b'L' => {
                if size as usize > MAX_LONG_NAME {
                    return Err(b"tar long name is too long");
                }

                self.state = State::Data;
                self.remaining = size;
                self.padding = padding(size);
                self.header_len = 0;
                return Ok(());
            }
            b'x' => {
                if size as usize > MAX_PAX_SIZE {
                    return Err(b"tar pax header is too large");
                }

                self.state = State::Data;
                self.remaining = size;
                self.padding = padding(size);
                self.header_len = 0;
                return Ok(());
            }
            b'g' => {
                self.state = State::Data;
                self.remaining = size;
                self.padding = padding(size);
                self.header_len = 0;
                return Ok(());
            }
            _ => {}
        }

        let path = self.entry_path()?;
        let normalized = paths::normalize(&path)?;
        self.reset_metadata();

        self.total = self
            .total
            .checked_add(size)
            .ok_or(b"tar package is too large" as Error)?;

        if self.total > MAX_TOTAL_SIZE {
            return Err(b"tar package is too large");
        }

        match typeflag {
            b'0' | 0 => {
                if size as usize > MAX_FILE_SIZE {
                    return Err(b"tar file is too large");
                }

                if !paths::allowed(&normalized) {
                    return Err(b"package writes outside allowed paths");
                }

                if size == 0 {
                    self.target.file(&normalized, &[])?;
                    self.state = State::Header;
                    self.header_len = 0;
                    return Ok(());
                }

                self.release_file();
                self.file_mark = arena::mark();
                let Some(buffer) = arena::allocate(size as usize, 16) else {
                    return Err(b"not enough memory for package file");
                };

                self.file = Some(buffer);
                self.file_len = 0;
                self.remaining = size;
                self.padding = padding(size);
                self.state = State::Data;
            }
            b'5' => {
                if !paths::allowed(&normalized) {
                    return Err(b"package creates directory outside allowed paths");
                }

                self.target.directory(&normalized)?;
                self.remaining = size;
                self.padding = padding(size);
                self.state = if size == 0 {
                    State::Header
                } else {
                    State::Data
                };
            }
            b'1' | b'2' => return Err(b"package contains a link (not allowed)"),
            b'3' | b'4' | b'6' => {
                return Err(b"package contains a special file (not allowed)")
            }
            b'V' | b'K' | b'M' | b'S' | b'N' | b'D' => {
                self.remaining = size;
                self.padding = padding(size);
                self.state = if size == 0 {
                    State::Header
                } else {
                    State::Data
                };
            }
            _ => {
                return Err(b"unsupported tar entry type");
            }
        }

        self.header_len = 0;
        Ok(())
    }

    fn entry_path(&mut self) -> Result<[u8; 512], Error> {
        if self.pax_len > 0 {
            if let Some(path) = pax_path(&self.pax[..self.pax_len]) {
                let mut out = [0u8; 512];
                if path.len() >= out.len() {
                    return Err(b"tar path is too long");
                }

                out[..path.len()].copy_from_slice(path);
                return Ok(out);
            }
        }

        if self.long_name_len > 0 {
            let mut out = [0u8; 512];
            let length = self.long_name_len.min(out.len() - 1);
            out[..length].copy_from_slice(&self.long_name[..length]);
            return Ok(out);
        }

        let mut out = [0u8; 512];
        let name = c_string(&self.header[0..100]);
        let magic = &self.header[257..263];
        let prefix = if magic.starts_with(b"ustar") {
            c_string(&self.header[345..500])
        } else {
            b"".as_slice()
        };

        let length = if prefix.is_empty() {
            name.len()
        } else {
            prefix.len() + 1 + name.len()
        };

        if length >= out.len() {
            return Err(b"tar path is too long");
        }

        if prefix.is_empty() {
            out[..name.len()].copy_from_slice(name);
        } else {
            out[..prefix.len()].copy_from_slice(prefix);
            out[prefix.len()] = b'/';
            out[prefix.len() + 1..length].copy_from_slice(name);
        }

        Ok(out)
    }

    fn data(&mut self, input: &[u8]) -> Result<(), Error> {
        let count = input.len().min(self.remaining as usize);
        let chunk = &input[..count];

        match self.header[156] {
            b'L' => {
                let length = self.long_name_len + count;
                if length > MAX_LONG_NAME {
                    return Err(b"tar long name is too long");
                }

                self.long_name[self.long_name_len..length].copy_from_slice(chunk);
                self.long_name_len = length;
            }
            b'x' => {
                let length = self.pax_len + count;
                if length > MAX_PAX_SIZE {
                    return Err(b"tar pax header is too large");
                }

                self.pax[self.pax_len..length].copy_from_slice(chunk);
                self.pax_len = length;
            }
            _ => {
                if let Some(file) = self.file.as_deref_mut() {
                    let end = self.file_len + count;
                    if end > file.len() {
                        return Err(b"tar file overflow");
                    }

                    file[self.file_len..end].copy_from_slice(chunk);
                    self.file_len = end;
                }
            }
        }

        self.remaining -= count as u64;

        if self.remaining == 0 {
            self.finish_data()?;
        }

        Ok(())
    }

    fn finish_data(&mut self) -> Result<(), Error> {
        let typeflag = self.header[156];

        if let Some(file) = self.file.take() {
            let path = self.entry_path()?;
            let normalized = paths::normalize(&path)?;

            if !paths::allowed(&normalized) {
                arena::release(self.file_mark);
                return Err(b"package writes outside allowed paths");
            }

            let result = self.target.file(&normalized, &file[..self.file_len]);
            arena::release(self.file_mark);
            self.file_len = 0;
            result?;
        }

        self.padding = padding(self.total_entry_size());
        self.state = if self.padding == 0 {
            State::Header
        } else {
            State::Padding
        };
        self.header_len = 0;

        // Данные GNU longname и pax относятся к следующему заголовку,
        // поэтому очищать их здесь нельзя.
        if typeflag != b'L' && typeflag != b'x' && typeflag != b'g' {
            self.reset_metadata();
        }

        Ok(())
    }

    fn total_entry_size(&self) -> u64 {
        // remaining уже равен нулю; размер записи восстанавливается из
        // заголовка. Для метаданных и пропускаемых записей это безопасно.
        parse_number(&self.header[124..136]).unwrap_or(0)
    }
}

impl crate::pfpak::inflate::Sink for Extractor<'_> {
    fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut input = data;

        while !input.is_empty() && self.state != State::Done {
            match self.state {
                State::Header => {
                    let need = 512 - self.header_len;
                    let count = input.len().min(need);
                    self.header[self.header_len..self.header_len + count]
                        .copy_from_slice(&input[..count]);
                    self.header_len += count;
                    input = &input[count..];

                    if self.header_len == 512 {
                        self.header_complete()?;
                    }
                }
                State::Data => {
                    if self.remaining == 0 {
                        self.finish_data()?;
                        continue;
                    }

                    let take = input.len().min(self.remaining as usize);
                    self.data(&input[..take])?;
                    input = &input[take..];
                }
                State::Padding => {
                    let count = input.len().min(self.padding);
                    self.padding -= count;
                    input = &input[count..];

                    if self.padding == 0 {
                        self.state = State::Header;
                        self.header_len = 0;
                    }
                }
                State::Done => break,
            }
        }

        Ok(())
    }
}

fn padding(size: u64) -> usize {
    ((512 - (size % 512)) % 512) as usize
}

fn c_string(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&byte| byte == 0).unwrap_or(field.len());
    &field[..end]
}

fn parse_number(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|byte| byte & 0x80 != 0) {
        let mut value = (field[0] & 0x7F) as u64;

        for &byte in &field[1..] {
            value = value.checked_mul(256)?.checked_add(byte as u64)?;
        }

        return Some(value);
    }

    let mut value = 0u64;
    let mut digits = 0usize;

    for &byte in field {
        if byte == 0 || byte == b' ' {
            if digits > 0 {
                break;
            }

            continue;
        }

        if !(b'0'..=b'7').contains(&byte) {
            return None;
        }

        value = value.checked_mul(8)?.checked_add((byte - b'0') as u64)?;
        digits += 1;
    }

    if digits == 0 {
        Some(0)
    } else {
        Some(value)
    }
}

fn verify_checksum(header: &[u8; 512]) -> Result<(), Error> {
    let stored = parse_number(&header[148..156]).ok_or(b"invalid tar checksum" as Error)?;
    let mut sum = 0u64;

    for (index, &byte) in header.iter().enumerate() {
        sum += if (148..156).contains(&index) {
            b' ' as u64
        } else {
            byte as u64
        };
    }

    if sum != stored {
        return Err(b"tar checksum mismatch");
    }

    Ok(())
}

fn pax_path(data: &[u8]) -> Option<&[u8]> {
    let mut position = 0usize;

    while position < data.len() {
        let space = data[position..].iter().position(|&byte| byte == b' ')?;
        let length_text = &data[position..position + space];
        let length = parse_decimal(length_text)?;
        let end = position.checked_add(length)?;

        if length == 0 || end > data.len() {
            return None;
        }

        let record = &data[position + space + 1..end];
        let newline = record.iter().position(|&byte| byte == b'\n');
        let entry = if let Some(newline) = newline {
            &record[..newline]
        } else {
            record
        };

        if let Some(value) = entry.strip_prefix(b"path=") {
            return Some(value);
        }

        position = end;
    }

    None
}

fn parse_decimal(data: &[u8]) -> Option<usize> {
    let mut value = 0usize;

    if data.is_empty() {
        return None;
    }

    for &byte in data {
        if !byte.is_ascii_digit() {
            return None;
        }

        value = value.checked_mul(10)?.checked_add((byte - b'0') as usize)?;
    }

    Some(value)
}
