//! Доступ pfpak к выбранному ext4-разделу.
//!
//! Пакетный менеджер работает только с ext4: установленная система живёт
//! на ext4, а exFAT используется как обменный том. Все операции идут
//! через штатный драйвер Perfix, поэтому не дублируют разбор файловой
//! системы.

use crate::disk;
use crate::fs::ext4::{AtaDevice, Ext4, Ext4Error};

pub type Error = &'static [u8];

static DEVICE: AtaDevice = AtaDevice;

pub fn mount_readonly() -> Result<Ext4<'static, AtaDevice>, Error> {
    if !matches!(disk::selected_kind(), disk::FsKind::Ext4) {
        return Err(b"pfpak needs an ext4 system partition");
    }

    Ext4::mount(&DEVICE, disk::selected_lba(), disk::selected_sectors())
        .map_err(map_error)
}

pub fn mount_readwrite() -> Result<Ext4<'static, AtaDevice>, Error> {
    if !matches!(disk::selected_kind(), disk::FsKind::Ext4) {
        return Err(b"pfpak needs an ext4 system partition");
    }

    Ext4::mount_read_write(&DEVICE, disk::selected_lba(), disk::selected_sectors())
        .map_err(map_error)
}

pub fn read_file(
    filesystem: &Ext4<'_, AtaDevice>,
    path: &[u8],
    output: &mut [u8],
) -> Result<usize, Error> {
    let inode_number = filesystem
        .find_inode_path(path)
        .map_err(map_error)?
        .ok_or(b"file not found" as Error)?;
    let inode = filesystem.read_inode(inode_number).map_err(map_error)?;

    if inode.is_directory {
        return Err(b"path is a directory");
    }

    if inode.size > output.len() as u64 {
        return Err(b"file is too large");
    }

    let mut offset = 0u64;

    while offset < inode.size {
        let count = filesystem
            .read_file(inode_number, offset, &mut output[offset as usize..])
            .map_err(map_error)?;

        if count == 0 {
            break;
        }

        offset += count as u64;
    }

    Ok(offset as usize)
}

pub fn write_file(
    filesystem: &Ext4<'_, AtaDevice>,
    path: &[u8],
    data: &[u8],
) -> Result<(), Error> {
    let parent = parent_path(path);

    if parent != b"/" {
        filesystem
            .ensure_directory_path(parent)
            .map_err(map_error)?;
    }

    filesystem
        .write_file_path(path, data)
        .map(|_| ())
        .map_err(map_error)
}

pub fn ensure_directory(
    filesystem: &Ext4<'_, AtaDevice>,
    path: &[u8],
) -> Result<(), Error> {
    filesystem.ensure_directory_path(path).map(|_| ()).map_err(map_error)
}

pub fn remove_file(
    filesystem: &Ext4<'_, AtaDevice>,
    path: &[u8],
) -> Result<(), Error> {
    filesystem.remove_file_path(path).map_err(map_error)
}

pub fn remove_directory(
    filesystem: &Ext4<'_, AtaDevice>,
    path: &[u8],
) -> Result<(), Error> {
    filesystem.remove_directory_path(path).map_err(map_error)
}

pub fn exists(filesystem: &Ext4<'_, AtaDevice>, path: &[u8]) -> bool {
    matches!(filesystem.find_inode_path(path), Ok(Some(_)))
}

pub fn is_directory(filesystem: &Ext4<'_, AtaDevice>, path: &[u8]) -> bool {
    match filesystem.find_inode_path(path) {
        Ok(Some(inode_number)) => filesystem
            .read_inode(inode_number)
            .map(|inode| inode.is_directory)
            .unwrap_or(false),
        _ => false,
    }
}

/// Перебирает имена в каталоге. Возвращает `false` из замыкания для остановки.
pub fn for_each_entry<F>(
    filesystem: &Ext4<'_, AtaDevice>,
    path: &[u8],
    mut f: F,
) -> Result<(), Error>
where
    F: FnMut(&[u8], bool) -> bool,
{
    let inode_number = filesystem
        .find_inode_path(path)
        .map_err(map_error)?
        .ok_or(b"directory not found" as Error)?;
    let inode = filesystem.read_inode(inode_number).map_err(map_error)?;

    if !inode.is_directory {
        return Err(b"not a directory");
    }

    let mut result = Ok(());

    filesystem
        .for_each_directory_entry(
            inode_number,
            |_inode, name, file_type| {
                if name == b"." || name == b".." {
                    return true;
                }

                f(name, file_type == 2)
            },
        )
        .map_err(|error| {
            result = Err(map_error(error));
        })
        .ok();

    result
}

pub fn parent_path(path: &[u8]) -> &[u8] {
    let mut index = path.len();

    while index > 1 && path[index - 1] != b'/' {
        index -= 1;
    }

    if index <= 1 {
        b"/"
    } else {
        &path[..index - 1]
    }
}

pub fn map_error(error: Ext4Error) -> Error {
    match error {
        Ext4Error::NoSpace => b"no free space",
        Ext4Error::ReadOnly => b"ext4 is read-only",
        Ext4Error::UnsafeWrite => b"ext4 write disabled: unsafe features",
        Ext4Error::UnsupportedFeature => b"operation not supported by ext4",
        Ext4Error::UnsupportedBlockSize => b"unsupported ext4 block size",
        Ext4Error::InvalidSuperblock => b"invalid ext4 superblock",
        Ext4Error::BufferTooSmall => b"filesystem buffer is too small",
        Ext4Error::OutOfBounds => b"path not found",
        Ext4Error::NotEmpty => b"directory is not empty",
        Ext4Error::InvalidGeometry => b"invalid path or inode",
        Ext4Error::Io => b"I/O error",
    }
}
