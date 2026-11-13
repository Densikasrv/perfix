//! Perfbrowse — простой текстовый браузер.
//!
//! Загружает страницу по HTTP/1.0 через TCP, вырезает из HTML текст и
//! печатает его в консоль с переносом по словам. Поддерживаются
//! `http://`, DNS-имена и порты, отличные от 80. HTTPS и сложная
//! вёрстка (таблицы, CSS, картинки) пока не поддерживаются.

use crate::http;
use crate::vga;

const BODY_CAPACITY: usize = 32 * 1024;
const TEXT_CAPACITY: usize = 32 * 1024;
const MAX_LINES: usize = 300;

static mut BODY: [u8; BODY_CAPACITY] = [0; BODY_CAPACITY];
static mut TEXT: [u8; TEXT_CAPACITY] = [0; TEXT_CAPACITY];

fn eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}

fn find_ignore_case(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from >= haystack.len() {
        return None;
    }

    let last = haystack.len() - needle.len();

    for index in from..=last {
        if eq_ignore_case(&haystack[index..index + needle.len()], needle) {
            return Some(index);
        }
    }

    None
}

fn push_newline(out: &mut [u8], length: &mut usize) {
    if *length == 0 || out[*length - 1] != b'\n' {
        if *length < out.len() {
            out[*length] = b'\n';
            *length += 1;
        }
    }
}

fn block_tag(name: &[u8]) -> bool {
    matches!(
        name,
        b"p" | b"div"
            | b"br"
            | b"tr"
            | b"li"
            | b"ul"
            | b"ol"
            | b"table"
            | b"section"
            | b"article"
            | b"header"
            | b"footer"
            | b"h1"
            | b"h2"
            | b"h3"
            | b"h4"
            | b"h5"
            | b"h6"
    )
}

/// Превращает HTML в простой текст.
fn render_html(body: &[u8], out: &mut [u8]) -> usize {
    let mut length = 0usize;
    let mut index = 0usize;
    let mut pending_space = false;

    while index < body.len() && length < out.len() {
        let byte = body[index];

        if byte == b'<' {
            let Some(relative_end) = body[index..].iter().position(|&b| b == b'>') else {
                break;
            };
            let end = index + relative_end;
            let tag = &body[index + 1..end];
            let closing = tag.first() == Some(&b'/');
            let name = if closing { &tag[1..] } else { tag };
            let name_end = name
                .iter()
                .position(|&b| b == b' ' || b == b'/' || b == b'\t')
                .unwrap_or(name.len());
            let name = &name[..name_end];

            if !closing && (eq_ignore_case(name, b"script") || eq_ignore_case(name, b"style")) {
                let closing_tag = if eq_ignore_case(name, b"script") {
                    b"</script".as_slice()
                } else {
                    b"</style".as_slice()
                };

                if let Some(found) = find_ignore_case(body, end + 1, closing_tag) {
                    index = body[found..]
                        .iter()
                        .position(|&b| b == b'>')
                        .map(|offset| found + offset + 1)
                        .unwrap_or(body.len());
                } else {
                    break;
                }

                continue;
            }

            if block_tag(name) {
                push_newline(out, &mut length);
                pending_space = false;
            }

            index = end + 1;
            continue;
        }

        if byte == b'\n' || byte == b'\r' || byte == b'\t' || byte == b' ' {
            pending_space = true;
            index += 1;
            continue;
        }

        if byte == b'&' {
            let mut decoded = None;
            let mut entity_end = index + 1;

            while entity_end < body.len() && entity_end < index + 12 && body[entity_end] != b';' {
                entity_end += 1;
            }

            if entity_end < body.len() && body[entity_end] == b';' {
                let entity = &body[index + 1..entity_end];

                decoded = if eq_ignore_case(entity, b"amp") {
                    Some(b'&')
                } else if eq_ignore_case(entity, b"lt") {
                    Some(b'<')
                } else if eq_ignore_case(entity, b"gt") {
                    Some(b'>')
                } else if eq_ignore_case(entity, b"quot") {
                    Some(b'"')
                } else if eq_ignore_case(entity, b"apos") || entity == b"#39" {
                    Some(b'\'')
                } else if eq_ignore_case(entity, b"nbsp") {
                    Some(b' ')
                } else if entity.first() == Some(&b'#') {
                    let digits = if entity.get(1) == Some(&b'x') || entity.get(1) == Some(&b'X') {
                        u32::from_str_radix(
                            core::str::from_utf8(&entity[2..]).unwrap_or(""),
                            16,
                        )
                        .ok()
                    } else {
                        core::str::from_utf8(&entity[1..])
                            .ok()
                            .and_then(|text| text.parse::<u32>().ok())
                    };

                    digits
                        .filter(|&code| code >= 32 && code < 127)
                        .map(|code| code as u8)
                } else {
                    None
                };

                if decoded.is_some() {
                    index = entity_end + 1;
                }
            }

            let Some(character) = decoded else {
                // Неизвестная сущность — выводим амперсанд как обычный текст.
                if pending_space && length > 0 && out[length - 1] != b'\n' {
                    if length < out.len() {
                        out[length] = b' ';
                        length += 1;
                    }
                }

                pending_space = false;

                if length < out.len() {
                    out[length] = b'&';
                    length += 1;
                }

                index += 1;
                continue;
            };

            if pending_space && length > 0 && out[length - 1] != b'\n' {
                if length < out.len() {
                    out[length] = b' ';
                    length += 1;
                }
            }

            pending_space = false;

            if length < out.len() {
                out[length] = character;
                length += 1;
            }

            continue;
        }

        if pending_space && length > 0 && out[length - 1] != b'\n' {
            if length < out.len() {
                out[length] = b' ';
                length += 1;
            }
        }

        pending_space = false;

        if length < out.len() {
            out[length] = byte;
            length += 1;
        }

        index += 1;
    }

    length
}

