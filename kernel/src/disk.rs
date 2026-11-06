use crate::{ata, vga};

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

fn detect_fs(start_lba: u32, partition_type: u8) -> &'static [u8] {
    let mut sector = [0u8; SECTOR_SIZE];

    // exFAT: загрузочный сектор начинается с "EXFAT   ".
    if ata::read_sector(start_lba, &mut sector).is_ok() {
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
        if ata::read_sector(superblock_lba, &mut sector).is_ok() {
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

/// Общая информация о primary master ATA-диске и MBR-разделах.
pub fn print_disk_info(row: usize) -> usize {
    let mut sector = [0u8; SECTOR_SIZE];

    vga::print_at(row, 0, b"Disk: Primary Master ATA");

    if ata::read_sector(0, &mut sector).is_err() {
        vga::print_at(row + 1, 0, b"ATA read error");
        return 2;
    }

    if sector[510] != 0x55 || sector[511] != 0xAA {
        vga::print_at(row + 1, 0, b"No valid MBR signature");
        return 2;
    }

    if sector[PARTITION_TABLE_OFFSET + 4] == 0xEE {
        vga::print_at(row + 1, 0, b"Partition table: GPT protective MBR");
        vga::print_at(row + 2, 0, b"GPT parser is not implemented yet");
        return 3;
    }

    vga::print_at(row + 1, 0, b"Partition table: MBR");

    let mut output_row = row + 2;
    let mut found = false;

    for index in 0..4 {
        let offset = PARTITION_TABLE_OFFSET + index * 16;

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

        let fs_name = detect_fs(partition.start_lba, partition.partition_type);

        vga::print_at(output_row, 0, b"Partition ");
        print_decimal(output_row, 10, index as u32 + 1);

        vga::print_at(output_row, 12, b" Type: ");
        print_hex_byte(output_row, 19, partition.partition_type);

        vga::print_at(output_row, 22, b" FS: ");
        vga::print_at(output_row, 27, fs_name);

        vga::print_at(output_row + 1, 2, b"Start LBA: ");
        print_decimal(output_row + 1, 13, partition.start_lba);

        vga::print_at(output_row + 1, 27, b"Sectors: ");
        print_decimal(output_row + 1, 36, partition.sectors);

        if partition.bootable {
            vga::print_at(output_row + 1, 55, b"Bootable");
        }

        output_row += 2;
    }

    if !found {
        vga::print_at(output_row, 0, b"No partitions found");
        output_row += 1;
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
    pub number: usize,
    pub kind: FsKind,
    pub start_lba: u32,
    pub sectors: u32,
}

pub fn read_partitions(out: &mut [DiskPartition]) -> usize {
    let mut sector = [0u8; SECTOR_SIZE];

    if ata::read_sector(0, &mut sector).is_err() {
        return 0;
    }

    // Том exFAT на диске целиком (без таблицы разделов) — это один раздел.
    // Раньше такие диски видел только драйвер exFAT, а listpart/cdpart
    // умели работать лишь с MBR и сообщали, что разделов нет.
    if is_exfat_boot(&sector) {
        if out.is_empty() {
            return 0;
        }

        let volume_length = read_u64(&sector, 72).min(u32::MAX as u64) as u32;

        out[0] = DiskPartition {
            number: 1,
            kind: FsKind::Exfat,
            start_lba: 0,
            sectors: volume_length,
        };

        return 1;
    }

    if sector[510] != 0x55 || sector[511] != 0xAA {
        return 0;
    }

    // GPT: таблицу разделов разбирает драйвер exFAT (MBR/GPT/extended MBR),
    // поэтому для защитного MBR берём список из него.
    if sector[PARTITION_TABLE_OFFSET + 4] == 0xEE {
        let count = read_exfat_partitions(out);

        if count > 0 {
            return count;
        }
    }

    let mut count = 0;

    for index in 0..4 {
        let offset = PARTITION_TABLE_OFFSET + index * 16;
        let partition_type = sector[offset + 4];
        let start_lba = read_u32(&sector, offset + 8);
        let sectors = read_u32(&sector, offset + 12);

        if partition_type == 0 || sectors == 0 || count >= out.len() {
            continue;
        }

        let detected = detect_fs(start_lba, partition_type);

        let kind = if detected == b"exFAT" {
            FsKind::Exfat
        } else if detected == b"ext4" {
            FsKind::Ext4
        } else {
            FsKind::Other
        };

        out[count] = DiskPartition {
            number: index + 1,
            kind,
            start_lba,
            sectors,
        };

        count += 1;
    }

    count
}

/// Заполняет `out` разделами exFAT, найденными драйвером файловой системы.
///
/// Драйвер умеет MBR, extended MBR и GPT; номера разделов идут с единицы,
/// как их показывает listpart и принимает cdpart.
fn read_exfat_partitions(out: &mut [DiskPartition]) -> usize {
    let mut found =
        [crate::fs::exfat::PartitionInfo::empty(); crate::fs::exfat::MAX_PARTITIONS];

    let found_count = match crate::fs::exfat::partition_list(&mut found) {
        Ok(count) => count,
        Err(_) => 0,
    };

    let mut count = 0usize;

    for partition in found[..found_count].iter() {
        if count >= out.len() {
            break;
        }

        out[count] = DiskPartition {
            number: count + 1,
            kind: FsKind::Exfat,
            start_lba: partition.base_lba,
            sectors: partition.sectors.min(u32::MAX as u64) as u32,
        };

        count += 1;
    }

    count
}

static mut SELECTED_KIND: FsKind = FsKind::Exfat;
static mut SELECTED_LBA: u32 = 0;
static mut SELECTED_SECTORS: u32 = 0;

pub fn selected_kind() -> FsKind {
    unsafe { SELECTED_KIND }
}

pub fn selected_lba() -> u32 {
    unsafe { SELECTED_LBA }
}

pub fn selected_sectors() -> u32 {
    unsafe { SELECTED_SECTORS }
}

pub fn select_partition(number: usize) -> Result<FsKind, &'static [u8]> {
    let mut partitions = [DiskPartition {
        number: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; 4];

    let count = read_partitions(&mut partitions);

    let partition = partitions[..count]
        .iter()
        .find(|p| p.number == number)
        .ok_or(b"Partition not found" as &'static [u8])?;

    match partition.kind {
        FsKind::Exfat => {
            // Находим порядковый номер раздела именно среди exFAT.
            let mut exfat_index = 0usize;

            for p in &partitions[..count] {
                if p.number == number {
                    break;
                }

                if p.kind == FsKind::Exfat {
                    exfat_index += 1;
                }
            }

            crate::fs::exfat::select_partition(exfat_index)
                .map_err(|_| b"Could not select exFAT partition" as &'static [u8])?;
        }

        FsKind::Ext4 => {
            let device = crate::fs::ext4::AtaDevice;

            crate::fs::ext4::Ext4::mount(&device, partition.start_lba, partition.sectors)
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
    }
    Ok(partition.kind) 
}
pub fn read_ext4_root_inode(
    number: usize,
) -> Result<crate::fs::ext4::InodeInfo, &'static [u8]> {
    let mut partitions = [DiskPartition {
        number: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; 4];

    let count = read_partitions(&mut partitions);

    let partition = partitions[..count]
        .iter()
        .find(|p| p.number == number && p.kind == FsKind::Ext4)
        .ok_or(b"ext4 partition not found" as &'static [u8])?;

    let device = crate::fs::ext4::AtaDevice;

    let filesystem =
        crate::fs::ext4::Ext4::mount(&device, partition.start_lba, partition.sectors)
            .map_err(|_| b"ext4 mount failed" as &'static [u8])?;

    filesystem
        .read_inode(2)
        .map_err(|_| b"could not read root inode" as &'static [u8])
}

pub fn read_ext4_inode(
    partition_number: usize,
    inode_number: u32,
) -> Result<crate::fs::ext4::InodeInfo, &'static [u8]> {
    let mut partitions = [DiskPartition {
        number: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; 4];

    let count = read_partitions(&mut partitions);

    let partition = partitions[..count]
        .iter()
        .find(|p| {
            p.number == partition_number && p.kind == FsKind::Ext4
        })
        .ok_or(b"ext4 partition not found" as &'static [u8])?;

    let device = crate::fs::ext4::AtaDevice;

    let filesystem =
        crate::fs::ext4::Ext4::mount(&device, partition.start_lba, partition.sectors)
            .map_err(|_| b"ext4 mount failed" as &'static [u8])?;

    filesystem
        .read_inode(inode_number)
        .map_err(|_| b"could not read inode" as &'static [u8])
}
pub fn read_ext4_root_directory_block(
    number: usize,
    buffer: &mut [u8; 4096],
) -> Result<usize, &'static [u8]> {
    let mut partitions = [DiskPartition {
        number: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; 4];

    let count = read_partitions(&mut partitions);

    let partition = partitions[..count]
        .iter()
        .find(|p| p.number == number && p.kind == FsKind::Ext4)
        .ok_or(b"ext4 partition not found" as &'static [u8])?;

    let device = crate::fs::ext4::AtaDevice;

    let filesystem =
        crate::fs::ext4::Ext4::mount(&device, partition.start_lba, partition.sectors)
            .map_err(|_| b"ext4 mount failed" as &'static [u8])?;

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
    let mut partitions = [DiskPartition {
        number: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; 4];

    let count = read_partitions(&mut partitions);

    let partition = partitions[..count]
        .iter()
        .find(|p| p.number == number && p.kind == FsKind::Ext4)
        .ok_or(b"ext4 partition not found" as &'static [u8])?;

    let device = crate::fs::ext4::AtaDevice;

    let filesystem =
        crate::fs::ext4::Ext4::mount(&device, partition.start_lba, partition.sectors)
            .map_err(|_| b"ext4 mount failed" as &'static [u8])?;

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