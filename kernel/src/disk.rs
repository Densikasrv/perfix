use crate::{block, vga};

const SECTOR_SIZE: usize = 512;
const PARTITION_TABLE_OFFSET: usize = 446;

#[derive(Clone, Copy)]
struct Partition {
    bootable: bool,
    partition_type: u8,
    start_lba: u32,
    sectors: u32,
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

/// Признак загрузочного сектора exFAT (OEM-имя "EXFAT   ").
fn is_exfat_boot(sector: &[u8]) -> bool {
    &sector[3..11] == b"EXFAT   "
}

fn detect_fs(device: usize, start_lba: u32, partition_type: u8) -> &'static [u8] {
    let mut sector = [0u8; SECTOR_SIZE];

    // exFAT: загрузочный сектор начинается с "EXFAT   ".
    if block::read_sector(device, start_lba, &mut sector).is_ok() {
        if &sector[3..11] == b"EXFAT   " {
            return b"exFAT";
        }

        // FAT32: тип раздела MBR и сигнатура загрузочного сектора.
        if (partition_type == 0x0B || partition_type == 0x0C)
            && sector[510] == 0x55
            && sector[511] == 0xAA
        {
            return b"FAT32";
        }
    }

    // ext4: суперблок находится через 1024 байта
    // от начала раздела, то есть в секторе start_lba + 2.
    if let Some(superblock_lba) = start_lba.checked_add(2) {
        if block::read_sector(device, superblock_lba, &mut sector).is_ok() {
            if u16::from_le_bytes([sector[56], sector[57]]) == 0xEF53 {
                return b"ext4";
            }
        }
    }

    match partition_type {
        0x07 => b"NTFS/exFAT",
        0x0B | 0x0C => b"FAT32?",
        0x83 => b"Linux (unknown)",
        0x05 | 0x0F => b"Extended",
        0xEE => b"GPT protective",
        0x82 => b"Linux swap",
        0x00 => b"Empty",
        _ => b"Unknown",
    }
}

fn print_hex_byte(row: usize, col: usize, value: u8) {
    const HEX: &[u8] = b"0123456789ABCDEF";

    let chars = [
        HEX[(value >> 4) as usize],
        HEX[(value & 0x0F) as usize],
    ];

    vga::print_at(row, col, &chars);
}

fn print_decimal(row: usize, col: usize, value: u32) {
    let mut digits = [0u8; 10];
    let mut count = 0usize;
    let mut value = value;

    if value == 0 {
        digits[0] = b'0';
        count = 1;
    } else {
        while value > 0 {
            digits[count] = b'0' + (value % 10) as u8;
            value /= 10;
            count += 1;
        }
    }

    let mut output = [0u8; 10];

    for i in 0..count {
        output[i] = digits[count - i - 1];
    }

    vga::print_at(row, col, &output[..count]);
}

