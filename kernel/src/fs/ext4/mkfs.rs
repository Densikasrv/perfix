//! Минимальное форматирование раздела в ext4.
//!
//! Создаётся файловая система без journal, metadata_csum, 64bit и flex_bg:
//! только те функции, которые драйвер Perfix умеет читать и обновлять.
//! Раскладка групп повторяет mke2fs с SPARSE_SUPER: копии суперблока и
//! таблицы дескрипторов пишутся в группы 1, 3, 5, 7, 9, ...
//!
//! Форматирование идёт напрямую через блочное устройство и не требует
//! чтения старого содержимого раздела.

use super::fs::{BlockDevice, Ext4Error, SECTOR_SIZE};

const EXT4_MAGIC: u16 = 0xEF53;
const EXT4_VALID_FS: u16 = 0x0001;
const EXT4_ERRORS_CONTINUE: u16 = 0x0001;

const EXT4_FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
const EXT4_FEATURE_INCOMPAT_EXTENTS: u32 = 0x0040;
const EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
const EXT4_FEATURE_RO_COMPAT_LARGE_FILE: u32 = 0x0002;
const EXT4_FEATURE_RO_COMPAT_DIR_NLINK: u32 = 0x0020;
const EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;

const EXT4_EXTENTS_FL: u32 = 0x0008_0000;
const EXT4_BG_INODE_ZEROED: u16 = 0x0004;

const INODE_SIZE: u32 = 256;
const INODE_SIZE_USIZE: usize = 256;
const FIRST_INO: u32 = 11;
const ROOT_INO: u32 = 2;
const LOST_FOUND_INO: u32 = 11;
const DESC_SIZE: usize = 32;
const MAX_BLOCK_SIZE: usize = 4096;
/// Сколько байт файловой системы приходится на один inode.
const BYTES_PER_INODE: u64 = 16 * 1024;
/// Фиксированная метка времени для новых inode (2024-01-01).
const MKFS_TIME: u32 = 1_704_067_200;

// Смещения полей суперблока.
const SB_INODES_COUNT: usize = 0x00;
const SB_BLOCKS_COUNT_LO: usize = 0x04;
const SB_R_BLOCKS_COUNT_LO: usize = 0x08;
const SB_FREE_BLOCKS_COUNT_LO: usize = 0x0C;
const SB_FREE_INODES_COUNT: usize = 0x10;
const SB_FIRST_DATA_BLOCK: usize = 0x14;
const SB_LOG_BLOCK_SIZE: usize = 0x18;
const SB_LOG_CLUSTER_SIZE: usize = 0x1C;
const SB_BLOCKS_PER_GROUP: usize = 0x20;
const SB_CLUSTERS_PER_GROUP: usize = 0x24;
const SB_INODES_PER_GROUP: usize = 0x28;
const SB_MTIME: usize = 0x2C;
const SB_WTIME: usize = 0x30;
const SB_MNT_COUNT: usize = 0x34;
const SB_MAX_MNT_COUNT: usize = 0x36;
const SB_MAGIC: usize = 0x38;
const SB_STATE: usize = 0x3A;
const SB_ERRORS: usize = 0x3C;
const SB_LASTCHECK: usize = 0x40;
const SB_CREATOR_OS: usize = 0x48;
const SB_REV_LEVEL: usize = 0x4C;
const SB_FIRST_INO: usize = 0x54;
const SB_INODE_SIZE: usize = 0x58;
const SB_BLOCK_GROUP_NR: usize = 0x5A;
const SB_FEATURE_COMPAT: usize = 0x5C;
const SB_FEATURE_INCOMPAT: usize = 0x60;
const SB_FEATURE_RO_COMPAT: usize = 0x64;
const SB_UUID: usize = 0x68;
const SB_VOLUME_NAME: usize = 0x78;
const SB_HASH_SEED: usize = 0xEC;
const SB_DEF_HASH_VERSION: usize = 0xFC;
const SB_DESC_SIZE: usize = 0xFE;
const SB_MKFS_TIME: usize = 0x108;
const SB_MIN_EXTRA_ISIZE: usize = 0x15C;
const SB_WANT_EXTRA_ISIZE: usize = 0x15E;

