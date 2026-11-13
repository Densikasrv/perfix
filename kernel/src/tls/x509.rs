//! Проверка цепочки сертификатов X.509 и сообщения CertificateVerify.
//!
//! Поддерживаются:
//!   * DER-разбор Certificate/TBSCertificate, расширение subjectAltName;
//!   * подписи `sha256WithRSAEncryption` (ключи 2048/3072/4096 бит);
//!   * проверка срока действия по UTC-времени RTC;
//!   * сверка имени узла с dNSName (включая шаблон `*.example.com`) и
//!     с iPAddress для IP-адресов;
//!   * цепочка до встроенного корневого сертификата (tls/roots.rs);
//!   * CertificateVerify с rsa_pss_rsae_sha256 и rsa_pkcs1_sha256.
//!
//! Не поддерживаются (сознательно, до следующего этапа): ECDSA-подписи,
//! SHA-384/SHA-512 и подписанные сертификаты с SHA-1.

use crate::crypto::p256;
use crate::crypto::rsa;
use crate::crypto::sha256::Sha256;
use crate::rtc;

use super::roots::ROOTS;

/// DER-значение OID sha256WithRSAEncryption (1.2.840.113549.1.1.11).
const OID_SHA256_RSA: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];

/// DER-значение OID ecdsa-with-SHA256 (1.2.840.10045.4.3.2).
const OID_ECDSA_SHA256: [u8; 8] = [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];

/// DER-значение OID id-ecPublicKey (1.2.840.10045.2.1).
const OID_EC_PUBLIC_KEY: [u8; 7] = [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];

/// DER-значение OID prime256v1 (1.2.840.10045.3.1.7).
const OID_PRIME256V1: [u8; 8] = [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

/// DER-значение OID subjectAltName (2.5.29.17).
const OID_SUBJECT_ALT_NAME: [u8; 3] = [0x55, 0x1d, 0x11];

/// Максимум сертификатов в цепочке, которые рассматриваются.
const MAX_CHAIN: usize = 8;

/// Вид открытого ключа листа (для CertificateVerify).
const KEY_NONE: u8 = 0;
const KEY_RSA: u8 = 1;
const KEY_EC: u8 = 2;

/// Ключ листа, нужный для проверки CertificateVerify.
static mut LEAF_KIND: u8 = KEY_NONE;
static mut LEAF_MODULUS: [u8; 512] = [0; 512];
static mut LEAF_MODULUS_LEN: usize = 0;
static mut LEAF_EXPONENT: [u8; 8] = [0; 8];
static mut LEAF_EXPONENT_LEN: usize = 0;
static mut LEAF_POINT: [u8; 65] = [0; 65];
static mut LEAF_POINT_LEN: usize = 0;

fn get_u16(buffer: &[u8], offset: usize) -> u16 {
    ((buffer[offset] as u16) << 8) | buffer[offset + 1] as u16
}

fn get_u24(buffer: &[u8], offset: usize) -> usize {
    ((buffer[offset] as usize) << 16)
        | ((buffer[offset + 1] as usize) << 8)
        | buffer[offset + 2] as usize
}

/// Один элемент DER: тег, значение и весь элемент (заголовок + значение).
#[derive(Clone, Copy)]
struct Element<'a> {
    tag: u8,
    value: &'a [u8],
    raw: &'a [u8],
}

/// Читает очередной DER-элемент из `data`.
fn element<'a>(data: &'a [u8], position: &mut usize) -> Option<Element<'a>> {
    let start = *position;

    if start + 2 > data.len() {
        return None;
    }

    let tag = data[start];
    let first = data[start + 1];

    let (length, header) = if first & 0x80 != 0 {
        let count = (first & 0x7f) as usize;

        if count == 0 || count > 4 || start + 2 + count > data.len() {
            return None;
        }

        let mut length = 0usize;

        for index in 0..count {
            length = (length << 8) | data[start + 2 + index] as usize;
        }

        (length, 2 + count)
    } else {
        (first as usize, 2)
    };

    let body = start + header;

    if body + length > data.len() {
        return None;
    }

    *position = body + length;

    Some(Element {
        tag,
        value: &data[body..body + length],
        raw: &data[start..body + length],
    })
}

/// Читает элемент, значение которого — последовательность DER.
fn children(element: Element<'_>) -> Option<&[u8]> {
    if element.tag != 0x30 {
        return None;
    }

    Some(element.value)
}