/// Печатает описание всех найденных дисков и их MBR-разделов.
pub fn print_disk_info(row: usize) -> usize {
    let count = block::count();

    if count == 0 {
        vga::print_at(row, 0, b"No ATA/SATA disks found");
        return 1;
    }

    let mut output_row = row;

    for index in 0..count {
        let Some(device) = block::device(index) else {
            break;
        };

        vga::print_at(output_row, 0, b"Disk ");
        print_decimal(output_row, 5, index as u32 + 1);
        vga::print_at(output_row, 7, b": ");
        vga::print_at(output_row, 9, device.bus_name());
        vga::print_at(output_row, 32, b"Sectors: ");
        print_decimal(output_row, 41, device.sectors);

        let mut model_len = device.model.len();

        while model_len > 0 && device.model[model_len - 1] == b' ' {
            model_len -= 1;
        }

        if model_len > 0 {
            vga::print_at(output_row + 1, 2, b"Model: ");
            vga::print_at(output_row + 1, 9, &device.model[..model_len]);
            output_row += 1;
        }

        let mut sector = [0u8; SECTOR_SIZE];

        if block::read_sector(index, 0, &mut sector).is_err() {
            vga::print_at(output_row + 1, 2, b"Read error");
            output_row += 2;
            continue;
        }

        if sector[510] != 0x55 || sector[511] != 0xAA {
            vga::print_at(output_row + 1, 2, b"No valid MBR signature");
            output_row += 2;
            continue;
        }

        if sector[PARTITION_TABLE_OFFSET + 4] == 0xEE {
            vga::print_at(output_row + 1, 2, b"Partition table: GPT protective MBR");
            output_row += 2;
        } else {
            vga::print_at(output_row + 1, 2, b"Partition table: MBR");
            output_row += 2;
        }

        let mut found = false;

        for part_index in 0..4 {
            let offset = PARTITION_TABLE_OFFSET + part_index * 16;

            let partition_type = sector[offset + 4];
            let start_lba = read_u32(&sector, offset + 8);
            let sectors = read_u32(&sector, offset + 12);

            if partition_type == 0 || sectors == 0 {
                continue;
            }

            found = true;

            let partition = Partition {
                bootable: sector[offset] == 0x80,
                partition_type,
                start_lba,
                sectors,
            };

            let fs_name =
                detect_fs(index, partition.start_lba, partition.partition_type);

            vga::print_at(output_row, 2, b"Partition ");
            print_decimal(output_row, 12, part_index as u32 + 1);

            vga::print_at(output_row, 14, b" Type: ");
            print_hex_byte(output_row, 21, partition.partition_type);

            vga::print_at(output_row, 24, b" FS: ");
            vga::print_at(output_row, 29, fs_name);

            vga::print_at(output_row + 1, 4, b"Start LBA: ");
            print_decimal(output_row + 1, 15, partition.start_lba);

            vga::print_at(output_row + 1, 29, b"Sectors: ");
            print_decimal(output_row + 1, 38, partition.sectors);

            if partition.bootable {
                vga::print_at(output_row + 1, 55, b"Bootable");
            }

            output_row += 2;
        }

        if !found {
            vga::print_at(output_row, 2, b"No partitions found");
            output_row += 1;
        }
    }

    output_row - row
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Exfat,
    Ext4,
    Other,
}

#[derive(Clone, Copy)]
pub struct DiskPartition {
    /// Глобальный номер раздела (через все устройства), с единицы.
    /// Оставлен для совместимости со старыми командами и установщиком.
    pub number: usize,
    /// Индекс блочного устройства в таблице [`block`].
    pub device: usize,
    /// Номер раздела внутри физического диска, с единицы.
    pub partition_index: usize,
    pub kind: FsKind,
    pub start_lba: u32,
    pub sectors: u32,
    /// Тип раздела MBR; 0 для GPT и диска без таблицы разделов.
    pub partition_type: u8,
}

impl DiskPartition {
    pub const EMPTY: Self = Self {
        number: 0,
        device: 0,
        partition_index: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
        partition_type: 0,
    };
}

/// Буква физического диска: `a` для первого, `b` для второго и т. д.
pub fn disk_letter(device: usize) -> u8 {
    if device < 26 {
        b'a' + device as u8
    } else {
        b'A' + (device - 26) as u8
    }
}

fn push_partition(
    out: &mut [DiskPartition],
    count: &mut usize,
    device: usize,
    partition_index: usize,
    kind: FsKind,
    start_lba: u32,
    sectors: u32,
    partition_type: u8,
) {
    if *count >= out.len() {
        return;
    }

    out[*count] = DiskPartition {
        number: *count + 1,
        device,
        partition_index,
        kind,
        start_lba,
        sectors,
        partition_type,
    };
    *count += 1;
}

