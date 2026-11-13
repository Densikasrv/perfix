//! Список репозиториев pfpak.
//!
//! Формат `/etc/pfpak/repo.list`:
//!
//! ```text
//! # комментарий
//! https://example.com/perfix/pfpak RELEASE:fatcat
//! ```
//!
//! Первое поле — базовый URL, второе — релиз (каталог внутри репозитория).
//! Метаданные пакета лежат по пути
//! `<url>/<release>/<package>/MANIFEST`, а архив —
//! `<url>/<release>/<package>/<package>-<version>.tar.gz`.
//!
//! Ссылки GitHub вида
//! `https://raw.githubusercontent.com/<owner>/<repo>/tree/<branch>/<path>`
//! автоматически приводятся к raw-адресу без `/tree/`.

const MAX_REPOS: usize = 8;
const URL_MAX: usize = 256;
const RELEASE_MAX: usize = 64;

#[derive(Clone, Copy)]
pub struct Repo {
    url: [u8; URL_MAX],
    url_len: usize,
    release: [u8; RELEASE_MAX],
    release_len: usize,
}

impl Repo {
    pub const EMPTY: Self = Self {
        url: [0; URL_MAX],
        url_len: 0,
        release: [0; RELEASE_MAX],
        release_len: 0,
    };
}

/// Разбирает содержимое `repo.list`.
pub fn parse(data: &[u8], repos: &mut [Repo; MAX_REPOS]) -> usize {
    let mut count = 0usize;
    let mut position = 0usize;

    for repo in repos.iter_mut() {
        *repo = Repo::EMPTY;
    }

    while position < data.len() && count < MAX_REPOS {
        let end = data[position..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map(|offset| position + offset)
            .unwrap_or(data.len());
        let mut line = trim(&data[position..end]);
        position = end + 1;

        if line.is_empty() || line[0] == b'#' {
            continue;
        }

        let split = line
            .iter()
            .position(|&byte| byte == b' ' || byte == b'\t')
            .unwrap_or(line.len());
        let url = &line[..split];
        line = trim(&line[split..]);

        let has_scheme = starts_with_ignore_case(url, b"http://")
            || starts_with_ignore_case(url, b"https://");

        if !has_scheme {
            continue;
        }

        // GitHub отдаёт веб-адрес вида
        // https://raw.githubusercontent.com/<owner>/<repo>/tree/<branch>/<path>.
        // Для загрузки нужен адрес без `/tree`: <owner>/<repo>/<branch>/<path>.
        let mut normalized = [0u8; URL_MAX];
        let Some(url) = normalize_url(url, &mut normalized) else {
            continue;
        };

        // Второе поле имеет вид `RELEASE:<name>`. Для совместимости со
        // старыми записями принимаем и просто имя релиза без префикса.
        let release = line.strip_prefix(b"RELEASE:").unwrap_or(line);

        if release.len() > RELEASE_MAX || !valid_release(release) {
            continue;
        }

        repos[count].url[..url.len()].copy_from_slice(url);
        repos[count].url_len = url.len();
        repos[count].release[..release.len()].copy_from_slice(release);
        repos[count].release_len = release.len();
        count += 1;
    }

    count
}

/// Приводит веб-ссылки GitHub к адресам raw-хоста.
///
/// Поддерживается только `raw.githubusercontent.com/<owner>/<repo>/tree/`
/// (и `/blob/`) с непустыми ветвью и путём. Остальные URL копируются как
/// есть: pfpak не выполняет редиректы и не разбирает HTML-страницы.
fn normalize_url<'a>(url: &'a [u8], out: &'a mut [u8; URL_MAX]) -> Option<&'a [u8]> {
    const RAW_PREFIX: &[u8] = b"https://raw.githubusercontent.com/";

    if url.len() > URL_MAX {
        return None;
    }

    if starts_with_ignore_case(url, RAW_PREFIX) {
        let rest = &url[RAW_PREFIX.len()..];
        let tree = find(rest, b"/tree/").or_else(|| find(rest, b"/blob/"));

        if let Some(position) = tree {
            let before = &url[..RAW_PREFIX.len() + position];
            let after = &url[RAW_PREFIX.len() + position + b"/tree/".len()..];

            // В `rest` должны быть owner и repo, а после — ветвь и путь.
            if find(&rest[..position], b"/").is_some()
                && !after.is_empty()
                && after[0] != b'/'
                && after[after.len() - 1] != b'/'
                && find(after, b"/").is_some()
            {
                let length = before.len() + 1 + after.len();

                if length <= URL_MAX {
                    out[..before.len()].copy_from_slice(before);
                    out[before.len()] = b'/';
                    out[before.len() + 1..length].copy_from_slice(after);
                    return Some(&out[..length]);
                }
            }

            return None;
        }
    }

    out[..url.len()].copy_from_slice(url);
    Some(&out[..url.len()])
}

fn find(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || data.len() < needle.len() {
        return None;
    }

    data.windows(needle.len())
        .position(|window| window == needle)
}

/// Строит URL файла пакета.
pub fn build_url(
    repo: &Repo,
    package: &[u8],
    file: &[u8],
    out: &mut [u8; 512],
) -> Option<usize> {
    let mut length = 0usize;

    push(out, &mut length, &repo.url[..repo.url_len])?;

    if repo.release_len > 0 {
        push_separator(out, &mut length)?;
        push(out, &mut length, &repo.release[..repo.release_len])?;
    }

    push_separator(out, &mut length)?;
    push(out, &mut length, package)?;
    push_separator(out, &mut length)?;
    push(out, &mut length, file)?;

    Some(length)
}

/// Строит URL служебного файла репозитория (`INDEX`).
pub fn build_index_url(repo: &Repo, out: &mut [u8; 512]) -> Option<usize> {
    let mut length = 0usize;

    push(out, &mut length, &repo.url[..repo.url_len])?;
    push_separator(out, &mut length)?;

    if repo.release_len > 0 {
        push(out, &mut length, &repo.release[..repo.release_len])?;
        push_separator(out, &mut length)?;
    }

    push(out, &mut length, b"INDEX")?;
    Some(length)
}

fn push_separator(out: &mut [u8; 512], length: &mut usize) -> Option<()> {
    if *length > 0 && out[*length - 1] != b'/' {
        if *length >= out.len() {
            return None;
        }

        out[*length] = b'/';
        *length += 1;
    }

    Some(())
}

fn push(out: &mut [u8; 512], length: &mut usize, data: &[u8]) -> Option<()> {
    if *length + data.len() > out.len() {
        return None;
    }

    out[*length..*length + data.len()].copy_from_slice(data);
    *length += data.len();
    Some(())
}

fn valid_release(release: &[u8]) -> bool {
    release.iter().all(|&byte| {
        byte > b' '
            && byte != b'#'
            && byte != b'?'
            && byte != b'\\'
            && !(byte == b'.' && release == b"..")
    })
}

/// Начинается ли строка с префикса без учёта регистра.
fn starts_with_ignore_case(data: &[u8], prefix: &[u8]) -> bool {
    data.len() >= prefix.len() && data[..prefix.len()].eq_ignore_ascii_case(prefix)
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
