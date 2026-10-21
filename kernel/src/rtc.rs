//! Чтение даты и времени из CMOS RTC.
//!
//! Используется для таймстампов в наборах записей exFAT. Время берётся
//! локальное, как его хранит RTC; UTC-смещение в записях не заполняется.

use core::arch::asm;

const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

/// Запасное значение: 1 января 2024 года, 00:00:00.
const DEFAULT_TIMESTAMP: u32 = pack(2024, 1, 1, 0, 0, 0);

fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn inb(port: u16) -> u8 {
    let value: u8;

    unsafe {
        asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }

    value
}

fn cmos(index: u8) -> u8 {
    outb(CMOS_INDEX, index);
    inb(CMOS_DATA)
}

fn bcd_to_binary(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0x0f)
}

/// Упаковывает дату и время в формат timestamp exFAT.
pub const fn pack(
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> u32 {
    ((year.saturating_sub(1980)) << 25)
        | (month << 21)
        | (day << 16)
        | (hour << 11)
        | (minute << 5)
        | (second / 2)
}

fn read_rtc() -> Option<(u32, u32, u32, u32, u32, u32)> {
    // Ждём, пока RTC не начнёт обновление регистров.
    let mut ready = false;

    for _ in 0..100_000 {
        if cmos(0x0a) & 0x80 == 0 {
            ready = true;
            break;
        }
    }

    if !ready {
        return None;
    }

    let seconds = cmos(0x00);
    let minutes = cmos(0x02);
    let hours = cmos(0x04);
    let day = cmos(0x07);
    let month = cmos(0x08);
    let year = cmos(0x09);
    let status_b = cmos(0x0b);

    let binary = status_b & 0x04 != 0;
    let hour_24 = status_b & 0x02 != 0;

    let convert = |value: u8| if binary { value } else { bcd_to_binary(value) };

    let mut seconds = convert(seconds) as u32;
    let minutes = convert(minutes) as u32;
    let mut hours = hours;

    if !hour_24 {
        let pm = hours & 0x80 != 0;
        hours &= 0x7f;
        hours = convert(hours);

        if pm {
            hours = hours % 12 + 12;
        }
    } else {
        hours = convert(hours);
    }

    let day = convert(day) as u32;
    let month = convert(month) as u32;
    let short_year = convert(year) as u32;

    let year = if short_year < 80 {
        2000 + short_year
    } else {
        1900 + short_year
    };

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hours >= 24
        || minutes >= 60
        || seconds >= 60
        || year < 1980
    {
        return None;
    }

    seconds &= !1; // exFAT хранит секунды с шагом 2
    let hours = hours as u32;

    Some((year, month, day, hours, minutes, seconds))
}

/// Текущее время в формате timestamp exFAT.
pub fn timestamp() -> u32 {
    match read_rtc() {
        Some((year, month, day, hour, minute, second)) => {
            pack(year, month, day, hour, minute, second)
        }
        None => DEFAULT_TIMESTAMP,
    }
}
