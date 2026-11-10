//! Драйвер ext4 с поддержкой чтения и записи.
//!
//! Поддерживается: extents, 64bit, flex_bg, metadata_csum (CRC32C),
//! csum_seed, файлы и каталоги, создание и перезапись файлов,
//! выделение/освобождение блоков и inode.
//!
//! Ограничения записи:
//! - journal не обновляется. Запись разрешена только на чистой ФС
//!   с HAS_JOURNAL без RECOVER; после аварийного завершения
//!   целостность journal не гарантируется;
//! - индексированные каталоги (EXT4_INDEX_FL) не поддерживаются
//!   для добавления записей;
//! - bigalloc, meta_bg и не-extent файлы не поддерживаются.
//!
use core::convert::TryInto;

pub const SECTOR_SIZE: usize = 512;
pub const SUPERBLOCK_OFFSET: u64 = 1024;
pub const EXT4_MAGIC: u16 = 0xEF53;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ext4Error {
    Io,
    InvalidSuperblock,
    UnsupportedBlockSize,
    UnsupportedFeature,
    ReadOnly,
    UnsafeWrite,
    NoSpace,
    InvalidGeometry,
    BufferTooSmall,
    OutOfBounds,
    NotEmpty,
}

/// Режим доступа, с которым смонтирована файловая система.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    ReadOnly,
    ReadWrite,
}

// Совместимые (compat) функции, которые не мешают записи данных
// и обновлению метаданных: HAS_JOURNAL (0x4, только чистый journal),
// DIR_PREALLOC (0x1), EXT_ATTR (0x8),
// RESIZE_INODE (0x10), DIR_INDEX (0x20), LAZY_BG (0x40),
// EXCLUDE_INODE (0x80), EXCLUDE_BITMAP (0x100), SPARSE_SUPER2 (0x200),
// ORPHAN_FILE (0x1000).
const WRITE_SUPPORTED_COMPAT: u32 =
    0x1 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40 | 0x80 | 0x100 | 0x200 | 0x1000;

// FILETYPE (0x2), EXTENTS (0x40), 64BIT (0x80), FLEX_BG (0x200),
// CSUM_SEED (0x2000).
const WRITE_SUPPORTED_INCOMPAT: u32 =
    0x2 | 0x40 | 0x80 | 0x200 | 0x2000;

// SPARSE_SUPER (0x1), LARGE_FILE (0x2), HUGE_FILE (0x8),
// GDT_CSUM (0x10), DIR_NLINK (0x20), EXTRA_ISIZE (0x40),
// METADATA_CSUM (0x400). GDT_CSUM без METADATA_CSUM не поддерживается.
const WRITE_SUPPORTED_RO_COMPAT: u32 =
    0x1 | 0x2 | 0x8 | 0x10 | 0x20 | 0x40 | 0x400;

const FEATURE_COMPAT_HAS_JOURNAL: u32 = 0x4;
const FEATURE_COMPAT_ORPHAN_FILE: u32 = 0x1000;
const FEATURE_INCOMPAT_RECOVER: u32 = 0x4;
const FEATURE_INCOMPAT_CSUM_SEED: u32 = 0x2000;
const FEATURE_RO_COMPAT_GDT_CSUM: u32 = 0x10;
const FEATURE_RO_COMPAT_METADATA_CSUM: u32 = 0x400;

const EXT4_EXTENTS_FL: u32 = 0x0008_0000;
const EXT4_INDEX_FL: u32 = 0x0000_1000;
const EXTENT_MAGIC: u16 = 0xF30A;
const EXTENT_HEADER_SIZE: usize = 12;
const EXTENT_ENTRY_SIZE: usize = 12;
const EXTENT_INIT_MAX_LEN: u16 = 0x8000;

const INODE_SIZE_MAX: usize = 1024;
const MAX_EXTENTS: usize = 128;
const MAX_INDEX_NODES: usize = 32;

// Смещения полей inode.
const INODE_MODE: usize = 0x00;
const INODE_SIZE_LO: usize = 0x04;
const INODE_ATIME: usize = 0x08;
const INODE_CTIME: usize = 0x0C;
const INODE_MTIME: usize = 0x10;
const INODE_DTIME: usize = 0x14;
const INODE_LINKS: usize = 0x1A;
const INODE_BLOCKS_LO: usize = 0x1C;
const INODE_FLAGS: usize = 0x20;
const INODE_BLOCK: usize = 0x28;
const INODE_GENERATION: usize = 0x64;
const INODE_SIZE_HIGH: usize = 0x6C;
const INODE_BLOCKS_HIGH: usize = 0x74;
const INODE_CHECKSUM_LO: usize = 0x7C;
const INODE_EXTRA_ISIZE: usize = 0x80;
const INODE_CHECKSUM_HI: usize = 0x82;

// Смещения полей group descriptor.
const GD_BLOCK_BITMAP: usize = 0x00;
const GD_INODE_BITMAP: usize = 0x04;
const GD_INODE_TABLE: usize = 0x08;
const GD_FREE_BLOCKS: usize = 0x0C;
const GD_FREE_INODES: usize = 0x0E;
const GD_USED_DIRS: usize = 0x10;
const GD_FLAGS: usize = 0x12;
const GD_BLOCK_BITMAP_CSUM: usize = 0x18;
const GD_INODE_BITMAP_CSUM: usize = 0x1A;
const GD_ITABLE_UNUSED: usize = 0x1C;
const GD_CHECKSUM: usize = 0x1E;
const GD_BLOCK_BITMAP_HI: usize = 0x20;
const GD_INODE_BITMAP_HI: usize = 0x24;
const GD_INODE_TABLE_HI: usize = 0x28;
const GD_FREE_BLOCKS_HI: usize = 0x2C;
const GD_FREE_INODES_HI: usize = 0x2E;
const GD_USED_DIRS_HI: usize = 0x30;
const GD_ITABLE_UNUSED_HI: usize = 0x32;
const GD_BLOCK_BITMAP_CSUM_HI: usize = 0x38;
const GD_INODE_BITMAP_CSUM_HI: usize = 0x3A;

const GD_FLAG_INODE_UNINIT: u16 = 0x0001;
const GD_FLAG_BLOCK_UNINIT: u16 = 0x0002;

// Фиксированная метка времени, чтобы новые inode не имели нулевые даты.
const DEFAULT_EXT4_TIME: u32 = 1_704_067_200; // 2024-01-01 00:00:00 UTC

fn crc32c(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82F6_3B78 & mask);
        }
    }
    crc
}

fn align4(value: usize) -> usize {
    (value + 3) & !3
}

/// Абстракция блочного устройства.
/// Реализацию для ATA подключим отдельно.
/// Абстракция блочного устройства.
pub trait BlockDevice {
    fn read_sector(
        &self,
        lba: u32,
        buffer: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), Ext4Error>;

