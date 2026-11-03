pub mod exfat;
pub mod ext4;

pub use exfat::CAT_LINES;

use exfat::{FsError, PartitionInfo};

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
