//! ECDSA на кривой P-256 (secp256r1, prime256v1) с SHA-256.
//!
//! Нужен для проверки цепочек сертификатов и CertificateVerify у сайтов
//! с ECDSA-ключами (так работают example.com, Wikipedia и большинство
//! современных площадок). Точки считаются в якобиевых координатах, поле и
//! порядок группы — через арифметику Монтгомери.
//!
//! Проверяется тест-векторами (tools/crypto-check) и реальными сайтами.

use super::bignum::{add, cmp, from_be_bytes, sub, Montgomery};
use super::sha256::sha256;
use core::cmp::Ordering;

type Scalar = [u64; 4];

/// p = 2^256 - 2^224 + 2^192 + 2^96 - 1.
const P: Scalar = [
    0xFFFF_FFFF_FFFF_FFFF,
    0x0000_0000_FFFF_FFFF,
    0x0000_0000_0000_0000,
    0xFFFF_FFFF_0000_0001,
];

/// Порядок группы n.
const N: Scalar = [
    0xF3B9_CAC2_FC63_2551,
    0xBCE6_FAAD_A717_9E84,
    0xFFFF_FFFF_FFFF_FFFF,
    0xFFFF_FFFF_0000_0000,
];

/// Базовая точка G.
const GX: Scalar = [
    0xF4A1_3945_D898_C296,
    0x7703_7D81_2DEB_33A0,
    0xF8BC_E6E5_63A4_40F2,
    0x6B17_D1F2_E12C_4247,
];

const GY: Scalar = [
    0xCBB6_4068_37BF_51F5,
    0x2BCE_3357_6B31_5ECE,
    0x8EE7_EB4A_7C0F_9E16,
    0x4FE3_42E2_FE1A_7F9B,
];

/// Точка в якобиевых координатах; координаты — в форме Монтгомери.
#[derive(Clone, Copy)]
struct Point {
    x: Scalar,
    y: Scalar,
    z: Scalar,
    infinity: bool,
}

impl Point {
    const INFINITY: Self = Self {
        x: [0; 4],
        y: [0; 4],
        z: [0; 4],
        infinity: true,
    };
}

fn is_zero(value: &Scalar) -> bool {
    value.iter().all(|&limb| limb == 0)
}

/// p - 2 и n - 2: показатели для обращения в поле и в группе.
fn inverted_p() -> Scalar {
    let mut exponent = P;
    exponent[0] -= 2;
    exponent
}

fn inverted_n() -> Scalar {
    let mut exponent = N;
    exponent[0] -= 2;
    exponent
}


/// Арифметика кривой P-256.
struct Curve {
    field: Montgomery<4>,
    order: Montgomery<4>,
}

impl Curve {
    fn new() -> Option<Self> {
        Some(Self {
            field: Montgomery::new(&P)?,
            order: Montgomery::new(&N)?,
        })
    }

    fn add_field(&self, a: &Scalar, b: &Scalar) -> Scalar {
        let mut sum = [0u64; 4];
        let carry = add(a, b, &mut sum);

        if carry != 0 || cmp(&sum, &P) != Ordering::Less {
            let wide = sum;
            sub(&wide, &P, &mut sum);
        }

        sum
    }

    fn sub_field(&self, a: &Scalar, b: &Scalar) -> Scalar {
        if cmp(a, b) != Ordering::Less {
            let mut difference = [0u64; 4];
            sub(a, b, &mut difference);
            return difference;
        }

        // a - b + p
        let mut adjusted = [0u64; 4];
        sub(b, a, &mut adjusted);
        let mut difference = [0u64; 4];
        sub(&P, &adjusted, &mut difference);
        difference
    }

    /// Умножение по модулю p (аргументы и результат — в форме Монтгомери).
    fn mul_field(&self, a: &Scalar, b: &Scalar) -> Scalar {
        self.field.multiply(a, b)
    }

    fn to_field(&self, value: &Scalar) -> Scalar {
        self.field.to_montgomery(value)
    }

    fn from_field(&self, value: &Scalar) -> Scalar {
        self.field.from_montgomery(value)
    }

    fn times2(&self, value: &Scalar) -> Scalar {
        self.add_field(value, value)
    }

    fn times3(&self, value: &Scalar) -> Scalar {
        let doubled = self.times2(value);
        self.add_field(&doubled, value)
    }

