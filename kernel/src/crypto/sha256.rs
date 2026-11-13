//! SHA-256, HMAC-SHA256 и HKDF (RFC 6234, RFC 2104, RFC 5869).
//!
//! Реализация без аллокатора и без внешних крейтов: нужна ядру для TLS 1.3
//! и проверяется тест-векторами на хосте (tools/crypto-check).

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const INITIAL: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Инкрементальный SHA-256.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    used: usize,
    total: u64,
}

impl Sha256 {
    pub const fn new() -> Self {
        Self {
            state: INITIAL,
            block: [0; 64],
            used: 0,
            total: 0,
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];

        for index in 0..16 {
            let offset = index * 4;
            w[index] = ((block[offset] as u32) << 24)
                | ((block[offset + 1] as u32) << 16)
                | ((block[offset + 2] as u32) << 8)
                | block[offset + 3] as u32;
        }

        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);

        if self.used > 0 {
            let take = (64 - self.used).min(data.len());
            self.block[self.used..self.used + take].copy_from_slice(&data[..take]);
            self.used += take;
            data = &data[take..];

            if self.used < 64 {
                return;
            }

            let block = self.block;
            self.compress(&block);
            self.used = 0;
        }

        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }

        if !data.is_empty() {
            self.block[..data.len()].copy_from_slice(data);
            self.used = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        let mut tail = [0u8; 64];
        tail[..self.used].copy_from_slice(&self.block[..self.used]);
        tail[self.used] = 0x80;

        if self.used >= 56 {
            let block = tail;
            self.compress(&block);
            tail = [0; 64];
        }

        tail[56..64].copy_from_slice(&bits.to_be_bytes());
        let block = tail;
        self.compress(&block);

        let mut out = [0u8; 32];

        for index in 0..8 {
            out[index * 4..index * 4 + 4].copy_from_slice(&self.state[index].to_be_bytes());
        }

        out
    }
}


/// SHA-256 одной буферной строки.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(data);
    hash.finish()
}

/// HMAC-SHA256 от нескольких фрагментов данных (без склейки в буфер).
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; 64];

    if key.len() > 64 {
        let digest = sha256(key);
        block[..32].copy_from_slice(&digest);
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner = Sha256::new();
    let mut pad = [0u8; 64];

    for index in 0..64 {
        pad[index] = block[index] ^ 0x36;
    }

    inner.update(&pad);

    for part in parts {
        inner.update(part);
    }

    let inner_digest = inner.finish();

    let mut outer = Sha256::new();

    for index in 0..64 {
        pad[index] = block[index] ^ 0x5c;
    }

    outer.update(&pad);
    outer.update(&inner_digest);
    outer.finish()
}

/// HKDF-Extract (RFC 5869).
pub fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    hmac_sha256(salt, &[ikm])
}

/// HKDF-Expand (RFC 5869).
pub fn hkdf_expand(prk: &[u8; 32], info: &[u8], out: &mut [u8]) {
    let mut previous: [u8; 32] = [0; 32];
    let mut previous_len = 0usize;
    let mut counter = 1u8;
    let mut written = 0usize;

    while written < out.len() {
        let counter_byte = [counter];
        let digest = hmac_sha256(
            prk,
            &[&previous[..previous_len], info, &counter_byte],
        );
        let take = (out.len() - written).min(32);
        out[written..written + take].copy_from_slice(&digest[..take]);
        written += take;
        previous = digest;
        previous_len = 32;
        counter = counter.wrapping_add(1);
    }
}

/// HKDF-Expand-Label из TLS 1.3 (RFC 8446, раздел 7.1).
pub fn hkdf_expand_label(secret: &[u8; 32], label: &[u8], context: &[u8], out: &mut [u8]) {
    let mut info = [0u8; 96];
    let mut length = 2usize;

    info[0..2].copy_from_slice(&(out.len() as u16).to_be_bytes());
    let full_label = b"tls13 ";
    info[length] = (full_label.len() + label.len()) as u8;
    length += 1;
    info[length..length + full_label.len()].copy_from_slice(full_label);
    length += full_label.len();
    info[length..length + label.len()].copy_from_slice(label);
    length += label.len();
    info[length] = context.len() as u8;
    length += 1;
    info[length..length + context.len()].copy_from_slice(context);
    length += context.len();

    hkdf_expand(secret, &info[..length], out);
}
