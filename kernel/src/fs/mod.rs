pub mod exfat;
pub mod ext4;

pub use exfat::CAT_LINES;

use exfat::{FsError, PartitionInfo};
use crate::disk::FsKind;

/// Максимальная длина пути внутри файловой системы.
pub const FS_PATH_MAX: usize = 512;

/// Ошибка операций копирования/перемещения между разделами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferError {
    NotFound,
    IsDirectory,
    NotFile,
    TooLarge,
    NoSpace,
    ReadOnly,
    UnsafeWrite,
    Unsupported,
    ReadFailed,
    WriteFailed,
    DeleteFailed,
    VerifyFailed,
    InvalidName,
    Io,
    NotExfat,
    UnsupportedSectorSize,
    CorruptedGeometry,
    ClusterOutOfRange,
    FatOutOfRange,
    BrokenChain,
    AlreadyExists,
    DirectoryNotEmpty,
    PartitionNotFound,
    UnsupportedBlockSize,
    InvalidSuperblock,
    Fs,
}

impl TransferError {
    pub fn message(self) -> &'static [u8] {
        match self {
            TransferError::NotFound => b"path not found",
            TransferError::IsDirectory => b"is a directory",
            TransferError::NotFile => b"not a regular file",
            TransferError::TooLarge => b"file is too large for copy buffer",
            TransferError::NoSpace => b"no free space",
            TransferError::ReadOnly => b"filesystem is read-only",
            TransferError::UnsafeWrite => b"write disabled: unsafe features",
            TransferError::Unsupported => b"operation not supported",
            TransferError::ReadFailed => b"read failed",
            TransferError::WriteFailed => b"write failed",
            TransferError::DeleteFailed => b"could not delete source",
            TransferError::VerifyFailed => b"copy verification failed",
            TransferError::InvalidName => b"invalid file name",
            TransferError::Io => b"I/O error",
            TransferError::NotExfat => b"not an exFAT volume",
            TransferError::UnsupportedSectorSize => {
                b"unsupported sector size (need 512 bytes)"
            }
            TransferError::CorruptedGeometry => b"corrupted exFAT geometry",
            TransferError::ClusterOutOfRange => b"exFAT cluster out of range",
            TransferError::FatOutOfRange => b"exFAT FAT entry out of range",
            TransferError::BrokenChain => b"broken exFAT cluster chain",
            TransferError::AlreadyExists => b"destination already exists",
            TransferError::DirectoryNotEmpty => b"directory not empty",
            TransferError::PartitionNotFound => b"partition not found",
            TransferError::UnsupportedBlockSize => b"unsupported ext4 block size",
            TransferError::InvalidSuperblock => b"invalid ext4 superblock",
            TransferError::Fs => b"filesystem error",
        }
    }
}

/// Расположение файла: раздел, файловая система и путь внутри неё.
#[derive(Clone, Copy)]
pub struct FileLocation {
    pub kind: FsKind,
    pub start_lba: u32,
    pub sectors: u32,
    path: [u8; FS_PATH_MAX],
    path_len: usize,
}

impl FileLocation {
    pub fn new(
        kind: FsKind,
        start_lba: u32,
        sectors: u32,
        path: &[u8],
    ) -> Option<Self> {
        if path.is_empty() || path.len() > FS_PATH_MAX {
            return None;
        }

        let mut location = FileLocation {
            kind,
            start_lba,
            sectors,
            path: [0u8; FS_PATH_MAX],
            path_len: path.len(),
        };
        location.path[..path.len()].copy_from_slice(path);
        Some(location)
    }

    pub fn path(&self) -> &[u8] {
        &self.path[..self.path_len]
    }

    pub fn same_file(&self, other: &FileLocation) -> bool {
        if self.kind != other.kind || self.start_lba != other.start_lba {
            return false;
        }

        // exFAT не различает регистр имён, поэтому `FILE` и `file`
        // указывают на один и тот же файл.
        if self.kind == FsKind::Exfat {
            self.path().eq_ignore_ascii_case(other.path())
        } else {
            self.path() == other.path()
        }
    }
}

