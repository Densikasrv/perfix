use crate::ata;
use super::fs::{BlockDevice, Ext4Error, SECTOR_SIZE};

/// Адаптер ATA primary master для ext4.
pub struct AtaDevice;

impl BlockDevice for AtaDevice {
    fn read_sector(
        &self,
        lba: u32,
        buffer: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), Ext4Error> {
        ata::read_sector(lba, buffer)
            .map_err(|_| Ext4Error::Io)
    }
}