// Смещения полей inode.
const INODE_MODE: usize = 0x00;
const INODE_SIZE_LO: usize = 0x04;
const INODE_ATIME: usize = 0x08;
const INODE_CTIME: usize = 0x0C;
const INODE_MTIME: usize = 0x10;
const INODE_LINKS: usize = 0x1A;
const INODE_BLOCKS_LO: usize = 0x1C;
const INODE_FLAGS: usize = 0x20;
const INODE_BLOCK: usize = 0x28;
const INODE_EXTRA_ISIZE: usize = 0x80;
const INODE_CRTIME: usize = 0x90;

// Смещения полей дескриптора группы.
const GD_BLOCK_BITMAP: usize = 0x00;
const GD_INODE_BITMAP: usize = 0x04;
const GD_INODE_TABLE: usize = 0x08;
const GD_FREE_BLOCKS: usize = 0x0C;
const GD_FREE_INODES: usize = 0x0E;
const GD_USED_DIRS: usize = 0x10;
const GD_FLAGS: usize = 0x12;

/// Результат форматирования.
#[derive(Clone, Copy)]
pub struct FormatInfo {
    pub block_size: u32,
    pub blocks: u64,
    pub inodes: u32,
}

#[derive(Clone, Copy)]
struct Layout {
    block_size: u32,
    first_data_block: u32,
    blocks_count: u64,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_table_blocks: u32,
    groups_count: u32,
    gdt_blocks: u32,
    free_blocks: u64,
}
impl Layout {
    fn group_first(&self, group: u32) -> u64 {
        self.first_data_block as u64
            + group as u64 * self.blocks_per_group as u64
    }

    fn group_blocks(&self, group: u32) -> u64 {
        let first = self.group_first(group);
        if first >= self.blocks_count {
            0
        } else {
            core::cmp::min(
                self.blocks_per_group as u64,
                self.blocks_count - first,
            )
        }
    }

