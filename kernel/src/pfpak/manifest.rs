//! Манифест пакета pfpak.
//!
//! Формат намеренно простой, построчный:
//!
//! ```text
//! name: hello
//! version: 1.0.0
//! maintainer: Densikasrv
//! description: Example package
//! size: 1234
//! depends: libc, zlib
//! files:
//! /usr/bin/hello
//! /usr/share/hello/README
//! ```
//!
//! Манифест разбирается без копирования: все поля и список файлов
//! читаются прямо из буфера, в котором он был загружен.

pub type Error = &'static [u8];

/// Максимальный размер манифеста.
pub const MAX_SIZE: usize = 16 * 1024;

/// Находит значение поля `key:` в манифесте.
pub fn field<'a>(manifest: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut position = 0usize;

    while position < manifest.len() {
        let end = line_end(manifest, position);
        let line = trim(&manifest[position..end]);

        if let Some(colon) = line.iter().position(|&byte| byte == b':') {
            if trim(&line[..colon]).eq_ignore_ascii_case(key) {
                return Some(trim(&line[colon + 1..]));
            }
        }

        position = end + 1;
    }

    None
}

/// Перебирает пути из секции `files:`.
///
/// Возвращает `false` из замыкания, чтобы остановить обход.
pub fn for_each_file<F>(manifest: &[u8], mut f: F)
where
    F: FnMut(&[u8]) -> bool,
{
    let mut position = 0usize;
    let mut in_files = false;

    while position < manifest.len() {
        let end = line_end(manifest, position);
        let line = trim(&manifest[position..end]);

        if !in_files {
            if line.eq_ignore_ascii_case(b"files:") {
                in_files = true;
            }
        } else if !line.is_empty() && !f(line) {
            return;
        }

        position = end + 1;
    }
}

/// Перебирает зависимости из поля `depends:`.
pub fn for_each_depend<F>(manifest: &[u8], mut f: F)
where
    F: FnMut(&[u8]) -> bool,
{
    let Some(value) = field(manifest, b"depends") else {
        return;
    };

    let mut start = 0usize;

    while start < value.len() {
        while start < value.len()
            && (value[start] == b',' || value[start] == b' ' || value[start] == b'\t')
        {
            start += 1;
        }

        let mut end = start;

        while end < value.len()
            && value[end] != b','
            && value[end] != b' '
            && value[end] != b'\t'
        {
            end += 1;
        }

        if end > start && !f(&value[start..end]) {
            return;
        }

        start = end;
    }
}

/// Есть ли путь в списке файлов манифеста.
pub fn has_file(manifest: &[u8], path: &[u8]) -> bool {
    let mut found = false;

    for_each_file(manifest, |entry| {
        if entry == path {
            found = true;
            false
        } else {
            true
        }
    });

    found
}

/// Количество файлов в манифесте.
pub fn file_count(manifest: &[u8]) -> usize {
    let mut count = 0usize;
    for_each_file(manifest, |_| {
        count += 1;
        true
    });
    count
}

/// Есть ли в манифесте секция `files:`.
pub fn has_files_section(manifest: &[u8]) -> bool {
    let mut position = 0usize;

    while position < manifest.len() {
        let end = line_end(manifest, position);
        let line = trim(&manifest[position..end]);

        if line.eq_ignore_ascii_case(b"files:") {
            return true;
        }

        position = end + 1;
    }

    false
}

/// Проверяет обязательные поля и формат имени/версии.
pub fn validate(manifest: &[u8], expected_name: Option<&[u8]>) -> Result<(), Error> {
    if manifest.len() > MAX_SIZE {
        return Err(b"manifest is too large");
    }

    let Some(name) = field(manifest, b"name") else {
        return Err(b"manifest has no name");
    };

    if !super::paths::valid_package_name(name) {
        return Err(b"manifest has invalid package name");
    }

    if let Some(expected) = expected_name {
        if name != expected {
            return Err(b"manifest name does not match package");
        }
    }

    let Some(version) = field(manifest, b"version") else {
        return Err(b"manifest has no version");
    };

    if !super::paths::valid_version(version) {
        return Err(b"manifest has invalid version");
    }

    if field(manifest, b"maintainer").is_none() {
        return Err(b"manifest has no maintainer");
    }

    if field(manifest, b"description").is_none() {
        return Err(b"manifest has no description");
    }

    if field(manifest, b"size").is_none() {
        return Err(b"manifest has no size");
    }

    if field(manifest, b"depends").is_none() {
        return Err(b"manifest has no depends field");
    }

    if !has_files_section(manifest) {
        return Err(b"manifest has no files section");
    }

    Ok(())
}

fn line_end(data: &[u8], start: usize) -> usize {
    data[start..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|offset| start + offset)
        .unwrap_or(data.len())
}

fn trim(mut data: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = data.split_first() {
        if first == b' ' || first == b'\t' || first == b'\r' {
            data = rest;
        } else {
            break;
        }
    }

    while let Some((&last, rest)) = data.split_last() {
        if last == b' ' || last == b'\t' || last == b'\r' {
            data = rest;
        } else {
            break;
        }
    }

    data
}
