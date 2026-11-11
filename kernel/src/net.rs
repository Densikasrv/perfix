//! Минимальный сетевой стек: Ethernet, ARP, IPv4 и ICMP.
//!
//! Конфигурация статическая и рассчитана на user-mode сеть QEMU:
//! адрес `10.0.2.15/24`, шлюз `10.0.2.2`, DNS `10.0.2.3`. Пока
//! реализованы только ARP и ICMP (команда `ping`); UDP/TCP и DNS
//! появятся вместе с браузером Perfbrowse.

use crate::rtc;
use crate::drivers::net::rtl8139;
use crate::vga;

/// Наш адрес и параметры сети по умолчанию (user-mode network QEMU).
pub const IP: [u8; 4] = [10, 0, 2, 15];
const NETMASK: [u8; 4] = [255, 255, 255, 0];
const GATEWAY: [u8; 4] = [10, 0, 2, 2];
const DNS: [u8; 4] = [10, 0, 2, 3];

const BROADCAST_MAC: [u8; 6] = [0xFF; 6];

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;

const IP_PROTO_ICMP: u8 = 1;

const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY: u8 = 0;

const MAX_FRAME: usize = 1600;

#[derive(Clone, Copy)]
struct ArpEntry {
    ip: [u8; 4],
    mac: [u8; 6],
    valid: bool,
}

impl ArpEntry {
    const EMPTY: Self = Self {
        ip: [0; 4],
        mac: [0; 6],
        valid: false,
    };
}

#[derive(Clone, Copy)]
struct PingReply {
    valid: bool,
    seq: u16,
    from: [u8; 4],
    bytes: u16,
}

impl PingReply {
    const EMPTY: Self = Self {
        valid: false,
        seq: 0,
        from: [0; 4],
        bytes: 0,
    };
}

static mut READY: bool = false;
static mut ARP_CACHE: [ArpEntry; 8] = [ArpEntry::EMPTY; 8];
static mut NEXT_ID: u16 = 0x5000;
static mut PING_ID: u16 = 0;
static mut LAST_REPLY: PingReply = PingReply::EMPTY;

/// Инициализирует сетевую карту.
pub fn init() -> bool {
    let ok = rtl8139::init();

    unsafe {
        READY = ok;
    }

    ok
}

/// Готова ли сеть к работе.
pub fn ready() -> bool {
    unsafe { READY }
}

/// Записывает 16-битное поле в сетевом порядке байтов (big-endian).
fn put_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset] = (value >> 8) as u8;
    buffer[offset + 1] = value as u8;
}

/// Читает 16-битное поле в сетевом порядке байтов (big-endian).
fn get_u16(buffer: &[u8], offset: usize) -> u16 {
    ((buffer[offset] as u16) << 8) | buffer[offset + 1] as u16
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut index = 0usize;

    while index + 1 < data.len() {
        sum += ((data[index] as u32) << 8) | data[index + 1] as u32;
        index += 2;
    }

    if index < data.len() {
        sum += (data[index] as u32) << 8;
    }

    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    !(sum as u16)
}

fn arp_lookup(ip: [u8; 4]) -> Option<[u8; 6]> {
    let cache = unsafe { &*core::ptr::addr_of!(ARP_CACHE) };

    for entry in cache.iter() {
        if entry.valid && entry.ip == ip {
            return Some(entry.mac);
        }
    }

    None
}

fn arp_store(ip: [u8; 4], mac: [u8; 6]) {
    let cache = unsafe { &mut *core::ptr::addr_of_mut!(ARP_CACHE) };

    for entry in cache.iter_mut() {
        if entry.valid && entry.ip == ip {
            entry.mac = mac;
            return;
        }
    }

    for entry in cache.iter_mut() {
        if !entry.valid {
            entry.ip = ip;
            entry.mac = mac;
            entry.valid = true;
            return;
        }
    }

    cache[0].ip = ip;
    cache[0].mac = mac;
}

fn send_arp_request(target: [u8; 4]) {
    let mut frame = [0u8; 60];
    let mac = rtl8139::mac();

    frame[0..6].copy_from_slice(&BROADCAST_MAC);
    frame[6..12].copy_from_slice(&mac);
    put_u16(&mut frame, 12, ETHERTYPE_ARP);

    // ARP: Ethernet/IPv4, запрос.
    put_u16(&mut frame, 14, 1);
    put_u16(&mut frame, 16, ETHERTYPE_IPV4);
    frame[18] = 6;
    frame[19] = 4;
    put_u16(&mut frame, 20, 1);

    frame[22..28].copy_from_slice(&mac);
    frame[28..32].copy_from_slice(&IP);
    // MAC получателя пока неизвестен — нули.
    frame[38..42].copy_from_slice(&target);

    rtl8139::send(&frame[..42]);
}