    /// Есть ли в группе резервная копия суперблока и GDT.
    fn has_super(&self, group: u32) -> bool {
        if group == 0 || group == 1 {
            return true;
        }

        if group % 2 == 0 {
            return false;
        }

        // Копии хранятся в группах — степенях 3, 5 и 7.
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

    fn reserved_blocks(&self, group: u32) -> u64 {
        if self.has_super(group) {
            1 + self.gdt_blocks as u64
        } else {
            0
        }
    }

    fn block_bitmap(&self, group: u32) -> u64 {
        self.group_first(group) + self.reserved_blocks(group)
    }

    fn inode_bitmap(&self, group: u32) -> u64 {
        self.block_bitmap(group) + 1
    }

    fn inode_table(&self, group: u32) -> u64 {
        self.inode_bitmap(group) + 1
    }

    /// Сколько inode реально существует в группе.
    fn group_inodes(&self, group: u32) -> u32 {
        let first = group as u64 * self.inodes_per_group as u64;
        if first >= self.inodes_count() as u64 {
            0
        } else {
            core::cmp::min(
                self.inodes_per_group as u64,
                self.inodes_count() as u64 - first,
            ) as u32
        }
    }

    fn inodes_count(&self) -> u32 {
        self.inodes_per_group * self.groups_count
    }

    /// Занято блоков в группе: метаданные, хвост и данные корня.
    fn used_blocks(&self, group: u32) -> u64 {
        let actual = self.group_blocks(group);
        let metadata =
            self.reserved_blocks(group) + 2 + self.inode_table_blocks as u64;

        if metadata > actual {
            return actual;
        }

        let mut used = metadata;

        // Корневой каталог и lost+found лежат сразу за inode table.
        if group == 0 {
            used += 2;
        }

        core::cmp::min(used, actual)
    }
}

fn write_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Пишет последовательность байт в раздел по смещению от его начала.
fn write_bytes<D: BlockDevice>(
    device: &D,
    partition_start: u32,
    partition_sectors: u32,
    byte_offset: u64,
    data: &[u8],
) -> Result<(), Ext4Error> {
    let partition_bytes = partition_sectors as u64 * SECTOR_SIZE as u64;
    let end = byte_offset
        .checked_add(data.len() as u64)
        .ok_or(Ext4Error::OutOfBounds)?;

    if end > partition_bytes {
        return Err(Ext4Error::OutOfBounds);
    }

    let mut position = 0usize;
    let mut offset = byte_offset;
    let mut sector = [0u8; SECTOR_SIZE];

    while position < data.len() {
        let sector_index = offset / SECTOR_SIZE as u64;
        let sector_offset = (offset % SECTOR_SIZE as u64) as usize;
        let lba = partition_start as u64 + sector_index;

        if lba > u32::MAX as u64 {
            return Err(Ext4Error::OutOfBounds);
        }

        let available = SECTOR_SIZE - sector_offset;
        let remaining = data.len() - position;
        let count = core::cmp::min(available, remaining);

        if sector_offset == 0 && count == SECTOR_SIZE {
            sector.copy_from_slice(&data[position..position + SECTOR_SIZE]);
        } else {
            for byte in sector.iter_mut() {
                *byte = 0;
            }
            sector[sector_offset..sector_offset + count]
                .copy_from_slice(&data[position..position + count]);
        }

        device.write_sector(lba as u32, &sector)?;

        position += count;
        offset += count as u64;
    }

    Ok(())
}

fn write_block<D: BlockDevice>(
    device: &D,
    partition_start: u32,
    partition_sectors: u32,
    block_size: u32,
    block: u64,
    data: &[u8],
) -> Result<(), Ext4Error> {
    if data.len() < block_size as usize {
        return Err(Ext4Error::BufferTooSmall);
    }

    let offset = block
        .checked_mul(block_size as u64)
        .ok_or(Ext4Error::OutOfBounds)?;

    write_bytes(
        device,
        partition_start,
        partition_sectors,
        offset,
        &data[..block_size as usize],
    )
}

fn zero_block<D: BlockDevice>(
    device: &D,
    partition_start: u32,
    partition_sectors: u32,
    block_size: u32,
    block: u64,
) -> Result<(), Ext4Error> {
    let zero = [0u8; MAX_BLOCK_SIZE];
    write_block(
        device,
        partition_start,
        partition_sectors,
        block_size,
        block,
        &zero,
    )
}

fn compute_layout(partition_bytes: u64) -> Result<Layout, Ext4Error> {
    if partition_bytes < 512 * 1024 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let block_size = if partition_bytes >= 8 * 1024 * 1024 {
        4096u32
    } else {
        1024u32
    };

    let first_data_block = if block_size == 1024 { 1 } else { 0 };
    let blocks_count = partition_bytes / block_size as u64;

    if blocks_count <= first_data_block as u64 + 4
        || blocks_count > u32::MAX as u64
    {
        return Err(Ext4Error::InvalidGeometry);
    }

    let blocks_per_group = block_size * 8;

    let groups_count = (blocks_count - first_data_block as u64
        + blocks_per_group as u64
        - 1)
        / blocks_per_group as u64;

    if groups_count == 0 || groups_count > u32::MAX as u64 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let gdt_bytes = groups_count * DESC_SIZE as u64;
    let gdt_blocks = (gdt_bytes + block_size as u64 - 1) / block_size as u64;

    let mut inodes_per_group = blocks_per_group as u64 * block_size as u64
        / BYTES_PER_INODE;

    inodes_per_group = (inodes_per_group + 7) & !7;
    if inodes_per_group < 16 {
        inodes_per_group = 16;
    }
    if inodes_per_group > block_size as u64 * 8 {
        inodes_per_group = block_size as u64 * 8;
    }

    let inode_table_blocks = (inodes_per_group * INODE_SIZE as u64
        + block_size as u64
        - 1)
        / block_size as u64;

    if groups_count * inodes_per_group > u32::MAX as u64 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let mut layout = Layout {
        block_size,
        first_data_block,
        blocks_count,
        blocks_per_group,
        inodes_per_group: inodes_per_group as u32,
        inode_table_blocks: inode_table_blocks as u32,
        groups_count: groups_count as u32,
        gdt_blocks: gdt_blocks as u32,
        free_blocks: 0,
    };

    // В первой группе должны поместиться метаданные, корень и lost+found,
    // а в каждой группе — свои bitmap и inode table.
    for group in 0..layout.groups_count {
        let metadata = layout.reserved_blocks(group)
            + 2
            + layout.inode_table_blocks as u64;
        let needed = if group == 0 { metadata + 2 } else { metadata };

        if needed > layout.group_blocks(group) {
            return Err(Ext4Error::InvalidGeometry);
        }
    }

    let mut free = 0u64;
    for group in 0..layout.groups_count {
        free += layout.group_blocks(group) - layout.used_blocks(group);
    }
    layout.free_blocks = free;

    Ok(layout)
}

fn build_superblock(layout: &Layout, group: u32) -> [u8; 1024] {
    let mut sb = [0u8; 1024];
    let inodes = layout.inodes_count();

    write_u32(&mut sb, SB_INODES_COUNT, inodes);
    write_u32(&mut sb, SB_BLOCKS_COUNT_LO, layout.blocks_count as u32);
    write_u32(&mut sb, SB_R_BLOCKS_COUNT_LO, 0);
    write_u32(
        &mut sb,
        SB_FREE_BLOCKS_COUNT_LO,
        layout.free_blocks as u32,
    );
    write_u32(&mut sb, SB_FREE_INODES_COUNT, inodes - 11);
    write_u32(&mut sb, SB_FIRST_DATA_BLOCK, layout.first_data_block);

    let log_block_size = match layout.block_size {
        1024 => 0,
        2048 => 1,
        _ => 2,
    };
    write_u32(&mut sb, SB_LOG_BLOCK_SIZE, log_block_size);
    write_u32(&mut sb, SB_LOG_CLUSTER_SIZE, log_block_size);
    write_u32(&mut sb, SB_BLOCKS_PER_GROUP, layout.blocks_per_group);
    write_u32(&mut sb, SB_CLUSTERS_PER_GROUP, layout.blocks_per_group);
    write_u32(&mut sb, SB_INODES_PER_GROUP, layout.inodes_per_group);

    write_u32(&mut sb, SB_MTIME, 0);
    write_u32(&mut sb, SB_WTIME, MKFS_TIME);
    write_u16(&mut sb, SB_MNT_COUNT, 0);
    write_u16(&mut sb, SB_MAX_MNT_COUNT, 0xFFFF);
    write_u16(&mut sb, SB_MAGIC, EXT4_MAGIC);
    write_u16(&mut sb, SB_STATE, EXT4_VALID_FS);
    write_u16(&mut sb, SB_ERRORS, EXT4_ERRORS_CONTINUE);
    write_u32(&mut sb, SB_LASTCHECK, MKFS_TIME);
    write_u32(&mut sb, SB_CREATOR_OS, 0);
    write_u32(&mut sb, SB_REV_LEVEL, 1);
    write_u32(&mut sb, SB_FIRST_INO, FIRST_INO);
    write_u16(&mut sb, SB_INODE_SIZE, INODE_SIZE as u16);
    write_u16(&mut sb, SB_BLOCK_GROUP_NR, group as u16);

    write_u32(&mut sb, SB_FEATURE_COMPAT, 0);
    write_u32(
        &mut sb,
        SB_FEATURE_INCOMPAT,
        EXT4_FEATURE_INCOMPAT_FILETYPE | EXT4_FEATURE_INCOMPAT_EXTENTS,
    );
    write_u32(
        &mut sb,
        SB_FEATURE_RO_COMPAT,
        EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER
            | EXT4_FEATURE_RO_COMPAT_LARGE_FILE
            | EXT4_FEATURE_RO_COMPAT_DIR_NLINK
            | EXT4_FEATURE_RO_COMPAT_EXTRA_ISIZE,
    );

    // UUID зависит от положения раздела, но остаётся детерминированным.
    let mut seed = (layout.blocks_count as u32)
        ^ (layout.inodes_per_group.rotate_left(13))
        ^ 0x9E37_79B9;

    for index in 0..16 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        sb[SB_UUID + index] = (seed >> 24) as u8;
    }
    // Версия UUID 4 и вариант RFC 4122.
    sb[SB_UUID + 6] = (sb[SB_UUID + 6] & 0x0F) | 0x40;
    sb[SB_UUID + 8] = (sb[SB_UUID + 8] & 0x3F) | 0x80;