    fn times4(&self, value: &Scalar) -> Scalar {
        let doubled = self.times2(value);
        self.times2(&doubled)
    }

    fn times8(&self, value: &Scalar) -> Scalar {
        let doubled = self.times4(value);
        self.times2(&doubled)
    }

    /// Удвоение точки (a = -3, формулы dbl-2001-b).
    fn double(&self, point: &Point) -> Point {
        if point.infinity || is_zero(&point.y) {
            return Point::INFINITY;
        }

        let delta = self.mul_field(&point.z, &point.z);
        let gamma = self.mul_field(&point.y, &point.y);
        let beta = self.mul_field(&point.x, &gamma);

        let left = self.sub_field(&point.x, &delta);
        let right = self.add_field(&point.x, &delta);
        let alpha = self.times3(&self.mul_field(&left, &right));

        let x = self.sub_field(&self.mul_field(&alpha, &alpha), &self.times8(&beta));

        let z = {
            let sum = self.add_field(&point.y, &point.z);
            let square = self.mul_field(&sum, &sum);
            let difference = self.sub_field(&square, &gamma);
            self.sub_field(&difference, &delta)
        };

        let y = self.sub_field(
            &self.mul_field(&alpha, &self.sub_field(&self.times4(&beta), &x)),
            &self.times8(&self.mul_field(&gamma, &gamma)),
        );

        Point {
            x,
            y,
            z,
            infinity: false,
        }
    }
}

impl Curve {
    /// R = u1*G + u2*Q в якобиевых координатах.
    fn combine(&self, u1: &Scalar, u2: &Scalar, q: &Point) -> Option<Point> {
        let one = [1u64, 0, 0, 0];

        let g = Point {
            x: self.to_field(&GX),
            y: self.to_field(&GY),
            z: self.to_field(&one),
            infinity: false,
        };

        let first = self.normalize(&self.multiply(u1, &g))?;
        let second = self.normalize(&self.multiply(u2, q))?;
        Some(self.add_affine(&first, &second))
    }

    /// Приводит точку к z = 1 (аффинное представление в форме Монтгомери).
    fn normalize(&self, point: &Point) -> Option<Point> {
        if point.infinity || is_zero(&point.z) {
            return None;
        }

        let z = self.field.power(&self.from_field(&point.z), &inverted_p());
        let z = self.to_field(&z);
        let z_squared = self.mul_field(&z, &z);

        Some(Point {
            x: self.mul_field(&point.x, &z_squared),
            y: self.mul_field(&point.y, &self.mul_field(&z_squared, &z)),
            z: self.to_field(&[1u64, 0, 0, 0]),
            infinity: false,
        })
    }

    /// Прибавление аффинной точки к якобиевой (mixed addition).
    fn add_affine(&self, point: &Point, other: &Point) -> Point {
        if point.infinity {
            return *other;
        }

        if other.infinity {
            return *point;
        }

        let z_squared = self.mul_field(&point.z, &point.z);
        let u2 = self.mul_field(&other.x, &z_squared);
        let s2 = self.mul_field(&self.mul_field(&other.y, &point.z), &z_squared);

        let h = self.sub_field(&u2, &point.x);
        let r = self.sub_field(&s2, &point.y);

        if is_zero(&h) {
            if is_zero(&r) {
                return self.double(point);
            }

            return Point::INFINITY;
        }

        let h_squared = self.mul_field(&h, &h);
        let h_cubed = self.mul_field(&h_squared, &h);
        let v = self.mul_field(&point.x, &h_squared);

        let x = self.sub_field(
            &self.sub_field(&self.mul_field(&r, &r), &h_cubed),
            &self.times2(&v),
        );

        let y = self.sub_field(
            &self.mul_field(&r, &self.sub_field(&v, &x)),
            &self.mul_field(&point.y, &h_cubed),
        );

        Point {
            x,
            y,
            z: self.mul_field(&point.z, &h),
            infinity: false,
        }
    }

    /// Умножение точки на скаляр (двойное с прибавлением).
    fn multiply(&self, scalar: &Scalar, point: &Point) -> Point {
        let mut result = Point::INFINITY;
        let mut started = false;

        for bit in (0..256).rev() {
            let set = (scalar[bit / 64] >> (bit % 64)) & 1 != 0;

            if !started {
                if !set {
                    continue;
                }

                started = true;
                result = *point;
                continue;
            }

            result = self.double(&result);

            if set {
                result = self.add_affine(&result, point);
            }
        }

        result
    }