fn map_exfat_error(error: FsError) -> TransferError {
    match error {
        FsError::PathNotFound | FsError::NotADirectory => {
            TransferError::NotFound
        }
        FsError::NotAFile => TransferError::NotFile,
        FsError::NoSpace => TransferError::NoSpace,
        FsError::InvalidName => TransferError::InvalidName,
        FsError::Io(_) => TransferError::Io,
        FsError::NotExfat => TransferError::NotExfat,
        FsError::UnsupportedSectorSize => TransferError::UnsupportedSectorSize,
        FsError::BadGeometry => TransferError::CorruptedGeometry,
        FsError::ClusterOutOfRange => TransferError::ClusterOutOfRange,
        FsError::FatOutOfRange => TransferError::FatOutOfRange,
        FsError::TruncatedChain => TransferError::BrokenChain,
        FsError::AlreadyExists => TransferError::AlreadyExists,
        FsError::DirectoryNotEmpty => TransferError::DirectoryNotEmpty,
        FsError::PartitionNotFound => TransferError::PartitionNotFound,
    }
}

fn map_ext4_error(error: crate::fs::ext4::Ext4Error) -> TransferError {
    use crate::fs::ext4::Ext4Error;

    match error {
        Ext4Error::NoSpace => TransferError::NoSpace,
        Ext4Error::ReadOnly => TransferError::ReadOnly,
        Ext4Error::UnsafeWrite => TransferError::UnsafeWrite,
        Ext4Error::UnsupportedFeature => TransferError::Unsupported,
        Ext4Error::OutOfBounds => TransferError::NotFound,
        Ext4Error::Io => TransferError::Io,
        Ext4Error::UnsupportedBlockSize => TransferError::UnsupportedBlockSize,
        Ext4Error::InvalidSuperblock => TransferError::InvalidSuperblock,
        Ext4Error::NotEmpty => TransferError::DirectoryNotEmpty,
        _ => TransferError::Fs,
    }
}

fn print_ext4_error(row: usize, error: crate::fs::ext4::Ext4Error) -> usize {
    use crate::fs::ext4::Ext4Error;

    let message: &[u8] = match error {
        Ext4Error::ReadOnly => b"ext4 is read-only",
        Ext4Error::UnsafeWrite => b"ext4 write disabled: unsafe features",
        Ext4Error::NoSpace => b"ext4: no free space",
        Ext4Error::UnsupportedFeature => b"ext4: operation not supported",
        Ext4Error::InvalidGeometry => b"ext4: invalid path or inode",
        _ => b"ext4 operation failed",
    };

    crate::vga::print_at(row, 0, message);
    1
}

fn exfat_mount(location: &FileLocation) -> Result<exfat::fs::Volume, TransferError> {
    exfat::fs::mount_at(&exfat::AtaDevice, location.start_lba)
        .map_err(map_exfat_error)
}

fn ext4_mount_read(
    location: &FileLocation,
) -> Result<crate::fs::ext4::Ext4<'_, crate::fs::ext4::AtaDevice>, TransferError> {
    crate::fs::ext4::Ext4::mount(
        &crate::fs::ext4::AtaDevice,
        location.start_lba,
        location.sectors,
    )
    .map_err(map_ext4_error)
}

fn ext4_mount_write(
    location: &FileLocation,
) -> Result<crate::fs::ext4::Ext4<'_, crate::fs::ext4::AtaDevice>, TransferError> {
    crate::fs::ext4::Ext4::mount_read_write(
        &crate::fs::ext4::AtaDevice,
        location.start_lba,
        location.sectors,
    )
    .map_err(map_ext4_error)
}

fn ext4_inode_for_path(
    filesystem: &crate::fs::ext4::Ext4<'_, crate::fs::ext4::AtaDevice>,
    path: &[u8],
) -> Result<u32, TransferError> {
    filesystem
        .find_inode_path(path)
        .map_err(map_ext4_error)?
        .ok_or(TransferError::NotFound)
}

/// Возвращает размер обычного файла и проверяет, что это не каталог.
pub fn file_size(location: &FileLocation) -> Result<u64, TransferError> {
    match location.kind {
        FsKind::Exfat => {
            let device = exfat::AtaDevice;
            let volume = exfat_mount(location)?;
            let entry = exfat::fs::resolve(
                &device,
                &volume,
                location.path(),
            )
            .map_err(map_exfat_error)?;

            if entry.is_directory() {
                return Err(TransferError::IsDirectory);
            }

            Ok(entry.content_length())
        }
        FsKind::Ext4 => {
            let filesystem = ext4_mount_read(location)?;
            let inode_number =
                ext4_inode_for_path(&filesystem, location.path())?;
            let inode = filesystem
                .read_inode(inode_number)
                .map_err(map_ext4_error)?;

            if inode.is_directory {
                return Err(TransferError::IsDirectory);
            }

            if inode.mode & 0xF000 != 0x8000 {
                return Err(TransferError::NotFile);
            }

            Ok(inode.size)
        }
        FsKind::Other => Err(TransferError::Unsupported),
    }
}

