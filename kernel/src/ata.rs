
use core::arch::asm;
use crate::vga;

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

pub fn test_disk(row: usize) {
    let mut buffer = [0u8; 512];

    match read_sector(0, &mut buffer) {
        Ok(()) => {
            if &buffer[3..11] != b"EXFAT   " {
                vga::print_at(row, 0, b"Not an exFAT volume");
                return;
            }

            vga::print_at(row, 0, b"exFAT boot sector: OK");

            print_hex64(row + 1, b"Volume sectors:", read_u64(&buffer, 72));
            print_hex32(row + 2, b"FAT offset:", read_u32(&buffer, 80));
            print_hex32(row + 3, b"FAT length:", read_u32(&buffer, 84));
            print_hex32(row + 4, b"Cluster heap:", read_u32(&buffer, 88));
            print_hex32(row + 5, b"Cluster count:", read_u32(&buffer, 92));
            print_hex32(row + 6, b"Root cluster:", read_u32(&buffer, 96));

            let sector_shift = buffer[108];
            let cluster_shift = buffer[109];

            if sector_shift <= 12 && cluster_shift <= 25 {
                let sector_size = 1u32 << sector_shift;
                let cluster_size = sector_size
                    .checked_shl(cluster_shift as u32);

                print_hex32(row + 7, b"Sector bytes:", sector_size);

                if let Some(size) = cluster_size {
                    print_hex32(row + 8, b"Cluster bytes:", size);
                }
            } else {
                vga::print_at(row + 7, 0, b"Invalid cluster geometry");
            }
        }
        Err(err) => {
            vga::print_at(row, 0, b"ATA: read failed");

            match err {
                AtaError::Timeout =>
                    vga::print_at(row + 1, 0, b"Error: timeout"),
                AtaError::NoDevice =>
                    vga::print_at(row + 1, 0, b"Error: no device"),
                AtaError::DeviceError(_) =>
                    vga::print_at(row + 1, 0, b"Error: ATA device"),
                AtaError::InvalidLba =>
                    vga::print_at(row + 1, 0, b"Error: invalid LBA"),
            }
        }
    }
}

fn read_u32(buffer: &[u8; 512], offset: usize) -> u32 {
    u32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ])
}

fn read_u64(buffer: &[u8; 512], offset: usize) -> u64 {
    u64::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ])
}

fn print_hex32(row: usize, label: &[u8], value: u32) {
    let digits = b"0123456789ABCDEF";
    let mut output = *b"0x00000000";

    for i in 0..8 {
        let shift = (7 - i) * 4;
        output[2 + i] = digits[((value >> shift) & 0xF) as usize];
    }

    vga::print_at(row, 0, label);
    vga::print_at(row, 20, &output);
}

fn print_hex64(row: usize, label: &[u8], value: u64) {
    let digits = b"0123456789ABCDEF";
    let mut output = *b"0x0000000000000000";

    for i in 0..16 {
        let shift = (15 - i) * 4;
        output[2 + i] = digits[((value >> shift) & 0xF) as usize];
    }

    vga::print_at(row, 0, label);
    vga::print_at(row, 20, &output);
}


pub fn test_root_directory(row: usize) {
    let mut boot = [0u8; 512];

    if read_sector(0, &mut boot).is_err() {
        vga::print_at(row, 0, b"Cannot read exFAT boot sector");
        return;
    }

    if &boot[3..11] != b"EXFAT   " {
        vga::print_at(row, 0, b"Not an exFAT volume");
        return;
    }

    let cluster_heap = read_u32(&boot, 88);
    let cluster_count = read_u32(&boot, 92);
    let root_cluster = read_u32(&boot, 96);
    let sector_shift = boot[108];
    let cluster_shift = boot[109];

    if sector_shift != 9 || cluster_shift > 7 {
        vga::print_at(row, 0, b"Unsupported cluster geometry");
        return;
    }

    if root_cluster < 2 || root_cluster >= cluster_count.saturating_add(2) {
        vga::print_at(row, 0, b"Invalid root cluster");
        return;
    }

    let sectors_per_cluster = 1u32 << cluster_shift;

    let Some(cluster_offset) =
        (root_cluster - 2).checked_mul(sectors_per_cluster)
    else {
        vga::print_at(row, 0, b"Cluster offset overflow");
        return;
    };

    let Some(first_lba) = cluster_heap.checked_add(cluster_offset) else {
        vga::print_at(row, 0, b"LBA overflow");
        return;
    };

    vga::print_at(row, 0, b"Root directory:");
    let mut output_row = row + 1;
    let mut file_count = 0usize;
    let mut stop = false;

    for sector_index in 0..sectors_per_cluster {
        let Some(lba) = first_lba.checked_add(sector_index) else {
            vga::print_at(output_row, 0, b"LBA overflow");
            return;
        };

        let mut sector = [0u8; 512];

        if read_sector(lba, &mut sector).is_err() {
            vga::print_at(output_row, 0, b"Root sector read failed");
            return;
        }

        let mut offset = 0usize;

        while offset + 32 <= 512 {
            let entry_type = sector[offset];

            if entry_type == 0 {
                stop = true;
                break;
            }

            // Skip inactive entries.
            if entry_type & 0x80 == 0 {
                offset += 32;
                continue;
            }

            // File directory entry.
            if entry_type == 0x85 {
                let secondary_count = sector[offset + 1] as usize;
                let mut name = [b' '; 64];
                let mut name_len = 0usize;
                let mut stream_found = false;

                // Secondary entries follow the primary entry.
                let mut next_offset = offset + 32;

                for _ in 0..secondary_count {
                    if next_offset + 32 > 512 {
                        break;
                    }

                    let secondary_type = sector[next_offset];

                    if secondary_type == 0xC0 {
                        stream_found = true;
                    } else if secondary_type == 0xC1 {
                        let name_length =
                            sector[next_offset + 1] as usize;

                        for i in 0..15 {
                            if name_len >= name.len()
                                || name_len >= name_length
                            {
                                break;
                            }

                            let pos = next_offset + 2 + i * 2;
                            let code =
                                u16::from_le_bytes([
                                    sector[pos],
                                    sector[pos + 1],
                                ]);

                            // VGA text mode: display ASCII only.
                            name[name_len] = if (0x20..=0x7E).contains(&code) {
                                code as u8
                            } else {
                                b'?'
                            };

                            name_len += 1;
                        }
                    }

                    next_offset += 32;
                }

                if stream_found && name_len > 0 {
                    if output_row >= 24 {
                        vga::print_at(output_row, 0, b"More entries...");
                        return;
                    }

                    vga::print_at(output_row, 0, &name[..name_len]);
                    output_row += 1;
                    file_count += 1;
                }

                offset += (secondary_count + 1) * 32;
            } else {
                offset += 32;
            }

            if offset >= 512 {
                break;
            }
        }

        if stop {
            break;
        }
    }

    if file_count == 0 {
        vga::print_at(output_row, 0, b"No file names in root cluster");
    }
}

