//! Perfbrowse — простой текстовый браузер.
//!
//! Загружает страницу по HTTP/1.0 через TCP, вырезает из HTML текст и
//! печатает его в консоль с переносом по словам. Поддерживаются
//! `http://`, DNS-имена и порты, отличные от 80. HTTPS и сложная
//! вёрстка (таблицы, CSS, картинки) пока не поддерживаются.

use crate::net;
use crate::net::{dns, tcp};
use crate::rtc;
use crate::vga;

const DEFAULT_PORT: u16 = 80;
const BODY_CAPACITY: usize = 32 * 1024;
const TEXT_CAPACITY: usize = 32 * 1024;
const MAX_LINES: usize = 300;

static mut BODY: [u8; BODY_CAPACITY] = [0; BODY_CAPACITY];
static mut TEXT: [u8; TEXT_CAPACITY] = [0; TEXT_CAPACITY];

struct Url {
    host: [u8; 64],
    host_len: usize,
    port: u16,
    path: [u8; 160],
    path_len: usize,
}

fn push<const N: usize>(out: &mut [u8; N], length: &mut usize, bytes: &[u8]) {
    for &byte in bytes {
        if *length >= out.len() {
            return;
        }

        out[*length] = byte;
        *length += 1;
    }
}

fn push_dec<const N: usize>(out: &mut [u8; N], length: &mut usize, mut value: u32) {
    let mut digits = [0u8; 10];
    let mut count = 0usize;

    if value == 0 {
        digits[0] = b'0';
        count = 1;
    } else {
        while value > 0 {
            digits[count] = b'0' + (value % 10) as u8;
            value /= 10;
            count += 1;
        }
    }

    while count > 0 {
        count -= 1;
        push(out, length, &[digits[count]]);
    }
}

fn parse_url(text: &[u8]) -> Option<Url> {
    let rest = if let Some(rest) = text.strip_prefix(b"http://") {
        rest
    } else if text.starts_with(b"https://") {
        return None;
    } else {
        text
    };

    if rest.is_empty() {
        return None;
    }

    let host_end = rest
        .iter()
        .position(|&byte| byte == b'/' || byte == b':')
        .unwrap_or(rest.len());
    let host = &rest[..host_end];

    if host.is_empty() || host.len() > 63 {
        return None;
    }

    let mut url = Url {
        host: [0; 64],
        host_len: host.len(),
        port: DEFAULT_PORT,
        path: [0; 160],
        path_len: 1,
    };

    url.host[..host.len()].copy_from_slice(host);
    url.path[0] = b'/';

    let mut index = host_end;

    if rest.get(index) == Some(&b':') {
        index += 1;
        let mut port = 0u32;
        let start = index;

        while index < rest.len() && rest[index].is_ascii_digit() {
            port = port * 10 + (rest[index] - b'0') as u32;
            index += 1;
        }

        if index == start || port == 0 || port > 65535 {
            return None;
        }

        url.port = port as u16;
    }

    if rest.get(index) == Some(&b'/') {
        let path = &rest[index..];
        let count = path.len().min(url.path.len());
        url.path[..count].copy_from_slice(&path[..count]);
        url.path_len = count;
    }

    Some(url)
}

fn build_request(url: &Url) -> ([u8; 512], usize) {
    let mut request = [0u8; 512];
    let mut length = 0usize;

    push(&mut request, &mut length, b"GET ");
    push(&mut request, &mut length, &url.path[..url.path_len]);
    push(&mut request, &mut length, b" HTTP/1.0\r\nHost: ");
    push(&mut request, &mut length, &url.host[..url.host_len]);

    if url.port != DEFAULT_PORT {
        push(&mut request, &mut length, b":");
        push_dec(&mut request, &mut length, url.port as u32);
    }

    push(
        &mut request,
        &mut length,
        b"\r\nUser-Agent: Perfbrowse/0.1\r\nConnection: close\r\n\r\n",
    );

    (request, length)
}

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

fn find_header_end(body: &[u8]) -> Option<usize> {
    body.windows(4).position(|window| window == b"\r\n\r\n")
}

fn has_chunked(headers: &[u8]) -> bool {
    let Some(position) = find_ignore_case(headers, 0, b"transfer-encoding:") else {
        return false;
    };

    let start = position + b"transfer-encoding:".len();
    let end = headers[start..]
        .iter()
        .position(|&byte| byte == b'\r' || byte == b'\n')
        .map(|offset| start + offset)
        .unwrap_or(headers.len());

    find_ignore_case(&headers[start..end], 0, b"chunked").is_some()
}

