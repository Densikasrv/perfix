//! Проверка криптопримитивов ядра тест-векторами на хосте.
//!
//! Модули подключаются прямо из kernel/src/crypto, поэтому проверяется
//! тот же код, что компилируется в ядро.

#[path = "../../../kernel/src/crypto/aes.rs"]
mod aes;
#[path = "../../../kernel/src/crypto/bignum.rs"]
mod bignum;
#[path = "../../../kernel/src/crypto/gcm.rs"]
mod gcm;
#[path = "../../../kernel/src/crypto/p256.rs"]
mod p256;
#[path = "../../../kernel/src/crypto/sha256.rs"]
mod sha256;
#[path = "../../../kernel/src/crypto/x25519.rs"]
mod x25519;

use aes::Aes128;
use bignum::{to_be_bytes, Montgomery};
use gcm::Aes128Gcm;
use sha256::{hkdf_expand, hkdf_extract, hmac_sha256, sha256};
use x25519::x25519;

fn hex(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;

    while index + 1 < bytes.len() {
        let high = (bytes[index] as char).to_digit(16).expect("hex") as u8;
        let low = (bytes[index + 1] as char).to_digit(16).expect("hex") as u8;
        out.push((high << 4) | low);
        index += 2;
    }

    out
}

fn to_hex(data: &[u8]) -> String {
    let mut out = String::new();

    for byte in data {
        out.push_str(&format!("{:02x}", byte));
    }

    out
}

fn check(name: &str, actual: &[u8], expected: &str) -> bool {
    let ok = to_hex(actual) == expected;

    if ok {
        println!("ok   {}", name);
    } else {
        println!("FAIL {}\n     got {}\n     exp {}", name, to_hex(actual), expected);
    }

    ok
}

