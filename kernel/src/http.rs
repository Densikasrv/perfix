//! Минимальный HTTP/1.0-клиент для Perfbrowse и pfpak.
//!
//! Поддерживаются `http://`, `https://`, имена без схемы, DNS A-записи и
//! порты, отличные от 80/443. Ответ читается до закрытия соединения; при
//! `Transfer-Encoding: chunked` тело декодируется на месте.
//!
//! Для `https://` после TCP-соединения выполняется рукопожатие TLS 1.3
//! (модуль [`crate::tls`]) с проверкой цепочки сертификатов до встроенных
//! корневых CA; прикладные данные читаются и пишутся через TLS.

use crate::net;
use crate::net::{dns, tcp};

const DEFAULT_PORT_HTTP: u16 = 80;
const DEFAULT_PORT_HTTPS: u16 = 443;
const DEFAULT_TIMEOUT: u32 = 10;

/// Разобранный URL HTTP-запроса.
struct Url {
    host: [u8; 64],
    host_len: usize,
    port: u16,
    /// Схема `https://`: загрузка идёт по TLS 1.3 с проверкой сертификатов.
    secure: bool,
    path: [u8; 384],
    path_len: usize,
}

impl Url {
    /// Порт по умолчанию для схемы URL.
    fn default_port(&self) -> u16 {
        if self.secure {
            DEFAULT_PORT_HTTPS
        } else {
            DEFAULT_PORT_HTTP
        }
    }
}

/// Успешный ответ сервера.
///
/// `status_start..status_end` — строка состояния в буфере ответа,
/// `body_start..body_end` — тело после заголовков. Для ответов не 2xx
/// тело не декодируется: вызывающая сторона обычно показывает статус.
pub struct Response {
    pub status: u16,
    pub status_start: usize,
    pub status_end: usize,
    pub body_start: usize,
    pub body_end: usize,
}

impl Response {
    pub fn status_line<'a>(&self, buffer: &'a [u8]) -> &'a [u8] {
        &buffer[self.status_start..self.status_end]
    }

    pub fn body<'a>(&self, buffer: &'a [u8]) -> &'a [u8] {
        &buffer[self.body_start..self.body_end]
    }
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

/// Отделяет схему URL (регистр не важен) и сообщает, защищённая ли она.
fn strip_scheme(text: &[u8]) -> (&[u8], bool) {
    for (prefix, secure) in [
        (b"https://".as_slice(), true),
        (b"http://".as_slice(), false),
    ] {
        if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return (&text[prefix.len()..], secure);
        }
    }

    (text, false)
}

