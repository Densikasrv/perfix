//! Драйвер IDE/ATA в режиме PIO.
//!
//! Поддерживаются оба канала (primary и secondary) и оба устройства на
//! каждом канале (master и slave). Адресация — LBA28, по одному сектору
//! за операцию; прерывания не используются.

use core::arch::asm;

const PRIMARY_BASE: u16 = 0x1F0;
const SECONDARY_BASE: u16 = 0x170;

const BSY: u8 = 0x80;
const DRQ: u8 = 0x08;
const ERR: u8 = 0x01;
const TIMEOUT: usize = 1_000_000;

/// Каналы IDE.
pub const CHANNELS: usize = 2;
/// Устройства на канале.
pub const DRIVES: usize = 2;

#[derive(Debug, Clone, Copy)]
pub enum AtaError {
    Timeout,
    NoDevice,
    DeviceError(u8),
    InvalidLba,
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
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

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        asm!(
            "in ax, dx",
            in("dx") port,
            out("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

unsafe fn outw(port: u16, value: u16) {
    unsafe {
        asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn base(channel: u8) -> u16 {
    if channel == 0 {
        PRIMARY_BASE
    } else {
        SECONDARY_BASE
    }
}

fn drive_select(drive: u8, lba: u32) -> u8 {
    let master = if drive == 0 { 0x00 } else { 0x10 };
    0xE0 | master | ((lba >> 24) as u8 & 0x0F)
}

/// Выдерживает паузу около 400 нс после выбора устройства.
unsafe fn delay_400ns(channel: u8) {
    let status = unsafe { inb(base(channel) + 7) };
    let _ = status;
    let status = unsafe { inb(base(channel) + 7) };
    let _ = status;
    let status = unsafe { inb(base(channel) + 7) };
    let _ = status;
    let _ = unsafe { inb(base(channel) + 7) };
}

unsafe fn wait_ready(channel: u8) -> Result<u8, AtaError> {
    let status_port = base(channel) + 7;
    let error_port = base(channel) + 1;

    for _ in 0..TIMEOUT {
        let status = unsafe { inb(status_port) };

        if status == 0xFF || status == 0 {
            return Err(AtaError::NoDevice);
        }

        if status & BSY == 0 {
            if status & ERR != 0 {
                return Err(AtaError::DeviceError(unsafe { inb(error_port) }));
            }

            return Ok(status);
        }
    }

    Err(AtaError::Timeout)
}

unsafe fn wait_data_ready(channel: u8) -> Result<(), AtaError> {
    let status_port = base(channel) + 7;
    let error_port = base(channel) + 1;

    for _ in 0..TIMEOUT {
        let status = unsafe { inb(status_port) };

        if status == 0 || status == 0xFF {
            return Err(AtaError::NoDevice);
        }

        if status & BSY != 0 {
            continue;
        }

        if status & ERR != 0 {
            return Err(AtaError::DeviceError(unsafe { inb(error_port) }));
        }

        if status & DRQ != 0 {
            return Ok(());
        }
    }

    Err(AtaError::Timeout)
}

/// Описание найденного ATA-устройства.
#[derive(Clone, Copy)]
pub struct IdentifyInfo {
    pub sectors: u32,
    pub model: [u8; 40],
}

/// Опрашивает устройство командой IDENTIFY DEVICE.
///
/// Отсутствующее устройство (в том числе пустой slave) возвращает `None`.
pub fn identify(channel: u8, drive: u8) -> Option<IdentifyInfo> {
    if channel as usize >= CHANNELS || drive as usize >= DRIVES {
        return None;
    }

    unsafe {
        let b = base(channel);

        // IDENTIFY всегда читает 256 слов, поэтому LBA-биты не нужны.
        outb(b + 6, if drive == 0 { 0xA0 } else { 0xB0 });
        delay_400ns(channel);

        outb(b + 2, 0);
        outb(b + 3, 0);
        outb(b + 4, 0);
        outb(b + 5, 0);
        outb(b + 7, 0xEC); // IDENTIFY DEVICE

        let status = match wait_ready(channel) {
            Ok(status) => status,
            Err(_) => return None,
        };

        if status & DRQ == 0 {
            return None;
        }

        let mut words = [0u16; 256];

        for word in words.iter_mut() {
            *word = inw(b);
        }

        if words[0] == 0 || words[0] == 0xFFFF {
            return None;
        }

        // Бит 9 слова 49: поддерживается LBA-адресация.
        if words[49] & 0x0200 == 0 {
            return None;
        }

        let sectors =
            (words[60] as u32) | ((words[61] as u32) << 16);

        if sectors == 0 {
            return None;
        }

        let mut model = [b' '; 40];
        let mut length = 0usize;

        for index in 27..47 {
            let word = words[index];
            let hi = (word >> 8) as u8;
            let lo = word as u8;

            if length < model.len() {
                model[length] = hi;
                length += 1;
            }

            if length < model.len() {
                model[length] = lo;
                length += 1;
            }
        }

        while length > 0 && (model[length - 1] == b' ' || model[length - 1] == 0) {
            length -= 1;
        }

        // Хвост модели заменяем пробелами, чтобы в таблице не было мусора.
        for byte in model[length..].iter_mut() {
            *byte = b' ';
        }

        Some(IdentifyInfo { sectors, model })
    }
}

/// Читает один 512-байтовый сектор.
pub fn read_sector(
    channel: u8,
    drive: u8,
    lba: u32,
    buffer: &mut [u8; 512],
) -> Result<(), AtaError> {
    if channel as usize >= CHANNELS || drive as usize >= DRIVES {
        return Err(AtaError::NoDevice);
    }

    if lba > 0x0FFF_FFFF {
        return Err(AtaError::InvalidLba);
    }

    unsafe {
        let b = base(channel);

        outb(b + 6, drive_select(drive, lba));
        delay_400ns(channel);
        wait_ready(channel)?;

        outb(b + 2, 1);
        outb(b + 3, lba as u8);
        outb(b + 4, (lba >> 8) as u8);
        outb(b + 5, (lba >> 16) as u8);
        outb(b + 7, 0x20); // READ SECTORS

        wait_data_ready(channel)?;

        for i in 0..256 {
            let word = inw(b);
            buffer[i * 2] = word as u8;
            buffer[i * 2 + 1] = (word >> 8) as u8;
        }

        // Даём устройству завершить команду перед следующей.
        let _ = wait_ready(channel);
    }

    Ok(())
}

/// Записывает один 512-байтовый сектор.
pub fn write_sector(
    channel: u8,
    drive: u8,
    lba: u32,
    buffer: &[u8; 512],
) -> Result<(), AtaError> {
    if channel as usize >= CHANNELS || drive as usize >= DRIVES {
        return Err(AtaError::NoDevice);
    }

    if lba > 0x0FFF_FFFF {
        return Err(AtaError::InvalidLba);
    }

    unsafe {
        let b = base(channel);

        outb(b + 6, drive_select(drive, lba));
        delay_400ns(channel);
        wait_ready(channel)?;

        outb(b + 2, 1);
        outb(b + 3, lba as u8);
        outb(b + 4, (lba >> 8) as u8);
        outb(b + 5, (lba >> 16) as u8);
        outb(b + 7, 0x30); // WRITE SECTORS

        wait_data_ready(channel)?;

        for i in 0..256 {
            let word = u16::from_le_bytes([buffer[i * 2], buffer[i * 2 + 1]]);
            outw(b, word);
        }

        wait_ready(channel)?;
        flush(channel, drive)?;
    }

    Ok(())
}

/// Сбрасывает внутренний кэш устройства (FLUSH CACHE).
pub fn flush(channel: u8, drive: u8) -> Result<(), AtaError> {
    if channel as usize >= CHANNELS || drive as usize >= DRIVES {
        return Err(AtaError::NoDevice);
    }

    unsafe {
        let b = base(channel);

        outb(b + 6, if drive == 0 { 0xA0 } else { 0xB0 });
        delay_400ns(channel);
        wait_ready(channel)?;

        outb(b + 7, 0xE7);

        wait_ready(channel)?;
    }

    Ok(())
}
