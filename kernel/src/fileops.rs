//! Файловые операции для графического менеджера `fmgr`.
//!
//! В отличие от команд оболочки эти функции не печатают текст в VGA, а
//! возвращают записи каталога и коды ошибок. Пути адресуются так же, как
//! в оболочке: `/dsk/a1/...` или путь внутри текущего раздела.

use crate::fs::{self, exfat, ext4, FileLocation, TransferError};

/// Максимальная длина имени, сохраняемого для GUI.
pub const NAME_MAX: usize = 128;

/// Запись каталога для списка файлов.
#[derive(Clone, Copy)]
pub struct Entry {
    pub name: [u8; NAME_MAX],
    pub name_len: usize,
    pub is_dir: bool,
    pub size: u64,
}

impl Entry {
    pub const EMPTY: Self = Self {
        name: [0; NAME_MAX],
        name_len: 0,
        is_dir: false,
        size: 0,
    };

}

/// Раздел на боковой панели менеджера.
#[derive(Clone, Copy)]
pub struct Disk {
    pub name: [u8; 8],
    pub name_len: usize,
    /// 0 — другой, 1 — exFAT, 2 — ext4.
    pub kind: u8,
    pub sectors: u32,
}

impl Disk {
    pub const EMPTY: Self = Self {
        name: [0; 8],
        name_len: 0,
        kind: 0,
        sectors: 0,
    };

}

fn push_name(output: &mut Entry, name: &[u8]) {
    output.name_len = name.len().min(NAME_MAX);
    output.name[..output.name_len].copy_from_slice(&name[..output.name_len]);
}

/// Перечисляет каталог. Возвращает число записей (не больше `out.len()`).
pub fn list(location: &FileLocation, out: &mut [Entry]) -> Result<usize, &'static [u8]> {
    match location.kind {
        crate::disk::FsKind::Exfat => list_exfat(location, out),
        crate::disk::FsKind::Ext4 => list_ext4(location, out),
        crate::disk::FsKind::Other => Err(b"unsupported filesystem"),
    }
}

fn list_exfat(location: &FileLocation, out: &mut [Entry]) -> Result<usize, &'static [u8]> {
    let device = exfat::AtaDevice::new(location.device);
    let volume = exfat::fs::mount_at(&device, location.start_lba)
        .map_err(|error| error.message())?;
    let directory = exfat::fs::resolve(&device, &volume, location.path())
        .map_err(|error| error.message())?;

    if !directory.is_directory() {
        return Err(b"not a directory");
    }

    let mut cursor = exfat::fs::DirCursor::new(
        &volume,
        &device,
        directory.first_cluster,
        directory.contiguous,
        directory.data_length,
    );

    let mut count = 0usize;

    while count < out.len() {
        let Some(entry) = cursor.next_entry().map_err(|error| error.message())? else {
            break;
        };

        if entry.name() == b"." || entry.name() == b".." {
            continue;
        }

        out[count] = Entry::EMPTY;
        push_name(&mut out[count], entry.name());
        out[count].is_dir = entry.is_directory();
        out[count].size = entry.content_length();
        count += 1;
    }

    sort(&mut out[..count]);
    Ok(count)
}

fn list_ext4(location: &FileLocation, out: &mut [Entry]) -> Result<usize, &'static [u8]> {
    let device = ext4::AtaDevice::new(location.device);
    let filesystem = ext4::Ext4::mount(&device, location.start_lba, location.sectors)
        .map_err(|error| ext4_message(error))?;
    let inode_number = filesystem
        .find_inode_path(location.path())
        .map_err(|error| ext4_message(error))?
        .ok_or(b"path not found" as &'static [u8])?;
    let directory = filesystem
        .read_inode(inode_number)
        .map_err(|error| ext4_message(error))?;

    if !directory.is_directory {
        return Err(b"not a directory");
    }

    let mut count = 0usize;

    filesystem
        .for_each_directory_entry(inode_number, |child, name, _file_type| {
            if name == b"." || name == b".." {
                return true;
            }

            if count >= out.len() {
                return false;
            }

            out[count] = Entry::EMPTY;
            push_name(&mut out[count], name);

            match filesystem.read_inode(child) {
                Ok(inode) => {
                    out[count].is_dir = inode.is_directory;
                    out[count].size = inode.size;
                }
                Err(_) => {
                    out[count].is_dir = false;
                    out[count].size = 0;
                }
            }

            count += 1;
            true
        })
        .map_err(|error| ext4_message(error))?;

    sort(&mut out[..count]);
    Ok(count)
}