fn stream_file<F>(
    location: &FileLocation,
    mut sink: F,
) -> Result<u64, TransferError>
where
    F: FnMut(&[u8]) -> bool,
{
    match location.kind {
        FsKind::Exfat => {
            let device = exfat::AtaDevice;
            let volume = exfat_mount(location)?;
            let entry = exfat::fs::resolve(
                &device,
                &volume,
                location.path(),
            )
            .map_err(map_exfat_error)?;

            if entry.is_directory() {
                return Err(TransferError::IsDirectory);
            }

            let mut total = 0u64;

            exfat::fs::read_file(
                &device,
                &volume,
                &entry,
                |chunk| {
                    total += chunk.len() as u64;
                    sink(chunk)
                },
            )
            .map_err(map_exfat_error)?;

            Ok(total)
        }
        FsKind::Ext4 => {
            let filesystem = ext4_mount_read(location)?;
            let inode_number =
                ext4_inode_for_path(&filesystem, location.path())?;
            let inode = filesystem
                .read_inode(inode_number)
                .map_err(map_ext4_error)?;

            if inode.is_directory {
                return Err(TransferError::IsDirectory);
            }

            if inode.mode & 0xF000 != 0x8000 {
                return Err(TransferError::NotFile);
            }

            let mut offset = 0u64;
            let mut buffer = [0u8; 512];

            while offset < inode.size {
                let count = filesystem
                    .read_file(inode_number, offset, &mut buffer)
                    .map_err(map_ext4_error)?;

                if count == 0 {
                    break;
                }

                if !sink(&buffer[..count]) {
                    break;
                }

                offset += count as u64;
            }

            Ok(offset)
        }
        FsKind::Other => Err(TransferError::Unsupported),
    }
}

/// Читает файл целиком в буфер. Если файл больше буфера — `TooLarge`.
pub fn read_file(
    location: &FileLocation,
    output: &mut [u8],
) -> Result<usize, TransferError> {
    let size = file_size(location)?;

    if size > output.len() as u64 {
        return Err(TransferError::TooLarge);
    }

    let mut written = 0usize;

    let total = stream_file(location, |chunk| {
        if written + chunk.len() > output.len() {
            return false;
        }

        output[written..written + chunk.len()].copy_from_slice(chunk);
        written += chunk.len();
        true
    })?;

    if total != size || written as u64 != size {
        return Err(TransferError::ReadFailed);
    }

    Ok(written)
}

/// Записывает файл целиком, создавая его при необходимости.
pub fn write_file(
    location: &FileLocation,
    data: &[u8],
) -> Result<(), TransferError> {
    match location.kind {
        FsKind::Exfat => {
            let device = exfat::AtaDevice;
            let volume = exfat_mount(location)?;

            exfat::fs::write_file(
                &device,
                &volume,
                location.path(),
                data,
                crate::rtc::timestamp(),
            )
            .map_err(map_exfat_error)
        }
        FsKind::Ext4 => {
            let filesystem = ext4_mount_write(location)?;

            match filesystem.write_file_path(location.path(), data) {
                Ok(_) => Ok(()),
                Err(crate::fs::ext4::Ext4Error::InvalidGeometry) => {
                    Err(TransferError::IsDirectory)
                }
                Err(error) => Err(map_ext4_error(error)),
            }
        }
        FsKind::Other => Err(TransferError::Unsupported),
    }
}

/// Удаляет файл (используется командой `mv`).
pub fn remove_file(location: &FileLocation) -> Result<(), TransferError> {
    match location.kind {
        FsKind::Exfat => {
            let device = exfat::AtaDevice;
            let volume = exfat_mount(location)?;

            exfat::fs::remove_file(
                &device,
                &volume,
                location.path(),
            )
            .map_err(map_exfat_error)
        }
        FsKind::Ext4 => {
            let filesystem = ext4_mount_write(location)?;

            filesystem
                .remove_file_path(location.path())
                .map_err(map_ext4_error)
        }
        FsKind::Other => Err(TransferError::Unsupported),
    }
}

const HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const HASH_PRIME: u64 = 0x0000_0100_0000_01b3;