    let name = b"Perfix";
    sb[SB_VOLUME_NAME..SB_VOLUME_NAME + name.len()].copy_from_slice(name);

    // Seed для hash-функции каталогов.
    for index in 0..4 {
        let value = u32::from_le_bytes([
            sb[SB_UUID + index * 4],
            sb[SB_UUID + index * 4 + 1],
            sb[SB_UUID + index * 4 + 2],
            sb[SB_UUID + index * 4 + 3],
        ]);
        write_u32(&mut sb, SB_HASH_SEED + index * 4, value);
    }

    sb[SB_DEF_HASH_VERSION] = 1;
    write_u16(&mut sb, SB_DESC_SIZE, 0);
    write_u32(&mut sb, SB_MKFS_TIME, MKFS_TIME);
    write_u16(&mut sb, SB_MIN_EXTRA_ISIZE, 32);
    write_u16(&mut sb, SB_WANT_EXTRA_ISIZE, 32);

    sb
}

fn build_group_descriptor(layout: &Layout, group: u32) -> [u8; DESC_SIZE] {
    let mut desc = [0u8; DESC_SIZE];
    let used_blocks = layout.used_blocks(group);
    let group_blocks = layout.group_blocks(group);
    let group_inodes = layout.group_inodes(group);
    let used_inodes = if group == 0 { 11 } else { 0 };

    write_u32(
        &mut desc,
        GD_BLOCK_BITMAP,
        layout.block_bitmap(group) as u32,
    );
    write_u32(
        &mut desc,
        GD_INODE_BITMAP,
        layout.inode_bitmap(group) as u32,
    );
    write_u32(
        &mut desc,
        GD_INODE_TABLE,
        layout.inode_table(group) as u32,
    );
    write_u16(
        &mut desc,
        GD_FREE_BLOCKS,
        (group_blocks - used_blocks) as u16,
    );
    write_u16(
        &mut desc,
        GD_FREE_INODES,
        group_inodes.saturating_sub(used_inodes) as u16,
    );
    write_u16(
        &mut desc,
        GD_USED_DIRS,
        if group == 0 { 2 } else { 0 },
    );
    write_u16(&mut desc, GD_FLAGS, EXT4_BG_INODE_ZEROED);

    desc
}

