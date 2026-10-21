
use core::arch::asm;

const DATA: u16 = 0x1F0;
const ERROR: u16 = 0x1F1;
const SECTOR_COUNT: u16 = 0x1F2;
const LBA_LOW: u16 = 0x1F3;
const LBA_MID: u16 = 0x1F4;
const LBA_HIGH: u16 = 0x1F5;
const DRIVE: u16 = 0x1F6;
const STATUS: u16 = 0x1F7;
const COMMAND: u16 = 0x1F7;

const BSY: u8 = 0x80;
const DRQ: u8 = 0x08;
const ERR: u8 = 0x01;
const TIMEOUT: usize = 1_000_000;

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

unsafe fn wait_ready() -> Result<u8, AtaError> {
    for _ in 0..TIMEOUT {
        let status = unsafe { inb(STATUS) };

        if status == 0xFF || status == 0 {
            return Err(AtaError::NoDevice);
        }

        if status & BSY == 0 {
            if status & ERR != 0 {
                return Err(AtaError::DeviceError(
                    unsafe { inb(ERROR) }
                ));
            }
            return Ok(status);
        }
    }

    Err(AtaError::Timeout)
}


unsafe fn wait_data_ready() -> Result<(), AtaError> {
    for _ in 0..TIMEOUT {
        let status = unsafe { inb(STATUS) };

        if status == 0 || status == 0xFF {
            return Err(AtaError::NoDevice);
        }

        if status & BSY != 0 {
            continue;
        }

        if status & ERR != 0 {
            return Err(AtaError::DeviceError(unsafe { inb(ERROR) }));
        }

        if status & DRQ != 0 {
            return Ok(());
        }
    }

    Err(AtaError::Timeout)
}


/// Читает один 512-байтовый сектор с primary master IDE-диска.
/// LBA — номер сектора, начиная с нуля.
pub fn read_sector(
    lba: u32,
    buffer: &mut [u8; 512],
) -> Result<(), AtaError> {
    if lba > 0x0FFF_FFFF {
        return Err(AtaError::InvalidLba);
    }

    unsafe {
    // Сначала выбираем primary master в режиме LBA.
    outb(DRIVE, 0xE0 | ((lba >> 24) as u8 & 0x0F));

    // Даём контроллеру время применить выбор устройства.
    let _ = inb(STATUS);
    let _ = inb(STATUS);
    let _ = inb(STATUS);
    let _ = inb(STATUS);

    wait_ready()?;
        // Задержка после выбора устройства.
        let _ = inb(STATUS);
        let _ = inb(STATUS);
        let _ = inb(STATUS);
        let _ = inb(STATUS);

        wait_ready()?;

        outb(SECTOR_COUNT, 1);
        outb(LBA_LOW, lba as u8);
        outb(LBA_MID, (lba >> 8) as u8);
        outb(LBA_HIGH, (lba >> 16) as u8);
        outb(COMMAND, 0x20); // READ SECTORS

wait_data_ready()?;

let mut i = 0;
while i < 256 {
    let word = inw(DATA);
            buffer[i * 2] = word as u8;
            buffer[i * 2 + 1] = (word >> 8) as u8;
            i += 1;
        }
    }

    Ok(())
}