    fn write_sector(
        &self,
        _lba: u32,
        _buffer: &[u8; SECTOR_SIZE],
    ) -> Result<(), Ext4Error> {
        Err(Ext4Error::UnsupportedFeature)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Superblock {
    pub inodes_count: u32,
    pub blocks_count: u64,
    pub reserved_blocks_count: u32,
    pub free_blocks_count: u64,
    pub free_inodes_count: u32,

    pub first_data_block: u32,
    pub block_size: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,

    pub inode_size: u16,
    pub revision: u32,

    pub feature_compat: u32,
    pub feature_incompat: u32,
    pub feature_ro_compat: u32,

    pub uuid: [u8; 16],

    pub desc_size: u16,
    pub groups_count: u32,
    pub log_groups_per_flex: u32,
    pub journal_inum: u32,
    pub checksum_seed: u32,
    pub checksum_type: u8,
    pub has_metadata_csum: bool,
    pub has_csum_seed: bool,
    pub want_extra_isize: u16,
    pub first_ino: u32,
    pub reserved_gdt_blocks: u16,
}
#[derive(Debug, Clone, Copy)]
pub struct InodeInfo {
    pub number: u32,
    pub mode: u16,
    pub size: u64,
    pub flags: u32,
    pub is_directory: bool,
    pub first_extent_block: u64,
    pub first_extent_logical: u32,
    pub first_extent_length: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct GroupDescriptor {
    pub block_bitmap: u64,
    pub inode_bitmap: u64,
    pub inode_table: u64,
    pub free_blocks_count: u32,
    pub free_inodes_count: u32,
    pub used_dirs_count: u32,
    pub flags: u16,
    pub checksum: u16,
    pub block_bitmap_csum: u32,
    pub inode_bitmap_csum: u32,
    pub itable_unused: u32,
}

pub struct Ext4<'a, D: BlockDevice> {
    device: &'a D,
    partition_start: u32,
    partition_sectors: u32,
    access: AccessMode,
    checksum_seed: u32,
    pub superblock: Superblock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtentMap {
    Data(u64),
    Zero,
}

#[derive(Debug, Clone, Copy)]
struct ExtentRecord {
    logical: u32,
    physical: u64,
    length: u16,
}

#[derive(Debug, Clone, Copy)]
struct IndexRecord {
    logical: u32,
    block: u64,
}

impl<'a, D: BlockDevice> Ext4<'a, D> {
    /// Подключает ext4-раздел в режиме только для чтения.
    ///
    /// partition_start — начальный LBA раздела на диске.
    pub fn mount(
        device: &'a D,
        partition_start: u32,
        partition_sectors: u32,
    ) -> Result<Self, Ext4Error> {
        Self::mount_with_access(
            device,
            partition_start,
            partition_sectors,
            AccessMode::ReadOnly,
        )
    }

    /// Подключает ext4-раздел в режиме чтения и записи.
    ///
    /// Разрешается только для файловых систем, функции которых
    /// безопасны для текущей реализации. Journal, RECOVER и
    /// контрольные суммы ext4 пока запрещают запись.
    pub fn mount_read_write(
        device: &'a D,
        partition_start: u32,
        partition_sectors: u32,
    ) -> Result<Self, Ext4Error> {
        Self::mount_with_access(
            device,
            partition_start,
            partition_sectors,
            AccessMode::ReadWrite,
        )
    }

    fn mount_with_access(
        device: &'a D,
        partition_start: u32,
        partition_sectors: u32,
        access: AccessMode,
    ) -> Result<Self, Ext4Error> {
        let mut raw = [0u8; 1024];

        if partition_sectors == 0 {
    return Err(Ext4Error::InvalidGeometry);
}

let partition_end = (partition_start as u64)
    .checked_add(partition_sectors as u64)
    .ok_or(Ext4Error::InvalidGeometry)?;

if partition_end > 0x1_0000_0000 {
    return Err(Ext4Error::InvalidGeometry);
}

        Self::read_bytes_from_device(
            device,
            partition_start,
            partition_sectors,
            SUPERBLOCK_OFFSET,
            &mut raw,
        )?;



        let magic = Self::read_u16(&raw, 0x38)?;

        if magic != EXT4_MAGIC {
            return Err(Ext4Error::InvalidSuperblock);
        }

        let inodes_count = Self::read_u32(&raw, 0x00)?;
        let blocks_count_lo = Self::read_u32(&raw, 0x04)?;
        let reserved_blocks_count = Self::read_u32(&raw, 0x08)?;
        let free_blocks_count_lo = Self::read_u32(&raw, 0x0C)?;
        let free_inodes_count = Self::read_u32(&raw, 0x10)?;

        let first_data_block = Self::read_u32(&raw, 0x14)?;
        let log_block_size = Self::read_u32(&raw, 0x18)?;

        if log_block_size > 2 {
            return Err(Ext4Error::UnsupportedBlockSize);
        }

        let block_size = 1024u32
            .checked_shl(log_block_size)
            .ok_or(Ext4Error::InvalidGeometry)?;

        if block_size < 1024 || block_size > 4096 {
            return Err(Ext4Error::UnsupportedBlockSize);
        }

        let expected_first_data_block = if block_size == 1024 {
    1
} else {
    0
};

if first_data_block != expected_first_data_block {
    return Err(Ext4Error::InvalidGeometry);
}

        let blocks_per_group = Self::read_u32(&raw, 0x20)?;
        let inodes_per_group = Self::read_u32(&raw, 0x28)?;

        let revision = Self::read_u32(&raw, 0x4C)?;
        if revision > 1 {
    return Err(Ext4Error::UnsupportedFeature);
}

        let inode_size = if revision == 0 {
            128
        } else {
            Self::read_u16(&raw, 0x58)?
        };

        let feature_compat = Self::read_u32(&raw, 0x5C)?;
        let feature_incompat = Self::read_u32(&raw, 0x60)?;
        let feature_ro_compat = Self::read_u32(&raw, 0x64)?;

        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&raw[0x68..0x78]);

        let has_metadata_csum =
            (feature_ro_compat & FEATURE_RO_COMPAT_METADATA_CSUM) != 0;
        let checksum_type = raw[0x175];
        let has_csum_seed =
            (feature_incompat & FEATURE_INCOMPAT_CSUM_SEED) != 0;

        if has_metadata_csum && checksum_type != 1 {
            return Err(Ext4Error::UnsupportedFeature);
        }

        let checksum_seed = if has_csum_seed {
            Self::read_u32(&raw, 0x270)?
        } else if has_metadata_csum {
            crc32c(!0, &uuid)
        } else {
            0
        };

        let log_groups_per_flex = raw[0x174] as u32;
        let journal_inum = Self::read_u32(&raw, 0xE0)?;
        let want_extra_isize = Self::read_u16(&raw, 0x15E)?;
        let reserved_gdt_blocks = Self::read_u16(&raw, 0xCE)?;
        let first_ino = Self::read_u32(&raw, 0x54)?;

        // 64-bit block counters.
        let has_64bit = (feature_incompat & 0x80) != 0;

        let desc_size = if has_64bit {
            Self::read_u16(&raw, 0xFE)?
        } else {
            32
        };

        if desc_size < 32 || desc_size as u32 > block_size {
            return Err(Ext4Error::InvalidGeometry);
        }

        let blocks_count_hi = if has_64bit {
            Self::read_u32(&raw, 0x150)?
        } else {
            0
        };

        let free_blocks_count_hi = if has_64bit {
            Self::read_u32(&raw, 0x158)?
        } else {
            0
        };

        let blocks_count =
            ((blocks_count_hi as u64) << 32) | blocks_count_lo as u64;

        let free_blocks_count =
            ((free_blocks_count_hi as u64) << 32)
                | free_blocks_count_lo as u64;
        let partition_bytes = (partition_sectors as u64)
    .checked_mul(SECTOR_SIZE as u64)
    .ok_or(Ext4Error::InvalidGeometry)?;

let filesystem_bytes = blocks_count
    .checked_mul(block_size as u64)
    .ok_or(Ext4Error::InvalidGeometry)?;

if filesystem_bytes > partition_bytes {
    return Err(Ext4Error::InvalidGeometry);
}

        if inodes_count == 0
            || blocks_count == 0
            || blocks_per_group == 0
            || inodes_per_group == 0
            || blocks_per_group > block_size * 8
            || inodes_per_group > block_size * 8
            || free_blocks_count > blocks_count
            || free_inodes_count > inodes_count
            || inode_size < 128
            || !inode_size.is_power_of_two()
            || inode_size as u32 > block_size
        {
            return Err(Ext4Error::InvalidGeometry);
        }

        // ext4 incompat features, поддерживаемые драйвером.
        //
        // FILETYPE (0x2), EXTENTS (0x40), 64BIT (0x80),
        // FLEX_BG (0x200), CSUM_SEED (0x2000).
        let supported_incompat = 0x2 | 0x40 | 0x80 | 0x200 | 0x2000;

        if feature_incompat & !supported_incompat != 0 {
            return Err(Ext4Error::UnsupportedFeature);
        }

        // Read-only compatible features:
        // We reject unknown features conservatively.
        //
        // Supported:
        // SPARSE_SUPER (0x1), LARGE_FILE (0x2),
        // HUGE_FILE (0x8), GDT_CSUM (0x10), DIR_NLINK (0x20),
        // EXTRA_ISIZE (0x40), METADATA_CSUM (0x400).
        let supported_ro_compat =
            0x1 | 0x2 | 0x8 | 0x10 | 0x20 | 0x40 | 0x400;

        if feature_ro_compat & !supported_ro_compat != 0 {
            return Err(Ext4Error::UnsupportedFeature);
        }

        if access == AccessMode::ReadWrite {
            Self::validate_write_safety(
                feature_compat,
                feature_incompat,
                feature_ro_compat,
            )?;
        }

        let groups_count = blocks_count
            .checked_sub(first_data_block as u64)
            .ok_or(Ext4Error::InvalidGeometry)?
            .checked_add(blocks_per_group as u64 - 1)
            .ok_or(Ext4Error::InvalidGeometry)?
            / blocks_per_group as u64;

        if groups_count == 0 || groups_count > u32::MAX as u64 {
            return Err(Ext4Error::InvalidGeometry);
        }

        Ok(Self {
            device,
            partition_start,
            partition_sectors,
            access,
            checksum_seed,
            superblock: Superblock {
                inodes_count,
                blocks_count,
                reserved_blocks_count,
                free_blocks_count,
                free_inodes_count,
                first_data_block,
                block_size,
                blocks_per_group,
                inodes_per_group,
                inode_size,
                revision,
                feature_compat,
                feature_incompat,
                feature_ro_compat,
                uuid,
                desc_size,
                groups_count: groups_count as u32,
                log_groups_per_flex,
                journal_inum,
                checksum_seed,
                checksum_type,
                has_metadata_csum,
                has_csum_seed,
                want_extra_isize,
                first_ino,
                reserved_gdt_blocks,
            },
        })
    }

    /// Проверяет, что запись в эту ФС не может повредить
    /// не поддерживаемые драйвером метаданные.
    fn validate_write_safety(
        feature_compat: u32,
        feature_incompat: u32,
        feature_ro_compat: u32,
    ) -> Result<(), Ext4Error> {
        // Незавершённое восстановление journal означает, что на диске
        // есть не применённые транзакции. Писать в таком состоянии нельзя.
        if feature_incompat & FEATURE_INCOMPAT_RECOVER != 0 {
            return Err(Ext4Error::UnsafeWrite);
        }

        // Чистый journal разрешён: драйвер не создаёт собственных
        // транзакций, но и не оставляет journal в состоянии RECOVER.
        // Это ограничение отражено в документации.

        // METADATA_CSUM поддерживается через CRC32C. Старый GDT_CSUM
        // без METADATA_CSUM не поддерживается.
        if feature_ro_compat & FEATURE_RO_COMPAT_GDT_CSUM != 0
            && feature_ro_compat & FEATURE_RO_COMPAT_METADATA_CSUM == 0
        {
            return Err(Ext4Error::UnsafeWrite);
        }

        // Любые функции вне явного списка также запрещают запись.
        if feature_compat & !WRITE_SUPPORTED_COMPAT != 0
            || feature_incompat & !WRITE_SUPPORTED_INCOMPAT != 0
            || feature_ro_compat & !WRITE_SUPPORTED_RO_COMPAT != 0
        {
            return Err(Ext4Error::UnsafeWrite);
        }

        // Флаг HAS_JOURNAL допустим только вместе с чистым journal;
        // ORPHAN_FILE не требует обновления при обычном создании файла.
        let _ = FEATURE_COMPAT_HAS_JOURNAL;
        let _ = FEATURE_COMPAT_ORPHAN_FILE;

        Ok(())
    }

fn write_bytes_to_device(
    device: &D,
    partition_start: u32,
    partition_sectors: u32,
    byte_offset: u64,
    input: &[u8],
) -> Result<(), Ext4Error> {
        let partition_bytes = (partition_sectors as u64)
        .checked_mul(SECTOR_SIZE as u64)
        .ok_or(Ext4Error::OutOfBounds)?;

    let end_offset = byte_offset
        .checked_add(input.len() as u64)
        .ok_or(Ext4Error::OutOfBounds)?;

    if end_offset > partition_bytes {
        return Err(Ext4Error::OutOfBounds);
    }
    let mut sector_buffer = [0u8; SECTOR_SIZE];

    let mut position = 0usize;
    let mut offset = byte_offset;

    while position < input.len() {
        let sector_index = offset / SECTOR_SIZE as u64;
        let sector_offset = (offset % SECTOR_SIZE as u64) as usize;

        let lba = (partition_start as u64)
            .checked_add(sector_index)
            .ok_or(Ext4Error::OutOfBounds)?;

        let lba = u32::try_from(lba)
            .map_err(|_| Ext4Error::OutOfBounds)?;

        let available = SECTOR_SIZE - sector_offset;
        let remaining = input.len() - position;
        let count = core::cmp::min(available, remaining);

        if sector_offset != 0 || count != SECTOR_SIZE {
            device.read_sector(lba, &mut sector_buffer)?;
        }

        sector_buffer[sector_offset..sector_offset + count]
            .copy_from_slice(&input[position..position + count]);

        device.write_sector(lba, &sector_buffer)?;

        position += count;
        offset += count as u64;
    }

    Ok(())
}
    /// Читает ext4-блок в предоставленный буфер.
    ///
    /// Буфер должен иметь размер не меньше block_size.
    pub fn read_block(
        &self,
        block_number: u64,
        buffer: &mut [u8],
    ) -> Result<(), Ext4Error> {
        let block_size = self.superblock.block_size as usize;

        if buffer.len() < block_size {
            return Err(Ext4Error::BufferTooSmall);
        }

        if block_number >= self.superblock.blocks_count {
            return Err(Ext4Error::OutOfBounds);
        }

        let byte_offset = block_number
            .checked_mul(block_size as u64)
            .ok_or(Ext4Error::OutOfBounds)?;

        let partition_bytes = (self.partition_sectors as u64)
    .checked_mul(SECTOR_SIZE as u64)
    .ok_or(Ext4Error::OutOfBounds)?;

let end_offset = byte_offset
    .checked_add(block_size as u64)
    .ok_or(Ext4Error::OutOfBounds)?;

if end_offset > partition_bytes {
    return Err(Ext4Error::OutOfBounds);
}

        Self::read_bytes_from_device(
            self.device,
            self.partition_start,
            self.partition_sectors,
            byte_offset,
            &mut buffer[..block_size],
        )
    }

    /// Записывает ext4-блок из предоставленного буфера.
    pub fn write_block(
        &self,
        block_number: u64,
        buffer: &[u8],
    ) -> Result<(), Ext4Error> {
    self.ensure_writable()?;

    let block_size = self.superblock.block_size as usize;

    if buffer.len() < block_size {
        return Err(Ext4Error::BufferTooSmall);
    }

    if block_number >= self.superblock.blocks_count {
        return Err(Ext4Error::OutOfBounds);
    }

    let byte_offset = block_number
        .checked_mul(block_size as u64)
        .ok_or(Ext4Error::OutOfBounds)?;

    Self::write_bytes_to_device(
        self.device,
        self.partition_start,
        self.partition_sectors,
        byte_offset,
        &buffer[..block_size],
    )
}

    /// Возвращает размер блока ext4.
    pub fn block_size(&self) -> u32 {
        self.superblock.block_size
    }

    /// Возвращает количество блоков.
    pub fn blocks_count(&self) -> u64 {
        self.superblock.blocks_count
    }

    /// Возвращает количество inode.
    pub fn inodes_count(&self) -> u32 {
        self.superblock.inodes_count
    }

    /// Возвращает UUID файловой системы.
    pub fn uuid(&self) -> &[u8; 16] {
        &self.superblock.uuid
    }

    /// Возвращает режим доступа, с которым смонтирована ФС.
    pub fn access_mode(&self) -> AccessMode {
        self.access
    }

    /// Проверяет, разрешена ли запись в эту ФС.
    pub fn is_writable(&self) -> bool {
        self.access == AccessMode::ReadWrite
    }

    fn ensure_writable(&self) -> Result<(), Ext4Error> {
        if self.access == AccessMode::ReadWrite {
            Ok(())
        } else {
            Err(Ext4Error::ReadOnly)
        }
    }

    fn read_bytes_from_device(
        device: &D,
        partition_start: u32,
        partition_sectors: u32,
        byte_offset: u64,
        output: &mut [u8],
    ) -> Result<(), Ext4Error> {
        let partition_bytes = (partition_sectors as u64)
    .checked_mul(SECTOR_SIZE as u64)
    .ok_or(Ext4Error::OutOfBounds)?;

let end_offset = byte_offset
    .checked_add(output.len() as u64)
    .ok_or(Ext4Error::OutOfBounds)?;

if end_offset > partition_bytes {
    return Err(Ext4Error::OutOfBounds);
}
        let mut sector_buffer = [0u8; SECTOR_SIZE];

        let mut position = 0usize;
        let mut offset = byte_offset;

        while position < output.len() {
            let sector_index = offset / SECTOR_SIZE as u64;
            let sector_offset = (offset % SECTOR_SIZE as u64) as usize;

            let lba = (partition_start as u64)
                .checked_add(sector_index)
                .ok_or(Ext4Error::OutOfBounds)?;

            let lba = u32::try_from(lba)
                .map_err(|_| Ext4Error::OutOfBounds)?;

            device.read_sector(lba, &mut sector_buffer)?;

            let available = SECTOR_SIZE - sector_offset;
            let remaining = output.len() - position;
            let count = core::cmp::min(available, remaining);

            output[position..position + count].copy_from_slice(
                &sector_buffer[sector_offset..sector_offset + count],
            );

            position += count;
            offset += count as u64;
        }

        Ok(())
    }

    fn read_u16(data: &[u8], offset: usize) -> Result<u16, Ext4Error> {
        let bytes = data
            .get(offset..offset + 2)
            .ok_or(Ext4Error::InvalidSuperblock)?;

        Ok(u16::from_le_bytes(
            bytes.try_into().map_err(|_| Ext4Error::InvalidSuperblock)?,
        ))
    }

    fn read_u32(data: &[u8], offset: usize) -> Result<u32, Ext4Error> {
        let bytes = data
            .get(offset..offset + 4)
            .ok_or(Ext4Error::InvalidSuperblock)?;

        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(|_| Ext4Error::InvalidSuperblock)?,
        ))
    }

