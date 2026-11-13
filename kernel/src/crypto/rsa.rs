//! Проверка подписей RSA: PKCS#1 v1.5 и PSS с SHA-256.
//!
//! Используется при проверке цепочки сертификатов и сообщения
//! CertificateVerify в TLS 1.3. Проверяется тест-векторами на хосте
//! (tools/crypto-check) и реальными сертификатами в QEMU.

use super::bignum::{from_be_bytes, to_be_bytes, Montgomery};
use super::sha256::{sha256, Sha256};

/// DER-префикс DigestInfo для SHA-256 (RFC 8017, 9.2).
const DIGEST_INFO_SHA256: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

/// Восстанавливает `signature^exponent mod modulus` в виде EM.
///
/// Возвращает буфер и длину модуля: поддерживаются ключи 2048, 3072 и
/// 4096 бит (наиболее распространённые в цепочках сертификатов).
fn recover(modulus: &[u8], exponent: &[u8], signature: &[u8]) -> Option<([u8; 512], usize)> {
    match modulus.len() {
        256 => recover_sized::<32>(modulus, exponent, signature),
        384 => recover_sized::<48>(modulus, exponent, signature),
        512 => recover_sized::<64>(modulus, exponent, signature),
        _ => None,
    }
}

fn recover_sized<const N: usize>(
    modulus: &[u8],
    exponent: &[u8],
    signature: &[u8],
) -> Option<([u8; 512], usize)> {
    if signature.len() > N * 8 || exponent.len() > N * 8 {
        return None;
    }

    let mut n = [0u64; N];
    from_be_bytes(modulus, &mut n);

    let mut e = [0u64; N];
    from_be_bytes(exponent, &mut e);

    let mut s = [0u64; N];
    from_be_bytes(signature, &mut s);

    let montgomery = Montgomery::new(&n)?;
    let value = montgomery.power(&s, &e);

    let length = N * 8;
    let mut out = [0u8; 512];
    to_be_bytes(&value, &mut out[512 - length..]);

    Some((out, length))
}

fn equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut difference = 0u8;

    for index in 0..left.len() {
        difference |= left[index] ^ right[index];
    }

    difference == 0
}

/// Проверяет подпись RSASSA-PKCS1-v1_5 с SHA-256.
pub fn verify_pkcs1_sha256(
    modulus: &[u8],
    exponent: &[u8],
    signature: &[u8],
    digest: &[u8; 32],
) -> bool {
    let Some((em, length)) = recover(modulus, exponent, signature) else {
        return false;
    };

    let em = &em[512 - length..];

    if em.len() < 11 || em[0] != 0 || em[1] != 1 {
        return false;
    }

    let mut position = 2usize;

    while position < em.len() && em[position] == 0xff {
        position += 1;
    }

    // Не менее восьми байт 0xff и нулевой разделитель.
    if position < 10 || position >= em.len() || em[position] != 0 {
        return false;
    }

    let rest = &em[position + 1..];

    rest.len() == DIGEST_INFO_SHA256.len() + 32
        && equal(&rest[..DIGEST_INFO_SHA256.len()], &DIGEST_INFO_SHA256)
        && equal(&rest[DIGEST_INFO_SHA256.len()..], digest)
}

/// MGF1 на SHA-256 (RFC 8017, B.2.1).
fn mgf1(seed: &[u8], out: &mut [u8]) {
    let mut counter = 0u32;
    let mut written = 0usize;

    while written < out.len() {
        let mut hash = Sha256::new();
        hash.update(seed);
        hash.update(&counter.to_be_bytes());
        let block = hash.finish();

        let take = (out.len() - written).min(32);
        out[written..written + take].copy_from_slice(&block[..take]);

        written += take;
        counter += 1;
    }
}

/// Проверяет подпись RSASSA-PSS с SHA-256 и длиной соли 32 байта.
pub fn verify_pss_sha256(
    modulus: &[u8],
    exponent: &[u8],
    signature: &[u8],
    digest: &[u8; 32],
) -> bool {
    const HASH: usize = 32;

    let Some((em, length)) = recover(modulus, exponent, signature) else {
        return false;
    };

    let em = &em[512 - length..];

    if length < 2 * HASH + 2 || em[length - 1] != 0xbc {
        return false;
    }

    let db_length = length - HASH - 1;
    let mut db = [0u8; 512];
    db[..db_length].copy_from_slice(&em[..db_length]);

    let mut mask = [0u8; 512];
    mgf1(&em[db_length..db_length + HASH], &mut mask[..db_length]);

    for index in 0..db_length {
        db[index] ^= mask[index];
    }

    db[0] &= 0x7f;

    let separator = db_length - HASH - 1;

    if db[..separator].iter().any(|&byte| byte != 0) || db[separator] != 1 {
        return false;
    }

    let salt = &db[separator + 1..db_length];

    let mut hash = Sha256::new();
    hash.update(&[0u8; 8]);
    hash.update(digest);
    hash.update(salt);
    let computed = hash.finish();

    equal(&computed, &em[db_length..db_length + HASH])
}

/// Проверяет подпись сертификата по алгоритму `sha256WithRSAEncryption`.
pub fn verify_sha256(
    modulus: &[u8],
    exponent: &[u8],
    signature: &[u8],
    message: &[u8],
) -> bool {
    let digest = sha256(message);
    verify_pkcs1_sha256(modulus, exponent, signature, &digest)
}
