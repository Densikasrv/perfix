//! Большие числа для проверки подписей RSA (и ECDSA в перспективе).
//!
//! Числа хранятся массивами 64-битных слов, младшее слово первым.
//! Умножение по модулю — Монтгомери (модуль обязан быть нечётным), так что
//! 2048-битное возведение в степень выполняется без деления.
//!
//! Проверяется на хосте (tools/crypto-check).

use core::cmp::Ordering;

/// Сложение с переносом; возвращает вынесенный бит.
pub fn add(a: &[u64], b: &[u64], out: &mut [u64]) -> u64 {
    let mut carry = 0u128;

    for index in 0..a.len() {
        let sum = a[index] as u128 + b[index] as u128 + carry;
        out[index] = sum as u64;
        carry = sum >> 64;
    }

    carry as u64
}

/// Вычитание с займом (предполагается `a >= b`).
pub fn sub(a: &[u64], b: &[u64], out: &mut [u64]) -> u64 {
    let mut borrow = 0i128;

    for index in 0..a.len() {
        let difference = a[index] as i128 - b[index] as i128 - borrow;
        out[index] = difference as u64;
        borrow = if difference < 0 { 1 } else { 0 };
    }

    borrow as u64
}

pub fn cmp(a: &[u64], b: &[u64]) -> Ordering {
    for index in (0..a.len()).rev() {
        if a[index] != b[index] {
            return a[index].cmp(&b[index]);
        }
    }

    Ordering::Equal
}

/// Сдвиг влево на один бит; возвращает вынесенный бит.
pub fn shift_left(a: &[u64], out: &mut [u64]) -> u64 {
    let mut carry = 0u64;

    for index in 0..a.len() {
        out[index] = (a[index] << 1) | carry;
        carry = a[index] >> 63;
    }

    carry
}

pub fn from_be_bytes(bytes: &[u8], out: &mut [u64]) {
    for limb in out.iter_mut() {
        *limb = 0;
    }

    for (index, byte) in bytes.iter().rev().enumerate() {
        out[index / 8] |= (*byte as u64) << ((index % 8) * 8);
    }
}

pub fn to_be_bytes(value: &[u64], out: &mut [u8]) {
    let total = out.len();

    for (index, byte) in out.iter_mut().enumerate() {
        let position = total - 1 - index;
        *byte = (value[position / 8] >> ((position % 8) * 8)) as u8;
    }
}


/// Модуль для арифметики Монтгомери (не более 4096 бит).
pub struct Montgomery<const N: usize> {
    modulus: [u64; N],
    /// -n^(-1) mod 2^64.
    factor: u64,
}

impl<const N: usize> Montgomery<N> {
    pub fn new(modulus: &[u64; N]) -> Option<Self> {
        if modulus[0] & 1 == 0 {
            return None;
        }

        // Обратный элемент по методу Ньютона, затем смена знака.
        let mut inverse = 1u64;

        for _ in 0..6 {
            inverse = inverse.wrapping_mul(2u64.wrapping_sub(modulus[0].wrapping_mul(inverse)));
        }

        Some(Self {
            modulus: *modulus,
            factor: inverse.wrapping_neg(),
        })
    }

    /// R mod n, где R = 2^(64N).
    fn montgomery_one(&self) -> [u64; N] {
        let mut value = [0u64; N];
        value[0] = 1;

        for _ in 0..(N * 64) {
            let mut shifted = [0u64; N];
            let carry = shift_left(&value, &mut shifted);

            if carry != 0 || cmp(&shifted, &self.modulus) != Ordering::Less {
                sub(&shifted, &self.modulus, &mut value);
            } else {
                value = shifted;
            }
        }

        value
    }

    /// R^2 mod n.
    fn montgomery_radix(&self) -> [u64; N] {
        let mut value = self.montgomery_one();

        for _ in 0..(N * 64) {
            let mut shifted = [0u64; N];
            let carry = shift_left(&value, &mut shifted);

            if carry != 0 || cmp(&shifted, &self.modulus) != Ordering::Less {
                sub(&shifted, &self.modulus, &mut value);
            } else {
                value = shifted;
            }
        }

        value
    }

    /// Умножение по модулю в форме Монтгомери (алгоритм CIOS).
    pub fn multiply(&self, a: &[u64; N], b: &[u64; N]) -> [u64; N] {
        let mut work = [0u64; 2 * 64 + 2];
        let length = 2 * N;

        for i in 0..N {
            let mut carry = 0u128;

            for j in 0..N {
                let sum = work[j] as u128 + (a[j] as u128) * (b[i] as u128) + carry;
                work[j] = sum as u64;
                carry = sum >> 64;
            }

            let mut position = N;

            while carry != 0 {
                let sum = work[position] as u128 + carry;
                work[position] = sum as u64;
                carry = sum >> 64;
                position += 1;
            }

            let factor = work[0].wrapping_mul(self.factor);
            let mut carry = 0u128;

            for j in 0..N {
                let sum = work[j] as u128 + (factor as u128) * (self.modulus[j] as u128) + carry;
                work[j] = sum as u64;
                carry = sum >> 64;
            }

            let mut position = N;

            while carry != 0 {
                let sum = work[position] as u128 + carry;
                work[position] = sum as u64;
                carry = sum >> 64;
                position += 1;
            }

            for j in 0..length {
                work[j] = work[j + 1];
            }

            work[length] = 0;
        }

        let mut result = [0u64; N];
        result.copy_from_slice(&work[..N]);
        let high = work[N];

        if high != 0 || cmp(&result, &self.modulus) != Ordering::Less {
            let wide = result;
            sub(&wide, &self.modulus, &mut result);
        }

        result
    }

    pub fn to_montgomery(&self, value: &[u64; N]) -> [u64; N] {
        self.multiply(value, &self.montgomery_radix())
    }

    pub fn from_montgomery(&self, value: &[u64; N]) -> [u64; N] {
        let mut one = [0u64; N];
        one[0] = 1;
        self.multiply(value, &one)
    }

    /// `base^exponent mod n`.
    pub fn power(&self, base: &[u64; N], exponent: &[u64; N]) -> [u64; N] {
        let mut result = [0u64; N];
        let base = self.to_montgomery(base);

        // Начинаем со старшего значащего бита: у типичного exponent 65537
        // это 17 шагов вместо 2048.
        let mut started = false;

        for bit in (0..(N * 64)).rev() {
            let set = (exponent[bit / 64] >> (bit % 64)) & 1 != 0;

            if !started {
                if !set {
                    continue;
                }

                started = true;
                result = base;
                continue;
            }

            result = self.multiply(&result, &result);

            if set {
                result = self.multiply(&result, &base);
            }
        }

        if !started {
            result = self.montgomery_one();
        }

        self.from_montgomery(&result)
    }
}
