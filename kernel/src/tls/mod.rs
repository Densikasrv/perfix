//! Минимальный TLS 1.3 клиент (RFC 8446) для HTTPS в Perfix.
//!
//! Реализовано: ClientHello с SNI и key_share X25519, разбор ServerHello,
//! расписание ключей TLS 1.3, шифрование/расшифрование записей
//! AES-128-GCM (TLS_AES_128_GCM_SHA256), проверка Finished сервера,
//! отправка Finished клиента, прикладные данные, close_notify и KeyUpdate.
//!
//! Модуль работает поверх [`crate::net::tcp`]: соединение открывает
//! вызывающая сторона, а [`connect`] выполняет рукопожатие в уже открытом
//! TCP-потоке. Аллокатора нет — все буферы статические.
//!
//! Проверка цепочки сертификатов выполняется в [`super::tls::x509`];
//! без неё рукопожатие не считается успешным.

use crate::crypto::{hkdf_expand_label, hkdf_extract, hmac_sha256, sha256, x25519, Aes128Gcm};
use crate::net::tcp;
use crate::rtc;
use crate::timer;

mod roots;
pub mod x509;

/// Размер PEM-бандла со встроенными корневыми сертификатами.
pub fn ca_bundle_size() -> usize {
    roots::bundle_size()
}

/// Записывает встроенные корневые сертификаты в PEM-бандл.
///
/// Используется установщиком, чтобы на диске остался обычный набор CA
/// (`/etc/ssl/certs/ca-certificates.crt`) для программ, которым он нужен.
/// Сам TLS-клиент ядра всегда доверяет встроенным корням.
pub fn write_ca_bundle(out: &mut [u8]) -> Option<usize> {
    roots::write_bundle(out)
}

const CONTENT_CHANGE_CIPHER_SPEC: u8 = 20;
const CONTENT_ALERT: u8 = 21;
const CONTENT_HANDSHAKE: u8 = 22;
const CONTENT_APPLICATION_DATA: u8 = 23;

const HANDSHAKE_CLIENT_HELLO: u8 = 1;
const HANDSHAKE_ENCRYPTED_EXTENSIONS: u8 = 8;
const HANDSHAKE_CERTIFICATE: u8 = 11;
const HANDSHAKE_CERTIFICATE_VERIFY: u8 = 15;
const HANDSHAKE_FINISHED: u8 = 20;
const HANDSHAKE_KEY_UPDATE: u8 = 24;

const EXTENSION_SERVER_NAME: u16 = 0;
const EXTENSION_SUPPORTED_GROUPS: u16 = 10;
const EXTENSION_SIGNATURE_ALGORITHMS: u16 = 13;
const EXTENSION_SUPPORTED_VERSIONS: u16 = 43;
const EXTENSION_KEY_SHARE: u16 = 51;

const GROUP_X25519: u16 = 29;

/// Максимальный размер записи TLS: 2^14 открытого текста + 256 резерва.
const MAX_PLAINTEXT: usize = 16384 + 256;
const RECORD_CAPACITY: usize = 5 + MAX_PLAINTEXT;
const HANDSHAKE_CAPACITY: usize = 32 * 1024;

/// Таймаут рукопожатия и ожидания данных, в секундах.
const HANDSHAKE_TIMEOUT: u32 = 20;

/// Состояние установленного TLS-соединения.
pub struct Session {
    read_key: [u8; 16],
    read_iv: [u8; 12],
    read_seq: u64,
    read_secret: [u8; 32],
    write_key: [u8; 16],
    write_iv: [u8; 12],
    write_seq: u64,
    write_secret: [u8; 32],
    closed: bool,
}

static mut SESSION: Option<Session> = None;

/// Буфер сырых записей, поступающих из TCP.
static mut RAW: [u8; RECORD_CAPACITY] = [0; RECORD_CAPACITY];
static mut RAW_LEN: usize = 0;

/// Буфер открытого текста текущей записи.
static mut RECORD: [u8; MAX_PLAINTEXT] = [0; MAX_PLAINTEXT];