fn write_line(row: &mut usize, line: &[u8]) {
    let height = vga::height();

    if height == 0 {
        return;
    }

    if *row >= height {
        vga::scroll_screen();
        *row = height - 1;
    }

    vga::print_at(*row, 0, line);
    *row += 1;

    // Если строка была последней, сразу прокручиваем экран: тогда
    // приглашение оболочки окажется на освободившейся нижней строке,
    // а только что напечатанный текст останется видимым.
    if *row >= height {
        vga::scroll_screen();
        *row = height - 1;
    }
}

/// Печатает текст с переносом по словам. Возвращает следующую строку.
fn print_text(text: &[u8], start_row: usize) -> (usize, usize) {
    let width = vga::width().saturating_sub(2).max(20).min(vga::MAX_WIDTH);
    let mut row = start_row;
    let mut line = [b' '; vga::MAX_WIDTH];
    let mut length = 0usize;
    let mut lines = 0usize;
    let mut index = 0usize;

    while index < text.len() {
        if text[index] == b'\n' {
            if length > 0 {
                write_line(&mut row, &line[..length]);
                lines += 1;
                length = 0;
            } else {
                write_line(&mut row, b"");
                lines += 1;
            }

            index += 1;

            if lines >= MAX_LINES {
                break;
            }

            continue;
        }

        let start = index;

        while index < text.len() && text[index] != b'\n' && text[index] != b' ' {
            index += 1;
        }

        let word = &text[start..index];

        if length > 0 && length + 1 + word.len() > width {
            write_line(&mut row, &line[..length]);
            lines += 1;
            length = 0;

            if lines >= MAX_LINES {
                break;
            }
        }

        if length > 0 {
            line[length] = b' ';
            length += 1;
        }

        for &byte in word {
            if length < width {
                line[length] = byte;
                length += 1;
            }
        }

        while index < text.len() && text[index] == b' ' {
            index += 1;
        }
    }

    if length > 0 && lines < MAX_LINES {
        write_line(&mut row, &line[..length]);
        lines += 1;
    }

    (row, lines)
}

/// Загружает страницу и возвращает её текст без вывода на экран.
///
/// Функция используется и консольным `browse`, и GUI-приложением
/// Perfbrowse. Текст записывается в `out`, ошибки возвращаются статической
/// строкой. Ответ с кодом не 2xx попадает в `out` как строка статуса.
pub fn load(argument: &[u8], out: &mut [u8]) -> Result<usize, &'static [u8]> {
    if argument.is_empty() {
        return Err(b"usage: browse <http://host[:port]/path>");
    }

    let body = unsafe { &mut *core::ptr::addr_of_mut!(BODY) };
    let response = http::fetch(argument, body, true, 10)?;

    if !(200..300).contains(&response.status) {
        let status_line = response.status_line(body);
        let length = status_line.len().min(out.len());
        out[..length].copy_from_slice(&status_line[..length]);
        return Ok(length);
    }

    Ok(render_html(response.body(body), out))
}

/// Загружает страницу и печатает её текст. Возвращает следующую строку.
pub fn browse(argument: &[u8], start_row: usize) -> usize {
    let mut row = start_row;
    let text = unsafe { &mut *core::ptr::addr_of_mut!(TEXT) };

    match load(argument, text) {
        Ok(length) => {
            let (next_row, _) = print_text(&text[..length], row);
            next_row
        }
        Err(message) => {
            let mut line = [0u8; 96];
            let mut length = 0usize;

            for &byte in b"browse: " {
                line[length] = byte;
                length += 1;
            }

            for &byte in message.iter().take(line.len() - length) {
                line[length] = byte;
                length += 1;
            }

            vga::print_at(row, 0, &line[..length]);
            row += 1;
            row
        }
    }
}