fn hash_update(hash: &mut u64, data: &[u8]) {
    for &byte in data {
        *hash ^= byte as u64;
        *hash = hash.wrapping_mul(HASH_PRIME);
    }
}

fn verify_destination(
    location: &FileLocation,
    expected_size: u64,
    expected_hash: u64,
) -> Result<(), TransferError> {
    if file_size(location)? != expected_size {
        return Err(TransferError::VerifyFailed);
    }

    let mut hash = HASH_OFFSET;
    let total = stream_file(location, |chunk| {
        hash_update(&mut hash, chunk);
        true
    })?;

    if total != expected_size || hash != expected_hash {
        return Err(TransferError::VerifyFailed);
    }

    Ok(())
}

/// Копирует файл между любыми поддерживаемыми файловыми системами.
///
/// После записи содержимое назначения читается заново и сверяется
/// по размеру и контрольной сумме.
pub fn copy_file(
    source: &FileLocation,
    destination: &FileLocation,
    buffer: &mut [u8],
) -> Result<u64, TransferError> {
    let size = read_file(source, buffer)?;
    let mut hash = HASH_OFFSET;
    hash_update(&mut hash, &buffer[..size]);

    write_file(destination, &buffer[..size])?;
    verify_destination(destination, size as u64, hash)?;

    Ok(size as u64)
}

/// Перемещает файл: копирование, проверка, затем удаление оригинала.
pub fn move_file(
    source: &FileLocation,
    destination: &FileLocation,
    buffer: &mut [u8],
) -> Result<u64, TransferError> {
    let size = copy_file(source, destination, buffer)?;

    if let Err(error) = remove_file(source) {
        // Копия уже проверена и существует, поэтому исходник можно
        // оставить: данные не потеряны.
        return Err(error);
    }

    Ok(size)
}


/// Общий интерфейс файловых систем Perfix.
pub trait FileSystem {
    fn space_info(&self) -> Result<(u64, u64), FsError>;

    fn print_volume_info(&self, row: usize) -> usize;
    fn print_boot_sector(&self, row: usize) -> usize;

    fn shell_list(&self, path: &[u8], row: usize) -> usize;
    fn shell_cat(&self, path: &[u8], row: usize) -> usize;
    fn shell_write(&self, path: &[u8], text: &[u8], row: usize) -> usize;
    fn shell_remove(&self, path: &[u8], row: usize) -> usize;
    fn shell_mkdir(&self, path: &[u8], row: usize) -> usize;
    fn shell_rmdir(&self, path: &[u8], row: usize) -> usize;

    fn shell_listpart(&self, row: usize) -> usize;
    fn shell_setup(&self, index: usize) -> usize;

    fn select_partition(&self, index: usize)
        -> Result<PartitionInfo, FsError>;

    fn path_is_directory(&self, path: &[u8]) -> Result<bool, FsError>;
}
/// Ошибка операций навигации по файловым системам.
pub type DirectoryError = &'static [u8];
/// Общий интерфейс для навигации и чтения каталогов.
pub trait DirectoryFileSystem {
    fn shell_list(&self, path: &[u8], row: usize) -> usize;

    fn shell_cat(&self, path: &[u8], row: usize) -> usize;

    fn shell_write(&self, path: &[u8], text: &[u8], row: usize) -> usize;

    fn shell_remove(&self, path: &[u8], row: usize) -> usize;

    fn shell_mkdir(&self, path: &[u8], row: usize) -> usize;

    fn shell_rmdir(&self, path: &[u8], row: usize) -> usize;

    fn path_is_directory(&self, path: &[u8]) -> Result<bool, DirectoryError>;
}

/// Адаптер каталогов exFAT.
pub struct ExfatDirectoryFileSystem;

impl DirectoryFileSystem for ExfatDirectoryFileSystem {
    fn shell_list(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_list(path, row)
    }
    fn path_is_directory(&self, path: &[u8]) -> Result<bool, DirectoryError> {
        exfat::path_is_directory(path).map_err(|error| error.message())
    }
    fn shell_cat(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_cat(path, row)
    }

    fn shell_write(&self, path: &[u8], text: &[u8], row: usize) -> usize {
        let _ = (path, text);
        crate::vga::print_at(row, 0, b"Use exFAT write handler");
        1
    }

    fn shell_remove(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_remove(path, row)
    }

    fn shell_mkdir(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_mkdir(path, row)
    }

    fn shell_rmdir(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_rmdir(path, row)
    }
}

