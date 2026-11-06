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

fn binary_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 31,
    }
}

/// Прибавляет минуты к дате и времени с переносом через сутки, месяц и год.
fn add_minutes(
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    delta: i32,
) -> (u32, u32, u32, u32, u32) {
    let mut year = year as i32;
    let mut month = month as i32;
    let mut day = day as i32;
    let mut total = hour as i32 * 60 + minute as i32 + delta;

    while total < 0 {
        total += 24 * 60;
        day -= 1;

        if day < 1 {
            month -= 1;

            if month < 1 {
                month = 12;
                year -= 1;
            }

            day = days_in_month(year, month);
        }
    }

    while total >= 24 * 60 {
        total -= 24 * 60;
        day += 1;

        if day > days_in_month(year, month) {
            day = 1;
            month += 1;

            if month > 12 {
                month = 1;
                year += 1;
            }
        }
    }

    (
        year.max(0) as u32,
        month as u32,
        day as u32,
        (total / 60) as u32,
        (total % 60) as u32,
    )
}

/// Смещение локального времени относительно RTC в минутах.
static mut TIMEZONE_MINUTES: i32 = 0;

/// Текущий часовой пояс в минутах (например, 180 для GMT+3).
pub fn timezone_minutes() -> i32 {
    unsafe { TIMEZONE_MINUTES }
}

/// Устанавливает смещение часового пояса в минутах.
pub fn set_timezone_minutes(minutes: i32) {
    unsafe {
        TIMEZONE_MINUTES = minutes.clamp(-12 * 60, 14 * 60);
    }
}

/// Переводит локальное время в UTC для записи в RTC.
pub fn local_to_utc(
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
) -> (u32, u32, u32, u32, u32) {
    add_minutes(year, month, day, hour, minute, -timezone_minutes())
}

/// Секунды RTC (0..59) в двоичном виде.
///
/// Используются для грубой калибровки счётчика TSC в pdesk.
pub fn second() -> u32 {
    let status_b = cmos(0x0b);
    let value = cmos(0x00);
    let value = if status_b & 0x04 != 0 {
        value
    } else {
        bcd_to_binary(value)
    };

    (value % 60) as u32
}

fn write_cmos(index: u8, value: u8) {
    outb(CMOS_INDEX, index);
    outb(CMOS_DATA, value);
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

/// Текущие дата и время: (год, месяц, день, часы, минуты).
///
/// Учитывает установленный часовой пояс (см. [`set_timezone_minutes`]).
/// Используется часами в доке pdesk; секунды не нужны.
pub fn now() -> Option<(u32, u32, u32, u32, u32)> {
    read_rtc().map(|(year, month, day, hour, minute, _second)| {
        add_minutes(year, month, day, hour, minute, timezone_minutes())
    })
}

/// Записывает дату и время в RTC (значения уже в UTC).
///
/// Возвращает `false`, если значения некорректны.
pub fn set_datetime(
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> bool {
    let valid = (1980..=2099).contains(&year)
        && (1..=12).contains(&month)
        && day >= 1
        && day <= days_in_month(year as i32, month as i32) as u32
        && hour < 24
        && minute < 60
        && second < 60;

    if !valid {
        return false;
    }

    // Дожидаемся, пока RTC не начнёт обновление регистров.
    for _ in 0..100_000 {
        if cmos(0x0a) & 0x80 == 0 {
            break;
        }
    }

    let status_b = cmos(0x0b);
    let binary = status_b & 0x04 != 0;
    let hour_24 = status_b & 0x02 != 0;

    // Бит SET запрещает обновление регистров на время записи.
    write_cmos(0x0b, status_b | 0x80);

    let convert = |value: u32| {
        let value = value as u8;

        if binary {
            value
        } else {
            binary_to_bcd(value)
        }
    };

    write_cmos(0x00, convert(second));
    write_cmos(0x02, convert(minute));

    if hour_24 {
        write_cmos(0x04, convert(hour));
    } else {
        let pm = hour >= 12;
        let mut hours = hour % 12;

        if hours == 0 {
            hours = 12;
        }

        let mut value = convert(hours);

        if pm {
            value |= 0x80;
        }

        write_cmos(0x04, value);
    }

    write_cmos(0x07, convert(day));
    write_cmos(0x08, convert(month));
    write_cmos(0x09, convert(year % 100));

    // Снимаем SET — часы продолжают идти с новыми значениями.
    write_cmos(0x0b, status_b);
    true
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