    fn write_u16(data: &mut [u8], offset: usize, value: u16) {
        data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn descriptor_table_block(&self) -> u64 {
        if self.superblock.block_size == 1024 { 2 } else { 1 }
    }

    fn read_group_descriptor(
        &self,
        group: u32,
    ) -> Result<GroupDescriptor, Ext4Error> {
        if group >= self.superblock.groups_count {
            return Err(Ext4Error::OutOfBounds);
        }

        let desc_size = self.superblock.desc_size as usize;
        let block_size = self.superblock.block_size as usize;
        let desc_offset = (group as usize)
            .checked_mul(desc_size)
            .ok_or(Ext4Error::OutOfBounds)?;
        let block = self.descriptor_table_block()
            + (desc_offset / block_size) as u64;
        let offset = desc_offset % block_size;

        let mut buffer = [0u8; 4096];
        self.read_block(block, &mut buffer)?;
        let desc = &buffer[offset..offset + desc_size];

        let block_bitmap_lo = Self::read_u32(desc, GD_BLOCK_BITMAP)? as u64;
        let inode_bitmap_lo = Self::read_u32(desc, GD_INODE_BITMAP)? as u64;
        let inode_table_lo = Self::read_u32(desc, GD_INODE_TABLE)? as u64;
        let free_blocks_lo = Self::read_u16(desc, GD_FREE_BLOCKS)? as u32;
        let free_inodes_lo = Self::read_u16(desc, GD_FREE_INODES)? as u32;
        let used_dirs_lo = Self::read_u16(desc, GD_USED_DIRS)? as u32;
        let flags = Self::read_u16(desc, GD_FLAGS)?;
        let block_csum_lo = Self::read_u16(desc, GD_BLOCK_BITMAP_CSUM)? as u32;
        let inode_csum_lo = Self::read_u16(desc, GD_INODE_BITMAP_CSUM)? as u32;
        let itable_unused_lo = Self::read_u16(desc, GD_ITABLE_UNUSED)? as u32;
        let checksum = Self::read_u16(desc, GD_CHECKSUM)?;

        let (
            block_bitmap_hi,
            inode_bitmap_hi,
            inode_table_hi,
            free_blocks_hi,
            free_inodes_hi,
            used_dirs_hi,
            itable_unused_hi,
            block_csum_hi,
            inode_csum_hi,
        ) = if desc_size >= 64 {
            (
                Self::read_u32(desc, GD_BLOCK_BITMAP_HI)? as u64,
                Self::read_u32(desc, GD_INODE_BITMAP_HI)? as u64,
                Self::read_u32(desc, GD_INODE_TABLE_HI)? as u64,
                Self::read_u16(desc, GD_FREE_BLOCKS_HI)? as u32,
                Self::read_u16(desc, GD_FREE_INODES_HI)? as u32,
                Self::read_u16(desc, GD_USED_DIRS_HI)? as u32,
                Self::read_u16(desc, GD_ITABLE_UNUSED_HI)? as u32,
                Self::read_u16(desc, GD_BLOCK_BITMAP_CSUM_HI)? as u32,
                Self::read_u16(desc, GD_INODE_BITMAP_CSUM_HI)? as u32,
            )
        } else {
            (0, 0, 0, 0, 0, 0, 0, 0, 0)
        };

        Ok(GroupDescriptor {
            block_bitmap: (block_bitmap_hi << 32) | block_bitmap_lo,
            inode_bitmap: (inode_bitmap_hi << 32) | inode_bitmap_lo,
            inode_table: (inode_table_hi << 32) | inode_table_lo,
            free_blocks_count: (free_blocks_hi << 16) | free_blocks_lo,
            free_inodes_count: (free_inodes_hi << 16) | free_inodes_lo,
            used_dirs_count: (used_dirs_hi << 16) | used_dirs_lo,
            flags,
            checksum,
            block_bitmap_csum: (block_csum_hi << 16) | block_csum_lo,
            inode_bitmap_csum: (inode_csum_hi << 16) | inode_csum_lo,
            itable_unused: (itable_unused_hi << 16) | itable_unused_lo,
        })
    }

    fn group_desc_checksum(
        &self,
        group: u32,
        desc: &[u8],
    ) -> u16 {
        if !self.superblock.has_metadata_csum {
            return 0;
        }

        let desc_size = self.superblock.desc_size as usize;
        let mut csum = crc32c(self.checksum_seed, &group.to_le_bytes());
        csum = crc32c(csum, &desc[..GD_CHECKSUM]);
        csum = crc32c(csum, &[0u8, 0u8]);
        if GD_CHECKSUM + 2 < desc_size {
            csum = crc32c(csum, &desc[GD_CHECKSUM + 2..desc_size]);
        }
        csum as u16
    }

    fn write_group_descriptor(
        &self,
        group: u32,
        descriptor: &GroupDescriptor,
    ) -> Result<(), Ext4Error> {
        self.ensure_writable()?;

        if group >= self.superblock.groups_count {
            return Err(Ext4Error::OutOfBounds);
        }

        let desc_size = self.superblock.desc_size as usize;
        if desc_size > 256 {
            return Err(Ext4Error::UnsupportedFeature);
        }

        let mut desc = [0u8; 256];

        Self::write_u32(&mut desc, GD_BLOCK_BITMAP, descriptor.block_bitmap as u32);
        Self::write_u32(&mut desc, GD_INODE_BITMAP, descriptor.inode_bitmap as u32);
        Self::write_u32(&mut desc, GD_INODE_TABLE, descriptor.inode_table as u32);
        Self::write_u16(&mut desc, GD_FREE_BLOCKS, descriptor.free_blocks_count as u16);
        Self::write_u16(&mut desc, GD_FREE_INODES, descriptor.free_inodes_count as u16);
        Self::write_u16(&mut desc, GD_USED_DIRS, descriptor.used_dirs_count as u16);
        Self::write_u16(&mut desc, GD_FLAGS, descriptor.flags);
        Self::write_u16(
            &mut desc,
            GD_BLOCK_BITMAP_CSUM,
            descriptor.block_bitmap_csum as u16,
        );
        Self::write_u16(
            &mut desc,
            GD_INODE_BITMAP_CSUM,
            descriptor.inode_bitmap_csum as u16,
        );
        Self::write_u16(
            &mut desc,
            GD_ITABLE_UNUSED,
            if self.superblock.has_metadata_csum {
                descriptor.itable_unused as u16
            } else {
                0
            },
        );
        Self::write_u16(&mut desc, GD_CHECKSUM, 0);

        if desc_size >= 64 {
            Self::write_u32(
                &mut desc,
                GD_BLOCK_BITMAP_HI,
                (descriptor.block_bitmap >> 32) as u32,
            );
            Self::write_u32(
                &mut desc,
                GD_INODE_BITMAP_HI,
                (descriptor.inode_bitmap >> 32) as u32,
            );
            Self::write_u32(
                &mut desc,
                GD_INODE_TABLE_HI,
                (descriptor.inode_table >> 32) as u32,
            );
            Self::write_u16(
                &mut desc,
                GD_FREE_BLOCKS_HI,
                (descriptor.free_blocks_count >> 16) as u16,
            );
            Self::write_u16(
                &mut desc,
                GD_FREE_INODES_HI,
                (descriptor.free_inodes_count >> 16) as u16,
            );
            Self::write_u16(
                &mut desc,
                GD_USED_DIRS_HI,
                (descriptor.used_dirs_count >> 16) as u16,
            );
            Self::write_u16(
                &mut desc,
                GD_ITABLE_UNUSED_HI,
                (descriptor.itable_unused >> 16) as u16,
            );
            Self::write_u16(
                &mut desc,
                GD_BLOCK_BITMAP_CSUM_HI,
                (descriptor.block_bitmap_csum >> 16) as u16,
            );
            Self::write_u16(
                &mut desc,
                GD_INODE_BITMAP_CSUM_HI,
                (descriptor.inode_bitmap_csum >> 16) as u16,
            );
        }

        let checksum = self.group_desc_checksum(group, &desc[..desc_size]);
        Self::write_u16(&mut desc, GD_CHECKSUM, checksum);

        let block_size = self.superblock.block_size as usize;
        let desc_offset = (group as usize)
            .checked_mul(desc_size)
            .ok_or(Ext4Error::OutOfBounds)?;
        let block = self.descriptor_table_block()
            + (desc_offset / block_size) as u64;
        let offset = desc_offset % block_size;

        let mut buffer = [0u8; 4096];
        self.read_block(block, &mut buffer)?;
        buffer[offset..offset + desc_size]
            .copy_from_slice(&desc[..desc_size]);
        self.write_block(block, &buffer)?;

        Ok(())
    }

    fn bitmap_checksum(&self, data: &[u8]) -> u32 {
        crc32c(self.checksum_seed, data)
    }

    fn read_superblock_raw(
        &self,
    ) -> Result<[u8; 1024], Ext4Error> {
        let mut raw = [0u8; 1024];
        Self::read_bytes_from_device(
            self.device,
            self.partition_start,
            self.partition_sectors,
            SUPERBLOCK_OFFSET,
            &mut raw,
        )?;
        Ok(raw)
    }

    fn write_superblock_raw(
        &self,
        raw: &mut [u8; 1024],
    ) -> Result<(), Ext4Error> {
        self.ensure_writable()?;

        if self.superblock.has_metadata_csum {
            let checksum = crc32c(!0, &raw[..0x3FC]);
            Self::write_u32(raw, 0x3FC, checksum);
        }

        Self::write_bytes_to_device(
            self.device,
            self.partition_start,
            self.partition_sectors,
            SUPERBLOCK_OFFSET,
            raw,
        )
    }

    fn adjust_free_counts(
        &self,
        free_blocks_delta: i64,
        free_inodes_delta: i64,
    ) -> Result<(), Ext4Error> {
        self.ensure_writable()?;

        let mut raw = self.read_superblock_raw()?;
        let free_blocks_lo = Self::read_u32(&raw, 0x0C)? as u64;
        let free_blocks_hi = if self.superblock.feature_incompat & 0x80 != 0 {
            Self::read_u32(&raw, 0x158)? as u64
        } else {
            0
        };
        let free_blocks = ((free_blocks_hi << 32) | free_blocks_lo) as i64;
        let new_free_blocks = free_blocks
            .checked_add(free_blocks_delta)
            .ok_or(Ext4Error::OutOfBounds)?;

        if new_free_blocks < 0
            || new_free_blocks as u64 > self.superblock.blocks_count
        {
            return Err(Ext4Error::OutOfBounds);
        }

        let free_inodes = Self::read_u32(&raw, 0x10)? as i64;
        let new_free_inodes = free_inodes
            .checked_add(free_inodes_delta)
            .ok_or(Ext4Error::OutOfBounds)?;

        if new_free_inodes < 0
            || new_free_inodes as u64 > self.superblock.inodes_count as u64
        {
            return Err(Ext4Error::OutOfBounds);
        }

        let new_free_blocks = new_free_blocks as u64;
        Self::write_u32(&mut raw, 0x0C, new_free_blocks as u32);
        Self::write_u32(&mut raw, 0x10, new_free_inodes as u32);

        if self.superblock.feature_incompat & 0x80 != 0 {
            Self::write_u32(&mut raw, 0x158, (new_free_blocks >> 32) as u32);
        }

        self.write_superblock_raw(&mut raw)
    }

    fn inode_location(
        &self,
        inode_number: u32,
    ) -> Result<(u64, usize), Ext4Error> {
        if inode_number == 0 || inode_number > self.superblock.inodes_count {
            return Err(Ext4Error::OutOfBounds);
        }

        let group = (inode_number - 1) / self.superblock.inodes_per_group;
        let index = (inode_number - 1) % self.superblock.inodes_per_group;
        let descriptor = self.read_group_descriptor(group)?;

        let inode_offset = (index as u64)
            .checked_mul(self.superblock.inode_size as u64)
            .ok_or(Ext4Error::OutOfBounds)?;
        let block = descriptor.inode_table
            + inode_offset / self.superblock.block_size as u64;
        let offset =
            (inode_offset % self.superblock.block_size as u64) as usize;

        Ok((block, offset))
    }

    fn read_inode_raw(
        &self,
        inode_number: u32,
        output: &mut [u8],
    ) -> Result<(), Ext4Error> {
        let inode_size = self.superblock.inode_size as usize;
        if output.len() < inode_size {
            return Err(Ext4Error::BufferTooSmall);
        }

        let (block, offset) = self.inode_location(inode_number)?;
        let mut buffer = [0u8; 4096];
        self.read_block(block, &mut buffer)?;
        output[..inode_size]
            .copy_from_slice(&buffer[offset..offset + inode_size]);
        Ok(())
    }

    fn inode_checksum_seed(&self, inode_number: u32, generation: u32) -> u32 {
        let csum = crc32c(self.checksum_seed, &inode_number.to_le_bytes());
        crc32c(csum, &generation.to_le_bytes())
    }

    fn inode_checksum(
        &self,
        raw: &[u8],
        inode_number: u32,
    ) -> Result<u32, Ext4Error> {
        let inode_size = self.superblock.inode_size as usize;
        if raw.len() < inode_size {
            return Err(Ext4Error::BufferTooSmall);
        }

        let generation = Self::read_u32(raw, INODE_GENERATION)?;
        let mut csum =
            self.inode_checksum_seed(inode_number, generation);

        csum = crc32c(csum, &raw[..INODE_CHECKSUM_LO]);
        csum = crc32c(csum, &[0u8, 0u8]);
        csum = crc32c(csum, &raw[INODE_CHECKSUM_LO + 2..128]);

        if inode_size > 128 {
            let extra_isize =
                Self::read_u16(raw, INODE_EXTRA_ISIZE)? as usize;

            csum = crc32c(csum, &raw[128..INODE_CHECKSUM_HI]);

            if extra_isize >= 4
                && INODE_CHECKSUM_HI + 2 <= 128 + extra_isize
            {
                csum = crc32c(csum, &[0u8, 0u8]);
                csum = crc32c(csum, &raw[INODE_CHECKSUM_HI + 2..inode_size]);
            } else {
                csum = crc32c(csum, &raw[INODE_CHECKSUM_HI..inode_size]);
            }
        } else {
            csum = crc32c(csum, &raw[128..inode_size]);
        }

        Ok(csum)
    }

    fn update_inode_checksum(
        &self,
        raw: &mut [u8],
        inode_number: u32,
    ) -> Result<(), Ext4Error> {
        if !self.superblock.has_metadata_csum {
            return Ok(());
        }

        Self::write_u16(raw, INODE_CHECKSUM_LO, 0);

        let inode_size = self.superblock.inode_size as usize;
        let mut has_hi = false;
        if inode_size > 128 {
            let extra_isize =
                Self::read_u16(raw, INODE_EXTRA_ISIZE)? as usize;
            has_hi = extra_isize >= 4
                && INODE_CHECKSUM_HI + 2 <= 128 + extra_isize;
            if has_hi {
                Self::write_u16(raw, INODE_CHECKSUM_HI, 0);
            }
        }

        let csum = self.inode_checksum(raw, inode_number)?;
        Self::write_u16(raw, INODE_CHECKSUM_LO, csum as u16);
        if has_hi {
            Self::write_u16(raw, INODE_CHECKSUM_HI, (csum >> 16) as u16);
        }

        Ok(())
    }

    fn write_inode_raw(
        &self,
        inode_number: u32,
        raw: &mut [u8],
    ) -> Result<(), Ext4Error> {
        self.ensure_writable()?;

        let inode_size = self.superblock.inode_size as usize;
        if raw.len() < inode_size {
            return Err(Ext4Error::BufferTooSmall);
        }

        self.update_inode_checksum(raw, inode_number)?;

        let (block, offset) = self.inode_location(inode_number)?;
        let mut buffer = [0u8; 4096];
        self.read_block(block, &mut buffer)?;
        buffer[offset..offset + inode_size]
            .copy_from_slice(&raw[..inode_size]);
        self.write_block(block, &buffer)
    }

    fn read_bitmap_block(
        &self,
        block: u64,
        output: &mut [u8],
    ) -> Result<(), Ext4Error> {
        self.read_block(block, output)
    }

    fn write_bitmap_block(
        &self,
        block: u64,
        data: &[u8],
    ) -> Result<(), Ext4Error> {
        self.write_block(block, data)
    }

    fn bitmap_set(bitmap: &mut [u8], index: usize) {
        if index / 8 < bitmap.len() {
            bitmap[index / 8] |= 1u8 << (index % 8);
        }
    }

    /// true, если в группе есть резервная копия суперблока и GDT.
    fn bg_has_super(&self, group: u32) -> bool {
        if group == 0 {
            return true;
        }

        // SPARSE_SUPER2 (если включён) хранит номера двух групп
        // с резервными копиями.
        if self.superblock.feature_compat & 0x200 != 0 {
            let raw = match self.read_superblock_raw() {
                Ok(raw) => raw,
                Err(_) => return true,
            };
            let backup0 = u32::from_le_bytes([
                raw[0x24C], raw[0x24D], raw[0x24E], raw[0x24F],
            ]);
            let backup1 = u32::from_le_bytes([
                raw[0x250], raw[0x251], raw[0x252], raw[0x253],
            ]);
            return group == backup0 || group == backup1;
        }

        // Если SPARSE_SUPER выключен, копия есть в каждой группе.
        if self.superblock.feature_ro_compat & 0x1 == 0 {
            return true;
        }

        if group == 1 {
            return true;
        }

        // Резервные копии только в группах — степенях 3, 5 или 7.
        if group % 2 == 0 {
            return false;
        }

        fn power_of(base: u32, mut value: u32) -> bool {
            if value < base {
                return false;
            }
            while value % base == 0 {
                value /= base;
            }
            value == 1
        }

        power_of(3, group) || power_of(5, group) || power_of(7, group)
    }

    fn metadata_blocks_per_group(&self) -> u64 {
        let block_size = self.superblock.block_size as u64;
        let inode_size = self.superblock.inode_size as u64;
        let inodes = self.superblock.inodes_per_group as u64;
        (inodes * inode_size + block_size - 1) / block_size
    }

    /// Материализует bitmap блоков группы, помеченной BLOCK_UNINIT.
    ///
    /// После этого группа становится обычной: в bitmap отмечены
    /// суперблок/GDT (если есть), все блоки метаданных любых групп,
    /// попавшие в диапазон этой группы, и хвост за концом ФС.
    fn initialize_block_bitmap(
        &self,
        group: u32,
    ) -> Result<GroupDescriptor, Ext4Error> {
        self.ensure_writable()?;

        let mut descriptor = self.read_group_descriptor(group)?;
        let block_size = self.superblock.block_size as u64;
        let blocks_per_group = self.superblock.blocks_per_group as u64;
        let first = self.superblock.first_data_block as u64
            + group as u64 * blocks_per_group;
        let group_blocks = core::cmp::min(
            blocks_per_group,
            self.superblock.blocks_count.saturating_sub(first),
        );

        let bitmap_size = self.superblock.blocks_per_group as usize / 8;
        if bitmap_size > 4096 {
            return Err(Ext4Error::UnsupportedFeature);
        }

        let mut bitmap = [0u8; 4096];

        // Padding за пределами blocks_per_group в bitmap-блоке
        // помечаем занятым, как это делает mke2fs.
        let total_bits = self.superblock.block_size as usize * 8;
        let mut padding = self.superblock.blocks_per_group as usize;
        while padding < total_bits {
            Self::bitmap_set(&mut bitmap, padding);
            padding += 1;
        }

        // Хвост группы за концом файловой системы считается занятым.
        let mut index = group_blocks as usize;
        while index < self.superblock.blocks_per_group as usize {
            Self::bitmap_set(&mut bitmap, index);
            index += 1;
        }

        if self.bg_has_super(group) {
            let gdt_blocks = (self.superblock.groups_count as u64
                * self.superblock.desc_size as u64
                + block_size
                - 1)
                / block_size;
            let count = core::cmp::min(
                group_blocks,
                1 + gdt_blocks + self.superblock.reserved_gdt_blocks as u64,
            );
            for block in 0..count as usize {
                Self::bitmap_set(&mut bitmap, block);
            }
        }

        // Отмечаем все блоки метаданных (bitmap/inode table) любых групп,
        // физически находящиеся в диапазоне этой группы. Это корректно
        // и для flex_bg, где метаданные лежат в другой группе.
        let table_blocks = self.metadata_blocks_per_group();
        for other in 0..self.superblock.groups_count {
            let other_desc = self.read_group_descriptor(other)?;
            let ranges = [
                (other_desc.block_bitmap, 1u64),
                (other_desc.inode_bitmap, 1u64),
                (other_desc.inode_table, table_blocks),
            ];

            for &(start, length) in &ranges {
                let begin = core::cmp::max(start, first);
                let end = core::cmp::min(
                    start.saturating_add(length),
                    first + group_blocks,
                );
                if begin < end {
                    let mut block = begin;
                    while block < end {
                        Self::bitmap_set(
                            &mut bitmap,
                            (block - first) as usize,
                        );
                        block += 1;
                    }
                }
            }
        }

        let mut used = 0u64;
        for bit in 0..self.superblock.blocks_per_group as usize {
            if bitmap[bit / 8] & (1u8 << (bit % 8)) != 0 {
                used += 1;
            }
        }

        let new_free = (self.superblock.blocks_per_group as u64 - used) as u32;
        let old_free = descriptor.free_blocks_count;
        descriptor.free_blocks_count = new_free;
        descriptor.flags &= !GD_FLAG_BLOCK_UNINIT;
        descriptor.block_bitmap_csum =
            self.bitmap_checksum(&bitmap[..bitmap_size]);

        self.write_bitmap_block(descriptor.block_bitmap, &bitmap)?;
        self.write_group_descriptor(group, &descriptor)?;

        if new_free != old_free {
            self.adjust_free_counts(
                new_free as i64 - old_free as i64,
                0,
            )?;
        }

        Ok(descriptor)
    }

    /// Материализует inode bitmap группы, помеченной INODE_UNINIT.
    fn initialize_inode_bitmap(
        &self,
        group: u32,
    ) -> Result<GroupDescriptor, Ext4Error> {
        self.ensure_writable()?;

        let mut descriptor = self.read_group_descriptor(group)?;
        let inodes_per_group = self.superblock.inodes_per_group as usize;
        let bitmap_size = inodes_per_group / 8;
        if bitmap_size > 4096 {
            return Err(Ext4Error::UnsupportedFeature);
        }

        let mut bitmap = [0u8; 4096];

        // Байты bitmap за пределами inodes_per_group — это padding,
        // который должен быть помечен занятым (как это делает mke2fs).
        let total_bits = self.superblock.block_size as usize * 8;
        for index in inodes_per_group..total_bits {
            Self::bitmap_set(&mut bitmap, index);
        }

        // В последней группе несуществующие inode помечаем занятыми.
        let first_inode =
            group as u64 * inodes_per_group as u64 + 1;
        let last_inode = core::cmp::min(
            first_inode + inodes_per_group as u64 - 1,
            self.superblock.inodes_count as u64,
        );
        if last_inode < first_inode {
            // Группа целиком вне ФС (не должна встречаться).
            for index in 0..inodes_per_group {
                Self::bitmap_set(&mut bitmap, index);
            }
        } else {
            let existing = (last_inode - first_inode + 1) as usize;
            for index in existing..inodes_per_group {
                Self::bitmap_set(&mut bitmap, index);
            }
        }

        descriptor.flags &= !GD_FLAG_INODE_UNINIT;
        descriptor.inode_bitmap_csum =
            self.bitmap_checksum(&bitmap[..bitmap_size]);
        descriptor.itable_unused = inodes_per_group as u32;

        self.write_bitmap_block(descriptor.inode_bitmap, &bitmap)?;
        self.write_group_descriptor(group, &descriptor)?;

        Ok(descriptor)
    }

    fn allocate_block(&self) -> Result<u64, Ext4Error> {
        self.ensure_writable()?;

        let block_size = self.superblock.block_size as usize;
        let blocks_per_group = self.superblock.blocks_per_group as usize;

        for group in 0..self.superblock.groups_count {
            let mut descriptor = self.read_group_descriptor(group)?;

            if descriptor.flags & GD_FLAG_BLOCK_UNINIT != 0 {
                descriptor = self.initialize_block_bitmap(group)?;
            }

            if descriptor.free_blocks_count == 0 {
                continue;
            }

            let mut bitmap = [0u8; 4096];
            self.read_bitmap_block(descriptor.block_bitmap, &mut bitmap)?;

            for index in 0..blocks_per_group {
                let bit = 1u8 << (index % 8);
                let byte = index / 8;
                if bitmap[byte] & bit != 0 {
                    continue;
                }

                let block = self.superblock.first_data_block as u64
                    + (group as u64) * (blocks_per_group as u64)
                    + index as u64;

                if block >= self.superblock.blocks_count {
                    continue;
                }

                bitmap[byte] |= bit;
                self.write_bitmap_block(descriptor.block_bitmap, &bitmap)?;

                descriptor.free_blocks_count =
                    descriptor.free_blocks_count.saturating_sub(1);

                let bitmap_size = blocks_per_group / 8;
                descriptor.block_bitmap_csum =
                    self.bitmap_checksum(&bitmap[..bitmap_size]);
                self.write_group_descriptor(group, &descriptor)?;
                self.adjust_free_counts(-1, 0)?;

                let zero = [0u8; 4096];
                self.write_block(block, &zero[..block_size])?;

                return Ok(block);
            }
        }

        Err(Ext4Error::NoSpace)
    }

    fn free_block(&self, block: u64) -> Result<(), Ext4Error> {
        self.ensure_writable()?;

        if block < self.superblock.first_data_block as u64
            || block >= self.superblock.blocks_count
        {
            return Err(Ext4Error::OutOfBounds);
        }

        let relative = block - self.superblock.first_data_block as u64;
        let group = (relative / self.superblock.blocks_per_group as u64) as u32;
        let index = (relative % self.superblock.blocks_per_group as u64) as usize;
        let mut descriptor = self.read_group_descriptor(group)?;

        let mut bitmap = [0u8; 4096];
        self.read_bitmap_block(descriptor.block_bitmap, &mut bitmap)?;

        let bit = 1u8 << (index % 8);
        let byte = index / 8;
        if bitmap[byte] & bit == 0 {
            return Ok(());
        }

        bitmap[byte] &= !bit;
        self.write_bitmap_block(descriptor.block_bitmap, &bitmap)?;

        descriptor.free_blocks_count =
            descriptor.free_blocks_count.saturating_add(1);
        let bitmap_size = self.superblock.blocks_per_group as usize / 8;
        descriptor.block_bitmap_csum =
            self.bitmap_checksum(&bitmap[..bitmap_size]);
        self.write_group_descriptor(group, &descriptor)?;
        self.adjust_free_counts(1, 0)
    }

    fn allocate_inode(&self, is_directory: bool) -> Result<u32, Ext4Error> {
        self.ensure_writable()?;

        let inodes_per_group = self.superblock.inodes_per_group as usize;

        for group in 0..self.superblock.groups_count {
            let mut descriptor = self.read_group_descriptor(group)?;

            if descriptor.flags & GD_FLAG_INODE_UNINIT != 0 {
                descriptor = self.initialize_inode_bitmap(group)?;
            }

            if descriptor.free_inodes_count == 0 {
                continue;
            }

            let mut bitmap = [0u8; 4096];
            self.read_bitmap_block(descriptor.inode_bitmap, &mut bitmap)?;

            for index in 0..inodes_per_group {
                let inode_number = (group as u64)
                    .checked_mul(inodes_per_group as u64)
                    .ok_or(Ext4Error::OutOfBounds)?
                    + index as u64
                    + 1;

                if inode_number > self.superblock.inodes_count as u64
                    || inode_number < self.superblock.first_ino as u64
                {
                    continue;
                }

                let bit = 1u8 << (index % 8);
                let byte = index / 8;
                if bitmap[byte] & bit != 0 {
                    continue;
                }

                bitmap[byte] |= bit;
                self.write_bitmap_block(descriptor.inode_bitmap, &bitmap)?;

                descriptor.free_inodes_count =
                    descriptor.free_inodes_count.saturating_sub(1);
                if is_directory {
                    descriptor.used_dirs_count =
                        descriptor.used_dirs_count.saturating_add(1);
                }

                let mut highest_used = 0usize;
                for scan in (0..inodes_per_group).rev() {
                    if bitmap[scan / 8] & (1u8 << (scan % 8)) != 0 {
                        highest_used = scan;
                        break;
                    }
                }
                descriptor.itable_unused =
                    inodes_per_group.saturating_sub(highest_used + 1) as u32;

                let bitmap_size = inodes_per_group / 8;
                descriptor.inode_bitmap_csum =
                    self.bitmap_checksum(&bitmap[..bitmap_size]);
                self.write_group_descriptor(group, &descriptor)?;
                self.adjust_free_counts(0, -1)?;

                let (block, offset) = self.inode_location(inode_number as u32)?;
                let mut table_block = [0u8; 4096];
                self.read_block(block, &mut table_block)?;
                let inode_size = self.superblock.inode_size as usize;
                for byte in &mut table_block[offset..offset + inode_size] {
                    *byte = 0;
                }
                self.write_block(block, &table_block)?;

                return Ok(inode_number as u32);
            }
        }

        Err(Ext4Error::NoSpace)
    }

    fn free_inode(
        &self,
        inode_number: u32,
        is_directory: bool,
    ) -> Result<(), Ext4Error> {
        self.ensure_writable()?;

        if inode_number == 0 || inode_number > self.superblock.inodes_count {
            return Err(Ext4Error::OutOfBounds);
        }

        let group = (inode_number - 1) / self.superblock.inodes_per_group;
        let index = ((inode_number - 1) % self.superblock.inodes_per_group)
            as usize;
        let mut descriptor = self.read_group_descriptor(group)?;

        let mut bitmap = [0u8; 4096];
        self.read_bitmap_block(descriptor.inode_bitmap, &mut bitmap)?;

        let bit = 1u8 << (index % 8);
        let byte = index / 8;
        if bitmap[byte] & bit == 0 {
            return Ok(());
        }

        bitmap[byte] &= !bit;
        self.write_bitmap_block(descriptor.inode_bitmap, &bitmap)?;

        descriptor.free_inodes_count =
            descriptor.free_inodes_count.saturating_add(1);
        if is_directory {
            descriptor.used_dirs_count =
                descriptor.used_dirs_count.saturating_sub(1);
        }

        let bitmap_size = self.superblock.inodes_per_group as usize / 8;
        descriptor.inode_bitmap_csum =
            self.bitmap_checksum(&bitmap[..bitmap_size]);
        self.write_group_descriptor(group, &descriptor)?;
        self.adjust_free_counts(0, 1)
    }

pub fn read_inode(&self, inode_number: u32) -> Result<InodeInfo, Ext4Error> {
    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut raw = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(inode_number, &mut raw)?;
    let inode = &raw[..inode_size];

    let mode = Self::read_u16(inode, INODE_MODE)?;
    let size_lo = Self::read_u32(inode, INODE_SIZE_LO)? as u64;
    let flags = Self::read_u32(inode, INODE_FLAGS)?;

    let size = if mode & 0xF000 == 0x8000 {
        let size_hi = Self::read_u32(inode, INODE_SIZE_HIGH)? as u64;
        (size_hi << 32) | size_lo
    } else {
        size_lo
    };

    let mut first_extent_block = 0u64;
    let mut first_extent_logical = 0u32;
    let mut first_extent_length = 0u32;

    if flags & EXT4_EXTENTS_FL != 0 {
        let node = &inode[INODE_BLOCK..INODE_BLOCK + 60];
        let (entries, _, depth) = Self::extent_header(node)?;

        if depth == 0 && entries > 0 {
            let entry = &node[EXTENT_HEADER_SIZE..EXTENT_HEADER_SIZE + 12];
            let raw_len = Self::read_u16(entry, 4)?;
            if raw_len == 0 {
                return Err(Ext4Error::InvalidGeometry);
            }
            first_extent_logical = Self::read_u32(entry, 0)?;
            first_extent_length = if raw_len > EXTENT_INIT_MAX_LEN {
                (raw_len - EXTENT_INIT_MAX_LEN) as u32
            } else {
                raw_len as u32
            };
            let start_hi = Self::read_u16(entry, 6)? as u64;
            let start_lo = Self::read_u32(entry, 8)? as u64;
            first_extent_block = (start_hi << 32) | start_lo;
        }
    }

    Ok(InodeInfo {
        number: inode_number,
        mode,
        size,
        flags,
        is_directory: mode & 0xF000 == 0x4000,
        first_extent_block,
        first_extent_logical,
        first_extent_length,
    })
}

fn extent_header(node: &[u8]) -> Result<(usize, usize, u16), Ext4Error> {
    if node.len() < EXTENT_HEADER_SIZE {
        return Err(Ext4Error::BufferTooSmall);
    }

    let magic = Self::read_u16(node, 0)?;
    let entries = Self::read_u16(node, 2)? as usize;
    let max = Self::read_u16(node, 4)? as usize;
    let depth = Self::read_u16(node, 6)?;

    if magic != EXTENT_MAGIC {
        return Err(Ext4Error::InvalidGeometry);
    }

    let capacity = (node.len() - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE;
    if max > capacity || entries > max {
        return Err(Ext4Error::InvalidGeometry);
    }

    Ok((entries, max, depth))
}

fn write_extent_header(
    node: &mut [u8],
    entries: usize,
    max: usize,
    depth: u16,
) {
    Self::write_u16(node, 0, EXTENT_MAGIC);
    Self::write_u16(node, 2, entries as u16);
    Self::write_u16(node, 4, max as u16);
    Self::write_u16(node, 6, depth);
    Self::write_u16(node, 8, 0);
    Self::write_u32(node, 10, 0);
}

fn write_extent_entry(node: &mut [u8], offset: usize, extent: &ExtentRecord) {
    Self::write_u32(node, offset, extent.logical);
    Self::write_u16(node, offset + 4, extent.length);
    Self::write_u16(node, offset + 6, (extent.physical >> 32) as u16);
    Self::write_u32(node, offset + 8, extent.physical as u32);
}

fn write_index_entry(node: &mut [u8], offset: usize, index: &IndexRecord) {
    Self::write_u32(node, offset, index.logical);
    Self::write_u32(node, offset + 4, index.block as u32);
    Self::write_u16(node, offset + 8, (index.block >> 32) as u16);
    Self::write_u16(node, offset + 10, 0);
}

fn map_extent_node(
    &self,
    node: &[u8],
    logical: u32,
    level: u16,
) -> Result<ExtentMap, Ext4Error> {
    if level > 5 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let (entries, _, depth) = Self::extent_header(node)?;

    if depth == 0 {
        for index in 0..entries {
            let offset = EXTENT_HEADER_SIZE + index * EXTENT_ENTRY_SIZE;
            let start = Self::read_u32(node, offset)?;
            let raw_len = Self::read_u16(node, offset + 4)?;

            if raw_len == 0 {
                continue;
            }

            let (initialized, length) = if raw_len > EXTENT_INIT_MAX_LEN {
                (false, (raw_len - EXTENT_INIT_MAX_LEN) as u32)
            } else {
                (true, raw_len as u32)
            };

            if logical < start || logical >= start.saturating_add(length) {
                continue;
            }

            if !initialized {
                return Ok(ExtentMap::Zero);
            }

            let start_hi = Self::read_u16(node, offset + 6)? as u64;
            let start_lo = Self::read_u32(node, offset + 8)? as u64;
            let physical = (start_hi << 32) | start_lo;
            return Ok(ExtentMap::Data(
                physical + (logical - start) as u64,
            ));
        }

        return Ok(ExtentMap::Zero);
    }

    let mut chosen = None;
    for index in 0..entries {
        let offset = EXTENT_HEADER_SIZE + index * EXTENT_ENTRY_SIZE;
        let start = Self::read_u32(node, offset)?;
        if start <= logical {
            let leaf_lo = Self::read_u32(node, offset + 4)? as u64;
            let leaf_hi = Self::read_u16(node, offset + 8)? as u64;
            chosen = Some((leaf_hi << 32) | leaf_lo);
        } else {
            break;
        }
    }

    match chosen {
        Some(block) => {
            let mut child = [0u8; 4096];
            self.read_block(block, &mut child)?;
            let block_size = self.block_size() as usize;
            self.map_extent_node(&child[..block_size], logical, level + 1)
        }
        None => Ok(ExtentMap::Zero),
    }
}

fn inode_extent_map(
    &self,
    inode: &[u8],
    logical: u32,
) -> Result<ExtentMap, Ext4Error> {
    let flags = Self::read_u32(inode, INODE_FLAGS)?;
    if flags & EXT4_EXTENTS_FL == 0 {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let root = &inode[INODE_BLOCK..INODE_BLOCK + 60];
    self.map_extent_node(root, logical, 0)
}

fn collect_extent_node(
    &self,
    node: &[u8],
    extents: &mut [ExtentRecord],
    extent_count: &mut usize,
    indexes: &mut [u64],
    index_count: &mut usize,
    level: u16,
) -> Result<(), Ext4Error> {
    if level > 5 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let (entries, _, depth) = Self::extent_header(node)?;

    if depth == 0 {
        for index in 0..entries {
            let offset = EXTENT_HEADER_SIZE + index * EXTENT_ENTRY_SIZE;
            let logical = Self::read_u32(node, offset)?;
            let raw_len = Self::read_u16(node, offset + 4)?;

            if raw_len == 0 {
                continue;
            }

            let length = if raw_len > EXTENT_INIT_MAX_LEN {
                raw_len - EXTENT_INIT_MAX_LEN
            } else {
                raw_len
            };

            let start_hi = Self::read_u16(node, offset + 6)? as u64;
            let start_lo = Self::read_u32(node, offset + 8)? as u64;
            let physical = (start_hi << 32) | start_lo;

            if *extent_count >= extents.len() {
                return Err(Ext4Error::UnsupportedFeature);
            }

            extents[*extent_count] = ExtentRecord {
                logical,
                physical,
                length,
            };
            *extent_count += 1;
        }
    } else {
        for index in 0..entries {
            let offset = EXTENT_HEADER_SIZE + index * EXTENT_ENTRY_SIZE;
            let leaf_lo = Self::read_u32(node, offset + 4)? as u64;
            let leaf_hi = Self::read_u16(node, offset + 8)? as u64;
            let block = (leaf_hi << 32) | leaf_lo;

            if *index_count >= indexes.len() {
                return Err(Ext4Error::UnsupportedFeature);
            }
            indexes[*index_count] = block;
            *index_count += 1;

            let mut child = [0u8; 4096];
            self.read_block(block, &mut child)?;
            let block_size = self.block_size() as usize;
            self.collect_extent_node(
                &child[..block_size],
                extents,
                extent_count,
                indexes,
                index_count,
                level + 1,
            )?;
        }
    }

    Ok(())
}

fn collect_inode_tree(
    &self,
    inode: &[u8],
    extents: &mut [ExtentRecord],
    extent_count: &mut usize,
    indexes: &mut [u64],
    index_count: &mut usize,
) -> Result<(), Ext4Error> {
    let flags = Self::read_u32(inode, INODE_FLAGS)?;
    let size_lo = Self::read_u32(inode, INODE_SIZE_LO)? as u64;
    let size_hi = Self::read_u32(inode, INODE_SIZE_HIGH)? as u64;
    let size = (size_hi << 32) | size_lo;

    if flags & EXT4_EXTENTS_FL == 0 {
        if size == 0 {
            return Ok(());
        }
        return Err(Ext4Error::UnsupportedFeature);
    }

    let root = &inode[INODE_BLOCK..INODE_BLOCK + 60];
    self.collect_extent_node(
        root,
        extents,
        extent_count,
        indexes,
        index_count,
        0,
    )
}

fn free_tree_blocks(
    &self,
    extents: &[ExtentRecord],
    indexes: &[u64],
) -> Result<(), Ext4Error> {
    for extent in extents {
        let mut offset = 0u64;
        while offset < extent.length as u64 {
            self.free_block(extent.physical + offset)?;
            offset += 1;
        }
    }

    for &block in indexes {
        self.free_block(block)?;
    }

    Ok(())
}

fn set_extent_block_checksum(
    &self,
    inode_number: u32,
    generation: u32,
    block: &mut [u8],
) {
    if !self.superblock.has_metadata_csum {
        return;
    }

    let block_size = self.block_size() as usize;
    if block.len() < block_size || block_size < EXTENT_HEADER_SIZE {
        return;
    }

    let max = match Self::read_u16(block, 4) {
        Ok(max) => max as usize,
        Err(_) => return,
    };
    let tail_offset = EXTENT_HEADER_SIZE + max * EXTENT_ENTRY_SIZE;
    if tail_offset + 4 > block_size {
        return;
    }

    let seed = self.inode_checksum_seed(inode_number, generation);
    let checksum = crc32c(seed, &block[..tail_offset]);
    Self::write_u32(block, tail_offset, checksum);
}

fn build_extent_tree(
    &self,
    inode: &mut [u8],
    extents: &[ExtentRecord],
    inode_number: u32,
) -> Result<usize, Ext4Error> {
    let block_size = self.block_size() as usize;
    let root_capacity = (60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE;
    let generation = Self::read_u32(inode, INODE_GENERATION)?;
    let root = &mut inode[INODE_BLOCK..INODE_BLOCK + 60];

    for byte in root.iter_mut() {
        *byte = 0;
    }

    if extents.is_empty() {
        Self::write_extent_header(root, 0, root_capacity, 0);
        return Ok(0);
    }

    if extents.len() <= root_capacity {
        Self::write_extent_header(root, extents.len(), root_capacity, 0);
        for (index, extent) in extents.iter().enumerate() {
            let offset = EXTENT_HEADER_SIZE + index * EXTENT_ENTRY_SIZE;
            Self::write_extent_entry(root, offset, extent);
        }
        return Ok(0);
    }

    let leaf_capacity = (block_size - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE;
    let mut refs = [IndexRecord {
        logical: 0,
        block: 0,
    }; MAX_INDEX_NODES];
    let mut ref_count = 0usize;
    let mut allocated = 0usize;
    let mut position = 0usize;

    while position < extents.len() {
        let count = core::cmp::min(leaf_capacity, extents.len() - position);
        let block = self.allocate_block()?;
        allocated += 1;

        let mut buffer = [0u8; 4096];
        Self::write_extent_header(&mut buffer, count, leaf_capacity, 0);
        for index in 0..count {
            let offset = EXTENT_HEADER_SIZE + index * EXTENT_ENTRY_SIZE;
            Self::write_extent_entry(
                &mut buffer,
                offset,
                &extents[position + index],
            );
        }
        self.set_extent_block_checksum(
            inode_number,
            generation,
            &mut buffer,
        );
        self.write_block(block, &buffer)?;

        if ref_count >= refs.len() {
            return Err(Ext4Error::UnsupportedFeature);
        }
        refs[ref_count] = IndexRecord {
            logical: extents[position].logical,
            block,
        };
        ref_count += 1;
        position += count;
    }

    let mut level = 1u16;

    while ref_count > root_capacity {
        let capacity =
            (block_size - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE;
        let mut next = [IndexRecord {
            logical: 0,
            block: 0,
        }; MAX_INDEX_NODES];
        let mut next_count = 0usize;
        let mut position = 0usize;

        while position < ref_count {
            let count = core::cmp::min(capacity, ref_count - position);
            let block = self.allocate_block()?;
            allocated += 1;

            let mut buffer = [0u8; 4096];
            Self::write_extent_header(&mut buffer, count, capacity, level);
            for index in 0..count {
                let offset = EXTENT_HEADER_SIZE + index * EXTENT_ENTRY_SIZE;
                Self::write_index_entry(
                    &mut buffer,
                    offset,
                    &refs[position + index],
                );
            }
            self.set_extent_block_checksum(
                inode_number,
                generation,
                &mut buffer,
            );
            self.write_block(block, &buffer)?;

            if next_count >= next.len() {
                return Err(Ext4Error::UnsupportedFeature);
            }
            next[next_count] = IndexRecord {
                logical: refs[position].logical,
                block,
            };
            next_count += 1;
            position += count;
        }

        for index in 0..next_count {
            refs[index] = next[index];
        }
        ref_count = next_count;
        level += 1;

        if level > 5 {
            return Err(Ext4Error::UnsupportedFeature);
        }
    }

    Self::write_extent_header(root, ref_count, root_capacity, level);
    for index in 0..ref_count {
        let offset = EXTENT_HEADER_SIZE + index * EXTENT_ENTRY_SIZE;
        Self::write_index_entry(root, offset, &refs[index]);
    }

    Ok(allocated)
}

fn write_data_and_extents(
    &self,
    data: &[u8],
    extents: &mut [ExtentRecord],
    extent_count: &mut usize,
) -> Result<usize, Ext4Error> {
    let block_size = self.block_size() as usize;
    let blocks = (data.len() + block_size - 1) / block_size;
    let mut allocated_blocks = 0usize;

    for logical in 0..blocks {
        let block = match self.allocate_block() {
            Ok(block) => block,
            Err(error) => {
                let _ = self.free_tree_blocks(&extents[..*extent_count], &[]);
                return Err(error);
            }
        };

        let mut buffer = [0u8; 4096];
        let start = logical * block_size;
        let count = core::cmp::min(block_size, data.len() - start);
        buffer[..count].copy_from_slice(&data[start..start + count]);

        if let Err(error) = self.write_block(block, &buffer) {
            let _ = self.free_block(block);
            let _ = self.free_tree_blocks(&extents[..*extent_count], &[]);
            return Err(error);
        }

        if *extent_count > 0 {
            let last = &mut extents[*extent_count - 1];
            let expected_logical = last.logical as u64 + last.length as u64;
            let expected_physical = last.physical + last.length as u64;

            if expected_logical == logical as u64
                && expected_physical == block
                && last.length < EXTENT_INIT_MAX_LEN
            {
                last.length += 1;
                allocated_blocks += 1;
                continue;
            }
        }

        if *extent_count >= extents.len() {
            let _ = self.free_block(block);
            let _ = self.free_tree_blocks(&extents[..*extent_count], &[]);
            return Err(Ext4Error::NoSpace);
        }

        extents[*extent_count] = ExtentRecord {
            logical: logical as u32,
            physical: block,
            length: 1,
        };
        *extent_count += 1;
        allocated_blocks += 1;
    }

    Ok(allocated_blocks)
}

fn inode_blocks_sectors(&self) -> u64 {
    (self.superblock.block_size / 512) as u64
}

fn set_inode_size(&self, inode: &mut [u8], size: u64) -> Result<(), Ext4Error> {
    Self::write_u32(inode, INODE_SIZE_LO, size as u32);
    Self::write_u32(inode, INODE_SIZE_HIGH, (size >> 32) as u32);
    Ok(())
}

fn set_inode_blocks(
    &self,
    inode: &mut [u8],
    blocks: u64,
) -> Result<(), Ext4Error> {
    Self::write_u32(inode, INODE_BLOCKS_LO, blocks as u32);
    Self::write_u16(inode, INODE_BLOCKS_HIGH, (blocks >> 32) as u16);
    Ok(())
}

fn touch_inode_times(&self, inode: &mut [u8]) {
    Self::write_u32(inode, INODE_ATIME, DEFAULT_EXT4_TIME);
    Self::write_u32(inode, INODE_CTIME, DEFAULT_EXT4_TIME);
    Self::write_u32(inode, INODE_MTIME, DEFAULT_EXT4_TIME);
}

pub fn read_file(
    &self,
    inode_number: u32,
    offset: u64,
    output: &mut [u8],
) -> Result<usize, Ext4Error> {
    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut inode = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(inode_number, &mut inode)?;

    let mode = Self::read_u16(&inode, INODE_MODE)?;
    if mode & 0xF000 != 0x8000 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let size_lo = Self::read_u32(&inode, INODE_SIZE_LO)? as u64;
    let size_hi = Self::read_u32(&inode, INODE_SIZE_HIGH)? as u64;
    let size = (size_hi << 32) | size_lo;

    if offset >= size || output.is_empty() {
        return Ok(0);
    }

    let block_size = self.block_size() as u64;
    let available = size - offset;
    let count = core::cmp::min(output.len() as u64, available) as usize;
    let mut copied = 0usize;

    while copied < count {
        let file_position = offset + copied as u64;
        let logical_block = (file_position / block_size) as u32;
        let block_offset = (file_position % block_size) as usize;
        let chunk = core::cmp::min(
            block_size as usize - block_offset,
            count - copied,
        );

        match self.inode_extent_map(&inode[..inode_size], logical_block)? {
            ExtentMap::Data(physical) => {
                let mut block = [0u8; 4096];
                self.read_block(physical, &mut block)?;
                output[copied..copied + chunk]
                    .copy_from_slice(&block[block_offset..block_offset + chunk]);
            }
            ExtentMap::Zero => {
                for byte in &mut output[copied..copied + chunk] {
                    *byte = 0;
                }
            }
        }

        copied += chunk;
    }

    Ok(copied)
}

pub fn for_each_directory_entry<F>(
    &self,
    directory_inode: u32,
    mut callback: F,
) -> Result<(), Ext4Error>
where
    F: FnMut(u32, &[u8], u8) -> bool,
{
    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut directory = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(directory_inode, &mut directory)?;

    let mode = Self::read_u16(&directory, INODE_MODE)?;
    if mode & 0xF000 != 0x4000 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let size = Self::read_u32(&directory, INODE_SIZE_LO)? as u64;
    if size == 0 {
        return Ok(());
    }

    let block_size = self.block_size() as usize;
    let limit = self.dir_data_limit();
    let blocks = ((size + block_size as u64 - 1) / block_size as u64)
        as usize;

    for logical in 0..blocks {
        let physical = match self.inode_extent_map(
            &directory[..inode_size],
            logical as u32,
        )? {
            ExtentMap::Data(block) => block,
            ExtentMap::Zero => continue,
        };

        let mut block = [0u8; 4096];
        self.read_block(physical, &mut block)?;

        let mut offset = 0usize;
        while offset + 8 <= limit {
            let inode = u32::from_le_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);

            let rec_len = u16::from_le_bytes([
                block[offset + 4],
                block[offset + 5],
            ]) as usize;

            let name_len = block[offset + 6] as usize;
            let file_type = block[offset + 7];

            if rec_len < 8 || offset + rec_len > limit {
                return Err(Ext4Error::InvalidGeometry);
            }

            if name_len > rec_len - 8 {
                return Err(Ext4Error::InvalidGeometry);
            }

            if inode != 0 {
                let name = &block[offset + 8..offset + 8 + name_len];
                if !callback(inode, name, file_type) {
                    return Ok(());
                }
            }

            offset += rec_len;
        }
    }

    Ok(())
}

pub fn find_directory_entry(
    &self,
    directory_inode: u32,
    name: &[u8],
) -> Result<Option<u32>, Ext4Error> {
    let mut found = None;

    self.for_each_directory_entry(
        directory_inode,
        |inode, entry_name, _file_type| {
            if entry_name == name {
                found = Some(inode);
                false
            } else {
                true
            }
        },
    )?;

    Ok(found)
}

fn dir_data_limit(&self) -> usize {
    let block_size = self.block_size() as usize;
    if self.superblock.has_metadata_csum {
        block_size.saturating_sub(12)
    } else {
        block_size
    }
}

fn init_empty_dir_block(&self, block: &mut [u8]) {
    let limit = self.dir_data_limit();
    for byte in block.iter_mut() {
        *byte = 0;
    }

    Self::write_u16(block, 4, limit as u16);

    if self.superblock.has_metadata_csum {
        Self::write_u32(block, limit, 0);
        Self::write_u16(block, limit + 4, 12);
        block[limit + 6] = 0;
        block[limit + 7] = 0xDE;
        Self::write_u32(block, limit + 8, 0);
    }
}

fn set_dir_tail_checksum(
    &self,
    directory_inode: u32,
    generation: u32,
    block: &mut [u8],
) {
    if !self.superblock.has_metadata_csum {
        return;
    }

    let limit = self.dir_data_limit();
    let seed = self.inode_checksum_seed(directory_inode, generation);
    let checksum = crc32c(seed, &block[..limit]);
    Self::write_u32(block, limit + 8, checksum);
}

fn insert_dirent(
    &self,
    block: &mut [u8],
    name: &[u8],
    inode_number: u32,
    file_type: u8,
) -> Result<bool, Ext4Error> {
    if name.is_empty() || name.len() > 255 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let limit = self.dir_data_limit();
    let needed = align4(8 + name.len());
    let mut offset = 0usize;

    while offset + 8 <= limit {
        let inode = u32::from_le_bytes([
            block[offset],
            block[offset + 1],
            block[offset + 2],
            block[offset + 3],
        ]);

        let rec_len = u16::from_le_bytes([
            block[offset + 4],
            block[offset + 5],
        ]) as usize;

        let name_len = block[offset + 6] as usize;

        if rec_len < 8 || offset + rec_len > limit {
            return Err(Ext4Error::InvalidGeometry);
        }

        if name_len > rec_len - 8 {
            return Err(Ext4Error::InvalidGeometry);
        }

        if inode == 0 {
            if rec_len >= needed {
                Self::write_u32(block, offset, inode_number);
                Self::write_u16(block, offset + 4, rec_len as u16);
                block[offset + 6] = name.len() as u8;
                block[offset + 7] = file_type;
                block[offset + 8..offset + 8 + name.len()]
                    .copy_from_slice(name);
                return Ok(true);
            }
        } else {
            let used = align4(8 + name_len);
            if rec_len >= used + needed {
                Self::write_u16(block, offset + 4, used as u16);

                let new_offset = offset + used;
                let new_rec_len = rec_len - used;
                Self::write_u32(block, new_offset, inode_number);
                Self::write_u16(block, new_offset + 4, new_rec_len as u16);
                block[new_offset + 6] = name.len() as u8;
                block[new_offset + 7] = file_type;
                block[new_offset + 8..new_offset + 8 + name.len()]
                    .copy_from_slice(name);
                return Ok(true);
            }
        }

        offset += rec_len;
    }

    Ok(false)
}

fn add_directory_entry(
    &self,
    directory_inode: u32,
    name: &[u8],
    new_inode: u32,
    file_type: u8,
) -> Result<(), Ext4Error> {
    self.ensure_writable()?;

    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut directory = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(directory_inode, &mut directory)?;

    let mode = Self::read_u16(&directory, INODE_MODE)?;
    if mode & 0xF000 != 0x4000 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let flags = Self::read_u32(&directory, INODE_FLAGS)?;
    if flags & EXT4_INDEX_FL != 0 {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let generation = Self::read_u32(&directory, INODE_GENERATION)?;
    let block_size = self.block_size() as usize;
    let size = Self::read_u32(&directory, INODE_SIZE_LO)? as u64;
    let blocks = ((size + block_size as u64 - 1) / block_size as u64)
        as usize;

    for logical in 0..blocks {
        let physical = match self.inode_extent_map(
            &directory[..inode_size],
            logical as u32,
        )? {
            ExtentMap::Data(block) => block,
            ExtentMap::Zero => continue,
        };

        let mut block = [0u8; 4096];
        self.read_block(physical, &mut block)?;

        if self.insert_dirent(&mut block, name, new_inode, file_type)? {
            self.set_dir_tail_checksum(
                directory_inode,
                generation,
                &mut block,
            );
            self.write_block(physical, &block)?;
            self.touch_inode_times(&mut directory);
            self.write_inode_raw(directory_inode, &mut directory)?;
            return Ok(());
        }
    }

    // В существующих блоках места нет — добавляем новый блок каталога.
    let logical = (size / block_size as u64) as u32;
    let physical = self.allocate_block()?;

    let mut block = [0u8; 4096];
    self.init_empty_dir_block(&mut block);

    if !self.insert_dirent(&mut block, name, new_inode, file_type)? {
        let _ = self.free_block(physical);
        return Err(Ext4Error::NoSpace);
    }

    self.set_dir_tail_checksum(directory_inode, generation, &mut block);

    let mut old_extents = [ExtentRecord {
        logical: 0,
        physical: 0,
        length: 0,
    }; MAX_EXTENTS];
    let mut old_extent_count = 0usize;
    let mut old_indexes = [0u64; MAX_INDEX_NODES];
    let mut old_index_count = 0usize;
    self.collect_inode_tree(
        &directory[..inode_size],
        &mut old_extents,
        &mut old_extent_count,
        &mut old_indexes,
        &mut old_index_count,
    )?;

    let mut extents = old_extents;
    let mut extent_count = old_extent_count;

    if extent_count > 0 {
        let last = &mut extents[extent_count - 1];
        let expected_logical = last.logical as u64 + last.length as u64;
        let expected_physical = last.physical + last.length as u64;

        if expected_logical == logical as u64
            && expected_physical == physical
            && last.length < EXTENT_INIT_MAX_LEN
        {
            last.length += 1;
        } else {
            if extent_count >= extents.len() {
                let _ = self.free_block(physical);
                return Err(Ext4Error::NoSpace);
            }
            extents[extent_count] = ExtentRecord {
                logical,
                physical,
                length: 1,
            };
            extent_count += 1;
        }
    } else {
        extents[0] = ExtentRecord {
            logical,
            physical,
            length: 1,
        };
        extent_count = 1;
    }

    let old_blocks = Self::read_u32(&directory, INODE_BLOCKS_LO)? as u64
        | ((Self::read_u16(&directory, INODE_BLOCKS_HIGH)? as u64) << 32);

    let tree_blocks = match self.build_extent_tree(
        &mut directory,
        &extents[..extent_count],
        directory_inode,
    ) {
        Ok(count) => count,
        Err(error) => {
            let _ = self.free_block(physical);
            return Err(error);
        }
    };

    let sectors = self.inode_blocks_sectors();
    let new_tree_blocks = 1 + tree_blocks;
    let new_blocks = if new_tree_blocks >= old_index_count {
        let added = (new_tree_blocks - old_index_count) as u64;
        old_blocks
            .checked_add(added.saturating_mul(sectors))
            .ok_or(Ext4Error::OutOfBounds)?
    } else {
        let removed = (old_index_count - new_tree_blocks) as u64;
        old_blocks
            .checked_sub(removed.saturating_mul(sectors))
            .ok_or(Ext4Error::OutOfBounds)?
    };

    self.set_inode_size(&mut directory, size + block_size as u64)?;
    self.set_inode_blocks(&mut directory, new_blocks)?;
    self.touch_inode_times(&mut directory);

    self.write_block(physical, &block)?;
    self.write_inode_raw(directory_inode, &mut directory)?;

    for &index in &old_indexes[..old_index_count] {
        self.free_block(index)?;
    }

    Ok(())
}

fn create_inode(&self, data: &[u8]) -> Result<u32, Ext4Error> {
    self.ensure_writable()?;

    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let inode_number = self.allocate_inode(false)?;
    let mut inode = [0u8; INODE_SIZE_MAX];

    let mut extents = [ExtentRecord {
        logical: 0,
        physical: 0,
        length: 0,
    }; MAX_EXTENTS];
    let mut extent_count = 0usize;

    let data_blocks = match self.write_data_and_extents(
        data,
        &mut extents,
        &mut extent_count,
    ) {
        Ok(blocks) => blocks,
        Err(error) => {
            let _ = self.free_inode(inode_number, false);
            return Err(error);
        }
    };

    Self::write_u16(&mut inode, INODE_MODE, 0x81A4);
    Self::write_u16(&mut inode, INODE_LINKS, 1);
    Self::write_u32(&mut inode, INODE_FLAGS, EXT4_EXTENTS_FL);
    Self::write_u32(&mut inode, INODE_DTIME, 0);

    if inode_size > 128 {
        let mut extra = self.superblock.want_extra_isize as usize;
        if extra < 32 {
            extra = 32;
        }
        extra &= !3;
        if extra > inode_size - 128 {
            extra = inode_size - 128;
        }
        Self::write_u16(&mut inode, INODE_EXTRA_ISIZE, extra as u16);
    }

    let tree_blocks = match self.build_extent_tree(
        &mut inode,
        &extents[..extent_count],
        inode_number,
    ) {
        Ok(blocks) => blocks,
        Err(error) => {
            let _ = self.free_tree_blocks(&extents[..extent_count], &[]);
            let _ = self.free_inode(inode_number, false);
            return Err(error);
        }
    };

    self.set_inode_size(&mut inode, data.len() as u64)?;
    let blocks = (data_blocks + tree_blocks) as u64
        * self.inode_blocks_sectors();
    self.set_inode_blocks(&mut inode, blocks)?;
    self.touch_inode_times(&mut inode);

    if let Err(error) = self.write_inode_raw(inode_number, &mut inode) {
        let _ = self.free_tree_blocks(&extents[..extent_count], &[]);
        let _ = self.free_inode(inode_number, false);
        return Err(error);
    }

    Ok(inode_number)
}

pub fn create_file(
    &self,
    parent_inode: u32,
    name: &[u8],
    data: &[u8],
) -> Result<u32, Ext4Error> {
    self.ensure_writable()?;

    if name.is_empty() || name.len() > 255 || name == b"." || name == b".." {
        return Err(Ext4Error::InvalidGeometry);
    }

    let parent = self.read_inode(parent_inode)?;
    if !parent.is_directory {
        return Err(Ext4Error::InvalidGeometry);
    }

    if self.find_directory_entry(parent_inode, name)?.is_some() {
        return Err(Ext4Error::InvalidGeometry);
    }

    let inode_number = self.create_inode(data)?;

    if let Err(error) =
        self.add_directory_entry(parent_inode, name, inode_number, 1)
    {
        if let Ok(raw) = {
            let mut buffer = [0u8; INODE_SIZE_MAX];
            self.read_inode_raw(inode_number, &mut buffer).map(|_| buffer)
        } {
            let mut extents = [ExtentRecord {
                logical: 0,
                physical: 0,
                length: 0,
            }; MAX_EXTENTS];
            let mut extent_count = 0usize;
            let mut indexes = [0u64; MAX_INDEX_NODES];
            let mut index_count = 0usize;
            if self.collect_inode_tree(
                &raw[..self.superblock.inode_size as usize],
                &mut extents,
                &mut extent_count,
                &mut indexes,
                &mut index_count,
            ).is_ok() {
                let _ = self.free_tree_blocks(
                    &extents[..extent_count],
                    &indexes[..index_count],
                );
            }
        }
        let _ = self.free_inode(inode_number, false);
        return Err(error);
    }

    Ok(inode_number)
}

/// Создаёт каталог по пути. Родительский каталог должен существовать.
pub fn mkdir_path(&self, path: &[u8]) -> Result<u32, Ext4Error> {
    self.ensure_writable()?;

    let mut parent = 2u32;
    let mut name: &[u8] = b"";

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        if !name.is_empty() {
            parent = self
                .find_directory_entry(parent, name)?
                .ok_or(Ext4Error::OutOfBounds)?;
            let inode = self.read_inode(parent)?;
            if !inode.is_directory {
                return Err(Ext4Error::InvalidGeometry);
            }
        }

        name = component;
    }

    if name.is_empty() || name.len() > 255 || name == b"." || name == b".." {
        return Err(Ext4Error::InvalidGeometry);
    }

    self.create_directory(parent, name)
}

/// Создаёт каталог в существующем каталоге.
///
/// Новый каталог получает один блок с записями `.` и `..`, поэтому
/// его можно сразу использовать как родительский для файлов.
pub fn create_directory(
    &self,
    parent_inode: u32,
    name: &[u8],
) -> Result<u32, Ext4Error> {
    self.ensure_writable()?;

    if name.is_empty() || name.len() > 255 || name == b"." || name == b".." {
        return Err(Ext4Error::InvalidGeometry);
    }

    let parent = self.read_inode(parent_inode)?;
    if !parent.is_directory {
        return Err(Ext4Error::InvalidGeometry);
    }

    if self.find_directory_entry(parent_inode, name)?.is_some() {
        return Err(Ext4Error::InvalidGeometry);
    }

    let inode_number = self.allocate_inode(true)?;
    let block = match self.allocate_block() {
        Ok(block) => block,
        Err(error) => {
            let _ = self.free_inode(inode_number, true);
            return Err(error);
        }
    };

    let block_size = self.block_size() as usize;
    let limit = self.dir_data_limit();
    let mut data = [0u8; 4096];

    // Запись "." указывает на сам каталог.
    Self::write_u32(&mut data, 0, inode_number);
    Self::write_u16(&mut data, 4, 12);
    data[6] = 1;
    data[7] = 2;
    data[8] = b'.';

    // Запись ".." указывает на родительский каталог.
    Self::write_u32(&mut data, 12, parent_inode);
    Self::write_u16(&mut data, 16, (limit - 12) as u16);
    data[18] = 2;
    data[19] = 2;
    data[20] = b'.';
    data[21] = b'.';

    if self.superblock.has_metadata_csum {
        Self::write_u32(&mut data, limit, 0);
        Self::write_u16(&mut data, limit + 4, 12);
        data[limit + 6] = 0;
        data[limit + 7] = 0xDE;
    }

    self.set_dir_tail_checksum(inode_number, 0, &mut data);

    if let Err(error) = self.write_block(block, &data) {
        let _ = self.free_block(block);
        let _ = self.free_inode(inode_number, true);
        return Err(error);
    }

    let inode_size = self.superblock.inode_size as usize;
    let mut inode = [0u8; INODE_SIZE_MAX];
    let mut extents = [ExtentRecord {
        logical: 0,
        physical: 0,
        length: 0,
    }; MAX_EXTENTS];
    extents[0] = ExtentRecord {
        logical: 0,
        physical: block,
        length: 1,
    };

    Self::write_u16(&mut inode, INODE_MODE, 0x41ED);
    Self::write_u16(&mut inode, INODE_LINKS, 2);
    Self::write_u32(&mut inode, INODE_FLAGS, EXT4_EXTENTS_FL);
    Self::write_u32(&mut inode, INODE_DTIME, 0);

    if inode_size > 128 {
        let mut extra = self.superblock.want_extra_isize as usize;
        if extra < 32 {
            extra = 32;
        }
        extra &= !3;
        if extra > inode_size - 128 {
            extra = inode_size - 128;
        }
        Self::write_u16(&mut inode, INODE_EXTRA_ISIZE, extra as u16);
    }

    let tree_blocks = match self.build_extent_tree(
        &mut inode,
        &extents[..1],
        inode_number,
    ) {
        Ok(blocks) => blocks,
        Err(error) => {
            let _ = self.free_tree_blocks(&extents[..1], &[]);
            let _ = self.free_inode(inode_number, true);
            return Err(error);
        }
    };

    self.set_inode_size(&mut inode, block_size as u64)?;
    let blocks = (1 + tree_blocks) as u64 * self.inode_blocks_sectors();
    self.set_inode_blocks(&mut inode, blocks)?;
    self.touch_inode_times(&mut inode);

    if let Err(error) = self.write_inode_raw(inode_number, &mut inode) {
        let _ = self.free_tree_blocks(&extents[..1], &[]);
        let _ = self.free_inode(inode_number, true);
        return Err(error);
    }

    if let Err(error) = self.add_directory_entry(parent_inode, name, inode_number, 2) {
        let _ = self.free_tree_blocks(&extents[..1], &[]);
        let _ = self.free_inode(inode_number, true);
        return Err(error);
    }

    // Каталог ".." нового каталога ссылается на родителя, поэтому у
    // родителя увеличивается число ссылок.
    let mut parent_raw = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(parent_inode, &mut parent_raw)?;
    let links = Self::read_u16(&parent_raw, INODE_LINKS)?;
    Self::write_u16(&mut parent_raw, INODE_LINKS, links.saturating_add(1));
    self.touch_inode_times(&mut parent_raw);
    self.write_inode_raw(parent_inode, &mut parent_raw)?;

    Ok(inode_number)
}

/// Проверяет, существует ли путь в файловой системе.
pub fn path_exists(&self, path: &[u8]) -> Result<bool, Ext4Error> {
    let mut current = 2u32;
    let mut name: &[u8] = b"";

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        if !name.is_empty() {
            current = match self.find_directory_entry(current, name)? {
                Some(inode_number) => inode_number,
                None => return Ok(false),
            };

            if !self.read_inode(current)?.is_directory {
                return Ok(false);
            }
        }

        name = component;
    }

    if name.is_empty() {
        return Ok(true);
    }

    Ok(self.find_directory_entry(current, name)?.is_some())
}

/// Создаёт все отсутствующие каталоги пути и возвращает inode последнего.
pub fn ensure_directory_path(&self, path: &[u8]) -> Result<u32, Ext4Error> {
    self.ensure_writable()?;

    let mut current = 2u32;
    let mut name: &[u8] = b"";

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        if !name.is_empty() {
            current = match self.find_directory_entry(current, name)? {
                Some(inode_number) => {
                    if !self.read_inode(inode_number)?.is_directory {
                        return Err(Ext4Error::InvalidGeometry);
                    }
                    inode_number
                }
                None => self.create_directory(current, name)?,
            };
        }

        name = component;
    }

    if !name.is_empty() {
        current = match self.find_directory_entry(current, name)? {
            Some(inode_number) => {
                if !self.read_inode(inode_number)?.is_directory {
                    return Err(Ext4Error::InvalidGeometry);
                }
                inode_number
            }
            None => self.create_directory(current, name)?,
        };
    }

    Ok(current)
}

pub fn write_file_contents(
    &self,
    inode_number: u32,
    data: &[u8],
) -> Result<(), Ext4Error> {
    self.ensure_writable()?;

    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut old_inode = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(inode_number, &mut old_inode)?;

    let mode = Self::read_u16(&old_inode, INODE_MODE)?;
    if mode & 0xF000 != 0x8000 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let mut old_extents = [ExtentRecord {
        logical: 0,
        physical: 0,
        length: 0,
    }; MAX_EXTENTS];
    let mut old_extent_count = 0usize;
    let mut old_indexes = [0u64; MAX_INDEX_NODES];
    let mut old_index_count = 0usize;

    self.collect_inode_tree(
        &old_inode[..inode_size],
        &mut old_extents,
        &mut old_extent_count,
        &mut old_indexes,
        &mut old_index_count,
    )?;

    let mut extents = [ExtentRecord {
        logical: 0,
        physical: 0,
        length: 0,
    }; MAX_EXTENTS];
    let mut extent_count = 0usize;

    let data_blocks = match self.write_data_and_extents(
        data,
        &mut extents,
        &mut extent_count,
    ) {
        Ok(blocks) => blocks,
        Err(error) => return Err(error),
    };

    let mut inode = [0u8; INODE_SIZE_MAX];
    inode[..inode_size].copy_from_slice(&old_inode[..inode_size]);

    let current_flags = Self::read_u32(&inode, INODE_FLAGS)?;
    Self::write_u32(&mut inode, INODE_FLAGS, current_flags | EXT4_EXTENTS_FL);

    let tree_blocks = match self.build_extent_tree(
        &mut inode,
        &extents[..extent_count],
        inode_number,
    ) {
        Ok(blocks) => blocks,
        Err(error) => {
            let _ = self.free_tree_blocks(&extents[..extent_count], &[]);
            return Err(error);
        }
    };

    self.set_inode_size(&mut inode, data.len() as u64)?;
    let blocks = (data_blocks + tree_blocks) as u64
        * self.inode_blocks_sectors();
    self.set_inode_blocks(&mut inode, blocks)?;
    self.touch_inode_times(&mut inode);

    let mut new_indexes = [0u64; MAX_INDEX_NODES];
    let mut new_index_count = 0usize;
    let mut scratch_extents = [ExtentRecord {
        logical: 0,
        physical: 0,
        length: 0,
    }; MAX_EXTENTS];
    let mut scratch_count = 0usize;
    let _ = self.collect_inode_tree(
        &inode[..inode_size],
        &mut scratch_extents,
        &mut scratch_count,
        &mut new_indexes,
        &mut new_index_count,
    );

    if let Err(error) = self.write_inode_raw(inode_number, &mut inode) {
        let _ = self.free_tree_blocks(
            &extents[..extent_count],
            &new_indexes[..new_index_count],
        );
        return Err(error);
    }

    self.free_tree_blocks(
        &old_extents[..old_extent_count],
        &old_indexes[..old_index_count],
    )?;

    Ok(())
}

/// Находит inode по абсолютному пути. Возвращает `None`, если путь
/// не существует. Корень (`/`) соответствует inode 2.
pub fn find_inode_path(
    &self,
    path: &[u8],
) -> Result<Option<u32>, Ext4Error> {
    let mut current = 2u32;
    let mut found = false;

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        found = true;

        current = match self.find_directory_entry(current, component)? {
            Some(inode) => inode,
            None => return Ok(None),
        };
    }

    Ok(if found { Some(current) } else { Some(2) })
}

fn clear_directory_entry(
    &self,
    directory_inode: u32,
    name: &[u8],
) -> Result<(), Ext4Error> {
    self.ensure_writable()?;

    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut directory = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(directory_inode, &mut directory)?;

    let mode = Self::read_u16(&directory, INODE_MODE)?;
    if mode & 0xF000 != 0x4000 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let generation = Self::read_u32(&directory, INODE_GENERATION)?;
    let block_size = self.block_size() as usize;
    let size = Self::read_u32(&directory, INODE_SIZE_LO)? as u64;
    let blocks = ((size + block_size as u64 - 1) / block_size as u64)
        as usize;

    for logical in 0..blocks {
        let physical = match self.inode_extent_map(
            &directory[..inode_size],
            logical as u32,
        )? {
            ExtentMap::Data(block) => block,
            ExtentMap::Zero => continue,
        };

        let mut block = [0u8; 4096];
        self.read_block(physical, &mut block)?;
        let limit = self.dir_data_limit();
        let mut offset = 0usize;

        while offset + 8 <= limit {
            let inode = u32::from_le_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);

            let rec_len = u16::from_le_bytes([
                block[offset + 4],
                block[offset + 5],
            ]) as usize;

            let name_len = block[offset + 6] as usize;

            if rec_len < 8 || offset + rec_len > limit {
                return Err(Ext4Error::InvalidGeometry);
            }

            if name_len > rec_len - 8 {
                return Err(Ext4Error::InvalidGeometry);
            }

            if inode != 0
                && name_len == name.len()
                && &block[offset + 8..offset + 8 + name_len] == name
            {
                // Достаточно обнулить inode и длину имени: rec_len
                // остаётся прежним, а запись переиспользуется при вставке.
                Self::write_u32(&mut block, offset, 0);
                block[offset + 6] = 0;
                block[offset + 7] = 0;

                self.set_dir_tail_checksum(
                    directory_inode,
                    generation,
                    &mut block,
                );
                self.write_block(physical, &block)?;

                self.touch_inode_times(&mut directory);
                self.write_inode_raw(directory_inode, &mut directory)?;
                return Ok(());
            }

            offset += rec_len;
        }
    }

    Err(Ext4Error::OutOfBounds)
}

/// Удаляет обычный файл по пути и освобождает его блоки и inode.
pub fn remove_file_path(&self, path: &[u8]) -> Result<(), Ext4Error> {
    self.ensure_writable()?;

    let mut parent = 2u32;
    let mut name: &[u8] = b"";

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        if !name.is_empty() {
            parent = self
                .find_directory_entry(parent, name)?
                .ok_or(Ext4Error::OutOfBounds)?;
            let inode = self.read_inode(parent)?;
            if !inode.is_directory {
                return Err(Ext4Error::InvalidGeometry);
            }
        }

        name = component;
    }

    if name.is_empty() || name == b"." || name == b".." {
        return Err(Ext4Error::InvalidGeometry);
    }

    let inode_number = self
        .find_directory_entry(parent, name)?
        .ok_or(Ext4Error::OutOfBounds)?;
    let inode = self.read_inode(inode_number)?;

    if inode.is_directory || inode.mode & 0xF000 != 0x8000 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut raw = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(inode_number, &mut raw)?;

    let mut extents = [ExtentRecord {
        logical: 0,
        physical: 0,
        length: 0,
    }; MAX_EXTENTS];
    let mut extent_count = 0usize;
    let mut indexes = [0u64; MAX_INDEX_NODES];
    let mut index_count = 0usize;

    self.collect_inode_tree(
        &raw[..inode_size],
        &mut extents,
        &mut extent_count,
        &mut indexes,
        &mut index_count,
    )?;

    // Сначала убираем запись из каталога: если дальнейшая операция
    // прервётся, файл хотя бы перестанет быть видимым, а e2fsck
    // сможет освободить осиротевшие блоки.
    self.clear_directory_entry(parent, name)?;
    self.free_tree_blocks(
        &extents[..extent_count],
        &indexes[..index_count],
    )?;

    // Обнуляем inode и пересчитываем checksum, затем освобождаем
    // его в bitmap.
    for byte in &mut raw[..inode_size] {
        *byte = 0;
    }
    self.write_inode_raw(inode_number, &mut raw)?;
    self.free_inode(inode_number, false)?;

    Ok(())
}

/// Удаляет пустой каталог по пути и освобождает его блоки и inode.
pub fn remove_directory_path(&self, path: &[u8]) -> Result<(), Ext4Error> {
    self.ensure_writable()?;

    let mut parent = 2u32;
    let mut name: &[u8] = b"";

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        if !name.is_empty() {
            parent = self
                .find_directory_entry(parent, name)?
                .ok_or(Ext4Error::OutOfBounds)?;
            let inode = self.read_inode(parent)?;
            if !inode.is_directory {
                return Err(Ext4Error::InvalidGeometry);
            }
        }

        name = component;
    }