/// Заполняет GDT-блок дескрипторами групп, попадающих в этот блок.
fn build_gdt_block(layout: &Layout, gdt_block: u32) -> [u8; MAX_BLOCK_SIZE] {
    let mut block = [0u8; MAX_BLOCK_SIZE];
    let block_size = layout.block_size as usize;
    let descriptors_per_block = block_size / DESC_SIZE;

    for slot in 0..descriptors_per_block {
        let group = gdt_block as usize * descriptors_per_block + slot;
        if group >= layout.groups_count as usize {
            break;
        }

        let desc = build_group_descriptor(layout, group as u32);
        let offset = slot * DESC_SIZE;
        block[offset..offset + DESC_SIZE].copy_from_slice(&desc);
    }

    block
}

fn build_inode(
    mode: u16,
    links: u16,
    size: u32,
    inode_blocks: u64,
    data_block: u64,
    extra_isize: u16,
) -> [u8; INODE_SIZE_USIZE] {
    let mut inode = [0u8; INODE_SIZE_USIZE];

    write_u16(&mut inode, INODE_MODE, mode);
    write_u32(&mut inode, INODE_SIZE_LO, size);
    write_u32(&mut inode, INODE_ATIME, MKFS_TIME);
    write_u32(&mut inode, INODE_CTIME, MKFS_TIME);
    write_u32(&mut inode, INODE_MTIME, MKFS_TIME);
    write_u16(&mut inode, INODE_LINKS, links);
    write_u32(&mut inode, INODE_BLOCKS_LO, inode_blocks as u32);
    write_u32(&mut inode, INODE_FLAGS, EXT4_EXTENTS_FL);

    // Один extent: логический блок 0 -> data_block.
    write_u16(&mut inode, INODE_BLOCK, 0xF30A);
    write_u16(&mut inode, INODE_BLOCK + 2, 1);
    write_u16(&mut inode, INODE_BLOCK + 4, 4);
    write_u16(&mut inode, INODE_BLOCK + 6, 0);
    write_u32(&mut inode, INODE_BLOCK + 8, 0);
    write_u32(&mut inode, INODE_BLOCK + 12, 0);
    write_u16(&mut inode, INODE_BLOCK + 16, 1);
    write_u16(&mut inode, INODE_BLOCK + 18, 0);
    write_u32(&mut inode, INODE_BLOCK + 20, data_block as u32);

    if extra_isize > 0 {
        write_u16(&mut inode, INODE_EXTRA_ISIZE, extra_isize);
        write_u32(&mut inode, INODE_CRTIME, MKFS_TIME);
    }

    inode
}