/// Заполняет `out` разделами всех найденных дисков.
///
/// Номера разделов сквозные (`number`), но для пользователя каждый раздел
/// называется по физическому диску и своему номеру на нём: `/dsk/a1`,
/// `/dsk/a2`, `/dsk/b1`.
pub fn read_partitions(out: &mut [DiskPartition]) -> usize {
    let mut count = 0usize;

    for device in 0..block::count() {
        let mut sector = [0u8; SECTOR_SIZE];

        if block::read_sector(device, 0, &mut sector).is_err() {
            continue;
        }

        // Том exFAT на диске целиком (без таблицы разделов) — это один раздел.
        if is_exfat_boot(&sector) {
            let volume_length = read_u64(&sector, 72).min(u32::MAX as u64) as u32;

            push_partition(
                out,
                &mut count,
                device,
                1,
                FsKind::Exfat,
                0,
                volume_length,
                0,
            );
            continue;
        }

        if sector[510] != 0x55 || sector[511] != 0xAA {
            continue;
        }

        // GPT: таблицу разделов разбирает драйвер exFAT (MBR/GPT/extended MBR),
        // поэтому для защитного MBR берём список из него.
        if sector[PARTITION_TABLE_OFFSET + 4] == 0xEE {
            count += read_exfat_partitions(device, &mut out[count..], count);
            continue;
        }

        let mut partition_index = 0usize;

        for index in 0..4 {
            if count >= out.len() {
                break;
            }

            let offset = PARTITION_TABLE_OFFSET + index * 16;
            let partition_type = sector[offset + 4];
            let start_lba = read_u32(&sector, offset + 8);
            let sectors = read_u32(&sector, offset + 12);

            if partition_type == 0 || sectors == 0 {
                continue;
            }

            partition_index += 1;

            let detected = detect_fs(device, start_lba, partition_type);

            let kind = if detected == b"exFAT" {
                FsKind::Exfat
            } else if detected == b"ext4" {
                FsKind::Ext4
            } else {
                FsKind::Other
            };

            push_partition(
                out,
                &mut count,
                device,
                partition_index,
                kind,
                start_lba,
                sectors,
                partition_type,
            );
        }
    }

    count
}

/// Заполняет `out` разделами exFAT, найденными драйвером файловой системы.
///
/// Драйвер умеет MBR, extended MBR и GPT; номера разделов продолжают
/// сквозную нумерацию `start_number`.
fn read_exfat_partitions(
    device: usize,
    out: &mut [DiskPartition],
    start_number: usize,
) -> usize {
    let mut found =
        [crate::fs::exfat::PartitionInfo::empty(); crate::fs::exfat::MAX_PARTITIONS];

    let found_count = match crate::fs::exfat::partition_list_on(device, &mut found) {
        Ok(count) => count,
        Err(_) => 0,
    };

    let mut count = 0usize;

    for partition in found[..found_count].iter() {
        if count >= out.len() {
            break;
        }

        out[count] = DiskPartition {
            number: start_number + count + 1,
            device,
            partition_index: count + 1,
            kind: FsKind::Exfat,
            start_lba: partition.base_lba,
            sectors: partition.sectors.min(u32::MAX as u64) as u32,
            partition_type: 0,
        };

        count += 1;
    }

    count
}

/// Разбирает имя раздела в формате `a1` (буква диска + номер на нём)
/// или старый сквозной номер `3`.
pub fn parse_partition_reference(input: &[u8]) -> Option<DiskPartition> {
    if input.is_empty() {
        return None;
    }

    let mut partitions = [DiskPartition::EMPTY; 32];
    let count = read_partitions(&mut partitions);

    if input[0].is_ascii_digit() {
        let mut number = 0usize;

        for &byte in input {
            if !byte.is_ascii_digit() {
                return None;
            }

            number = number.checked_mul(10)?;
            number = number.checked_add((byte - b'0') as usize)?;
        }

        return partitions[..count]
            .iter()
            .find(|p| p.number == number)
            .copied();
    }

    let letter = input[0].to_ascii_lowercase();

    if !letter.is_ascii_alphabetic() || input.len() < 2 {
        return None;
    }

    let mut index = 0usize;

    for &byte in &input[1..] {
        if !byte.is_ascii_digit() {
            return None;
        }

        index = index.checked_mul(10)?;
        index = index.checked_add((byte - b'0') as usize)?;
    }

    if index == 0 {
        return None;
    }

    partitions[..count]
        .iter()
        .find(|p| {
            disk_letter(p.device).to_ascii_lowercase() == letter
                && p.partition_index == index
        })
        .copied()
}