/// Дата из UTCTime или GeneralizedTime.
fn parse_time(element: Element<'_>) -> Option<(u32, u32, u32)> {
    let text = element.value;

    let (year, offset) = match element.tag {
        // UTCTime: YYMMDDHHMMSSZ
        0x17 => {
            if text.len() < 13 {
                return None;
            }

            let digits = |start: usize| -> Option<u32> {
                let mut value = 0u32;

                for index in start..start + 2 {
                    let byte = *text.get(index)?;

                    if !byte.is_ascii_digit() {
                        return None;
                    }

                    value = value * 10 + (byte - b'0') as u32;
                }

                Some(value)
            };

            let short = digits(0)?;

            let year = if short < 50 { 2000 + short } else { 1900 + short };
            (year, 2)
        }
        // GeneralizedTime: YYYYMMDDHHMMSSZ
        0x18 => {
            if text.len() < 15 || !text[..4].iter().all(|byte| byte.is_ascii_digit()) {
                return None;
            }

            let mut year = 0u32;

            for index in 0..4 {
                year = year * 10 + (text[index] - b'0') as u32;
            }

            (year, 4)
        }
        _ => return None,
    };

    let mut parts = [0u32; 2];

    for (index, part) in parts.iter_mut().enumerate() {
        let start = offset + index * 2;

        if start + 2 > text.len() {
            return None;
        }

        if !text[start].is_ascii_digit() || !text[start + 1].is_ascii_digit() {
            return None;
        }

        *part = ((text[start] - b'0') as u32) * 10 + (text[start + 1] - b'0') as u32;
    }

    Some((year, parts[0], parts[1]))
}


fn strip_leading_zero(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;

    while start + 1 < bytes.len() && bytes[start] == 0 {
        start += 1;
    }

    &bytes[start..]
}

/// Открытый ключ сертификата.
#[derive(Clone, Copy)]
enum PublicKey<'a> {
    Rsa {
        modulus: &'a [u8],
        exponent: &'a [u8],
    },
    /// Несжатая точка P-256: `0x04 || X || Y`.
    Ec(&'a [u8]),
}

/// Разобранный сертификат: всё, что нужно для проверки цепочки.
struct Certificate<'a> {
    /// Подписанная часть (tbsCertificate целиком).
    tbs: &'a [u8],
    signature_oid: &'a [u8],
    signature: &'a [u8],
    /// DER имени издателя и владельца: по ним подбирается издатель.
    issuer: &'a [u8],
    subject: &'a [u8],
    not_before: (u32, u32, u32),
    not_after: (u32, u32, u32),
    key: PublicKey<'a>,
    san: Option<&'a [u8]>,
}

/// Разбирает SubjectPublicKeyInfo с ключом RSA или P-256.
fn parse_spki(spki: Element<'_>) -> Option<PublicKey<'_>> {
    const OID_RSA: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];

    let mut position = 0;
    let algorithm = element(spki.value, &mut position)?;
    let mut algorithm_position = 0;
    let oid = element(algorithm.value, &mut algorithm_position)?;

    if oid.tag != 0x06 {
        return None;
    }

    let bits = element(spki.value, &mut position)?;

    if bits.tag != 0x03 || bits.value.is_empty() {
        return None;
    }

    let key = &bits.value[1..];

    if oid.value == OID_RSA {
        let mut key_position = 0;
        let sequence = element(key, &mut key_position)?;
        let mut inner = 0;
        let modulus = element(sequence.value, &mut inner)?;
        let exponent = element(sequence.value, &mut inner)?;

        if modulus.tag != 0x02 || exponent.tag != 0x02 {
            return None;
        }

        return Some(PublicKey::Rsa {
            modulus: strip_leading_zero(modulus.value),
            exponent: exponent.value,
        });
    }

    if oid.value == OID_EC_PUBLIC_KEY {
        let parameters = element(algorithm.value, &mut algorithm_position)?;

        if parameters.tag != 0x06 || parameters.value != OID_PRIME256V1 {
            return None;
        }

        if key.len() != 65 || key[0] != 0x04 {
            return None;
        }

        return Some(PublicKey::Ec(key));
    }

    None
}