    if name.is_empty() || name == b"." || name == b".." {
        return Err(Ext4Error::InvalidGeometry);
    }

    let inode_number = self
        .find_directory_entry(parent, name)?
        .ok_or(Ext4Error::OutOfBounds)?;

    if inode_number == 2 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let inode = self.read_inode(inode_number)?;
    if !inode.is_directory {
        return Err(Ext4Error::InvalidGeometry);
    }

    let mut not_empty = false;
    self.for_each_directory_entry(
        inode_number,
        |_, entry_name, _| {
            if entry_name == b"." || entry_name == b".." {
                true
            } else {
                not_empty = true;
                false
            }
        },
    )?;

    if not_empty {
        return Err(Ext4Error::NotEmpty);
    }

    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut raw = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(inode_number, &mut raw)?;

    let mut extents = [ExtentRecord {
        logical: 0,
        physical: 0,
        length: 0,
    }; MAX_EXTENTS];
    let mut extent_count = 0usize;
    let mut indexes = [0u64; MAX_INDEX_NODES];
    let mut index_count = 0usize;

    self.collect_inode_tree(
        &raw[..inode_size],
        &mut extents,
        &mut extent_count,
        &mut indexes,
        &mut index_count,
    )?;

    self.clear_directory_entry(parent, name)?;
    self.free_tree_blocks(
        &extents[..extent_count],
        &indexes[..index_count],
    )?;

