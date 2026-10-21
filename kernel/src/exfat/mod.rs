//! Интеграция драйвера exFAT с ядром: ATA-устройство и команды оболочки.
//!
//! Сам драйвер живёт в [`fs`] и не знает ни про ATA, ни про VGA.

mod fs;

pub use fs::{FsError, Volume};

use crate::ata::{self, AtaError};
use crate::vga;

/// Устройство чтения секторов через ATA PIO (primary master).
struct AtaDevice;

impl fs::BlockReader for AtaDevice {
    fn read_sector(
        &self,
        lba: u32,
        buffer: &mut [u8; fs::SECTOR_SIZE],
    ) -> Result<(), fs::BlockError> {
        ata::read_sector(lba, buffer).map_err(|error| match error {
            AtaError::Timeout => fs::BlockError::Timeout,
            AtaError::NoDevice => fs::BlockError::NoDevice,
            AtaError::DeviceError(_) => fs::BlockError::Device,
            AtaError::InvalidLba => fs::BlockError::InvalidLba,
        })
    }
}

fn mount_volume() -> Result<Volume, FsError> {
    fs::mount(&AtaDevice)
}

const VALUE_COLUMN: usize = 22;

fn write_dec(mut value: u64, out: &mut [u8]) -> usize {
    if out.is_empty() {
        return 0;
    }

    if value == 0 {
        out[0] = b'0';
        return 1;
    }

    let mut digits = [0u8; 20];
    let mut len = 0usize;

    while value > 0 && len < digits.len() {
        digits[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }

    let mut written = 0usize;

    while written < len && written < out.len() {
        out[written] = digits[len - 1 - written];
        written += 1;
    }

    written
}

fn push<const N: usize>(text: &mut [u8; N], len: &mut usize, bytes: &[u8]) {
    for &byte in bytes {
        if *len >= text.len() {
            return;
        }

        text[*len] = byte;
        *len += 1;
    }
}

fn print_hex16(row: usize, label: &[u8], value: u16) {
    let digits = b"0123456789ABCDEF";
    let mut output = *b"0x0000";

    for i in 0..4 {
        let shift = (3 - i) * 4;
        output[2 + i] = digits[((value >> shift) & 0xF) as usize];
    }

    vga::print_at(row, 0, label);
    vga::print_at(row, VALUE_COLUMN, &output);
}

fn print_hex32(row: usize, label: &[u8], value: u32) {
    let digits = b"0123456789ABCDEF";
    let mut output = *b"0x00000000";

    for i in 0..8 {
        let shift = (7 - i) * 4;
        output[2 + i] = digits[((value >> shift) & 0xF) as usize];
    }

    vga::print_at(row, 0, label);
    vga::print_at(row, VALUE_COLUMN, &output);
}

fn print_hex64(row: usize, label: &[u8], value: u64) {
    let digits = b"0123456789ABCDEF";
    let mut output = *b"0x0000000000000000";

    for i in 0..16 {
        let shift = (15 - i) * 4;
        output[2 + i] = digits[((value >> shift) & 0xF) as usize];
    }

    vga::print_at(row, 0, label);
    vga::print_at(row, VALUE_COLUMN, &output);
}

fn print_dec(row: usize, label: &[u8], value: u64, suffix: &[u8]) {
    let mut digits = [0u8; 20];
    let len = write_dec(value, &mut digits);

    vga::print_at(row, 0, label);
    vga::print_at(row, VALUE_COLUMN, &digits[..len]);

    if !suffix.is_empty() {
        vga::print_at(row, VALUE_COLUMN + len, suffix);
    }
}

fn print_revision(row: usize, revision: u16) {
    let mut digits = [0u8; 20];

    let len = write_dec((revision >> 8) as u64, &mut digits);
    vga::print_at(row, 0, b"Revision:");
    vga::print_at(row, VALUE_COLUMN, &digits[..len]);
    vga::print_at(row, VALUE_COLUMN + len, b".");

    let len = write_dec((revision & 0xFF) as u64, &mut digits);
    vga::print_at(row, VALUE_COLUMN + len + 1, &digits[..len]);
}

fn print_error(row: usize, error: FsError) -> usize {
    let mut text = [b' '; 80];
    let mut len = 0usize;

    push(&mut text, &mut len, b"exFAT error: ");
    push(&mut text, &mut len, error.message());

    vga::print_at(row, 0, &text[..len]);

    1
}

/// Команда `mount`: параметры смонтированного тома exFAT.
pub fn print_volume_info(row: usize) -> usize {
    let volume = match mount_volume() {
        Ok(volume) => volume,
        Err(error) => return print_error(row, error),
    };

    vga::print_at(row, 0, b"exFAT volume mounted");

    print_hex32(row + 1, b"Partition LBA:", volume.base_lba);
    print_hex64(row + 2, b"Volume sectors:", volume.volume_length);
    print_hex32(row + 3, b"FAT LBA:", volume.fat_lba);
    print_hex32(row + 4, b"FAT length:", volume.fat_length);
    print_hex32(row + 5, b"Cluster heap LBA:", volume.cluster_heap_lba);
    print_hex32(row + 6, b"Cluster count:", volume.cluster_count);
    print_hex32(row + 7, b"Cluster bytes:", volume.cluster_bytes());
    print_hex32(row + 8, b"Root cluster:", volume.root_cluster);
    print_hex32(row + 9, b"Volume serial:", volume.serial);
    print_revision(row + 10, volume.revision);

    if volume.percent_in_use == 0xFF {
        vga::print_at(row + 11, 0, b"Percent in use: unknown");
    } else {
        print_dec(row + 11, b"Percent in use:", volume.percent_in_use as u64, b"%");
    }

    vga::print_at(row + 12, 0, b"Volume flags:");
    print_hex16(row + 12, b"", volume.flags);
    vga::print_at(
        row + 12,
        VALUE_COLUMN + 8,
        if volume.is_dirty() { b"dirty" } else { b"clean" },
    );

    vga::print_at(row + 13, 0, b"Boot checksum:");
    vga::print_at(
        row + 13,
        VALUE_COLUMN,
        if volume.boot_checksum_ok { b"OK" } else { b"FAILED" },
    );

    14
}

/// Команда `diskinfo`: поля загрузочного сектора в «сыром» виде.
pub fn print_boot_sector(row: usize) -> usize {
    let base_lba = match mount_volume() {
        Ok(volume) => volume.base_lba,
        Err(error) => return print_error(row, error),
    };

    let mut boot = [0u8; fs::SECTOR_SIZE];

    if let Err(error) = fs::BlockReader::read_sector(&AtaDevice, base_lba, &mut boot) {
        return print_error(row, error.into());
    }

    let sector_bytes = 1u32.checked_shl(boot[108] as u32).unwrap_or(0);
    let cluster_bytes = sector_bytes
        .checked_shl(boot[109] as u32)
        .unwrap_or(0);

    vga::print_at(row, 0, b"exFAT boot sector: OK");
    print_hex64(row + 1, b"Volume sectors:", fs::read_u64(&boot, 72));
    print_hex32(row + 2, b"FAT offset:", fs::read_u32(&boot, 80));
    print_hex32(row + 3, b"FAT length:", fs::read_u32(&boot, 84));
    print_hex32(row + 4, b"Cluster heap:", fs::read_u32(&boot, 88));
    print_hex32(row + 5, b"Cluster count:", fs::read_u32(&boot, 92));
    print_hex32(row + 6, b"Root cluster:", fs::read_u32(&boot, 96));
    print_hex32(row + 7, b"Sector bytes:", sector_bytes);
    print_hex32(row + 8, b"Cluster bytes:", cluster_bytes);

    9
}

fn format_entry(entry: &fs::Entry, out: &mut [u8; 80]) -> usize {
    let prefix: &[u8] = if entry.is_directory() {
        b"<DIR>  "
    } else {
        b"       "
    };

    let mut len = 0usize;
    push(out, &mut len, prefix);

    let mut suffix = [0u8; 24];
    let mut suffix_len = 0usize;

    if entry.is_directory() {
        suffix[suffix_len] = b'/';
        suffix_len += 1;
    } else {
        let mut digits = [0u8; 20];
        let digits_len = write_dec(entry.data_length, &mut digits);

        suffix[suffix_len] = b' ';
        suffix[suffix_len + 1] = b' ';
        suffix_len += 2;
        push(&mut suffix, &mut suffix_len, &digits[..digits_len]);
        suffix[suffix_len] = b' ';
        suffix[suffix_len + 1] = b'B';
        suffix_len += 2;
    }

    let available = 80usize.saturating_sub(len + suffix_len);

    for &byte in entry.name() {
        if len >= available + prefix.len() {
            break;
        }

        out[len] = byte;
        len += 1;
    }

    push(out, &mut len, &suffix[..suffix_len]);

    len
}

/// Максимум строк, которые команда `ls` выводит на экран.
const LIST_MAX_LINES: usize = 15;

/// Команда `ls [path]`: список каталога.
pub fn shell_list(path: &[u8], row: usize) -> usize {
    let default_path: &[u8] = b"/";
    let path = if path.is_empty() { default_path } else { path };

    let volume = match mount_volume() {
        Ok(volume) => volume,
        Err(error) => return print_error(row, error),
    };

    let directory = match fs::resolve(&AtaDevice, &volume, path) {
        Ok(entry) => entry,
        Err(error) => return print_error(row, error),
    };

    if !directory.is_directory() {
        vga::print_at(row, 0, b"Not a directory");
        return 1;
    }

    let mut header = [b' '; 80];
    let mut header_len = 0usize;
    push(&mut header, &mut header_len, b"Directory: ");
    push(&mut header, &mut header_len, path);
    vga::print_at(row, 0, &header[..header_len]);

    let mut line = 1usize;
    let mut entries = 0usize;
    let mut cursor = fs::DirCursor::new(
        &volume,
        &AtaDevice,
        directory.first_cluster,
        directory.contiguous,
    );

    loop {
        if row + line > 23 {
            break;
        }

        match cursor.next_entry() {
            Ok(Some(entry)) => {
                if line >= LIST_MAX_LINES {
                    vga::print_at(row + line, 0, b"More entries...");
                    line += 1;
                    break;
                }

                let mut text = [b' '; 80];
                let text_len = format_entry(&entry, &mut text);
                vga::print_at(row + line, 0, &text[..text_len]);

                line += 1;
                entries += 1;
            }
            Ok(None) => break,
            Err(error) => {
                line += print_error(row + line, error);
                break;
            }
        }
    }

    if entries == 0 && line == 1 {
        vga::print_at(row + 1, 0, b"Empty directory");
        line = 2;
    }

    line
}

/// Количество строк, отводимых команде `cat` в оболочке.
pub const CAT_LINES: usize = 14;

/// Команда `cat <file>`: вывод содержимого файла в текстовом режиме.
pub fn shell_cat(path: &[u8], row: usize) -> usize {
    if path.is_empty() {
        vga::print_at(row, 0, b"Usage: cat <file>");
        return 1;
    }

    let volume = match mount_volume() {
        Ok(volume) => volume,
        Err(error) => return print_error(row, error),
    };

    let file = match fs::resolve(&AtaDevice, &volume, path) {
        Ok(entry) => entry,
        Err(error) => return print_error(row, error),
    };

    if file.is_directory() {
        vga::print_at(row, 0, b"Is a directory");
        return 1;
    }

    let mut header = [b' '; 80];
    let mut header_len = 0usize;
    push(&mut header, &mut header_len, b"File: ");
    push(&mut header, &mut header_len, path);
    push(&mut header, &mut header_len, b" (");

    let mut digits = [0u8; 20];
    let digits_len = write_dec(file.data_length, &mut digits);
    push(&mut header, &mut header_len, &digits[..digits_len]);
    push(&mut header, &mut header_len, b" B)");

    vga::print_at(row, 0, &header[..header_len]);

    let last_row = row + CAT_LINES - 1;
    let mut text_row = row + 1;
    let mut col = 0usize;
    let mut empty = true;
    let mut truncated = false;
    let mut decoder = crate::utf8::Decoder::new();

    let result = fs::read_file(&AtaDevice, &volume, &file, |chunk| {
        for &byte in chunk {
            let mut decoded = [0u8; 4];
            let decoded_len = decoder.feed(byte, &mut decoded, fs::utf16_to_cp866);

            for &code in &decoded[..decoded_len] {
                empty = false;

                if code == b'\r' {
                    continue;
                }

                if code == b'\n' {
                    text_row += 1;
                    col = 0;

                    if text_row > last_row {
                        truncated = true;
                        return false;
                    }

                    continue;
                }

                if col >= 80 {
                    text_row += 1;
                    col = 0;

                    if text_row > last_row {
                        truncated = true;
                        return false;
                    }
                }

                // Старшие байты — это CP866 (кириллица, псевдографика),
                // поэтому отбрасываем только управляющие символы ASCII.
                let display = if code == b'\t' {
                    b' '
                } else if code >= 0x20 && code != 0x7f {
                    code
                } else {
                    b'.'
                };

                vga::print_at(text_row, col, &[display]);
                col += 1;
            }
        }

        true
    });

    if let Err(error) = result {
        let error_row = if text_row <= last_row { text_row + 1 } else { last_row };
        print_error(error_row, error);
        return CAT_LINES;
    }

    if empty {
        vga::print_at(row + 1, 0, b"Empty file");
        return 2;
    }

    if truncated {
        if text_row + 1 <= row + CAT_LINES - 1 {
            vga::print_at(text_row + 1, 0, b"--- output truncated ---");
        }

        return CAT_LINES;
    }

    (text_row - row) + 1
}



