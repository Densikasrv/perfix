//! AES-128-GCM (NIST SP 800-38D) с 96-битным nonce — шифр TLS 1.3
//! `TLS_AES_128_GCM_SHA256`.

use super::aes::Aes128;

/// Дополнительный множитель GHASH: R = 0xe1 || 0^120.
const R: u8 = 0xe1;

pub struct Aes128Gcm {
    aes: Aes128,
    h: [u8; 16],
}

impl Aes128Gcm {
    pub fn new(key: &[u8; 16]) -> Self {
        let aes = Aes128::new(key);
        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);
        Self { aes, h }
    }

    /// Счётчик J0: nonce || 0x00000001.
    fn j0(nonce: &[u8; 12]) -> [u8; 16] {
        let mut block = [0u8; 16];
        block[..12].copy_from_slice(nonce);
        block[15] = 1;
        block
    }

    fn ctr(&self, counter: &mut [u8; 16], data: &mut [u8]) {
        let mut offset = 0usize;

        while offset < data.len() {
            let mut keystream = *counter;
            self.aes.encrypt_block(&mut keystream);

            let take = (data.len() - offset).min(16);

            for index in 0..take {
                data[offset + index] ^= keystream[index];
            }

            offset += take;

            // Инкремент младших 32 бит счётчика.
            for index in (12..16).rev() {
                counter[index] = counter[index].wrapping_add(1);

                if counter[index] != 0 {
                    break;
                }
            }
        }
    }

    fn ghash_update(y: &mut [u8; 16], h: &[u8; 16], data: &[u8]) {
        let mut offset = 0usize;

        while offset < data.len() {
            let take = (data.len() - offset).min(16);
            let mut block = [0u8; 16];
            block[..take].copy_from_slice(&data[offset..offset + take]);

            for index in 0..16 {
                y[index] ^= block[index];
            }

            Self::ghash_mul(y, h);
            offset += take;
        }
    }

    /// Умножение в GF(2^128) по алгоритму NIST (бит за битом).
    fn ghash_mul(x: &mut [u8; 16], h: &[u8; 16]) {
        let mut z = [0u8; 16];
        let mut v = *h;

        for bit in 0..128 {
            let set = (x[bit / 8] >> (7 - (bit % 8))) & 1 != 0;

            if set {
                for index in 0..16 {
                    z[index] ^= v[index];
                }
            }

            let low = v[15] & 1 != 0;

            // v >>= 1
            for index in (1..16).rev() {
                v[index] = (v[index] >> 1) | (v[index - 1] << 7);
            }

            v[0] >>= 1;

            if low {
                v[0] ^= R;
            }
        }

        *x = z;
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
        let mut y = [0u8; 16];

        Self::ghash_update(&mut y, &self.h, aad);
        Self::ghash_update(&mut y, &self.h, ciphertext);

        let mut lengths = [0u8; 16];
        lengths[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
        lengths[8..].copy_from_slice(&((ciphertext.len() as u64) * 8).to_be_bytes());

        for index in 0..16 {
            y[index] ^= lengths[index];
        }

        Self::ghash_mul(&mut y, &self.h);

        let mut mask = Self::j0(nonce);
        self.aes.encrypt_block(&mut mask);

        for index in 0..16 {
            mask[index] ^= y[index];
        }

        mask
    }

    /// Шифрует данные и вычисляет тег.
    pub fn encrypt(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8], tag: &mut [u8; 16]) {
        let mut counter = Self::j0(nonce);
        counter[12..16].copy_from_slice(&1u32.to_be_bytes());
        // Первый блок счётчика для данных — J0 + 1.
        let mut data_counter = counter;

        for index in (12..16).rev() {
            data_counter[index] = data_counter[index].wrapping_add(1);

            if data_counter[index] != 0 {
                break;
            }
        }

        self.ctr(&mut data_counter, data);
        *tag = self.tag(nonce, aad, data);
    }

    /// Расшифровывает данные и проверяет тег. Возвращает `false`, если тег
    /// не совпал (данные в этом случае уже испорчены — их нельзя отдавать).
    pub fn decrypt(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        data: &mut [u8],
        tag: &[u8; 16],
    ) -> bool {
        let expected = self.tag(nonce, aad, data);

        let mut difference = 0u8;

        for index in 0..16 {
            difference |= expected[index] ^ tag[index];
        }

        if difference != 0 {
            return false;
        }

        let mut counter = Self::j0(nonce);

        for index in (12..16).rev() {
            counter[index] = counter[index].wrapping_add(1);

            if counter[index] != 0 {
                break;
            }
        }

        self.ctr(&mut counter, data);
        true
    }
}