/// Ищет subjectAltName в расширениях `[3]` сертификата.
///
/// `None` — расширения испорчены, `Some(None)` — SAN отсутствует.
fn find_san(extensions: Element<'_>) -> Option<Option<&'_ [u8]>> {
    let mut position = 0;
    let list = element(extensions.value, &mut position)?;
    let mut entry_position = 0;

    while entry_position < list.value.len() {
        let extension = element(list.value, &mut entry_position)?;
        let mut inner = 0;
        let oid = element(extension.value, &mut inner)?;
        let mut value = element(extension.value, &mut inner)?;

        if value.tag == 0x01 {
            value = element(extension.value, &mut inner)?;
        }

        if oid.tag == 0x06 && oid.value == OID_SUBJECT_ALT_NAME {
            if value.tag != 0x04 {
                return None;
            }

            return Some(Some(value.value));
        }
    }

    Some(None)
}


/// Разбирает DER-сертификат.
fn parse_certificate(der: &[u8]) -> Option<Certificate<'_>> {
    let mut position = 0;
    let certificate = element(der, &mut position)?;
    let body = children(certificate)?;

    let mut body_position = 0;
    let tbs = element(body, &mut body_position)?;
    let algorithm = element(body, &mut body_position)?;
    let signature = element(body, &mut body_position)?;

    if signature.tag != 0x03 || signature.value.len() < 2 {
        return None;
    }

    let mut algorithm_position = 0;
    let oid = element(algorithm.value, &mut algorithm_position)?;

    if oid.tag != 0x06 {
        return None;
    }

    let inner = tbs.value;
    let mut field_position = 0;
    let mut field = element(inner, &mut field_position)?;

    if field.tag == 0xa0 {
        field = element(inner, &mut field_position)?;
    }

    // serialNumber
    if field.tag != 0x02 {
        return None;
    }

    let tbs_algorithm = element(inner, &mut field_position)?;
    let mut tbs_algorithm_position = 0;
    let tbs_oid = element(tbs_algorithm.value, &mut tbs_algorithm_position)?;

    if tbs_oid.tag != 0x06 || tbs_oid.value != oid.value {
        return None;
    }

    // issuer, validity
    let issuer = element(inner, &mut field_position)?;
    let validity = element(inner, &mut field_position)?;
    let mut validity_position = 0;
    let not_before = parse_time(element(validity.value, &mut validity_position)?)?;
    let not_after = parse_time(element(validity.value, &mut validity_position)?)?;

    // subject, subjectPublicKeyInfo
    let subject = element(inner, &mut field_position)?;
    let spki = element(inner, &mut field_position)?;
    let key = parse_spki(spki)?;

    let mut san = None;

    while field_position < inner.len() {
        let extra = element(inner, &mut field_position)?;

        if extra.tag == 0xa3 {
            san = find_san(extra)?;
        }
    }

    Some(Certificate {
        tbs: tbs.raw,
        signature_oid: oid.value,
        signature: &signature.value[1..],
        issuer: issuer.raw,
        subject: subject.raw,
        not_before,
        not_after,
        key,
        san,
    })
}

/// Сверяет имя узла с одним шаблоном dNSName.
fn dns_matches(host: &[u8], pattern: &[u8]) -> bool {
    // Шаблон `*.example.com` покрывает ровно одну метку.
    if let Some(suffix) = pattern.strip_prefix(b"*".as_slice()) {
        if host.len() <= suffix.len() + 1 {
            return false;
        }

        let tail = &host[host.len() - suffix.len()..];

        if !tail.eq_ignore_ascii_case(suffix) {
            return false;
        }

        let label = &host[..host.len() - suffix.len()];
        !label.contains(&b'.')
    } else {
        host.eq_ignore_ascii_case(pattern)
    }
}

