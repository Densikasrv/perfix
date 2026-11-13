//! Проверка путей пакета.
//!
//! Пакет не должен уметь записывать файлы вне системных каталогов и
//! подменять базу pfpak. Все имена из tar и манифеста проходят через
//! [`normalize`] и [`allowed`]: абсолютные пути, `..`, `.` и NUL
//! отбрасываются, а запись разрешена только в белый список префиксов.

use core::ops::Deref;

pub type Error = &'static [u8];

/// Максимальная длина нормализованного пути.
pub const PATH_MAX: usize = 480;

/// Нормализованный абсолютный путь.
#[derive(Clone, Copy)]
pub struct Path {
    data: [u8; PATH_MAX],
    len: usize,
}

impl Path {
    pub fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

impl Deref for Path {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// Нормализует имя записи tar или строку манифеста.
///
/// Возвращает путь, начинающийся с `/`, без повторных и завершающих
/// слешей. Компоненты `.` и `..` запрещены: именно они чаще всего
/// используются для path traversal. NUL считается концом строки.
pub fn normalize(input: &[u8]) -> Result<Path, Error> {
    let mut path = Path {
        data: [0; PATH_MAX],
        len: 1,
    };
    path.data[0] = b'/';

    let mut start = 0usize;
    let mut components = 0usize;

    while start < input.len() && input[start] != 0 {
        let mut end = start;

        while end < input.len() && input[end] != 0 && input[end] != b'/' {
            end += 1;
        }

        let component = &input[start..end];

        if !component.is_empty() && component != b"." {
            if component == b".." {
                return Err(b"package path contains '..'");
            }

            if component.len() > 255 {
                return Err(b"package path component is too long");
            }

            if component
                .iter()
                .any(|&byte| byte < 0x20 || byte == b'\\')
            {
                return Err(b"package path contains invalid characters");
            }

            let separator = if path.len > 1 { 1 } else { 0 };

            if path.len + separator + component.len() > path.data.len() {
                return Err(b"package path is too long");
            }

            if separator == 1 {
                path.data[path.len] = b'/';
                path.len += 1;
            }

            path.data[path.len..path.len + component.len()]
                .copy_from_slice(component);
            path.len += component.len();
            components += 1;
        }

        if end >= input.len() || input[end] == 0 {
            break;
        }

        start = end + 1;
    }

    if components == 0 {
        return Err(b"package path is empty");
    }

    Ok(path)
}

const ALLOWED: [&[u8]; 8] = [
    b"/usr",
    b"/opt",
    b"/bin",
    b"/sbin",
    b"/lib",
    b"/etc",
    b"/var",
    b"/share",
];

const PROTECTED: [&[u8]; 5] = [
    b"/etc/pfpak",
    b"/etc/users",
    b"/perfix.elf",
    b"/etc/repo.list",
    b"/boot",
];

fn under(path: &[u8], prefix: &[u8]) -> bool {
    path == prefix
        || (path.len() > prefix.len()
            && path.starts_with(prefix)
            && path[prefix.len()] == b'/')
}

/// Разрешена ли запись по нормализованному пути.
pub fn allowed(path: &Path) -> bool {
    let bytes = path.as_bytes();

    if PROTECTED.iter().any(|prefix| under(bytes, prefix)) {
        return false;
    }

    ALLOWED.iter().any(|prefix| under(bytes, prefix))
}

/// Имя пакета: буквы, цифры, `.`, `_`, `-`; без слэшей и `..`.
pub fn valid_package_name(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > 64 || name == b"." || name == b".." {
        return false;
    }

    name.iter().all(|&byte| {
        byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-'
    })
}

/// Версия: печатные ASCII-символы без пробелов и слэшей.
pub fn valid_version(version: &[u8]) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version.iter().all(|&byte| {
            (0x21..=0x7e).contains(&byte) && byte != b'/' && byte != b'\\'
        })
}