    for byte in &mut raw[..inode_size] {
        *byte = 0;
    }
    self.write_inode_raw(inode_number, &mut raw)?;
    self.free_inode(inode_number, true)?;

    // Удалённый каталог больше не ссылается на родителя через "..".
    let mut parent_raw = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(parent, &mut parent_raw)?;
    let links = Self::read_u16(&parent_raw, INODE_LINKS)?;
    Self::write_u16(
        &mut parent_raw,
        INODE_LINKS,
        links.saturating_sub(1),
    );
    self.touch_inode_times(&mut parent_raw);
    self.write_inode_raw(parent, &mut parent_raw)?;

    Ok(())
}

pub fn write_file_path(
    &self,
    path: &[u8],
    data: &[u8],
) -> Result<u32, Ext4Error> {
    self.ensure_writable()?;

    let mut parent = 2u32;
    let mut name: &[u8] = b"";

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        if !name.is_empty() {
            parent = self
                .find_directory_entry(parent, name)?
                .ok_or(Ext4Error::OutOfBounds)?;
            let inode = self.read_inode(parent)?;
            if !inode.is_directory {
                return Err(Ext4Error::InvalidGeometry);
            }
        }

        name = component;
    }

    if name.is_empty() || name.len() > 255 {
        return Err(Ext4Error::InvalidGeometry);
    }

    match self.find_directory_entry(parent, name)? {
        Some(inode_number) => {
            let inode = self.read_inode(inode_number)?;
            if inode.is_directory || inode.mode & 0xF000 != 0x8000 {
                return Err(Ext4Error::InvalidGeometry);
            }
            self.write_file_contents(inode_number, data)?;
            Ok(inode_number)
        }
        None => self.create_file(parent, name, data),
    }
}

