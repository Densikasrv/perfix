use super::fs::{BlockDevice, Ext4Error, SECTOR_SIZE};

/// Адаптер выбранного блочного устройства (IDE или SATA) для ext4.
#[derive(Clone, Copy)]
pub struct AtaDevice {
    pub device: usize,
}

impl AtaDevice {
    pub const fn new(device: usize) -> Self {
        Self { device }
    }

    /// Устройство текущего выбранного раздела.
    pub fn selected() -> Self {
        Self {
            device: crate::disk::selected_device(),
        }
    }
}

impl BlockDevice for AtaDevice {
    fn read_sector(
        &self,
        lba: u32,
        buffer: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), Ext4Error> {
        crate::block::read_sector(self.device, lba, buffer)
            .map_err(|_| Ext4Error::Io)
    }

    fn write_sector(
        &self,
        lba: u32,
        buffer: &[u8; SECTOR_SIZE],
    ) -> Result<(), Ext4Error> {
        crate::block::write_sector(self.device, lba, buffer)
            .map_err(|_| Ext4Error::Io)
    }
}