fn sort(entries: &mut [Entry]) {
    // Каталоги идут первыми, затем имена без учёта регистра.
    for index in 1..entries.len() {
        let mut current = index;

        while current > 0 && less(&entries[current], &entries[current - 1]) {
            entries.swap(current, current - 1);
            current -= 1;
        }
    }
}

fn less(a: &Entry, b: &Entry) -> bool {
    if a.is_dir != b.is_dir {
        return a.is_dir;
    }

    let length = a.name_len.min(b.name_len);

    for index in 0..length {
        let left = a.name[index].to_ascii_lowercase();
        let right = b.name[index].to_ascii_lowercase();

        if left != right {
            return left < right;
        }
    }

    a.name_len < b.name_len
}

fn ext4_message(error: ext4::Ext4Error) -> &'static [u8] {
    match error {
        ext4::Ext4Error::NoSpace => b"no free space",
        ext4::Ext4Error::ReadOnly => b"filesystem is read-only",
        ext4::Ext4Error::UnsafeWrite => b"ext4 write disabled: unsafe features",
        ext4::Ext4Error::NotEmpty => b"directory is not empty",
        ext4::Ext4Error::InvalidGeometry | ext4::Ext4Error::OutOfBounds => {
            b"path not found"
        }
        _ => b"ext4 operation failed",
    }
}

/// Проверяет, существует ли путь и является ли он каталогом.
pub fn is_directory(location: &FileLocation) -> Result<bool, &'static [u8]> {
    match location.kind {
        crate::disk::FsKind::Exfat => {
            let device = exfat::AtaDevice::new(location.device);
            let volume = exfat::fs::mount_at(&device, location.start_lba)
                .map_err(|error| error.message())?;
            let entry = exfat::fs::resolve(&device, &volume, location.path())
                .map_err(|error| error.message())?;
            Ok(entry.is_directory())
        }
        crate::disk::FsKind::Ext4 => {
            let device = ext4::AtaDevice::new(location.device);
            let filesystem = ext4::Ext4::mount(&device, location.start_lba, location.sectors)
                .map_err(|error| ext4_message(error))?;
            let inode_number = filesystem
                .find_inode_path(location.path())
                .map_err(|error| ext4_message(error))?
                .ok_or(b"path not found" as &'static [u8])?;
            let inode = filesystem
                .read_inode(inode_number)
                .map_err(|error| ext4_message(error))?;
            Ok(inode.is_directory)
        }
        crate::disk::FsKind::Other => Err(b"unsupported filesystem"),
    }
}

pub fn exists(location: &FileLocation) -> bool {
    is_directory(location).is_ok()
}

/// Создаёт каталог.
pub fn create_directory(location: &FileLocation) -> Result<(), &'static [u8]> {
    match location.kind {
        crate::disk::FsKind::Exfat => {
            let device = exfat::AtaDevice::new(location.device);
            let volume = exfat::fs::mount_at(&device, location.start_lba)
                .map_err(|error| error.message())?;
            exfat::fs::make_directory(
                &device,
                &volume,
                location.path(),
                crate::rtc::timestamp(),
            )
            .map_err(|error| error.message())
        }
        crate::disk::FsKind::Ext4 => {
            let device = ext4::AtaDevice::new(location.device);
            let filesystem = ext4::Ext4::mount_read_write(
                &device,
                location.start_lba,
                location.sectors,
            )
            .map_err(|error| ext4_message(error))?;
            filesystem
                .mkdir_path(location.path())
                .map(|_| ())
                .map_err(|error| ext4_message(error))
        }
        crate::disk::FsKind::Other => Err(b"unsupported filesystem"),
    }
}

