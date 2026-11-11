//! Миллисекундный таймер на основе счётчика тактов процессора (TSC).
//!
//! TSC не имеет фиксированной частоты по стандарту, поэтому её надо
//! измерить. Основной способ — калибровка по каналу 2 таймера PIT
//! (частота 1,193182 МГц): это быстро и не зависит от RTC. Если PIT
//! недоступен, частота измеряется по секундам RTC.

use crate::port::{inb, outb};
use crate::rtc;
use core::arch::asm;

const PIT_FREQ: u64 = 1_193_182;
/// Делитель канала 2 PIT: примерно 10 мс.
const CALIBRATION_DIVISOR: u16 = 11_932;

static mut TICKS_PER_SECOND: u64 = 0;

/// Читает счётчик тактов процессора.
pub fn rdtsc() -> u64 {
    let low: u32;
    let high: u32;

    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }

    ((high as u64) << 32) | low as u64
}

/// Калибрует TSC по каналу 2 PIT. Возвращает 0 при неудаче.
fn calibrate_pit() -> u64 {
    // Порт 0x61: бит 0 — гейт канала 2, бит 1 — динамик.
    let saved = inb(0x61);
    outb(0x61, (saved & 0xFC) | 0x01);

    // Канал 2, режим 0 (interrupt on terminal count), доступ lo/hi.
    outb(0x43, 0xB0);
    outb(0x42, (CALIBRATION_DIVISOR & 0xFF) as u8);
    outb(0x42, (CALIBRATION_DIVISOR >> 8) as u8);

    let start = rdtsc();
    let mut timeout = 50_000_000u32;

    // Ждём, пока выход канала 2 перейдёт в высокий уровень.
    while inb(0x61) & 0x20 == 0 {
        if timeout == 0 {
            break;
        }

        timeout -= 1;
        core::hint::spin_loop();
    }

    let end = rdtsc();

    let current = inb(0x61);
    outb(0x61, current & 0xFC);

    if timeout == 0 {
        return 0;
    }

    let delta = end.wrapping_sub(start);

    if delta == 0 {
        return 0;
    }

    delta.saturating_mul(PIT_FREQ) / CALIBRATION_DIVISOR as u64
}

/// Запасная калибровка по двум переходам секунды RTC.
fn calibrate_rtc() -> u64 {
    let second = rtc::second();

    while rtc::second() == second {
        core::hint::spin_loop();
    }

    let start = rdtsc();
    let second = rtc::second();

    while rtc::second() == second {
        core::hint::spin_loop();
    }

    rdtsc().wrapping_sub(start)
}

fn ensure_calibrated() {
    if unsafe { TICKS_PER_SECOND } != 0 {
        return;
    }

    let measured = calibrate_pit();

    let frequency = if measured > 1_000_000 {
        measured
    } else {
        calibrate_rtc().max(1_000_000)
    };

    unsafe {
        TICKS_PER_SECOND = frequency;
    }
}

/// Выполняет калибровку заранее, чтобы первый вызов не тормозил.
pub fn init() {
    ensure_calibrated();
}

/// Текущее значение счётчика тактов.
pub fn ticks() -> u64 {
    rdtsc()
}

/// Десятые доли миллисекунды с момента `start` (то есть 0.1 мс).
pub fn tenth_millis_since(start: u64) -> u64 {
    ensure_calibrated();

    let frequency = unsafe { TICKS_PER_SECOND };

    if frequency == 0 {
        return 0;
    }

    rdtsc().wrapping_sub(start).saturating_mul(10_000) / frequency
}
