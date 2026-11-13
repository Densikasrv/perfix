//! X25519 (RFC 7748) — обмен ключами ECDHE для TLS 1.3.
//!
//! Портировано с компактной эталонной реализации TweetNaCl: поле
//! GF(2^255-19) представлено 16 знаковыми 16-битными «конечностями»,
//! что удобно для 32-битного ядра без 64x64->128 умножений.
//! Проверяется тест-векторами RFC 7748 на хосте (tools/crypto-check).

type Gf = [i64; 16];

/// Число 121665 из формулы Монтгомери.
const A24: Gf = [0xDB41, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

fn carry(o: &mut Gf) {
    for index in 0..16 {
        o[index] += 1 << 16;
        let c = o[index] >> 16;
        let next = if index < 15 { index + 1 } else { 0 };
        let extra = if index == 15 { 37 * (c - 1) } else { 0 };
        o[next] += c - 1 + extra;
        o[index] -= c << 16;
    }
}

fn add(out: &mut Gf, left: &Gf, right: &Gf) {
    for index in 0..16 {
        out[index] = left[index] + right[index];
    }
}

fn sub(out: &mut Gf, left: &Gf, right: &Gf) {
    for index in 0..16 {
        out[index] = left[index] - right[index];
    }
}

fn mul(out: &mut Gf, left: &Gf, right: &Gf) {
    let mut product = [0i64; 31];

    for i in 0..16 {
        for j in 0..16 {
            product[i + j] += left[i] * right[j];
        }
    }

    for index in 0..15 {
        product[index] += 38 * product[index + 16];
    }

    for index in 0..16 {
        out[index] = product[index];
    }

    carry(out);
    carry(out);
}

fn square(out: &mut Gf, value: &Gf) {
    let copy = *value;
    mul(out, &copy, &copy);
}

/// Условный обмен: при `swap == 1` меняет `left` и `right`.
fn select(left: &mut Gf, right: &mut Gf, swap: i64) {
    let mask = !(swap - 1);

    for index in 0..16 {
        let difference = mask & (left[index] ^ right[index]);
        left[index] ^= difference;
        right[index] ^= difference;
    }
}

fn unpack(bytes: &[u8; 32]) -> Gf {
    let mut value = [0i64; 16];

    for index in 0..16 {
        value[index] = bytes[2 * index] as i64 + ((bytes[2 * index + 1] as i64) << 8);
    }

    value[15] &= 0x7fff;
    value
}

fn pack(out: &mut [u8; 32], value: &Gf) {
    let mut reduced = *value;
    carry(&mut reduced);
    carry(&mut reduced);
    carry(&mut reduced);

    for _ in 0..2 {
        let mut candidate = [0i64; 16];
        candidate[0] = reduced[0] - 0xffed;

        for index in 1..15 {
            candidate[index] = reduced[index] - 0xffff - ((candidate[index - 1] >> 16) & 1);
            candidate[index - 1] &= 0xffff;
        }

        candidate[15] = reduced[15] - 0x7fff - ((candidate[14] >> 16) & 1);
        let borrow = (candidate[15] >> 16) & 1;
        candidate[14] &= 0xffff;
        select(&mut reduced, &mut candidate, 1 - borrow);
    }

    for index in 0..16 {
        out[2 * index] = (reduced[index] & 0xff) as u8;
        out[2 * index + 1] = ((reduced[index] >> 8) & 0xff) as u8;
    }
}

/// Обращение по модулю p через возведение в степень p-2.
fn invert(out: &mut Gf, value: &Gf) {
    let mut result = *value;

    for exponent in (0..=253).rev() {
        let copy = result;
        square(&mut result, &copy);

        if exponent != 2 && exponent != 4 {
            let copy = result;
            mul(&mut result, &copy, value);
        }
    }

    *out = result;
}

/// Умножение точки на скаляр (Montgomery ladder).
pub fn x25519(scalar: &[u8; 32], point: &[u8; 32]) -> [u8; 32] {
    let mut clamped = *scalar;
    clamped[31] = (clamped[31] & 127) | 64;
    clamped[0] &= 248;

    let x = unpack(point);

    let mut a = [0i64; 16];
    let mut b = x;
    let mut c = [0i64; 16];
    let mut d = [0i64; 16];
    a[0] = 1;
    d[0] = 1;

    let mut e = [0i64; 16];
    let mut f = [0i64; 16];

    for bit in (0..=254).rev() {
        let swap = ((clamped[bit >> 3] >> (bit & 7)) & 1) as i64;
        select(&mut a, &mut b, swap);
        select(&mut c, &mut d, swap);

        let (a0, b0, c0, d0) = (a, b, c, d);

        add(&mut e, &a0, &c0);
        sub(&mut a, &a0, &c0);
        add(&mut c, &b0, &d0);
        sub(&mut b, &b0, &d0);
        square(&mut d, &e);
        square(&mut f, &a);

        let a1 = a;
        mul(&mut a, &c, &a1);
        let b1 = b;
        mul(&mut c, &b1, &e);
        add(&mut e, &a, &c);
        let a2 = a;
        sub(&mut a, &a2, &c);
        let a3 = a;
        square(&mut b, &a3);

        let f0 = f;
        sub(&mut c, &d, &f0);
        let c1 = c;
        mul(&mut a, &c1, &A24);
        let d1 = d;
        let a5 = a;
        add(&mut a, &a5, &d1);
        let c2 = c;
        let a4 = a;
        mul(&mut c, &c2, &a4);
        let d2 = d;
        let f1 = f;
        mul(&mut a, &d2, &f1);
        let b2 = b;
        mul(&mut d, &b2, &x);
        let e0 = e;
        square(&mut b, &e0);

        select(&mut a, &mut b, swap);
        select(&mut c, &mut d, swap);
    }

    let c0 = c;
    invert(&mut c, &c0);
    let a1 = a;
    mul(&mut a, &a1, &c);

    let mut out = [0u8; 32];
    pack(&mut out, &a);
    out
}
