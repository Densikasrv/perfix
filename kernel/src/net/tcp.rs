//! TCP: одно соединение, достаточное для HTTP-запроса.
//!
//! Реализованы рукопожатие, передача данных, приём и закрытие. Повторная
//! передача не выполняется: в user-mode сети QEMU потери практически
//! исключены, а таймауты ограничивают ожидание.

use super::{get_u16, put_u16, send_ipv4};
use crate::rtc;
use crate::timer;

const IP_PROTO_TCP: u8 = 6;

const FLAG_FIN: u8 = 0x01;
const FLAG_SYN: u8 = 0x02;
const FLAG_RST: u8 = 0x04;
const FLAG_PSH: u8 = 0x08;
const FLAG_ACK: u8 = 0x10;

const STATE_CLOSED: u8 = 0;
const STATE_SYN_SENT: u8 = 1;
const STATE_ESTABLISHED: u8 = 2;
const STATE_FIN_WAIT: u8 = 3;
const STATE_CLOSED_BY_PEER: u8 = 4;

const RX_CAPACITY: usize = 32 * 1024;
const MAX_SEGMENT: usize = 1400;

static mut LOCAL_PORT: u16 = 0;
static mut REMOTE_IP: [u8; 4] = [0; 4];
static mut REMOTE_PORT: u16 = 0;
static mut SND_NXT: u32 = 0;
static mut RCV_NXT: u32 = 0;
static mut STATE: u8 = STATE_CLOSED;
static mut PEER_CLOSED: bool = false;
static mut NEXT_PORT: u16 = 49152;

static mut RX_BUFFER: [u8; RX_CAPACITY] = [0; RX_CAPACITY];
static mut RX_LENGTH: usize = 0;

fn get_u32(buffer: &[u8], offset: usize) -> u32 {
    ((buffer[offset] as u32) << 24)
        | ((buffer[offset + 1] as u32) << 16)
        | ((buffer[offset + 2] as u32) << 8)
        | buffer[offset + 3] as u32
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset] = (value >> 24) as u8;
    buffer[offset + 1] = (value >> 16) as u8;
    buffer[offset + 2] = (value >> 8) as u8;
    buffer[offset + 3] = value as u8;
}

fn checksum(source: [u8; 4], destination: [u8; 4], segment: &[u8]) -> u16 {
    let mut sum = 0u32;

    sum += ((source[0] as u32) << 8) | source[1] as u32;
    sum += ((source[2] as u32) << 8) | source[3] as u32;
    sum += ((destination[0] as u32) << 8) | destination[1] as u32;
    sum += ((destination[2] as u32) << 8) | destination[3] as u32;
    sum += IP_PROTO_TCP as u32;
    sum += segment.len() as u32;

    let mut index = 0usize;

    while index + 1 < segment.len() {
        sum += ((segment[index] as u32) << 8) | segment[index + 1] as u32;
        index += 2;
    }

    if index < segment.len() {
        sum += (segment[index] as u32) << 8;
    }

    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    !(sum as u16)
}

fn send_segment(flags: u8, payload: &[u8]) -> bool {
    let local_port = unsafe { LOCAL_PORT };

    if local_port == 0 || payload.len() > MAX_SEGMENT {
        return false;
    }

    let mut segment = [0u8; MAX_SEGMENT + 20];
    let length = 20 + payload.len();

    put_u16(&mut segment, 0, local_port);
    put_u16(&mut segment, 2, unsafe { REMOTE_PORT });
    put_u32(&mut segment, 4, unsafe { SND_NXT });
    put_u32(&mut segment, 8, unsafe { RCV_NXT });
    segment[12] = 5 << 4;
    segment[13] = flags;
    put_u16(&mut segment, 14, 64240);
    put_u16(&mut segment, 16, 0);
    put_u16(&mut segment, 18, 0);
    segment[20..length].copy_from_slice(payload);

    let sum = checksum(super::IP, unsafe { REMOTE_IP }, &segment[..length]);
    put_u16(&mut segment, 16, sum);

    send_ipv4(unsafe { REMOTE_IP }, IP_PROTO_TCP, &segment[..length])
}

fn send_ack() {
    send_segment(FLAG_ACK, &[]);
}

fn append_received(data: &[u8]) -> usize {
    let length = unsafe { RX_LENGTH };
    let free = RX_CAPACITY - length;
    let count = data.len().min(free);

    if count == 0 {
        return 0;
    }

    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(RX_BUFFER) };
    buffer[length..length + count].copy_from_slice(&data[..count]);

    unsafe {
        RX_LENGTH = length + count;
    }

    count
}