/// Удаляет файл или пустой каталог.
pub fn delete(location: &FileLocation) -> Result<(), &'static [u8]> {
    let directory = is_directory(location)?;

    match location.kind {
        crate::disk::FsKind::Exfat => {
            let device = exfat::AtaDevice::new(location.device);
            let volume = exfat::fs::mount_at(&device, location.start_lba)
                .map_err(|error| error.message())?;

            if directory {
                exfat::fs::remove_directory(&device, &volume, location.path())
                    .map_err(|error| error.message())
            } else {
                exfat::fs::remove_file(&device, &volume, location.path())
                    .map_err(|error| error.message())
            }
        }
        crate::disk::FsKind::Ext4 => {
            let device = ext4::AtaDevice::new(location.device);
            let filesystem = ext4::Ext4::mount_read_write(
                &device,
                location.start_lba,
                location.sectors,
            )
            .map_err(|error| ext4_message(error))?;

            if directory {
                filesystem
                    .remove_directory_path(location.path())
                    .map_err(|error| ext4_message(error))
            } else {
                filesystem
                    .remove_file_path(location.path())
                    .map_err(|error| ext4_message(error))
            }
        }
        crate::disk::FsKind::Other => Err(b"unsupported filesystem"),
    }
}

fn copy_buffer() -> Result<&'static mut [u8], &'static [u8]> {
    crate::arena::allocate(2 * 1024 * 1024, 16)
        .ok_or(b"not enough memory for copy" as &'static [u8])
}

/// Копирует или перемещает файл. Каталоги не поддерживаются.
pub fn copy(
    source: &FileLocation,
    destination: &FileLocation,
    moving: bool,
) -> Result<u64, &'static [u8]> {
    if source.same_file(destination) {
        return Err(b"source and destination are the same");
    }

    if exists(destination) {
        return Err(b"destination already exists");
    }

    if is_directory(source)? {
        return Err(b"directory copying is not supported");
    }

    let mark = crate::arena::mark();
    let buffer = match copy_buffer() {
        Ok(buffer) => buffer,
        Err(error) => {
            crate::arena::release(mark);
            return Err(error);
        }
    };
    let result = if moving {
        fs::move_file(source, destination, buffer)
    } else {
        fs::copy_file(source, destination, buffer)
    };
    crate::arena::release(mark);

    result.map_err(|error| error_message(error))
}

fn error_message(error: TransferError) -> &'static [u8] {
    error.message()
}

/// Переименовывает файл или пустой каталог.
pub fn rename(
    source: &FileLocation,
    destination: &FileLocation,
) -> Result<(), &'static [u8]> {
    if source.same_file(destination) {
        return Ok(());
    }

    if exists(destination) {
        return Err(b"destination already exists");
    }

    if !is_directory(source)? {
        return copy(source, destination, true).map(|_| ());
    }

    // Каталог переименовывается только если он пуст: создаём новый и
    // удаляем старый. Для непустого каталога операция отклоняется.
    create_directory(destination)?;

    match delete(source) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = delete(destination);
            Err(error)
        }
    }
}

/// Заполняет `out` списком разделов для боковой панели.
pub fn disks(out: &mut [Disk]) -> usize {
    let mut partitions = [crate::disk::DiskPartition::EMPTY; 32];
    let count = crate::disk::read_partitions(&mut partitions);
    let mut written = 0usize;

    for partition in partitions[..count].iter() {
        if written >= out.len() {
            break;
        }

        if partition.kind == crate::disk::FsKind::Other {
            continue;
        }

        out[written] = Disk::EMPTY;
        out[written].name[0] = crate::disk::disk_letter(partition.device);
        out[written].name_len = 1;

        let mut value = partition.partition_index;
        let mut digits = [0u8; 8];
        let mut digit_count = 0usize;

        if value == 0 {
            digits[0] = b'0';
            digit_count = 1;
        } else {
            while value > 0 && digit_count < digits.len() {
                digits[digit_count] = b'0' + (value % 10) as u8;
                value /= 10;
                digit_count += 1;
            }
        }

        while digit_count > 0 {
            digit_count -= 1;
            out[written].name[out[written].name_len] = digits[digit_count];
            out[written].name_len += 1;
        }

        out[written].kind = match partition.kind {
            crate::disk::FsKind::Exfat => 1,
            crate::disk::FsKind::Ext4 => 2,
            crate::disk::FsKind::Other => 0,
        };
        out[written].sectors = partition.sectors;
        written += 1;
    }

    written
}
