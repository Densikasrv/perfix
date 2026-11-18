//! Общий слой блочных устройств.
//!
//! Собирает в одну таблицу все найденные жёсткие диски: четыре IDE
//! устройства (primary/secondary, master/slave) и диски на SATA-портах
//! AHCI. Номера устройств стабильны в пределах загрузки и используются
//! разделами и файловыми системами.

use crate::ata;
use crate::drivers::sata;

/// Максимум устройств: 4 IDE + 32 порта AHCI.
pub const MAX_DEVICES: usize = 4 + sata::ahci::MAX_PORTS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    Ide { channel: u8, drive: u8 },
    Sata { index: usize },
}

#[derive(Clone, Copy)]
pub struct Device {
    pub bus: Bus,
    pub sectors: u32,
    pub model: [u8; 40],
}

impl Device {
    const EMPTY: Self = Self {
        bus: Bus::Ide { channel: 0, drive: 0 },
        sectors: 0,
        model: [b' '; 40],
    };

    /// Короткое описание для `diskinfo`/`listpart`.
    pub fn bus_name(&self) -> &'static [u8] {
        match self.bus {
            Bus::Ide { channel: 0, drive: 0 } => b"IDE primary master",
            Bus::Ide { channel: 0, drive: 1 } => b"IDE primary slave",
            Bus::Ide { channel: 1, drive: 0 } => b"IDE secondary master",
            Bus::Ide { .. } => b"IDE secondary slave",
            Bus::Sata { .. } => b"SATA",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Timeout,
    NoDevice,
    DeviceError,
    InvalidLba,
}

static mut DEVICES: [Device; MAX_DEVICES] = [Device::EMPTY; MAX_DEVICES];
static mut COUNT: usize = 0;

/// Ищет все IDE- и SATA-диски. Вызывается один раз при старте.
pub fn init() {
    let mut count = 0usize;

    for channel in 0..ata::CHANNELS as u8 {
        for drive in 0..ata::DRIVES as u8 {
            let Some(info) = ata::identify(channel, drive) else {
                continue;
            };

            if count >= MAX_DEVICES {
                break;
            }

            unsafe {
                DEVICES[count] = Device {
                    bus: Bus::Ide { channel, drive },
                    sectors: info.sectors,
                    model: info.model,
                };
                count += 1;
            }
        }
    }

    let sata_count = sata::ahci::init();

    for index in 0..sata_count {
        if count >= MAX_DEVICES {
            break;
        }

        unsafe {
            DEVICES[count] = Device {
                bus: Bus::Sata { index },
                sectors: sata::ahci::sectors(index),
                model: sata::ahci::model(index),
            };
            count += 1;
        }
    }

    unsafe {
        COUNT = count;
    }
}

pub fn count() -> usize {
    unsafe { COUNT }
}

pub fn device(index: usize) -> Option<&'static Device> {
    unsafe {
        if index < COUNT {
            Some(&DEVICES[index])
        } else {
            None
        }
    }
}

fn map_ata_error(error: ata::AtaError) -> Error {
    match error {
        ata::AtaError::Timeout => Error::Timeout,
        ata::AtaError::NoDevice => Error::NoDevice,
        ata::AtaError::DeviceError(_) => Error::DeviceError,
        ata::AtaError::InvalidLba => Error::InvalidLba,
    }
}

fn map_sata_error(error: sata::SataError) -> Error {
    match error {
        sata::SataError::Timeout => Error::Timeout,
        sata::SataError::NoPort => Error::NoDevice,
        sata::SataError::DeviceError => Error::DeviceError,
        sata::SataError::InvalidLba => Error::InvalidLba,
    }
}

/// Читает сектор с устройства `device`.
pub fn read_sector(
    device: usize,
    lba: u32,
    buffer: &mut [u8; 512],
) -> Result<(), Error> {
    match device_of(device)? {
        Bus::Ide { channel, drive } => {
            ata::read_sector(channel, drive, lba, buffer).map_err(map_ata_error)
        }
        Bus::Sata { index } => {
            sata::ahci::read_sector(index, lba, buffer).map_err(map_sata_error)
        }
    }
}

/// Записывает сектор на устройство `device`.
pub fn write_sector(
    device: usize,
    lba: u32,
    buffer: &[u8; 512],
) -> Result<(), Error> {
    match device_of(device)? {
        Bus::Ide { channel, drive } => {
            ata::write_sector(channel, drive, lba, buffer).map_err(map_ata_error)
        }
        Bus::Sata { index } => {
            sata::ahci::write_sector(index, lba, buffer).map_err(map_sata_error)
        }
    }
}

/// Сбрасывает кэш устройства.
pub fn flush(device: usize) -> Result<(), Error> {
    match device_of(device)? {
        Bus::Ide { channel, drive } => {
            ata::flush(channel, drive).map_err(map_ata_error)
        }
        Bus::Sata { index } => {
            sata::ahci::flush(index).map_err(map_sata_error)
        }
    }
}

fn device_of(index: usize) -> Result<Bus, Error> {
    match device(index) {
        Some(device) => Ok(device.bus),
        None => Err(Error::NoDevice),
    }
}