/// Длина имени раздела в начале строки: `a12` или `3`.
pub fn partition_reference_len(input: &[u8]) -> usize {
    if input.is_empty() {
        return 0;
    }

    if input[0].is_ascii_digit() {
        let mut length = 0usize;

        while length < input.len() && input[length].is_ascii_digit() {
            length += 1;
        }

        return length;
    }

    if input[0].is_ascii_alphabetic() {
        let mut length = 1usize;

        while length < input.len() && input[length].is_ascii_digit() {
            length += 1;
        }

        return length;
    }

    0
}

static mut SELECTED_KIND: FsKind = FsKind::Exfat;
static mut SELECTED_LBA: u32 = 0;
static mut SELECTED_SECTORS: u32 = 0;
static mut SELECTED_DEVICE: usize = 0;

pub fn selected_kind() -> FsKind {
    unsafe { SELECTED_KIND }
}

pub fn selected_lba() -> u32 {
    unsafe { SELECTED_LBA }
}

pub fn selected_sectors() -> u32 {
    unsafe { SELECTED_SECTORS }
}

pub fn selected_device() -> usize {
    unsafe { SELECTED_DEVICE }
}

/// Сколько разделов проверяет автоподключение при старте.
const AUTO_PARTITIONS: usize = 32;

/// Автоматически подключает файловую систему при старте.
///
/// Сначала ищется ext4-раздел с установленной системой Perfix (маркер
/// `/etc/InstallSuccess` или ядро `/perfix.elf`), затем первый раздел exFAT.
/// Если ничего не найдено, остаётся прежнее поведение: том exFAT на всём
/// диске.
pub fn auto_select() {
    let mut partitions = [DiskPartition::EMPTY; AUTO_PARTITIONS];

    let count = read_partitions(&mut partitions);

    for partition in partitions[..count].iter() {
        if partition.kind == FsKind::Ext4 && ext4_has_system(partition) {
            let _ = select_partition(partition.number);
            return;
        }
    }

    for partition in partitions[..count].iter() {
        if partition.kind == FsKind::Exfat {
            let _ = select_partition(partition.number);
            return;
        }
    }
}

/// Есть ли на ext4-разделе установленная система Perfix.
fn ext4_has_system(partition: &DiskPartition) -> bool {
    let device = crate::fs::ext4::AtaDevice::new(partition.device);

    let Ok(filesystem) = crate::fs::ext4::Ext4::mount(
        &device,
        partition.start_lba,
        partition.sectors,
    ) else {
        return false;
    };

    filesystem
        .path_exists(b"/etc/InstallSuccess")
        .unwrap_or(false)
        || filesystem
            .path_exists(b"/perfix.elf")
            .unwrap_or(false)
}