fn send_arp_reply(target_mac: [u8; 6], target_ip: [u8; 4]) {
    let mut frame = [0u8; 60];
    let mac = rtl8139::mac();

    frame[0..6].copy_from_slice(&target_mac);
    frame[6..12].copy_from_slice(&mac);
    put_u16(&mut frame, 12, ETHERTYPE_ARP);

    put_u16(&mut frame, 14, 1);
    put_u16(&mut frame, 16, ETHERTYPE_IPV4);
    frame[18] = 6;
    frame[19] = 4;
    put_u16(&mut frame, 20, 2);

    frame[22..28].copy_from_slice(&mac);
    frame[28..32].copy_from_slice(&IP);
    frame[32..38].copy_from_slice(&target_mac);
    frame[38..42].copy_from_slice(&target_ip);

    rtl8139::send(&frame[..42]);
}

fn handle_arp(frame: &[u8]) {
    if frame.len() < 42 {
        return;
    }

    let operation = get_u16(frame, 20);
    let mut sender_mac = [0u8; 6];
    let mut sender_ip = [0u8; 4];
    let mut target_ip = [0u8; 4];

    sender_mac.copy_from_slice(&frame[22..28]);
    sender_ip.copy_from_slice(&frame[28..32]);
    target_ip.copy_from_slice(&frame[38..42]);

    if operation == 2 {
        arp_store(sender_ip, sender_mac);
    } else if operation == 1 && target_ip == IP {
        send_arp_reply(sender_mac, sender_ip);
    }
}

fn handle_icmp(payload: &[u8], from: [u8; 4]) {
    if payload.len() < 8 {
        return;
    }

    if payload[0] != ICMP_ECHO_REPLY {
        return;
    }

    let id = get_u16(payload, 4);
    let seq = get_u16(payload, 6);

    if id != unsafe { PING_ID } {
        return;
    }

    unsafe {
        LAST_REPLY.valid = true;
        LAST_REPLY.seq = seq;
        LAST_REPLY.from = from;
        LAST_REPLY.bytes = payload.len() as u16;
    }
}

fn handle_ipv4(frame: &[u8]) {
    if frame.len() < 34 {
        return;
    }

    if frame[14] >> 4 != 4 {
        return;
    }

    let header_length = ((frame[14] & 0x0F) as usize) * 4;

    if header_length < 20 || frame.len() < 14 + header_length {
        return;
    }

    let mut destination = [0u8; 4];
    destination.copy_from_slice(&frame[30..34]);

    if destination != IP {
        return;
    }

    let total_length = get_u16(frame, 16) as usize;
    let available = frame.len() - 14;

    if total_length < header_length || total_length > available {
        return;
    }

    let mut source = [0u8; 4];
    source.copy_from_slice(&frame[26..30]);

    if frame[23] == IP_PROTO_ICMP {
        handle_icmp(&frame[14 + header_length..14 + total_length], source);
    }
}

fn handle_frame(frame: &[u8]) {
    if frame.len() < 14 {
        return;
    }

    match get_u16(frame, 12) {
        ETHERTYPE_ARP => handle_arp(frame),
        ETHERTYPE_IPV4 => handle_ipv4(frame),
        _ => {}
    }
}

/// Обрабатывает все накопившиеся входящие кадры.
pub fn poll() {
    if !ready() {
        return;
    }

    let mut frame = [0u8; MAX_FRAME];

    for _ in 0..16 {
        match rtl8139::recv(&mut frame) {
            Some(length) => handle_frame(&frame[..length]),
            None => break,
        }
    }
}

/// Разрешает IP в MAC через ARP (до двух попыток, тайм-аут ~1 с).
fn arp_resolve(ip: [u8; 4]) -> Option<[u8; 6]> {
    if let Some(mac) = arp_lookup(ip) {
        return Some(mac);
    }

    for _ in 0..2 {
        send_arp_request(ip);

        let start = rtc::second();

        loop {
            poll();

            if let Some(mac) = arp_lookup(ip) {
                return Some(mac);
            }

            if rtc::second() != start {
                break;
            }

            for _ in 0..2000 {
                core::hint::spin_loop();
            }
        }
    }

    None
}

/// Проверяет, лежит ли адрес в нашей подсети.
fn in_subnet(address: [u8; 4]) -> bool {
    let mut index = 0usize;

    while index < 4 {
        if address[index] & NETMASK[index] != IP[index] & NETMASK[index] {
            return false;
        }

        index += 1;
    }

    true
}