/// Адаптер каталогов ext4.
pub struct Ext4DirectoryFileSystem;

impl DirectoryFileSystem for Ext4DirectoryFileSystem {
        fn shell_list(&self, path: &[u8], row: usize) -> usize {
        let default_path: &[u8] = b"/";
        let path = if path.is_empty() {
            default_path
        } else {
            path
        };

        if !matches!(
            crate::disk::selected_kind(),
            crate::disk::FsKind::Ext4
        ) {
            crate::vga::print_at(row, 0, b"Selected partition is not ext4");
            return 1;
        }

        let device = crate::fs::ext4::AtaDevice;
        let ext4 = match crate::fs::ext4::Ext4::mount(
            &device,
            crate::disk::selected_lba(),
            crate::disk::selected_sectors(),
        ) {
            Ok(fs) => fs,
            Err(_) => {
                crate::vga::print_at(row, 0, b"ext4 mount failed");
                return 1;
            }
        };

        let mut current_inode = 2u32;

        for component in path.split(|&byte| byte == b'/') {
            if component.is_empty() || component == b"." {
                continue;
            }

            current_inode = match ext4.find_directory_entry(
                current_inode,
                component,
            ) {
                Ok(Some(inode)) => inode,
                Ok(None) => {
                    crate::vga::print_at(row, 0, b"Path not found");
                    return 1;
                }
                Err(_) => {
                    crate::vga::print_at(row, 0, b"ext4 path lookup failed");
                    return 1;
                }
            };
        }

        let directory = match ext4.read_inode(current_inode) {
            Ok(inode) => inode,
            Err(_) => {
                crate::vga::print_at(row, 0, b"ext4 inode read failed");
                return 1;
            }
        };

        if !directory.is_directory {
            crate::vga::print_at(row, 0, b"Not a directory");
            return 1;
        }

        let mut header = [b' '; 80];
        let prefix = b"Directory: ";
        let path_len = path.len().min(header.len() - prefix.len());

        header[..prefix.len()].copy_from_slice(prefix);
        header[prefix.len()..prefix.len() + path_len]
            .copy_from_slice(&path[..path_len]);

        crate::vga::print_at(row, 0, &header);

        let mut line = 1usize;
        let mut entries = 0usize;

        let result = ext4.for_each_directory_entry(
            current_inode,
            |inode, name, file_type| {
                if name == b"." || name == b".." {
                    return true;
                }

                if line >= 40 || row + line >= crate::vga::height() - 1 {
                    crate::vga::print_at(
                        row + line,
                        0,
                        b"More entries...",
                    );
                    line += 1;
                    return false;
                }

                let label: &[u8] = match file_type {
                    2 => b"DIR ",
                    7 => b"LINK",
                    1 => b"FILE",
                    _ => b"????",
                };

                crate::vga::print_at(row + line, 0, label);

                let name_len = name.len().min(76);
                crate::vga::print_at(row + line, 5, &name[..name_len]);

                let _ = inode;

                line += 1;
                entries += 1;
                true
            },
        );

        if result.is_err() {
            crate::vga::print_at(
                row + line,
                0,
                b"ext4 directory read failed",
            );
            line += 1;
        } else if entries == 0 {
            crate::vga::print_at(row + 1, 0, b"Empty directory");
            line = 2;
        }

        line
    }