/// Разбирает входящий TCP-сегмент.
pub fn handle(payload: &[u8], source_ip: [u8; 4]) {
    if payload.len() < 20 {
        return;
    }

    let local_port = unsafe { LOCAL_PORT };

    if local_port == 0 || get_u16(payload, 2) != local_port {
        return;
    }

    if source_ip != unsafe { REMOTE_IP } || get_u16(payload, 0) != unsafe { REMOTE_PORT } {
        return;
    }

    let sequence = get_u32(payload, 4);
    let acknowledgement = get_u32(payload, 8);
    let header_length = ((payload[12] >> 4) as usize) * 4;

    if header_length < 20 || header_length > payload.len() {
        return;
    }

    let flags = payload[13];
    let data = &payload[header_length..];
    let state = unsafe { STATE };

    if flags & FLAG_RST != 0 {
        unsafe {
            STATE = STATE_CLOSED_BY_PEER;
            PEER_CLOSED = true;
        }
        return;
    }

    if state == STATE_SYN_SENT && flags & FLAG_SYN != 0 && flags & FLAG_ACK != 0 {
        if acknowledgement != unsafe { SND_NXT } {
            return;
        }

        unsafe {
            RCV_NXT = sequence.wrapping_add(1);
            STATE = STATE_ESTABLISHED;
        }

        send_ack();
        return;
    }

    if state != STATE_ESTABLISHED && state != STATE_FIN_WAIT && state != STATE_CLOSED_BY_PEER {
        return;
    }

    let expected = unsafe { RCV_NXT };

    if !data.is_empty() {
        if sequence == expected {
            let copied = append_received(data);

            if copied == data.len() {
                unsafe {
                    RCV_NXT = expected.wrapping_add(copied as u32);
                }
            }

            send_ack();
        } else {
            // Дубликат или пакет не по порядку — подтверждаем то, что ждём.
            send_ack();
        }
    }

    if flags & FLAG_FIN != 0 {
        let boundary = sequence.wrapping_add(data.len() as u32);

        if boundary == unsafe { RCV_NXT } {
            unsafe {
                RCV_NXT = RCV_NXT.wrapping_add(1);
                STATE = STATE_CLOSED_BY_PEER;
                PEER_CLOSED = true;
            }
        }

        send_ack();
    }
}

/// Устанавливает TCP-соединение (до трёх секунд ожидания).
pub fn connect(destination: [u8; 4], port: u16) -> bool {
    let local_port = unsafe {
        let port = NEXT_PORT;
        NEXT_PORT = if NEXT_PORT >= 60000 {
            49152
        } else {
            NEXT_PORT + 1
        };
        port
    };

    let initial = (timer::ticks() as u32) ^ 0x5046_0000;

    unsafe {
        LOCAL_PORT = local_port;
        REMOTE_IP = destination;
        REMOTE_PORT = port;
        SND_NXT = initial;
        RCV_NXT = 0;
        STATE = STATE_SYN_SENT;
        PEER_CLOSED = false;
        RX_LENGTH = 0;
    }

    if !send_segment(FLAG_SYN, &[]) {
        unsafe {
            STATE = STATE_CLOSED;
            LOCAL_PORT = 0;
        }
        return false;
    }

    unsafe {
        SND_NXT = SND_NXT.wrapping_add(1);
    }

    let mut last_second = rtc::second();
    let mut elapsed = 0u32;

    loop {
        super::poll();

        if unsafe { STATE } == STATE_ESTABLISHED {
            return true;
        }

        if unsafe { STATE } == STATE_CLOSED_BY_PEER || unsafe { STATE } == STATE_CLOSED {
            return false;
        }

        let now = rtc::second();

        if now != last_second {
            elapsed += 1;
            last_second = now;
        }

        if elapsed >= 3 {
            unsafe {
                STATE = STATE_CLOSED;
            }
            return false;
        }

        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

/// Отправляет данные в установленном соединении.
pub fn send(data: &[u8]) -> bool {
    if unsafe { STATE } != STATE_ESTABLISHED {
        return false;
    }

    let mut offset = 0usize;

    while offset < data.len() {
        let end = (offset + MAX_SEGMENT).min(data.len());
        let chunk = &data[offset..end];

        if !send_segment(FLAG_PSH | FLAG_ACK, chunk) {
            return false;
        }

        unsafe {
            SND_NXT = SND_NXT.wrapping_add(chunk.len() as u32);
        }

        offset = end;
    }

    true
}

/// Забирает принятые данные из внутреннего буфера.
pub fn read(out: &mut [u8]) -> usize {
    let length = unsafe { RX_LENGTH };
    let count = out.len().min(length);

    if count == 0 {
        return 0;
    }

    let buffer = unsafe { &*core::ptr::addr_of!(RX_BUFFER) };
    out[..count].copy_from_slice(&buffer[..count]);

    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(RX_BUFFER) };
    buffer.copy_within(count..length, 0);

    unsafe {
        RX_LENGTH = length - count;
    }

    count
}

/// Закрыт ли поток со стороны сервера.
pub fn peer_closed() -> bool {
    unsafe { PEER_CLOSED }
}

/// Корректно закрывает соединение.
pub fn close() {
    if unsafe { STATE } == STATE_ESTABLISHED {
        send_segment(FLAG_FIN | FLAG_ACK, &[]);

        unsafe {
            SND_NXT = SND_NXT.wrapping_add(1);
            STATE = STATE_FIN_WAIT;
        }
    }

    unsafe {
        LOCAL_PORT = 0;
        STATE = STATE_CLOSED;
    }
}
