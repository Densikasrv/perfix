//! Криптографические примитивы для TLS 1.3 (no_std, без аллокатора).
//!
//! Каждый модуль проверяется тест-векторами на хосте:
//! `cargo run --manifest-path tools/crypto-check/Cargo.toml`.

pub mod aes;
pub mod bignum;
pub mod gcm;
pub mod p256;
pub mod rsa;
pub mod sha256;
pub mod x25519;

pub use gcm::Aes128Gcm;
pub use sha256::{hkdf_expand_label, hkdf_extract, hmac_sha256, sha256, Sha256};
pub use x25519::x25519;