fn put_dir_entry(
    block: &mut [u8],
    offset: usize,
    inode: u32,
    rec_len: usize,
    file_type: u8,
    name: &[u8],
) {
    write_u32(block, offset, inode);
    write_u16(block, offset + 4, rec_len as u16);
    block[offset + 6] = name.len() as u8;
    block[offset + 7] = file_type;
    block[offset + 8..offset + 8 + name.len()].copy_from_slice(name);
}

fn build_bitmap(layout: &Layout, group: u32, inode_bitmap: bool) -> [u8; MAX_BLOCK_SIZE] {
    let mut bitmap = [0u8; MAX_BLOCK_SIZE];
    let block_size = layout.block_size as usize;
    let per_group = if inode_bitmap {
        layout.inodes_per_group as usize
    } else {
        layout.blocks_per_group as usize
    };
    let actual = if inode_bitmap {
        layout.group_inodes(group) as usize
    } else {
        layout.group_blocks(group) as usize
    };

    // Хвост за концом группы/ФС и padding за пределами elements_per_group
    // помечаются занятыми, как это делает mke2fs.
    let tail_start = core::cmp::min(per_group, actual);

    for bit in tail_start..block_size * 8 {
        bitmap[bit / 8] |= 1u8 << (bit % 8);
    }

    if inode_bitmap {
        // inode 1..=10 зарезервированы, inode 11 — lost+found.
        if group == 0 {
            for bit in 0..11 {
                bitmap[bit / 8] |= 1u8 << (bit % 8);
            }
        }
    } else {
        let metadata = layout.reserved_blocks(group)
            + 2
            + layout.inode_table_blocks as u64;

        for bit in 0..metadata as usize {
            if bit < actual {
                bitmap[bit / 8] |= 1u8 << (bit % 8);
            }
        }

        if group == 0 {
            // Корневой каталог и lost+found.
            for bit in metadata as usize..metadata as usize + 2 {
                if bit < actual {
                    bitmap[bit / 8] |= 1u8 << (bit % 8);
                }
            }
        }
    }

    bitmap
}