/// Формирует и отправляет IPv4-пакет.
///
/// Адреса внутри подсети получают кадр напрямую, остальные — через
/// шлюз: для них MAC назначения в кадре — это MAC шлюза, а IP-адрес
/// получателя в заголовке остаётся исходным.
fn send_ipv4(destination: [u8; 4], protocol: u8, payload: &[u8]) -> bool {
    if payload.len() > 1480 {
        return false;
    }

    let next_hop = if in_subnet(destination) {
        destination
    } else {
        GATEWAY
    };

    let Some(destination_mac) = arp_resolve(next_hop) else {
        return false;
    };

    let source_mac = rtl8139::mac();
    let mut frame = [0u8; MAX_FRAME];

    frame[0..6].copy_from_slice(&destination_mac);
    frame[6..12].copy_from_slice(&source_mac);
    put_u16(&mut frame, 12, ETHERTYPE_IPV4);

    let total = 20 + payload.len();

    frame[14] = 0x45;
    frame[15] = 0;
    put_u16(&mut frame, 16, total as u16);

    let id = unsafe {
        let id = NEXT_ID;
        NEXT_ID = NEXT_ID.wrapping_add(1);
        id
    };

    put_u16(&mut frame, 18, id);
    put_u16(&mut frame, 20, 0);
    frame[22] = 64;
    frame[23] = protocol;
    put_u16(&mut frame, 24, 0);
    frame[26..30].copy_from_slice(&IP);
    frame[30..34].copy_from_slice(&destination);

    let sum = checksum(&frame[14..34]);
    put_u16(&mut frame, 24, sum);

    frame[34..34 + payload.len()].copy_from_slice(payload);

    rtl8139::send(&frame[..34 + payload.len()])
}

fn parse_ipv4(text: &[u8]) -> Option<[u8; 4]> {
    let mut address = [0u8; 4];
    let mut part = 0usize;
    let mut value = 0u32;
    let mut digits = 0usize;

    for &byte in text {
        if byte == b'.' {
            if digits == 0 || part >= 3 {
                return None;
            }

            address[part] = value as u8;
            part += 1;
            value = 0;
            digits = 0;
        } else if byte.is_ascii_digit() {
            value = value * 10 + (byte - b'0') as u32;

            if value > 255 {
                return None;
            }

            digits += 1;
        } else {
            return None;
        }
    }

    if part != 3 || digits == 0 {
        return None;
    }

    address[3] = value as u8;
    Some(address)
}

fn push_bytes(out: &mut [u8; 80], len: &mut usize, bytes: &[u8]) {
    for &byte in bytes {
        if *len >= out.len() {
            return;
        }

        out[*len] = byte;
        *len += 1;
    }
}

fn push_dec(out: &mut [u8; 80], len: &mut usize, value: usize) {
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
        push_bytes(out, len, &[digits[count]]);
    }
}

fn push_ip(out: &mut [u8; 80], len: &mut usize, ip: [u8; 4]) {
    for (index, byte) in ip.iter().enumerate() {
        if index > 0 {
            push_bytes(out, len, b".");
        }

        push_dec(out, len, *byte as usize);
    }
}

fn push_octet(out: &mut [u8; 80], len: &mut usize, value: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    push_bytes(out, len, &[HEX[(value >> 4) as usize], HEX[(value & 0xF) as usize]]);
}

fn push_mac(out: &mut [u8; 80], len: &mut usize, mac: [u8; 6]) {
    for (index, byte) in mac.iter().enumerate() {
        if index > 0 {
            push_bytes(out, len, b":");
        }

        push_octet(out, len, *byte);
    }
}