    fn scalar_from_bytes(bytes: &[u8; 32]) -> Scalar {
        let mut value = [0u64; 4];
        from_be_bytes(bytes, &mut value);
        value
    }

    fn scalar_from_slice(bytes: &[u8]) -> Option<Scalar> {
        if bytes.len() > 32 {
            return None;
        }

        let mut padded = [0u8; 32];
        padded[32 - bytes.len()..].copy_from_slice(bytes);
        Some(Self::scalar_from_bytes(&padded))
    }

    fn reduce_mod_order(&self, value: &Scalar) -> Scalar {
        if cmp(value, &N) == Ordering::Less {
            *value
        } else {
            let mut reduced = [0u64; 4];
            sub(value, &N, &mut reduced);
            reduced
        }
    }

    fn mul_mod_order(&self, a: &Scalar, b: &Scalar) -> Scalar {
        let left = self.order.to_montgomery(a);
        let right = self.order.to_montgomery(b);
        self.order.from_montgomery(&self.order.multiply(&left, &right))
    }

    fn invert_mod_order(&self, value: &Scalar) -> Scalar {
        self.order.power(value, &inverted_n())
    }
}

/// Читает один DER-элемент: (тег, значение).
fn der_element<'a>(data: &'a [u8], position: &mut usize) -> Option<(u8, &'a [u8])> {
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
    Some((tag, &data[body..body + length]))
}

fn strip_leading_zero(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;

    while start + 1 < bytes.len() && bytes[start] == 0 {
        start += 1;
    }

    &bytes[start..]
}

/// Разбирает DER-подпись ECDSA: SEQUENCE { r INTEGER, s INTEGER }.
fn parse_signature(der: &[u8]) -> Option<(Scalar, Scalar)> {
    let mut position = 0;
    let (tag, sequence) = der_element(der, &mut position)?;

    if tag != 0x30 {
        return None;
    }

    let mut inner = 0;
    let (r_tag, r) = der_element(sequence, &mut inner)?;
    let (s_tag, s) = der_element(sequence, &mut inner)?;

    if r_tag != 0x02 || s_tag != 0x02 {
        return None;
    }

    Some((
        Curve::scalar_from_slice(strip_leading_zero(r))?,
        Curve::scalar_from_slice(strip_leading_zero(s))?,
    ))
}

/// Проверяет подпись ECDSA P-256 по готовому SHA-256 хешу.
///
/// `public_key` — содержимое BIT STRING из SubjectPublicKeyInfo:
/// несжатая точка `0x04 || X || Y` (65 байт).
pub fn verify(public_key: &[u8], signature: &[u8], digest: &[u8; 32]) -> bool {
    let Some(curve) = Curve::new() else {
        return false;
    };

    if public_key.len() != 65 || public_key[0] != 0x04 {
        return false;
    }

    let mut x = [0u8; 32];
    let mut y = [0u8; 32];
    x.copy_from_slice(&public_key[1..33]);
    y.copy_from_slice(&public_key[33..65]);

    let Some((r, s)) = parse_signature(signature) else {
        return false;
    };

    if is_zero(&r)
        || is_zero(&s)
        || cmp(&r, &N) != Ordering::Less
        || cmp(&s, &N) != Ordering::Less
    {
        return false;
    }

    let e = curve.reduce_mod_order(&Curve::scalar_from_bytes(digest));
    let w = curve.invert_mod_order(&s);
    let u1 = curve.mul_mod_order(&e, &w);
    let u2 = curve.mul_mod_order(&r, &w);

    let public = Point {
        x: curve.to_field(&Curve::scalar_from_bytes(&x)),
        y: curve.to_field(&Curve::scalar_from_bytes(&y)),
        z: curve.to_field(&[1u64, 0, 0, 0]),
        infinity: false,
    };

    let Some(point) = curve.combine(&u1, &u2, &public) else {
        return false;
    };

    let Some(point) = curve.normalize(&point) else {
        return false;
    };

    let x_value = curve.from_field(&point.x);
    curve.reduce_mod_order(&x_value) == r
}

/// Проверяет подпись ECDSA P-256/SHA-256 по сообщению.
pub fn verify_sha256(public_key: &[u8], signature: &[u8], message: &[u8]) -> bool {
    verify(public_key, signature, &sha256(message))
}