fn parse_url(text: &[u8]) -> Option<Url> {
    let (rest, secure) = strip_scheme(text);

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
        port: if secure { DEFAULT_PORT_HTTPS } else { DEFAULT_PORT_HTTP },
        secure,
        path: [0; 384],
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

fn build_request(url: &Url) -> ([u8; 640], usize) {
    let mut request = [0u8; 640];
    let mut length = 0usize;

    push(&mut request, &mut length, b"GET ");
    push(&mut request, &mut length, &url.path[..url.path_len]);
    push(&mut request, &mut length, b" HTTP/1.0\r\nHost: ");
    push(&mut request, &mut length, &url.host[..url.host_len]);

    if url.port != url.default_port() {
        push(&mut request, &mut length, b":");
        push_dec(&mut request, &mut length, url.port as u32);
    }

    push(
        &mut request,
        &mut length,
        b"\r\nUser-Agent: Perfix/0.2.8\r\nConnection: close\r\n\r\n",
    );

    (request, length)
}

fn find_ignore_case(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from >= haystack.len() {
        return None;
    }

    let last = haystack.len() - needle.len();

    for index in from..=last {
        if haystack[index..index + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(index);
        }
    }

    None
}

fn find_header_end(body: &[u8]) -> Option<usize> {
    body.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_status_line(body: &[u8], header_end: usize) -> (usize, usize, u16) {
    let status_line_end = body[..header_end]
        .iter()
        .position(|&byte| byte == b'\r')
        .unwrap_or(header_end);

    let mut status = 0u16;
    let mut spaces = 0usize;

    for &byte in &body[..status_line_end] {
        if byte == b' ' {
            spaces += 1;

            if spaces == 2 {
                let mut index = body[..status_line_end]
                    .iter()
                    .position(|&b| b == b' ')
                    .map(|position| position + 1)
                    .unwrap_or(0);

                while index < status_line_end && body[index].is_ascii_digit() {
                    status = status
                        .saturating_mul(10)
                        .saturating_add((body[index] - b'0') as u16);
                    index += 1;
                }

                break;
            }
        }
    }

    (0, status_line_end, status)
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

/// Декодирует `Transfer-Encoding: chunked` прямо в буфере ответа.
fn decode_chunked(body: &mut [u8], start: usize, end: usize) -> Option<usize> {
    let mut source = start;
    let mut destination = start;

    loop {
        let mut size = 0usize;
        let mut digits = 0usize;

        while source < end {
            if body[source] == b'\r' || body[source] == b'\n' {
                break;
            }

            let digit = match body[source] {
                b';' => break,
                byte => hex_value(byte)?,
            };

            size = size.checked_mul(16)?.checked_add(digit)?;
            digits += 1;
            source += 1;
        }

        if digits == 0 {
            return None;
        }

        while source < end && body[source] != b'\n' {
            source += 1;
        }

        if source < end {
            source += 1;
        }

        if size == 0 {
            return Some(destination);
        }

        if source + size > end {
            return None;
        }

        body.copy_within(source..source + size, destination);
        destination += size;
        source += size;

        while source < end && (body[source] == b'\r' || body[source] == b'\n') {
            source += 1;
        }
    }
}

/// Загружает HTTP-ресурс в `out`.
///
/// Если `allow_truncate` равно `false`, переполнение буфера считается
/// ошибкой: так pfpak не примет обрезанный архив за целый. Таймаут
/// задаётся в секундах; `0` означает значение по умолчанию.
pub fn fetch(
    argument: &[u8],
    out: &mut [u8],
    allow_truncate: bool,
    timeout_seconds: u32,
) -> Result<Response, &'static [u8]> {
    if !net::ready() {
        return Err(b"no network device");
    }

    if argument.is_empty() {
        return Err(b"empty URL");
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

    // Для https:// после TCP-соединения выполняется рукопожатие TLS 1.3
    // с проверкой цепочки сертификатов до встроенного корня.
    if url.secure {
        if let Err(message) = crate::tls::connect(&url.host[..url.host_len]) {
            crate::tls::close();
            return Err(message);
        }
    }

    let (request, request_length) = build_request(&url);

    if url.secure {
        if !crate::tls::send(&request[..request_length]) {
            crate::tls::close();
            return Err(b"send failed");
        }
    } else if !tcp::send(&request[..request_length]) {
        tcp::close();
        return Err(b"send failed");
    }

    let mut body_length = 0usize;
    let timeout = if timeout_seconds == 0 {
        DEFAULT_TIMEOUT
    } else {
        timeout_seconds
    };
    let deadline = crate::timer::deadline(timeout);

    loop {
        net::poll();

        let count = if url.secure {
            crate::tls::read(&mut out[body_length..])
        } else {
            tcp::read(&mut out[body_length..])
        };

        body_length += count;

        let closed = if url.secure {
            crate::tls::peer_closed() || tcp::peer_closed()
        } else {
            tcp::peer_closed()
        };

        if closed {
            break;
        }

        if body_length >= out.len() {
            if allow_truncate {
                break;
            }

            if url.secure {
                crate::tls::close();
            } else {
                tcp::close();
            }

            return Err(b"response is too large");
        }

        if crate::timer::expired(deadline) {
            break;
        }

        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }

    if url.secure {
        crate::tls::close();
    } else {
        tcp::close();
    }

    if body_length == 0 {
        return Err(b"empty response");
    }

    let Some(header_end) = find_header_end(&out[..body_length]) else {
        return Err(b"malformed HTTP response");
    };

    let (status_start, status_end, status) = parse_status_line(&out[..body_length], header_end);
    let mut response = Response {
        status,
        status_start,
        status_end,
        body_start: header_end + 4,
        body_end: body_length,
    };

    if !(200..300).contains(&status) {
        return Ok(response);
    }

    if has_chunked(&out[..header_end]) {
        response.body_end = decode_chunked(out, response.body_start, body_length)
            .ok_or(b"malformed chunked response" as &'static [u8])?;
    }

    Ok(response)
}