    fn shell_cat(&self, path: &[u8], row: usize) -> usize {
        use crate::fs::ext4::{AtaDevice, Ext4};

        if path.is_empty() {
            crate::vga::print_at(row, 0, b"Usage: cat <file>");
            return 1;
        }

        let device = AtaDevice;
        let ext4 = match Ext4::mount(
            &device,
            crate::disk::selected_lba(),
            crate::disk::selected_sectors(),
        ) {
            Ok(fs) => fs,
            Err(_) => {
                crate::vga::print_at(row, 0, b"ext4 mount failed");
                return 1;
            }
        };

        let mut current_inode = 2u32;

        for component in path.split(|&byte| byte == b'/') {
            if component.is_empty() || component == b"." {
                continue;
            }

            current_inode = match ext4.find_directory_entry(
                current_inode,
                component,
            ) {
                Ok(Some(inode)) => inode,
                Ok(None) => {
                    crate::vga::print_at(row, 0, b"Path not found");
                    return 1;
                }
                Err(_) => {
                    crate::vga::print_at(row, 0, b"ext4 path lookup failed");
                    return 1;
                }
            };
        }

        let inode = match ext4.read_inode(current_inode) {
            Ok(inode) => inode,
            Err(_) => {
                crate::vga::print_at(row, 0, b"ext4 inode read failed");
                return 1;
            }
        };

        if inode.is_directory {
            crate::vga::print_at(row, 0, b"Is a directory");
            return 1;
        }

        if inode.mode & 0xF000 != 0x8000 {
            crate::vga::print_at(row, 0, b"Not a regular file");
            return 1;
        }

        let mut header = [b' '; 80];
        let prefix = b"File: ";
        let path_len = path.len().min(header.len() - prefix.len() - 4);

        header[..prefix.len()].copy_from_slice(prefix);
        header[prefix.len()..prefix.len() + path_len]
            .copy_from_slice(&path[..path_len]);

        let size_start = prefix.len() + path_len;
        header[size_start] = b' ';
        header[size_start + 1] = b'(';

        let mut digits = [0u8; 20];
        let mut value = inode.size;
        let mut digits_len = 0usize;

        if value == 0 {
            digits[0] = b'0';
            digits_len = 1;
        } else {
            while value > 0 && digits_len < digits.len() {
                digits[digits_len] = b'0' + (value % 10) as u8;
                value /= 10;
                digits_len += 1;
            }
        }

        for index in 0..digits_len {
            header[size_start + 2 + index] =
                digits[digits_len - 1 - index];
        }

        let end = size_start + 2 + digits_len;
        header[end] = b' ';
        header[end + 1] = b'B';
        header[end + 2] = b')';

        crate::vga::print_at(row, 0, &header[..end + 3]);

        if inode.size == 0 {
            crate::vga::print_at(row + 1, 0, b"Empty file");
            return 2;
        }

        let last_row = row + CAT_LINES - 1;
        let mut text_row = row + 1;
        let mut col = 0usize;
        let mut truncated = false;
        let mut decoder = crate::utf8::Decoder::new();
        let mut buffer = [0u8; 512];
        let mut offset = 0u64;

        while offset < inode.size {
            let count = match ext4.read_file(
                current_inode,
                offset,
                &mut buffer,
            ) {
                Ok(count) => count,
                Err(_) => {
                    crate::vga::print_at(
                        text_row.min(last_row),
                        0,
                        b"ext4 file read failed",
                    );
                    return CAT_LINES;
                }
            };

            if count == 0 {
                break;
            }

            for &byte in &buffer[..count] {
                let mut decoded = [0u8; 4];
                let decoded_len = decoder.feed(
                    byte,
                    &mut decoded,
                    crate::fs::exfat::fs::utf16_to_cp866,
                );

                for &code in &decoded[..decoded_len] {
                    if code == b'\r' {
                        continue;
                    }

                    if code == b'\n' {
                        text_row += 1;
                        col = 0;

                        if text_row > last_row {
                            truncated = true;
                            break;
                        }

                        continue;
                    }

                    if col >= 80 {
                        text_row += 1;
                        col = 0;

                        if text_row > last_row {
                            truncated = true;
                            break;
                        }
                    }

                    let display = if code == b'\t' {
                        b' '
                    } else if code >= 0x20 && code != 0x7f {
                        code
                    } else {
                        b'.'
                    };

                    crate::vga::print_at(text_row, col, &[display]);
                    col += 1;
                }

                if truncated {
                    break;
                }
            }

            if truncated {
                break;
            }

            offset += count as u64;
        }

        if truncated {
            return CAT_LINES;
        }

        (text_row - row) + 1
    }

    fn path_is_directory(&self, path: &[u8]) -> Result<bool, DirectoryError> {
        if !matches!(crate::disk::selected_kind(), crate::disk::FsKind::Ext4) {
            return Err(b"selected partition is not ext4");
        }

        let device = crate::fs::ext4::AtaDevice;
        let ext4 = crate::fs::ext4::Ext4::mount(
            &device,
            crate::disk::selected_lba(),
            crate::disk::selected_sectors(),
        )
        .map_err(|_| b"ext4 mount failed" as DirectoryError)?;

        let mut current_inode = 2u32;

        for component in path.split(|&byte| byte == b'/') {
            if component.is_empty() || component == b"." {
                continue;
            }

            current_inode = ext4
                .find_directory_entry(current_inode, component)
                .map_err(|_| b"ext4 directory read failed" as DirectoryError)?
                .ok_or(b"path not found" as DirectoryError)?;
        }

        let inode = ext4
            .read_inode(current_inode)
            .map_err(|_| b"ext4 inode read failed" as DirectoryError)?;

        Ok(inode.is_directory)
    }