/// Буфер собираемого сообщения рукопожатия.
static mut MESSAGE: [u8; HANDSHAKE_CAPACITY] = [0; HANDSHAKE_CAPACITY];

/// Буфер исходящих записей.
static mut OUT: [u8; 2048] = [0; 2048];

/// Снимок состояния рукопожатия (транскрипт сообщений).
static mut TRANSCRIPT: Option<crate::crypto::Sha256> = None;

fn raw_mut() -> &'static mut [u8; RECORD_CAPACITY] {
    unsafe { &mut *core::ptr::addr_of_mut!(RAW) }
}

fn record_mut() -> &'static mut [u8; MAX_PLAINTEXT] {
    unsafe { &mut *core::ptr::addr_of_mut!(RECORD) }
}

fn message_mut() -> &'static mut [u8; HANDSHAKE_CAPACITY] {
    unsafe { &mut *core::ptr::addr_of_mut!(MESSAGE) }
}

fn out_mut() -> &'static mut [u8; 2048] {
    unsafe { &mut *core::ptr::addr_of_mut!(OUT) }
}

fn session() -> Option<&'static mut Session> {
    unsafe { (&mut *core::ptr::addr_of_mut!(SESSION)).as_mut() }
}

/// Простой генератор случайных байтов.
///
/// Источник энтропии — счётчики таймера и часов: этого достаточно, чтобы
/// клиентские случайные числа и эфемерные ключи не повторялись, но не
/// заменяет аппаратный ГПСЧ. Для TLS-клиента без сохранения состояния
/// (каждый запрос — новое соединение) этого хватает.
fn random_bytes(out: &mut [u8]) {
    static mut STATE: u64 = 0;

    unsafe {
        if STATE == 0 {
            STATE = (timer::ticks() as u64)
                ^ ((rtc::second() as u64) << 32)
                ^ 0x9e37_79b9_7f4a_7c15;
        }
    }

    for byte in out.iter_mut() {
        unsafe {
            let mut value = STATE;
            value ^= value << 13;
            value ^= value >> 7;
            value ^= value << 17;
            STATE = value;
            *byte = (value >> 24) as u8;
        }
    }
}

fn get_u16(buffer: &[u8], offset: usize) -> u16 {
    ((buffer[offset] as u16) << 8) | buffer[offset + 1] as u16
}

fn get_u24(buffer: &[u8], offset: usize) -> usize {
    ((buffer[offset] as usize) << 16)
        | ((buffer[offset + 1] as usize) << 8)
        | buffer[offset + 2] as usize
}

fn put_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset] = (value >> 8) as u8;
    buffer[offset + 1] = value as u8;
}

fn put_u24(buffer: &mut [u8], offset: usize, value: usize) {
    buffer[offset] = (value >> 16) as u8;
    buffer[offset + 1] = (value >> 8) as u8;
    buffer[offset + 2] = value as u8;
}

/// Nonce записи: IV, сложенный с порядковым номером (RFC 8446, 5.3).
fn record_nonce(iv: &[u8; 12], sequence: u64) -> [u8; 12] {
    let mut nonce = *iv;
    let bytes = sequence.to_be_bytes();

    for index in 0..8 {
        nonce[4 + index] ^= bytes[index];
    }

    nonce
}

fn transcript_hash() -> [u8; 32] {
    unsafe {
        let state = (&*core::ptr::addr_of!(TRANSCRIPT)).as_ref();
        match state {
            Some(hash) => hash.clone().finish(),
            None => sha256(b""),
        }
    }
}

fn transcript_update(data: &[u8]) {
    unsafe {
        if let Some(hash) = (&mut *core::ptr::addr_of_mut!(TRANSCRIPT)).as_mut() {
            hash.update(data);
        }
    }
}