/// Печатает состояние сети начиная со строки `start_row`.
pub fn print_info(start_row: usize) -> usize {
    if !ready() {
        vga::print_at(start_row, 0, b"Network: no supported NIC found");
        return 1;
    }

    let mut row = start_row;
    let mut line = [b' '; 80];
    let mut len = 0usize;

    push_bytes(&mut line, &mut len, b"Network: RTL8139");
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    line = [b' '; 80];
    len = 0;
    push_bytes(&mut line, &mut len, b"MAC: ");
    push_mac(&mut line, &mut len, rtl8139::mac());
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    line = [b' '; 80];
    len = 0;
    push_bytes(&mut line, &mut len, b"IP: ");
    push_ip(&mut line, &mut len, IP);
    push_bytes(&mut line, &mut len, b"/24");
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    line = [b' '; 80];
    len = 0;
    push_bytes(&mut line, &mut len, b"Gateway: ");
    push_ip(&mut line, &mut len, GATEWAY);
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    line = [b' '; 80];
    len = 0;
    push_bytes(&mut line, &mut len, b"DNS: ");
    push_ip(&mut line, &mut len, DNS);
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    line = [b' '; 80];
    len = 0;

    if rtl8139::link_up() {
        push_bytes(&mut line, &mut len, b"Link: up");

        if rtl8139::link_is_10mbit() {
            push_bytes(&mut line, &mut len, b", 10 Mbps");
        } else {
            push_bytes(&mut line, &mut len, b", 100 Mbps");
        }
    } else {
        push_bytes(&mut line, &mut len, b"Link: down");
    }

    vga::print_at(row, 0, &line[..len]);
    row += 1;

    let (rx_ok, rx_bad, tx_ok, tx_fail) = rtl8139::stats();

    line = [b' '; 80];
    len = 0;
    push_bytes(&mut line, &mut len, b"RX: ");
    push_dec(&mut line, &mut len, rx_ok as usize);
    push_bytes(&mut line, &mut len, b" ok, ");
    push_dec(&mut line, &mut len, rx_bad as usize);
    push_bytes(&mut line, &mut len, b" errors");
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    line = [b' '; 80];
    len = 0;
    push_bytes(&mut line, &mut len, b"TX: ");
    push_dec(&mut line, &mut len, tx_ok as usize);
    push_bytes(&mut line, &mut len, b" ok, ");
    push_dec(&mut line, &mut len, tx_fail as usize);
    push_bytes(&mut line, &mut len, b" errors");
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    row - start_row
}

/// Отправляет четыре ICMP-эхо и печатает результат.
pub fn ping(argument: &[u8], start_row: usize) -> usize {
    if !ready() {
        vga::print_at(start_row, 0, b"ping: no network device");
        return 1;
    }

    let Some(destination) = parse_ipv4(argument) else {
        vga::print_at(start_row, 0, b"Usage: ping <a.b.c.d>");
        return 1;
    };

    let id = 0x5046; // 'P','F'
    unsafe {
        PING_ID = id;
    }

    let mut row = start_row;
    let mut line = [b' '; 80];
    let mut len = 0usize;

    push_bytes(&mut line, &mut len, b"PING ");
    push_ip(&mut line, &mut len, destination);
    push_bytes(&mut line, &mut len, b" with 32 bytes of data:");
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    let mut sent = 0usize;
    let mut received = 0usize;

    for seq in 1..=4u16 {
        sent += 1;

        let mut request = [0u8; 40];
        request[0] = ICMP_ECHO_REQUEST;
        request[1] = 0;
        // Контрольная сумма (сначала обнулена).
        request[4] = (id >> 8) as u8;
        request[5] = id as u8;
        request[6] = (seq >> 8) as u8;
        request[7] = seq as u8;

        for (index, byte) in request.iter_mut().enumerate().skip(8) {
            *byte = index as u8;
        }

        let sum = checksum(&request);
        put_u16(&mut request, 2, sum);

        unsafe {
            LAST_REPLY.valid = false;
        }

        let sent_ok = send_ipv4(destination, IP_PROTO_ICMP, &request);

        let mut reply: Option<PingReply> = None;

        if sent_ok {
            let mut last_second = rtc::second();
            let mut elapsed = 0u32;

            loop {
                poll();

                let current = unsafe { LAST_REPLY };

                if current.valid && current.seq == seq {
                    reply = Some(current);
                    break;
                }

                // Внешние адреса в user-mode сети QEMU (slirp) могут
                // отвечать с задержкой, поэтому ждём не один, а два
                // тика RTC: иначе ответ приходит уже после таймаута.
                let now = rtc::second();

                if now != last_second {
                    elapsed += 1;
                    last_second = now;
                }

                if elapsed >= 2 {
                    break;
                }

                for _ in 0..2000 {
                    core::hint::spin_loop();
                }
            }
        }

        line = [b' '; 80];
        len = 0;

        if let Some(reply) = reply {
            received += 1;
            push_bytes(&mut line, &mut len, b"Reply from ");
            push_ip(&mut line, &mut len, reply.from);
            push_bytes(&mut line, &mut len, b": bytes=");
            push_dec(&mut line, &mut len, reply.bytes as usize);
            push_bytes(&mut line, &mut len, b" seq=");
            push_dec(&mut line, &mut len, seq as usize);
        } else {
            push_bytes(&mut line, &mut len, b"Request timed out.");
        }

        vga::print_at(row, 0, &line[..len]);
        row += 1;
    }

    line = [b' '; 80];
    len = 0;
    push_dec(&mut line, &mut len, sent);
    push_bytes(&mut line, &mut len, b" packets transmitted, ");
    push_dec(&mut line, &mut len, received);
    push_bytes(&mut line, &mut len, b" received");
    vga::print_at(row, 0, &line[..len]);
    row += 1;

    row - start_row
}