pub fn select_partition(number: usize) -> Result<FsKind, &'static [u8]> {
    let mut partitions = [DiskPartition::EMPTY; AUTO_PARTITIONS];

    let count = read_partitions(&mut partitions);

    let partition = partitions[..count]
        .iter()
        .find(|p| p.number == number)
        .ok_or(b"Partition not found" as &'static [u8])?;

    match partition.kind {
        FsKind::Exfat => {
            // Находим порядковый номер раздела именно среди exFAT
            // на этом же устройстве.
            let mut exfat_index = 0usize;

            for p in &partitions[..count] {
                if p.number == number {
                    break;
                }

                if p.device == partition.device && p.kind == FsKind::Exfat {
                    exfat_index += 1;
                }
            }

            crate::fs::exfat::select_partition_on(partition.device, exfat_index)
                .map_err(|_| b"Could not select exFAT partition" as &'static [u8])?;
        }

        FsKind::Ext4 => {
            let device = crate::fs::ext4::AtaDevice::new(partition.device);

            crate::fs::ext4::Ext4::mount(
                &device,
                partition.start_lba,
                partition.sectors,
            )
            .map_err(|_| b"ext4 superblock validation failed" as &'static [u8])?;
        }

        FsKind::Other => {
            return Err(b"Unsupported filesystem");
        }
    }

    unsafe {
        SELECTED_KIND = partition.kind;
        SELECTED_LBA = partition.start_lba;
        SELECTED_SECTORS = partition.sectors;
        SELECTED_DEVICE = partition.device;
    }
    Ok(partition.kind)
}

fn find_ext4_partition(
    number: usize,
) -> Result<DiskPartition, &'static [u8]> {
    let mut partitions = [DiskPartition::EMPTY; AUTO_PARTITIONS];

    let count = read_partitions(&mut partitions);

    partitions[..count]
        .iter()
        .find(|p| p.number == number && p.kind == FsKind::Ext4)
        .copied()
        .ok_or(b"ext4 partition not found" as &'static [u8])
}

fn mount_ext4<'a>(
    device: &'a crate::fs::ext4::AtaDevice,
    partition: &DiskPartition,
) -> Result<
    crate::fs::ext4::Ext4<'a, crate::fs::ext4::AtaDevice>,
    &'static [u8],
> {
    crate::fs::ext4::Ext4::mount(
        device,
        partition.start_lba,
        partition.sectors,
    )
    .map_err(|_| b"ext4 mount failed" as &'static [u8])
}

pub fn read_ext4_root_inode(
    number: usize,
) -> Result<crate::fs::ext4::InodeInfo, &'static [u8]> {
    let partition = find_ext4_partition(number)?;
    let device = crate::fs::ext4::AtaDevice::new(partition.device);
    let filesystem = mount_ext4(&device, &partition)?;

    filesystem
        .read_inode(2)
        .map_err(|_| b"could not read root inode" as &'static [u8])
}

pub fn read_ext4_inode(
    partition_number: usize,
    inode_number: u32,
) -> Result<crate::fs::ext4::InodeInfo, &'static [u8]> {
    let partition = find_ext4_partition(partition_number)?;
    let device = crate::fs::ext4::AtaDevice::new(partition.device);
    let filesystem = mount_ext4(&device, &partition)?;

    filesystem
        .read_inode(inode_number)
        .map_err(|_| b"could not read inode" as &'static [u8])
}

pub fn read_ext4_root_directory_block(
    number: usize,
    buffer: &mut [u8; 4096],
) -> Result<usize, &'static [u8]> {
    let partition = find_ext4_partition(number)?;
    let device = crate::fs::ext4::AtaDevice::new(partition.device);
    let filesystem = mount_ext4(&device, &partition)?;

    let inode = filesystem
        .read_inode(2)
        .map_err(|_| b"could not read root inode" as &'static [u8])?;

    if !inode.is_directory {
        return Err(b"root inode is not a directory");
    }

    let block_size = filesystem.block_size() as usize;

    if block_size > buffer.len() || inode.first_extent_block == 0 {
        return Err(b"invalid root directory extent");
    }

    filesystem
        .read_block(inode.first_extent_block, buffer)
        .map_err(|_| b"could not read directory block" as &'static [u8])?;

    Ok(block_size)
}

pub fn read_ext4_directory_block(
    number: usize,
    inode_number: u32,
    buffer: &mut [u8; 4096],
) -> Result<usize, &'static [u8]> {
    let partition = find_ext4_partition(number)?;
    let device = crate::fs::ext4::AtaDevice::new(partition.device);
    let filesystem = mount_ext4(&device, &partition)?;

    let inode = filesystem
        .read_inode(inode_number)
        .map_err(|_| b"could not read directory inode" as &'static [u8])?;

    if !inode.is_directory {
        return Err(b"inode is not a directory");
    }

    let block_size = filesystem.block_size() as usize;

    if block_size > buffer.len() || inode.first_extent_block == 0 {
        return Err(b"invalid directory extent");
    }

    filesystem
        .read_block(inode.first_extent_block, buffer)
        .map_err(|_| b"could not read directory block" as &'static [u8])?;

    Ok(block_size)
}
