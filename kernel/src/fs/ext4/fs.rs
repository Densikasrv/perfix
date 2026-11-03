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
    InvalidGeometry,
    BufferTooSmall,
    OutOfBounds,
}

/// Абстракция блочного устройства.
/// Реализацию для ATA подключим отдельно.
pub trait BlockDevice {
    fn read_sector(
        &self,
        lba: u32,
        buffer: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), Ext4Error>;
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
}
#[derive(Debug, Clone, Copy)]
pub struct InodeInfo {
    pub number: u32,
    pub mode: u16,
    pub size: u64,
    pub flags: u32,
    pub is_directory: bool,
    pub first_extent_block: u64,
}
pub struct Ext4<'a, D: BlockDevice> {
    device: &'a D,
    partition_start: u32,
    pub superblock: Superblock,
}

impl<'a, D: BlockDevice> Ext4<'a, D> {
    /// Подключает ext4-раздел.
    ///
    /// partition_start — начальный LBA раздела на диске.
    pub fn mount(
        device: &'a D,
        partition_start: u32,
    ) -> Result<Self, Ext4Error> {
        let mut raw = [0u8; 1024];

        Self::read_bytes_from_device(
            device,
            partition_start,
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

        let blocks_per_group = Self::read_u32(&raw, 0x20)?;
        let inodes_per_group = Self::read_u32(&raw, 0x28)?;

        let revision = Self::read_u32(&raw, 0x4C)?;

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

        // 64-bit block counters.
        let has_64bit = (feature_incompat & 0x80) != 0;

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

        if inodes_count == 0
            || blocks_count == 0
            || blocks_per_group == 0
            || inodes_per_group == 0
            || inode_size < 128
            || !inode_size.is_power_of_two()
            || inode_size as u32 > block_size
        {
            return Err(Ext4Error::InvalidGeometry);
        }

        // ext4 incompat features not supported by this initial driver.
        //
        // Supported here:
        // FILETYPE (0x2), EXTENTS (0x40), 64BIT (0x80).
        let supported_incompat = 0x2 | 0x40 | 0x80 | 0x200;

        if feature_incompat & !supported_incompat != 0 {
            return Err(Ext4Error::UnsupportedFeature);
        }

        // Read-only compatible features:
        // We reject unknown features conservatively.
        //
        // Supported:
        // SPARSE_SUPER (0x1), LARGE_FILE (0x2),
        // HUGE_FILE (0x8), DIR_NLINK (0x20),
        // EXTRA_ISIZE (0x40), METADATA_CSUM (0x400).
        let supported_ro_compat =
            0x1 | 0x2 | 0x8 | 0x20 | 0x40 | 0x400;

        if feature_ro_compat & !supported_ro_compat != 0 {
            return Err(Ext4Error::UnsupportedFeature);
        }

        Ok(Self {
            device,
            partition_start,
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
            },
        })
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

        Self::read_bytes_from_device(
            self.device,
            self.partition_start,
            byte_offset,
            &mut buffer[..block_size],
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

    fn read_bytes_from_device(
        device: &D,
        partition_start: u32,
        byte_offset: u64,
        output: &mut [u8],
    ) -> Result<(), Ext4Error> {
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

pub fn read_inode(&self, inode_number: u32) -> Result<InodeInfo, Ext4Error> {
    if inode_number == 0 || inode_number > self.superblock.inodes_count {
        return Err(Ext4Error::OutOfBounds);
    }

    let sb = &self.superblock;

    let group = (inode_number - 1) / sb.inodes_per_group;
    let index = (inode_number - 1) % sb.inodes_per_group;

    // Размер дескриптора группы.
    let mut raw_sb = [0u8; 1024];

    Self::read_bytes_from_device(
        self.device,
        self.partition_start,
        SUPERBLOCK_OFFSET,
        &mut raw_sb,
    )?;

    let descriptor_size = if (sb.feature_incompat & 0x80) != 0 {
        Self::read_u16(&raw_sb, 0xFE)? as usize
    } else {
        32
    };

    if descriptor_size < 32 || descriptor_size > sb.block_size as usize {
        return Err(Ext4Error::InvalidGeometry);
    }

    // Таблица дескрипторов групп.
    let descriptor_table_block = if sb.block_size == 1024 { 2 } else { 1 };

    let descriptor_offset = (group as usize)
        .checked_mul(descriptor_size)
        .ok_or(Ext4Error::OutOfBounds)?;

    let descriptor_block = descriptor_table_block as u64
        + (descriptor_offset / sb.block_size as usize) as u64;

    let offset_in_block = descriptor_offset % sb.block_size as usize;

    let mut block = [0u8; 4096];
    self.read_block(descriptor_block, &mut block)?;

    let desc = &block[offset_in_block..offset_in_block + descriptor_size];

    // Начальный блок inode table.
    let inode_table_lo = Self::read_u32(desc, 8)? as u64;

    let inode_table_hi = if descriptor_size >= 64 {
        Self::read_u32(desc, 40)? as u64
    } else {
        0
    };

    let inode_table = (inode_table_hi << 32) | inode_table_lo;

    let inode_offset = (index as u64)
        .checked_mul(sb.inode_size as u64)
        .ok_or(Ext4Error::OutOfBounds)?;

    let inode_block = inode_table
        .checked_add(inode_offset / sb.block_size as u64)
        .ok_or(Ext4Error::OutOfBounds)?;

    let offset_in_inode_block =
        (inode_offset % sb.block_size as u64) as usize;

    self.read_block(inode_block, &mut block)?;

    let inode = &block[
        offset_in_inode_block..offset_in_inode_block + sb.inode_size as usize
    ];

    let mode = Self::read_u16(inode, 0)?;
    let size_lo = Self::read_u32(inode, 4)? as u64;
    let flags = Self::read_u32(inode, 32)?;
    let mut first_extent_block = 0u64;

if flags & 0x0008_0000 != 0 {
    let extent_magic = Self::read_u16(inode, 40)?;
    let extent_entries = Self::read_u16(inode, 42)?;
    let extent_depth = Self::read_u16(inode, 46)?;

    if extent_magic != 0xF30A {
        return Err(Ext4Error::InvalidGeometry);
    }

    // Пока поддерживаем только дерево глубины 0.
    if extent_depth == 0 && extent_entries > 0 {
        let extent_len = Self::read_u16(inode, 56)?;

        if extent_len == 0 {
            return Err(Ext4Error::InvalidGeometry);
        }

        let start_hi = Self::read_u16(inode, 58)? as u64;
        let start_lo = Self::read_u32(inode, 60)? as u64;

        first_extent_block = (start_hi << 32) | start_lo;
    }
}
    // Для каталогов размер хранится в i_size_lo.
    // Для обычных файлов верхняя часть размера находится по offset 108.
    let size = if mode & 0xF000 == 0x8000 {
        let size_hi = Self::read_u32(inode, 108)? as u64;
        (size_hi << 32) | size_lo
    } else {
        size_lo
    };

    Ok(InodeInfo {
        number: inode_number,
        mode,
        size,
        flags,
        is_directory: mode & 0xF000 == 0x4000,
        first_extent_block,
    })
}
}