    fn shell_write(&self, path: &[u8], text: &[u8], row: usize) -> usize {
        use crate::fs::ext4::{AtaDevice, Ext4, Ext4Error};

        if path.is_empty() {
            crate::vga::print_at(row, 0, b"Usage: write <file> <text>");
            return 1;
        }

        let mut utf8 = [0u8; 768];
        let text_len = crate::fs::exfat::cp866_to_utf8(text, &mut utf8);

        if text_len != text.len() {
            // UTF-8 может занимать больше места, чем CP866.
            // Проверяем, не переполнился ли буфер.
            if text_len == utf8.len() {
                crate::vga::print_at(row, 0, b"Text too long");
                return 1;
            }
        }

        let device = AtaDevice;
        let ext4 = match Ext4::mount_read_write(
            &device,
            crate::disk::selected_lba(),
            crate::disk::selected_sectors(),
        ) {
            Ok(fs) => fs,
            Err(Ext4Error::UnsafeWrite) => {
                crate::vga::print_at(
                    row,
                    0,
                    b"ext4 write disabled: unsafe features",
                );
                return 1;
            }
            Err(Ext4Error::ReadOnly) => {
                crate::vga::print_at(row, 0, b"ext4 is read-only");
                return 1;
            }
            Err(_) => {
                crate::vga::print_at(row, 0, b"ext4 mount failed");
                return 1;
            }
        };

        match ext4.write_file_path(path, &utf8[..text_len]) {
            Ok(_) => {
                crate::vga::print_at(row, 0, b"File written successfully");
                1
            }
            Err(Ext4Error::ReadOnly) => {
                crate::vga::print_at(row, 0, b"ext4 is read-only");
                1
            }
            Err(Ext4Error::NoSpace) => {
                crate::vga::print_at(row, 0, b"ext4: no free space");
                1
            }
            Err(Ext4Error::UnsupportedFeature) => {
                crate::vga::print_at(
                    row,
                    0,
                    b"ext4: operation not supported",
                );
                1
            }
            _ => {
                crate::vga::print_at(row, 0, b"ext4 write failed");
                1
            }
        }
    }

    fn shell_remove(&self, path: &[u8], row: usize) -> usize {
        use crate::fs::ext4::{AtaDevice, Ext4, Ext4Error};

        if path.is_empty() {
            crate::vga::print_at(row, 0, b"Usage: rm <file>");
            return 1;
        }

        let device = AtaDevice;
        let ext4 = match Ext4::mount_read_write(
            &device,
            crate::disk::selected_lba(),
            crate::disk::selected_sectors(),
        ) {
            Ok(fs) => fs,
            Err(Ext4Error::UnsafeWrite) => {
                crate::vga::print_at(
                    row,
                    0,
                    b"ext4 write disabled: unsafe features",
                );
                return 1;
            }
            Err(Ext4Error::ReadOnly) => {
                crate::vga::print_at(row, 0, b"ext4 is read-only");
                return 1;
            }
            Err(_) => {
                crate::vga::print_at(row, 0, b"ext4 mount failed");
                return 1;
            }
        };

        match ext4.remove_file_path(path) {
            Ok(()) => {
                crate::vga::print_at(row, 0, b"File removed successfully");
                1
            }
            Err(Ext4Error::OutOfBounds) => {
                crate::vga::print_at(row, 0, b"Path not found");
                1
            }
            Err(Ext4Error::InvalidGeometry) => {
                crate::vga::print_at(row, 0, b"Not a file");
                1
            }
            Err(error) => print_ext4_error(row, error),
        }
    }

