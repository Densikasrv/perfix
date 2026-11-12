//! DNS: разрешение имён через A-записи.
//!
//! Используется DNS-сервер из user-mode сети QEMU (`10.0.2.3`).
//! Поддерживается только один запрос одновременно — этого достаточно
//! для командной строки и браузера.

use super::{get_u16, put_u16};
use crate::rtc;

const DNS_PORT: u16 = 53;
const LOCAL_PORT: u16 = 53000;

static mut PENDING_ID: u16 = 0;
static mut NEXT_ID: u16 = 0x5000;
static mut RESULT: Option<[u8; 4]> = None;

fn skip_name(data: &[u8], mut offset: usize) -> Option<usize> {
    loop {
        let byte = *data.get(offset)?;

        if byte == 0 {
            return Some(offset + 1);
        }

        // Сжатое имя: указатель из двух байт.
        if byte & 0xC0 == 0xC0 {
            return Some(offset + 2);
        }

        if byte & 0xC0 != 0 {
            return None;
        }

        offset += 1 + byte as usize;
    }
}

/// Разбирает ответ DNS и сохраняет первый A-адрес.
pub fn handle(_source_ip: [u8; 4], source_port: u16, _destination_port: u16, data: &[u8]) {
    if source_port != DNS_PORT || data.len() < 12 {
        return;
    }

    if get_u16(data, 0) != unsafe { PENDING_ID } {
        return;
    }

    let flags = get_u16(data, 2);

    // Должен быть ответом и без ошибки.
    if flags & 0x8000 == 0 || flags & 0x000F != 0 {
        return;
    }

    let questions = get_u16(data, 4) as usize;
    let answers = get_u16(data, 6) as usize;

    let mut offset = 12;

    for _ in 0..questions {
        offset = match skip_name(data, offset) {
            Some(next) => next,
            None => return,
        };
        offset += 4;
    }

    for _ in 0..answers {
        offset = match skip_name(data, offset) {
            Some(next) => next,
            None => return,
        };

        if offset + 10 > data.len() {
            return;
        }

        let kind = get_u16(data, offset);
        let class = get_u16(data, offset + 2);
        let length = get_u16(data, offset + 8) as usize;
        offset += 10;

        if offset + length > data.len() {
            return;
        }

        if kind == 1 && class == 1 && length == 4 {
            let mut address = [0u8; 4];
            address.copy_from_slice(&data[offset..offset + 4]);

            unsafe {
                RESULT = Some(address);
            }

            return;
        }

        offset += length;
    }
}

/// Разрешает имя в IPv4-адрес (до трёх секунд ожидания).
pub fn resolve(host: &[u8]) -> Option<[u8; 4]> {
    if host.is_empty() || host.len() > 200 {
        return None;
    }

    let mut query = [0u8; 260];
    let id = unsafe {
        let id = NEXT_ID;
        NEXT_ID = NEXT_ID.wrapping_add(1);
        id
    };

    put_u16(&mut query, 0, id);
    put_u16(&mut query, 2, 0x0100); // стандартный запрос, рекурсия нужна
    put_u16(&mut query, 4, 1);

    let mut length = 12usize;

    for label in host.split(|&byte| byte == b'.') {
        if label.is_empty() || label.len() > 63 || length + label.len() + 6 > query.len() {
            return None;
        }

        query[length] = label.len() as u8;
        length += 1;
        query[length..length + label.len()].copy_from_slice(label);
        length += label.len();
    }

    query[length] = 0;
    length += 1;
    put_u16(&mut query, length, 1); // A
    length += 2;
    put_u16(&mut query, length, 1); // IN
    length += 2;

    unsafe {
        PENDING_ID = id;
        RESULT = None;
    }

    if !super::udp::send(super::DNS, LOCAL_PORT, DNS_PORT, &query[..length]) {
        return None;
    }

    let mut last_second = rtc::second();
    let mut elapsed = 0u32;

    loop {
        super::poll();

        if let Some(address) = unsafe { RESULT } {
            return Some(address);
        }

        let now = rtc::second();

        if now != last_second {
            elapsed += 1;
            last_second = now;
        }

        if elapsed >= 3 {
            return None;
        }

        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}