fn hex_value(byte: u8) -> Option<usize> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as usize),
        b'a'..=b'f' => Some((byte - b'a' + 10) as usize),
        b'A'..=b'F' => Some((byte - b'A' + 10) as usize),
        _ => None,
    }
}

/// Декодирует `Transfer-Encoding: chunked` прямо в буфере ответа:
/// результат всегда короче исходных данных, поэтому копирование вперёд
/// безопасно. Возвращает абсолютный конец декодированных данных.
fn decode_chunked(body: &mut [u8], start: usize, end: usize) -> Option<usize> {
    let mut source = start;
    let mut destination = start;

    loop {
        let mut size = 0usize;
        let mut digits = 0usize;

        while source < end {
            let Some(value) = hex_value(body[source]) else {
                break;
            };

            size = size.saturating_mul(16).saturating_add(value);
            digits += 1;
            source += 1;
        }

        if digits == 0 {
            return None;
        }

        // Пропускаем расширения чанка и CRLF.
        while source < end && body[source] != b'\n' {
            source += 1;
        }

        if source >= end {
            return None;
        }

        source += 1;

        if size == 0 {
            return Some(destination);
        }

        if size > end - source {
            return None;
        }

        for _ in 0..size {
            body[destination] = body[source];
            destination += 1;
            source += 1;
        }

        if source < end && body[source] == b'\r' {
            source += 1;
        }

        if source < end && body[source] == b'\n' {
            source += 1;
        }
    }
}

/// Загружает страницу и возвращает её текст без вывода на экран.
///
/// Функция используется и консольным `browse`, и GUI-приложением
/// Perfbrowse. Текст записывается в `out`, ошибки возвращаются статической
/// строкой. Ответ с кодом не 2xx попадает в `out` как строка статуса.
pub fn load(argument: &[u8], out: &mut [u8]) -> Result<usize, &'static [u8]> {
    if !net::ready() {
        return Err(b"no network device");
    }

    if argument.is_empty() {
        return Err(b"usage: browse <http://host[:port]/path>");
    }

    if argument.starts_with(b"https://") {
        return Err(b"HTTPS is not supported yet");
    }

    let Some(url) = parse_url(argument) else {
        return Err(b"invalid URL");
    };

    let address = if let Some(address) = net::parse_ipv4(&url.host[..url.host_len]) {
        address
    } else {
        match dns::resolve(&url.host[..url.host_len]) {
            Some(address) => address,
            None => return Err(b"cannot resolve host"),
        }
    };

    if !tcp::connect(address, url.port) {
        return Err(b"cannot connect");
    }

    let (request, request_length) = build_request(&url);

    if !tcp::send(&request[..request_length]) {
        tcp::close();
        return Err(b"send failed");
    }

    let body = unsafe { &mut *core::ptr::addr_of_mut!(BODY) };
    let mut body_length = 0usize;
    let mut last_second = rtc::second();
    let mut elapsed = 0u32;

    loop {
        net::poll();

        let count = tcp::read(&mut body[body_length..]);
        body_length += count;

        if tcp::peer_closed() || body_length >= body.len() {
            break;
        }

        let now = rtc::second();

        if now != last_second {
            elapsed += 1;
            last_second = now;
        }

        if elapsed >= 10 {
            break;
        }

        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }

    tcp::close();

    if body_length == 0 {
        return Err(b"empty response");
    }

    let Some(header_end) = find_header_end(&body[..body_length]) else {
        return Err(b"malformed HTTP response");
    };

    // Проверяем код ответа в первой строке.
    let status_line_end = body[..header_end]
        .iter()
        .position(|&byte| byte == b'\r')
        .unwrap_or(header_end);
    let status_line = &body[..status_line_end];

    let mut status_code = 0u32;
    let mut spaces = 0usize;

    for &byte in status_line {
        if byte == b' ' {
            spaces += 1;

            if spaces == 2 {
                let mut index = status_line
                    .iter()
                    .position(|&b| b == b' ')
                    .map(|position| position + 1)
                    .unwrap_or(0);

                while index < status_line.len() && status_line[index].is_ascii_digit() {
                    status_code = status_code * 10 + (status_line[index] - b'0') as u32;
                    index += 1;
                }

                break;
            }
        }
    }

    if !(200..300).contains(&status_code) {
        let length = status_line.len().min(out.len());
        out[..length].copy_from_slice(&status_line[..length]);
        return Ok(length);
    }

    let chunked = has_chunked(&body[..header_end]);
    let html_start = header_end + 4;
    let html_end = if chunked {
        decode_chunked(body, html_start, body_length).unwrap_or(body_length)
    } else {
        body_length
    };

    let html = &body[html_start..html_end];

    Ok(render_html(html, out))
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