fn main() {
    let mut failed = 0;

    // SHA-256 (RFC 6234 / FIPS 180-4).
    failed += !check(
        "sha256(\"\")",
        &sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ) as usize;
    failed += !check(
        "sha256(\"abc\")",
        &sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    ) as usize;
    failed += !check(
        "sha256(long)",
        &sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
    ) as usize;

    // HMAC-SHA256 (RFC 4231, тесты 1 и 2).
    let key1 = [0x0b_u8; 20];
    failed += !check(
        "hmac case1",
        &hmac_sha256(&key1, &[b"Hi There"]),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
    ) as usize;
    failed += !check(
        "hmac case2",
        &hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
    ) as usize;

    // HKDF (RFC 5869, тест 1).
    let ikm = [0x0b_u8; 22];
    let salt = hex("000102030405060708090a0b0c");
    let info = hex("f0f1f2f3f4f5f6f7f8f9");
    let prk = hkdf_extract(&salt, &ikm);
    failed += !check(
        "hkdf extract",
        &prk,
        "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5",
    ) as usize;

    let mut okm = [0u8; 42];
    hkdf_expand(&prk, &info, &mut okm);
    failed += !check(
        "hkdf expand",
        &okm,
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
    ) as usize;

    // AES-128 (FIPS-197, приложение C.1).
    let mut key = [0u8; 16];
    key.copy_from_slice(&hex("000102030405060708090a0b0c0d0e0f"));
    let mut block = [0u8; 16];
    block.copy_from_slice(&hex("00112233445566778899aabbccddeeff"));
    Aes128::new(&key).encrypt_block(&mut block);
    failed += !check("aes128", &block, "69c4e0d86a7b0430d8cdb78070b4c55a") as usize;

    // AES-128-GCM (NIST SP 800-38D, тесты 1 и 2).
    let zero_key = [0u8; 16];
    let zero_nonce = [0u8; 12];
    let gcm = Aes128Gcm::new(&zero_key);

    let mut empty: [u8; 0] = [];
    let mut tag = [0u8; 16];
    gcm.encrypt(&zero_nonce, &[], &mut empty, &mut tag);
    failed += !check("gcm empty tag", &tag, "58e2fccefa7e3061367f1d57a4e7455a") as usize;

    let mut data = [0u8; 16];
    let mut tag = [0u8; 16];
    gcm.encrypt(&zero_nonce, &[], &mut data, &mut tag);
    failed += !check(
        "gcm ciphertext",
        &data,
        "0388dace60b6a392f328c2b971b2fe78",
    ) as usize;
    failed += !check("gcm tag", &tag, "ab6e47d42cec13bdf53a67b21257bddf") as usize;

    // Расшифрование и проверка тега.
    let mut roundtrip = data;
    let decrypted = gcm.decrypt(&zero_nonce, &[], &mut roundtrip, &tag);
    failed += !(decrypted && roundtrip == [0u8; 16]) as usize;
    println!("ok   gcm roundtrip: {}", decrypted);

    // X25519 (RFC 7748, раздел 5.2).
    let scalar = hex("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
    let point = hex("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
    let mut scalar_bytes = [0u8; 32];
    scalar_bytes.copy_from_slice(&scalar);
    let mut point_bytes = [0u8; 32];
    point_bytes.copy_from_slice(&point);
    failed += !check(
        "x25519 rfc7748",
        &x25519(&scalar_bytes, &point_bytes),
        "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552",
    ) as usize;

    // Базовая точка: известный открытый ключ из RFC 7748, раздел 6.1.
    let alice = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let mut alice_bytes = [0u8; 32];
    alice_bytes.copy_from_slice(&alice);
    let mut basepoint = [0u8; 32];
    basepoint[0] = 9;
    failed += !check(
        "x25519 basepoint",
        &x25519(&alice_bytes, &basepoint),
        "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a",
    ) as usize;

    // Монтгомери: 3^65537 mod (2^2048 - 159), значение посчитано независимо.
    let mut modulus = [0u64; 32];

    for limb in modulus.iter_mut() {
        *limb = u64::MAX;
    }

    modulus[0] -= 158;

    let mont = Montgomery::new(&modulus).expect("odd modulus");

    let mut base = [0u64; 32];
    base[0] = 3;
    let mut exponent = [0u64; 32];
    exponent[0] = 65537;

    let powered = mont.power(&base, &exponent);
    let mut bytes = [0u8; 256];
    to_be_bytes(&powered, &mut bytes);
    failed += !check(
        "modpow 3^65537 mod (2^2048-159)",
        &bytes,
        "57d71bafb42db6f51ea25cbbfa4cdd316b9a1f862deef4ce7a121c489d577ee8ccae881d9018dcc6b728f7b003de18743a6923a8badd9b3813f9202eea2f58f5956743c67a28b98fb36057bec79097f6048ff3051824b191ccec8539d13bd9d2e570f975c2422cc80ebcb13e4fb60ea56ee3595366e10df689ed7abfef8bc76d2b9095683d7c965589ac8f34de4af5b93a03d8869cc63073bf4326353d8bd736a1c3b0bce679e543440d4e9593d84a47a861a299178bee06b43ec0ff4eddca91f62069b322efd0dd0bbb91dde56927b1ccaf205eb16bde90d38e2ba42e43e7dd83eff26d6d610d90d78b89901b93cdb50ca4eac623ce4a6f4544c8e39e722640",
    ) as usize;

    // Обратный элемент: x * x^(p-2) = 1 mod (2^255 - 19).
    let mut prime = [0u64; 4];
    prime[0] = u64::MAX - 18;
    prime[1] = u64::MAX;
    prime[2] = u64::MAX;
    prime[3] = u64::MAX >> 1;

    let field = Montgomery::new(&prime).expect("odd modulus");
    let mut value = [0u64; 4];
    value[0] = 123456789;

    let mut exponent = prime;
    exponent[0] -= 2;

    let inverse = field.power(&value, &exponent);
    let product = field.multiply(&field.to_montgomery(&value), &field.to_montgomery(&inverse));
    let mut one = [0u64; 4];
    one[0] = 1;
    failed += !(field.from_montgomery(&product) == one) as usize;
    println!("ok   montgomery inverse");

    // ECDSA P-256/SHA-256: подпись, сгенерированная openssl.
    let public = hex("04de6e1c55786abdb86e8f7c0f1fc94740cdd321ebe4dd1ed6140db4526335e162db69eb0c6b8e18f8da313784796212b61d8babb101f17fd07ed1c4c3c46aacc1");
    let signature = hex("30450220258c4ac09d8f6718c5d72ed7484c43abc9f6878bb9def0275f846d109c7fd3a8022100865266238ca954ff9a68a8b24123e4b9f36fde0445483ea48818344536de5af6");
    let message = b"perfix ecdsa test vector";

    failed += !p256::verify_sha256(&public, &signature, message) as usize;
    println!("ok   ecdsa p256 verify");

    let mut tampered = *message;
    tampered[0] ^= 1;
    let rejected = !p256::verify_sha256(&public, &signature, &tampered);
    failed += !rejected as usize;
    println!("ok   ecdsa p256 rejects tampered message: {}", rejected);


    if failed == 0 {
        println!("\nall crypto vectors passed");
    } else {
        println!("\n{} checks failed", failed);
        std::process::exit(1);
    }
}