/// Форматирует раздел в ext4.
pub fn format<D: BlockDevice>(
    device: &D,
    partition_start: u32,
    partition_sectors: u32,
) -> Result<FormatInfo, Ext4Error> {
    if partition_sectors == 0 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let partition_end = partition_start as u64 + partition_sectors as u64;
    if partition_end > u32::MAX as u64 {
        return Err(Ext4Error::InvalidGeometry);
    }

    let partition_bytes = partition_sectors as u64 * SECTOR_SIZE as u64;
    let layout = compute_layout(partition_bytes)?;
    let block_size = layout.block_size;

    // Блок 0 у 1k-ФС — загрузочный, у 4k-ФС в нём лежит суперблок.
    // В любом случае старое содержимое первого блока не нужно.
    zero_block(device, partition_start, partition_sectors, block_size, 0)?;

    // Основной суперблок всегда записывается по смещению 1024.
    let superblock = build_superblock(&layout, 0);
    write_bytes(
        device,
        partition_start,
        partition_sectors,
        1024,
        &superblock,
    )?;

    // Основная таблица дескрипторов: блок 1 или 2.
    let primary_gdt_block =
        if block_size == 1024 { 2u32 } else { 1u32 };

    for gdt_block in 0..layout.gdt_blocks {
        let block = build_gdt_block(&layout, gdt_block);
        write_block(
            device,
            partition_start,
            partition_sectors,
            block_size,
            primary_gdt_block as u64 + gdt_block as u64,
            &block,
        )?;
    }

    let extra_isize = 32u16;
    let inode_blocks = block_size as u64 / SECTOR_SIZE as u64;

    for group in 0..layout.groups_count {
        let group_first = layout.group_first(group);

        if group > 0 && layout.has_super(group) {
            // Резервная копия суперблока лежит в начале блока группы.
            let backup = build_superblock(&layout, group);
            write_bytes(
                device,
                partition_start,
                partition_sectors,
                group_first * block_size as u64,
                &backup,
            )?;

            for gdt_block in 0..layout.gdt_blocks {
                let block = build_gdt_block(&layout, gdt_block);
                write_block(
                    device,
                    partition_start,
                    partition_sectors,
                    block_size,
                    group_first + 1 + gdt_block as u64,
                    &block,
                )?;
            }
        }

        let block_bitmap = build_bitmap(&layout, group, false);
        write_block(
            device,
            partition_start,
            partition_sectors,
            block_size,
            layout.block_bitmap(group),
            &block_bitmap,
        )?;

        let inode_bitmap = build_bitmap(&layout, group, true);
        write_block(
            device,
            partition_start,
            partition_sectors,
            block_size,
            layout.inode_bitmap(group),
            &inode_bitmap,
        )?;

        // Обнуляем всю inode table, затем заполняем корень и lost+found.
        for block in 0..layout.inode_table_blocks as u64 {
            zero_block(
                device,
                partition_start,
                partition_sectors,
                block_size,
                layout.inode_table(group) + block,
            )?;
        }

        if group == 0 {
            let root_block =
                layout.inode_table(0) + layout.inode_table_blocks as u64;
            let lost_found_block = root_block + 1;

            let root = build_inode(
                0x41ED,
                3,
                block_size,
                inode_blocks,
                root_block,
                extra_isize,
            );
            let lost_found = build_inode(
                0x41C0,
                2,
                block_size,
                inode_blocks,
                lost_found_block,
                extra_isize,
            );

            // inode 2 и inode 11 могут оказаться в разных блоках inode table
            // при размере блока 1 КиБ.
            let root_offset = INODE_SIZE_USIZE;
            let lost_offset = (LOST_FOUND_INO as usize - 1) * INODE_SIZE_USIZE;
            let block_size_usize = block_size as usize;
            let root_block_index = root_offset / block_size_usize;
            let lost_block_index = lost_offset / block_size_usize;

            let mut inode_block = [0u8; MAX_BLOCK_SIZE];
            inode_block[root_offset % block_size_usize
                ..root_offset % block_size_usize + INODE_SIZE_USIZE]
                .copy_from_slice(&root);

            if lost_block_index == root_block_index {
                inode_block[lost_offset % block_size_usize
                    ..lost_offset % block_size_usize + INODE_SIZE_USIZE]
                    .copy_from_slice(&lost_found);
            }

            write_block(
                device,
                partition_start,
                partition_sectors,
                block_size,
                layout.inode_table(0) + root_block_index as u64,
                &inode_block[..block_size_usize],
            )?;

            if lost_block_index != root_block_index {
                let mut lost_block = [0u8; MAX_BLOCK_SIZE];
                lost_block[lost_offset % block_size_usize
                    ..lost_offset % block_size_usize + INODE_SIZE_USIZE]
                    .copy_from_slice(&lost_found);

                write_block(
                    device,
                    partition_start,
                    partition_sectors,
                    block_size,
                    layout.inode_table(0) + lost_block_index as u64,
                    &lost_block[..block_size_usize],
                )?;
            }

            let mut root_dir = [0u8; MAX_BLOCK_SIZE];
            put_dir_entry(&mut root_dir, 0, ROOT_INO, 12, 2, b".");
            put_dir_entry(&mut root_dir, 12, ROOT_INO, 12, 2, b"..");
            put_dir_entry(
                &mut root_dir,
                24,
                LOST_FOUND_INO,
                block_size as usize - 24,
                2,
                b"lost+found",
            );

            write_block(
                device,
                partition_start,
                partition_sectors,
                block_size,
                root_block,
                &root_dir,
            )?;

            let mut lost_dir = [0u8; MAX_BLOCK_SIZE];
            put_dir_entry(&mut lost_dir, 0, LOST_FOUND_INO, 12, 2, b".");
            put_dir_entry(
                &mut lost_dir,
                12,
                ROOT_INO,
                block_size as usize - 12,
                2,
                b"..",
            );

            write_block(
                device,
                partition_start,
                partition_sectors,
                block_size,
                lost_found_block,
                &lost_dir,
            )?;
        }
    }

    Ok(FormatInfo {
        block_size,
        blocks: layout.blocks_count,
        inodes: layout.inodes_count(),
    })
}