/// Проверяет, что имя узла есть в subjectAltName.
fn name_matches(host: &[u8], san: &[u8]) -> bool {
    let mut position = 0;
    let Some(sequence) = element(san, &mut position) else {
        return false;
    };

    let address = crate::net::parse_ipv4(host);
    let mut entry_position = 0;

    while entry_position < sequence.value.len() {
        let Some(entry) = element(sequence.value, &mut entry_position) else {
            return false;
        };

        match entry.tag {
            // dNSName
            0x82 => {
                if address.is_none() && dns_matches(host, entry.value) {
                    return true;
                }
            }
            // iPAddress
            0x87 => {
                if let Some(address) = address {
                    if entry.value == address {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }

    false
}

/// Дополнительный доверенный корень для локальных испытаний TLS.
///
/// Включается только флагом `dev-root` (tools/gen-test-pki.sh кладёт сюда
/// DER тестового УЦ). В обычной сборке этого модуля нет.
#[cfg(feature = "dev-root")]
mod dev_root {
    pub static DER: &[u8] = include_bytes!("dev-root.der");
}

#[cfg(feature = "dev-root")]
const EXTRA_ROOTS: &[&[u8]] = &[dev_root::DER];

#[cfg(not(feature = "dev-root"))]
const EXTRA_ROOTS: &[&[u8]] = &[];

/// Проверяет подпись сертификата ключом издателя.
fn signed_by(child: &Certificate<'_>, issuer_key: PublicKey<'_>) -> bool {
    match issuer_key {
        PublicKey::Rsa { modulus, exponent } => {
            child.signature_oid == OID_SHA256_RSA
                && rsa::verify_sha256(modulus, exponent, child.signature, child.tbs)
        }
        PublicKey::Ec(point) => {
            child.signature_oid == OID_ECDSA_SHA256
                && p256::verify_sha256(point, child.signature, child.tbs)
        }
    }
}

/// Действует ли сертификат на текущую дату (UTC из RTC).
fn valid_now(certificate: &Certificate<'_>) -> bool {
    let Some((year, month, day, _, _)) = rtc::utc_now() else {
        return false;
    };

    let today = (year, month, day);
    today >= certificate.not_before && today <= certificate.not_after
}

/// Проверяет цепочку из сообщения Certificate: подписи, сроки, имя узла
/// и доверие к корню.
///
/// `message` — сообщение рукопожатия целиком, включая 4-байтовый заголовок.
/// Сообщения об ошибках называют этап проверки: так их видно в консоли.
pub fn verify(message: &[u8], host: &[u8]) -> Result<(), &'static [u8]> {
    if message.len() < 8 {
        return Err(b"tls: cert: short message");
    }

    let body = &message[4..];
    let context = body[0] as usize;
    let mut position = 1 + context;

    if position + 3 > body.len() {
        return Err(b"tls: cert: bad list");
    }

    let list_length = get_u24(body, position);
    position += 3;

    if position + list_length > body.len() {
        return Err(b"tls: cert: bad list length");
    }

    let list_end = position + list_length;
    let mut chain: [&[u8]; MAX_CHAIN] = [&[] as &[u8]; MAX_CHAIN];
    let mut count = 0usize;

    while position < list_end {
        if count >= MAX_CHAIN || position + 3 > list_end {
            return Err(b"tls: cert: too many certificates");
        }

        let length = get_u24(body, position);
        position += 3;

        if length == 0 || position + length > list_end {
            return Err(b"tls: cert: bad entry");
        }

        chain[count] = &body[position..position + length];
        count += 1;
        position += length;

        if position + 2 > list_end {
            return Err(b"tls: cert: bad extensions");
        }

        let extensions = get_u16(body, position) as usize;
        position += 2;

        if position + extensions > list_end {
            return Err(b"tls: cert: bad extensions length");
        }

        position += extensions;
    }

    if count == 0 {
        return Err(b"tls: cert: empty chain");
    }

    let Some(leaf) = parse_certificate(chain[0]) else {
        return Err(b"tls: cert: cannot parse leaf");
    };

    if !valid_now(&leaf) {
        return Err(b"tls: cert: expired or not yet valid");
    }

    let Some(san) = leaf.san else {
        return Err(b"tls: cert: no subjectAltName");
    };

    if !name_matches(host, san) {
        return Err(b"tls: cert: name mismatch");
    }

    // Ключ листа понадобится для CertificateVerify, а буфер рукопожатия
    // к этому моменту уже может быть перезаписан.
    match leaf.key {
        PublicKey::Rsa { modulus, exponent } => {
            if modulus.len() > 512 || exponent.len() > 8 {
                return Err(b"tls: cert: unsupported key size");
            }

            unsafe {
                let buffer = &mut *core::ptr::addr_of_mut!(LEAF_MODULUS);
                buffer[..modulus.len()].copy_from_slice(modulus);
                LEAF_MODULUS_LEN = modulus.len();

                let buffer = &mut *core::ptr::addr_of_mut!(LEAF_EXPONENT);
                buffer[..exponent.len()].copy_from_slice(exponent);
                LEAF_EXPONENT_LEN = exponent.len();

                LEAF_KIND = KEY_RSA;
            }
        }
        PublicKey::Ec(point) => {
            unsafe {
                let buffer = &mut *core::ptr::addr_of_mut!(LEAF_POINT);
                buffer[..point.len()].copy_from_slice(point);
                LEAF_POINT_LEN = point.len();

                LEAF_KIND = KEY_EC;
            }
        }
    }

    // Промежуточные звенья: каждый сертификат подписан следующим за ним.
    let mut index = 0usize;

    while index + 1 < count {
        let Some(child) = parse_certificate(chain[index]) else {
            return Err(b"tls: cert: cannot parse entry");
        };

        let Some(issuer) = parse_certificate(chain[index + 1]) else {
            return Err(b"tls: cert: cannot parse issuer");
        };

        if !valid_now(&issuer) {
            return Err(b"tls: cert: issuer expired");
        }

        if child.issuer != issuer.subject {
            return Err(b"tls: cert: issuer name mismatch");
        }

        if !signed_by(&child, issuer.key) {
            return Err(b"tls: cert: bad chain signature");
        }

        index += 1;
    }

    // Последний сертификат должен быть подписан доверенным корнем.
    let Some(last) = parse_certificate(chain[count - 1]) else {
        return Err(b"tls: cert: cannot parse last");
    };

    for root in ROOTS {
        let Some(candidate) = parse_certificate(root.der()) else {
            continue;
        };

        // Сначала дешёвая проверка имени: полная RSA-проверка каждого из
        // десятков корней была бы слишком медленной.
        if candidate.subject != last.issuer {
            continue;
        }

        if signed_by(&last, candidate.key) {
            return Ok(());
        }
    }

    for der in EXTRA_ROOTS {
        let Some(candidate) = parse_certificate(der) else {
            continue;
        };

        if candidate.subject != last.issuer {
            continue;
        }

        if signed_by(&last, candidate.key) {
            return Ok(());
        }
    }

    Err(b"tls: cert: unknown issuer")
}

/// Проверяет подпись сообщения CertificateVerify ключом листа.
///
/// `message` — сообщение рукопожатия целиком, `transcript` — хеш всех
/// предыдущих сообщений рукопожатия.
pub fn verify_certificate_verify(message: &[u8], transcript: &[u8; 32]) -> Result<(), ()> {
    if message.len() < 8 {
        return Err(());
    }

    let scheme = get_u16(message, 4);
    let length = get_u16(message, 6) as usize;

    if message.len() < 8 + length {
        return Err(());
    }

    let signature = &message[8..8 + length];

    // Контекст подписи TLS 1.3 (RFC 8446, раздел 4.4.3).
    let mut hash = Sha256::new();
    hash.update(&[0x20u8; 64]);
    hash.update(b"TLS 1.3, server CertificateVerify");
    hash.update(&[0u8]);
    hash.update(transcript);
    let digest = hash.finish();

    let (kind, modulus_length, exponent_length, point_length) = unsafe {
        (
            LEAF_KIND,
            LEAF_MODULUS_LEN,
            LEAF_EXPONENT_LEN,
            LEAF_POINT_LEN,
        )
    };

    let mut modulus = [0u8; 512];
    let mut exponent = [0u8; 8];
    let mut point = [0u8; 65];

    unsafe {
        let source = &*core::ptr::addr_of!(LEAF_MODULUS);
        modulus[..modulus_length].copy_from_slice(&source[..modulus_length]);

        let source = &*core::ptr::addr_of!(LEAF_EXPONENT);
        exponent[..exponent_length].copy_from_slice(&source[..exponent_length]);

        let source = &*core::ptr::addr_of!(LEAF_POINT);
        point[..point_length].copy_from_slice(&source[..point_length]);
    }

    let modulus = &modulus[..modulus_length];
    let exponent = &exponent[..exponent_length];
    let point = &point[..point_length];

    let verified = match scheme {
        // rsa_pss_rsae_sha256 / rsa_pkcs1_sha256
        0x0804 | 0x0401 if kind == KEY_RSA => {
            if scheme == 0x0804 {
                rsa::verify_pss_sha256(modulus, exponent, signature, &digest)
            } else {
                rsa::verify_pkcs1_sha256(modulus, exponent, signature, &digest)
            }
        }
        // ecdsa_secp256r1_sha256
        0x0403 if kind == KEY_EC => p256::verify(point, signature, &digest),
        _ => false,
    };

    if verified {
        Ok(())
    } else {
        Err(())
    }
}