/// Перезаписывает данные существующего ext4-файла.
///
/// При offset == 0 файл целиком перезаписывается с возможным
/// изменением размера. Иначе изменяются только уже существующие
/// байты в пределах текущего размера файла.
pub fn write_file_data(
    &self,
    inode_number: u32,
    offset: u64,
    input: &[u8],
) -> Result<usize, Ext4Error> {
    self.ensure_writable()?;

    if offset == 0 {
        self.write_file_contents(inode_number, input)?;
        return Ok(input.len());
    }

    let inode_size = self.superblock.inode_size as usize;
    if inode_size > INODE_SIZE_MAX {
        return Err(Ext4Error::UnsupportedFeature);
    }

    let mut inode = [0u8; INODE_SIZE_MAX];
    self.read_inode_raw(inode_number, &mut inode)?;

    let mode = Self::read_u16(&inode, INODE_MODE)?;
    if mode & 0xF000 != 0x8000 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let size_lo = Self::read_u32(&inode, INODE_SIZE_LO)? as u64;
    let size_hi = Self::read_u32(&inode, INODE_SIZE_HIGH)? as u64;
    let size = (size_hi << 32) | size_lo;
    let end = offset
        .checked_add(input.len() as u64)
        .ok_or(Ext4Error::OutOfBounds)?;

    if end > size {
        return Err(Ext4Error::OutOfBounds);
    }

    let block_size = self.block_size() as u64;
    let mut written = 0usize;

    while written < input.len() {
        let position = offset + written as u64;
        let logical = (position / block_size) as u32;
        let block_offset = (position % block_size) as usize;
        let chunk = core::cmp::min(
            block_size as usize - block_offset,
            input.len() - written,
        );

        let physical = match self.inode_extent_map(
            &inode[..inode_size],
            logical,
        )? {
            ExtentMap::Data(block) => block,
            ExtentMap::Zero => return Err(Ext4Error::UnsupportedFeature),
        };

        let mut block = [0u8; 4096];
        self.read_block(physical, &mut block)?;
        block[block_offset..block_offset + chunk]
            .copy_from_slice(&input[written..written + chunk]);
        self.write_block(physical, &block)?;

        written += chunk;
    }

    self.touch_inode_times(&mut inode);
    self.write_inode_raw(inode_number, &mut inode)?;

    Ok(written)
}

}