    fn shell_mkdir(&self, path: &[u8], row: usize) -> usize {
        use crate::fs::ext4::{AtaDevice, Ext4, Ext4Error};

        if path.is_empty() {
            crate::vga::print_at(row, 0, b"Usage: mkdir <dir>");
            return 1;
        }

        let device = AtaDevice;
        let ext4 = match Ext4::mount_read_write(
            &device,
            crate::disk::selected_lba(),
            crate::disk::selected_sectors(),
        ) {
            Ok(fs) => fs,
            Err(Ext4Error::UnsafeWrite) => {
                crate::vga::print_at(
                    row,
                    0,
                    b"ext4 write disabled: unsafe features",
                );
                return 1;
            }
            Err(Ext4Error::ReadOnly) => {
                crate::vga::print_at(row, 0, b"ext4 is read-only");
                return 1;
            }
            Err(_) => {
                crate::vga::print_at(row, 0, b"ext4 mount failed");
                return 1;
            }
        };

        match ext4.mkdir_path(path) {
            Ok(_) => {
                crate::vga::print_at(row, 0, b"Directory created successfully");
                1
            }
            Err(Ext4Error::OutOfBounds) => {
                crate::vga::print_at(row, 0, b"Parent path not found");
                1
            }
            Err(Ext4Error::InvalidGeometry) => {
                crate::vga::print_at(row, 0, b"Directory already exists");
                1
            }
            Err(error) => print_ext4_error(row, error),
        }
    }

    fn shell_rmdir(&self, path: &[u8], row: usize) -> usize {
        use crate::fs::ext4::{AtaDevice, Ext4, Ext4Error};

        if path.is_empty() {
            crate::vga::print_at(row, 0, b"Usage: rmdir <dir>");
            return 1;
        }

        let device = AtaDevice;
        let ext4 = match Ext4::mount_read_write(
            &device,
            crate::disk::selected_lba(),
            crate::disk::selected_sectors(),
        ) {
            Ok(fs) => fs,
            Err(Ext4Error::UnsafeWrite) => {
                crate::vga::print_at(
                    row,
                    0,
                    b"ext4 write disabled: unsafe features",
                );
                return 1;
            }
            Err(Ext4Error::ReadOnly) => {
                crate::vga::print_at(row, 0, b"ext4 is read-only");
                return 1;
            }
            Err(_) => {
                crate::vga::print_at(row, 0, b"ext4 mount failed");
                return 1;
            }
        };

        match ext4.remove_directory_path(path) {
            Ok(()) => {
                crate::vga::print_at(row, 0, b"Directory removed successfully");
                1
            }
            Err(Ext4Error::OutOfBounds) => {
                crate::vga::print_at(row, 0, b"Path not found");
                1
            }
            Err(Ext4Error::NotEmpty) => {
                crate::vga::print_at(row, 0, b"Directory not empty");
                1
            }
            Err(Ext4Error::InvalidGeometry) => {
                crate::vga::print_at(row, 0, b"Not a directory");
                1
            }
            Err(error) => print_ext4_error(row, error),
        }
    }

}

static EXFAT_DIRECTORY: ExfatDirectoryFileSystem = ExfatDirectoryFileSystem;
static EXT4_DIRECTORY: Ext4DirectoryFileSystem = Ext4DirectoryFileSystem;

pub fn directory_filesystems() -> [&'static dyn DirectoryFileSystem; 2] {
    [&EXFAT_DIRECTORY, &EXT4_DIRECTORY]
}

/// Адаптер exFAT для общего интерфейса.
pub struct ExfatFileSystem;

impl FileSystem for ExfatFileSystem {
    fn space_info(&self) -> Result<(u64, u64), FsError> {
        exfat::space_info()
    }

    fn print_volume_info(&self, row: usize) -> usize {
        exfat::print_volume_info(row)
    }

    fn print_boot_sector(&self, row: usize) -> usize {
        exfat::print_boot_sector(row)
    }

    fn shell_list(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_list(path, row)
    }

    fn shell_cat(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_cat(path, row)
    }

    fn shell_write(&self, path: &[u8], text: &[u8], row: usize) -> usize {
        exfat::shell_write(path, text, row)
    }

    fn shell_remove(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_remove(path, row)
    }

    fn shell_mkdir(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_mkdir(path, row)
    }

    fn shell_rmdir(&self, path: &[u8], row: usize) -> usize {
        exfat::shell_rmdir(path, row)
    }

    fn shell_listpart(&self, row: usize) -> usize {
        exfat::shell_listpart(row)
    }

    fn shell_setup(&self, index: usize) -> usize {
        exfat::shell_setup(index)
    }

    fn select_partition(
        &self,
        index: usize,
    ) -> Result<PartitionInfo, FsError> {
        exfat::select_partition(index)
    }

    fn path_is_directory(&self, path: &[u8]) -> Result<bool, FsError> {
        exfat::path_is_directory(path)
    }
}

static EXFAT: ExfatFileSystem = ExfatFileSystem;

/// Зарегистрированные файловые системы.
pub fn filesystems() -> [&'static dyn FileSystem; 1] {
    [&EXFAT]
}