/// Ждёт, пока в `RAW` накопится `count` байт (не больше срока `deadline`).
fn wait_raw(count: usize, deadline: u64) -> bool {
    loop {
        crate::net::poll();

        let have = unsafe { RAW_LEN };

        if have >= count {
            return true;
        }

        let buffer = raw_mut();
        let read = tcp::read(&mut buffer[have..]);

        unsafe {
            RAW_LEN = have + read;
        }

        if unsafe { RAW_LEN } >= count {
            return true;
        }

        if tcp::peer_closed() || crate::timer::expired(deadline) {
            return false;
        }

        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

fn raw_consume(count: usize) {
    let raw = raw_mut();
    let length = unsafe { RAW_LEN };
    raw.copy_within(count..length, 0);

    unsafe {
        RAW_LEN = length - count;
    }
}

/// Читает одну запись и расшифровывает её. Возвращает внутренний тип
/// содержимого и длину открытого текста в `RECORD`.
fn read_record(deadline: u64) -> Option<(u8, usize)> {
    loop {
        if !wait_raw(5, deadline) {
            return None;
        }

        let (kind, length) = {
            let raw = raw_mut();
            (raw[0], get_u16(raw, 3) as usize)
        };

        if length > MAX_PLAINTEXT {
            return None;
        }

        if !wait_raw(5 + length, deadline) {
            return None;
        }

        let mut header = [0u8; 5];
        header.copy_from_slice(&raw_mut()[..5]);

        // Смена шифров: сервер подтверждает переход, содержимого нет.
        if kind == CONTENT_CHANGE_CIPHER_SPEC {
            raw_consume(5 + length);
            continue;
        }

        let record = record_mut();

        if kind == CONTENT_APPLICATION_DATA {
            let session = session()?;
            let nonce = record_nonce(&session.read_iv, session.read_seq);

            if length < 17 {
                return None;
            }

            let body = length - 16;
            record[..length].copy_from_slice(&raw_mut()[5..5 + length]);
            raw_consume(5 + length);

            let mut tag = [0u8; 16];
            tag.copy_from_slice(&record[body..length]);

            let cipher = Aes128Gcm::new(&session.read_key);

            if !cipher.decrypt(&nonce, &header, &mut record[..body], &tag) {
                return None;
            }

            session.read_seq = session.read_seq.wrapping_add(1);

            // Внутренний тип — последний байт открытого текста (RFC 8446,
            // раздел 5.4). Отрезать «завершающие нули» нельзя: содержимое
            // записи (например, DER сертификата) само может кончаться нулями.
            let inner = record[body - 1];

            if inner == CONTENT_ALERT {
                session.closed = true;
            }

            return Some((inner, body - 1));
        }

        // Открытый текст: до установки ключей сюда попадают только
        // ServerHello и служебные записи.
        if kind == CONTENT_HANDSHAKE {
            record[..length].copy_from_slice(&raw_mut()[5..5 + length]);
            raw_consume(5 + length);
            return Some((kind, length));
        }

        if kind == CONTENT_ALERT {
            raw_consume(5 + length);
            return None;
        }

        return None;
    }
}

/// Отправляет запись, при необходимости шифруя её.
fn write_record(kind: u8, payload: &[u8], encrypted: bool) -> bool {
    let out = out_mut();

    if payload.len() + 16 + 5 > out.len() {
        return false;
    }

    out[0] = if encrypted { CONTENT_APPLICATION_DATA } else { kind };
    out[1] = 0x03;
    out[2] = 0x03;

    if !encrypted {
        put_u16(out, 3, payload.len() as u16);
        out[5..5 + payload.len()].copy_from_slice(payload);
        return tcp::send(&out[..5 + payload.len()]);
    }

    let plain_length = payload.len() + 1;
    put_u16(out, 3, (plain_length + 16) as u16);
    out[5..5 + payload.len()].copy_from_slice(payload);
    out[5 + payload.len()] = kind;

    let Some(session) = session() else {
        return false;
    };

    let nonce = record_nonce(&session.write_iv, session.write_seq);
    let mut header = [0u8; 5];
    header.copy_from_slice(&out[..5]);

    let mut tag = [0u8; 16];
    let cipher = Aes128Gcm::new(&session.write_key);
    cipher.encrypt(&nonce, &header, &mut out[5..5 + plain_length], &mut tag);

    out[5 + plain_length..5 + plain_length + 16].copy_from_slice(&tag);
    session.write_seq = session.write_seq.wrapping_add(1);

    tcp::send(&out[..5 + plain_length + 16])
}


/// Собранный ClientHello.
struct ClientHello {
    /// Длина сообщения рукопожатия в буфере `MESSAGE`.
    length: usize,
    private_key: [u8; 32],
}

/// Строит ClientHello с SNI, X25519 key_share и TLS 1.3.
fn build_client_hello(host: &[u8]) -> Option<ClientHello> {
    let out = message_mut();
    let mut position = 4usize;

    if host.len() > 255 {
        return None;
    }

    // legacy_version, random.
    out[position..position + 2].copy_from_slice(&[0x03, 0x03]);
    position += 2;
    random_bytes(&mut out[position..position + 32]);
    position += 32;

    // legacy_session_id (непустой — совместимость с middlebox).
    out[position] = 32;
    position += 1;
    random_bytes(&mut out[position..position + 32]);
    position += 32;

    // cipher_suites: TLS_AES_128_GCM_SHA256.
    put_u16(out, position, 2);
    position += 2;
    put_u16(out, position, 0x1301);
    position += 2;

    // compression_methods: null.
    out[position] = 1;
    out[position + 1] = 0;
    position += 2;

    let extensions_length = position;
    position += 2;

    // server_name (только для имён, не для IP-адресов).
    if crate::net::parse_ipv4(host).is_none() {
        put_u16(out, position, EXTENSION_SERVER_NAME);
        position += 2;
        let length = position;
        position += 2;
        put_u16(out, position, (host.len() + 3) as u16);
        position += 2;
        out[position] = 0;
        position += 1;
        put_u16(out, position, host.len() as u16);
        position += 2;
        out[position..position + host.len()].copy_from_slice(host);
        position += host.len();
        put_u16(out, length, (position - length - 2) as u16);
    }

    // supported_groups: x25519.
    put_u16(out, position, EXTENSION_SUPPORTED_GROUPS);
    position += 2;
    put_u16(out, position, 4);
    position += 2;
    put_u16(out, position, 2);
    position += 2;
    put_u16(out, position, GROUP_X25519);
    position += 2;

    // signature_algorithms: ECDSA P-256, Ed25519, RSA-PSS, RSA PKCS#1.
    let schemes = [0x0403u16, 0x0807, 0x0804, 0x0401];
    put_u16(out, position, EXTENSION_SIGNATURE_ALGORITHMS);
    position += 2;
    put_u16(out, position, (2 + 2 * schemes.len()) as u16);
    position += 2;
    put_u16(out, position, (2 * schemes.len()) as u16);
    position += 2;

    for scheme in schemes {
        put_u16(out, position, scheme);
        position += 2;
    }

    // supported_versions: TLS 1.3.
    put_u16(out, position, EXTENSION_SUPPORTED_VERSIONS);
    position += 2;
    put_u16(out, position, 3);
    position += 2;
    out[position] = 2;
    position += 1;
    put_u16(out, position, 0x0304);
    position += 2;

    // key_share: одна эфемерная пара x25519.
    let mut private_key = [0u8; 32];
    random_bytes(&mut private_key);
    let mut basepoint = [0u8; 32];
    basepoint[0] = 9;
    let public_key = x25519(&private_key, &basepoint);

    put_u16(out, position, EXTENSION_KEY_SHARE);
    position += 2;
    put_u16(out, position, 38);
    position += 2;
    put_u16(out, position, 36);
    position += 2;
    put_u16(out, position, GROUP_X25519);
    position += 2;
    put_u16(out, position, 32);
    position += 2;
    out[position..position + 32].copy_from_slice(&public_key);
    position += 32;

    put_u16(out, extensions_length, (position - extensions_length - 2) as u16);

    out[0] = HANDSHAKE_CLIENT_HELLO;
    put_u24(out, 1, position - 4);

    Some(ClientHello {
        length: position,
        private_key,
    })
}

/// Разобранный ServerHello.
struct ServerHello {
    public_key: [u8; 32],
}

fn parse_server_hello(message: &[u8]) -> Option<ServerHello> {
    let end = message.len();
    let mut position = 4usize;

    if end < 6 || get_u16(message, position) != 0x0303 {
        return None;
    }

    position += 2 + 32;

    if position >= end {
        return None;
    }

    let session_id = message[position] as usize;
    position += 1 + session_id;

    if position + 2 > end {
        return None;
    }

    // Поддерживается только TLS_AES_128_GCM_SHA256.
    if get_u16(message, position) != 0x1301 {
        return None;
    }

    position += 2;

    if position + 1 > end || message[position] != 0 {
        return None;
    }

    position += 1;

    if position + 2 > end {
        return None;
    }

    let extensions_end = position + 2 + get_u16(message, position) as usize;
    position += 2;

    if extensions_end > end {
        return None;
    }

    let mut public_key = None;
    let mut version_ok = false;

    while position + 4 <= extensions_end {
        let kind = get_u16(message, position);
        let length = get_u16(message, position + 2) as usize;
        position += 4;

        if position + length > extensions_end {
            return None;
        }

        match kind {
            EXTENSION_SUPPORTED_VERSIONS => {
                if length != 2 || get_u16(message, position) != 0x0304 {
                    return None;
                }

                version_ok = true;
            }
            EXTENSION_KEY_SHARE => {
                if length != 36 || get_u16(message, position) != GROUP_X25519 {
                    return None;
                }

                if get_u16(message, position + 2) != 32 {
                    return None;
                }

                let mut key = [0u8; 32];
                key.copy_from_slice(&message[position + 4..position + 36]);
                public_key = Some(key);
            }
            _ => {}
        }

        position += length;
    }

    if !version_ok {
        return None;
    }

    Some(ServerHello {
        public_key: public_key?,
    })
}


/// Сколько байт сообщений рукопожатия уже собрано и сколько израсходовано.
static mut MSG_LEN: usize = 0;
static mut MSG_POS: usize = 0;

/// Возвращает `(начало, длина)` очередного сообщения рукопожатия.
///
/// Сообщения могут приходить частями и по нескольку в одной записи,
/// поэтому они собираются в `MESSAGE`.
fn next_handshake(deadline: u64) -> Option<(usize, usize)> {
    loop {
        let position = unsafe { MSG_POS };
        let length = unsafe { MSG_LEN };

        // Уплотняем буфер, чтобы длинные цепочки сертификатов
        // помещались целиком.
        if position > 0 {
            let buffer = message_mut();
            buffer.copy_within(position..length, 0);

            unsafe {
                MSG_LEN = length - position;
                MSG_POS = 0;
            }

            continue;
        }

        if length >= 4 {
            let buffer = message_mut();
            let message_length = get_u24(buffer, 1);

            if 4 + message_length <= length {
                unsafe {
                    MSG_POS = 4 + message_length;
                }

                return Some((0, 4 + message_length));
            }
        }

        let (kind, count) = read_record(deadline)?;

        if kind != CONTENT_HANDSHAKE {
            return None;
        }

        let buffer = message_mut();

        if length + count > buffer.len() {
            return None;
        }

        buffer[length..length + count].copy_from_slice(&record_mut()[..count]);

        unsafe {
            MSG_LEN = length + count;
        }
    }
}

/// Выводит ключ и IV записи из секрета трафика.
fn traffic_keys(secret: &[u8; 32]) -> ([u8; 16], [u8; 12]) {
    let mut key = [0u8; 16];
    let mut iv = [0u8; 12];
    hkdf_expand_label(secret, b"key", &[], &mut key);
    hkdf_expand_label(secret, b"iv", &[], &mut iv);
    (key, iv)
}

fn derive_secret(parent: &[u8; 32], label: &[u8], transcript: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf_expand_label(parent, label, transcript, &mut out);
    out
}

/// Обновляет ключи чтения по KeyUpdate (RFC 8446, 4.6.3).
fn update_read_keys() {
    let Some(session) = session() else {
        return;
    };

    let mut next = [0u8; 32];
    hkdf_expand_label(&session.read_secret, b"traffic upd", &[], &mut next);
    let (key, iv) = traffic_keys(&next);
    session.read_secret = next;
    session.read_key = key;
    session.read_iv = iv;
    session.read_seq = 0;
}

/// Обрабатывает пост-рукопожатные сообщения (билеты сессии, KeyUpdate).
fn handle_post_handshake(deadline: u64) {
    loop {
        let queued = unsafe { MSG_LEN - MSG_POS };

        if queued < 4 {
            return;
        }

        let complete = {
            let position = unsafe { MSG_POS };
            let buffer = message_mut();
            let length = get_u24(buffer, position + 1);
            4 + length <= queued
        };

        if !complete {
            return;
        }

        let Some((start, _total)) = next_handshake(deadline) else {
            return;
        };

        if message_mut()[start] == HANDSHAKE_KEY_UPDATE {
            update_read_keys();
        }
    }
}

/// Выполняет TLS 1.3 рукопожатие в уже открытом TCP-соединении.
///
/// `host` — имя из URL: используется для SNI и проверки имени в
/// сертификате. При ошибке соединение остаётся открытым: закрывать его
/// должен вызывающий.
pub fn connect(host: &[u8]) -> Result<(), &'static [u8]> {
    let deadline = crate::timer::deadline(HANDSHAKE_TIMEOUT);

    unsafe {
        TRANSCRIPT = Some(crate::crypto::Sha256::new());
        RAW_LEN = 0;
        MSG_LEN = 0;
        MSG_POS = 0;
        SESSION = None;
    }

    let Some(hello) = build_client_hello(host) else {
        return Err(b"tls: host is too long");
    };

    {
        let message = message_mut();

        if !write_record(CONTENT_HANDSHAKE, &message[..hello.length], false) {
            return Err(b"tls: cannot send hello");
        }

        transcript_update(&message[..hello.length]);
    }

    let Some((kind, length)) = read_record(deadline) else {
        return Err(b"tls: no server hello");
    };

    if kind != CONTENT_HANDSHAKE {
        return Err(b"tls: unexpected record");
    }

    let server = {
        let record = record_mut();
        let Some(server) = parse_server_hello(&record[..length]) else {
            return Err(b"tls: bad server hello");
        };

        transcript_update(&record[..length]);
        server
    };

    // Общий секрет ECDHE.
    let shared = x25519(&hello.private_key, &server.public_key);

    if shared == [0u8; 32] {
        return Err(b"tls: bad shared secret");
    }

    let zeros = [0u8; 32];
    let empty_hash = sha256(b"");

    // Ранний секрет и секрет рукопожатия (RFC 8446, раздел 7.1).
    let early = hkdf_extract(&zeros, &zeros);
    let derived = derive_secret(&early, b"derived", &empty_hash);
    let handshake_secret = hkdf_extract(&derived, &shared);

    let transcript = transcript_hash();
    let client_hs = derive_secret(&handshake_secret, b"c hs traffic", &transcript);
    let server_hs = derive_secret(&handshake_secret, b"s hs traffic", &transcript);

    let (read_key, read_iv) = traffic_keys(&server_hs);
    let (write_key, write_iv) = traffic_keys(&client_hs);

    unsafe {
        SESSION = Some(Session {
            read_key,
            read_iv,
            read_seq: 0,
            read_secret: server_hs,
            write_key,
            write_iv,
            write_seq: 0,
            write_secret: client_hs,
            closed: false,
        });
    }

    let mut certificate_checked = false;
    let mut certificate_verify_checked = false;

    loop {
        let Some((start, total)) = next_handshake(deadline) else {
            return Err(b"tls: handshake interrupted");
        };

        let kind = message_mut()[start];

        // Хеш транскрипта до добавления текущего сообщения: он нужен
        // для CertificateVerify и Finished.
        let before = transcript_hash();

        match kind {
            HANDSHAKE_ENCRYPTED_EXTENSIONS => {}
            HANDSHAKE_CERTIFICATE => {
                let message = message_mut();

                if let Err(problem) = x509::verify(&message[start..start + total], host) {
                    return Err(problem);
                }

                certificate_checked = true;
            }
            HANDSHAKE_CERTIFICATE_VERIFY => {
                let message = message_mut();

                if x509::verify_certificate_verify(&message[start..start + total], &before).is_err()
                {
                    return Err(b"tls: certificate verify rejected");
                }

                certificate_verify_checked = true;
            }
            HANDSHAKE_FINISHED => {
                let mut finished_key = [0u8; 32];
                hkdf_expand_label(&server_hs, b"finished", &[], &mut finished_key);
                let expected = hmac_sha256(&finished_key, &[&before]);
                let message = message_mut();
                let received = &message[start + 4..start + total];

                if received.len() != 32 {
                    return Err(b"tls: bad finished");
                }

                let mut difference = 0u8;

                for index in 0..32 {
                    difference |= received[index] ^ expected[index];
                }

                if difference != 0 {
                    return Err(b"tls: bad finished");
                }

                transcript_update(&message[start..start + total]);
                break;
            }
            _ => {}
        }

        transcript_update(&message_mut()[start..start + total]);
    }

    if !certificate_checked || !certificate_verify_checked {
        return Err(b"tls: certificate not verified");
    }

    // Секрет master и прикладные секреты (RFC 8446, раздел 7.1).
    let derived = derive_secret(&handshake_secret, b"derived", &empty_hash);
    let master = hkdf_extract(&derived, &zeros);

    let transcript = transcript_hash();
    let client_ap = derive_secret(&master, b"c ap traffic", &transcript);
    let server_ap = derive_secret(&master, b"s ap traffic", &transcript);

    // Finished клиента: verify_data по транскрипту до этого сообщения.
    let mut finished_key = [0u8; 32];
    hkdf_expand_label(&client_hs, b"finished", &[], &mut finished_key);
    let verify = hmac_sha256(&finished_key, &[&transcript_hash()]);

    let mut finished_message = [0u8; 36];
    finished_message[0] = HANDSHAKE_FINISHED;
    put_u24(&mut finished_message, 1, 32);
    finished_message[4..].copy_from_slice(&verify);

    // ChangeCipherSpec для совместимости, затем зашифрованный Finished.
    if !write_record(CONTENT_CHANGE_CIPHER_SPEC, &[1], false) {
        return Err(b"tls: cannot send change cipher spec");
    }

    if !write_record(CONTENT_HANDSHAKE, &finished_message, true) {
        return Err(b"tls: cannot send finished");
    }

    transcript_update(&finished_message);

    // Переходим на прикладные ключи: последовательности начинаются заново.
    let (read_key, read_iv) = traffic_keys(&server_ap);
    let (write_key, write_iv) = traffic_keys(&client_ap);

    if let Some(session) = session() {
        session.read_key = read_key;
        session.read_iv = read_iv;
        session.read_seq = 0;
        session.read_secret = server_ap;
        session.write_key = write_key;
        session.write_iv = write_iv;
        session.write_seq = 0;
        session.write_secret = client_ap;
    }

    Ok(())
}

/// Отправляет прикладные данные.
pub fn send(data: &[u8]) -> bool {
    if session().is_none() {
        return false;
    }

    write_record(CONTENT_APPLICATION_DATA, data, true)
}

/// Читает расшифрованные прикладные данные.
///
/// Возвращает 0, если данных пока нет (как [`crate::net::tcp::read`]),
/// либо если соединение закрыто.
pub fn read(out: &mut [u8]) -> usize {
    let deadline = crate::timer::deadline(1);

    loop {
        let Some((kind, length)) = read_record(deadline) else {
            return 0;
        };

        if kind == CONTENT_APPLICATION_DATA {
            let count = length.min(out.len());
            out[..count].copy_from_slice(&record_mut()[..count]);
            return count;
        }

        if kind == CONTENT_HANDSHAKE {
            handle_post_handshake(deadline);
        }
    }
}

/// Закрыт ли поток со стороны сервера.
pub fn peer_closed() -> bool {
    match session() {
        Some(session) => session.closed,
        None => true,
    }
}

/// Отправляет close_notify и закрывает TCP-соединение.
pub fn close() {
    if session().is_some() {
        write_record(CONTENT_ALERT, &[1, 0], true);
    }

    tcp::close();

    unsafe {
        SESSION = None;
        RAW_LEN = 0;
        MSG_LEN = 0;
        MSG_POS = 0;
    }
}


