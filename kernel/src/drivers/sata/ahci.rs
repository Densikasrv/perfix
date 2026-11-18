//! Драйвер AHCI (SATA) без прерываний.
//!
//! Контроллер ищется на шине PCI, регистры управления отображаются через
//! BAR5. Для каждого порта с подключённым ATA-диском выделяются командный
//! список, область FIS и командная таблица в `.bss`; обмен идёт через
//! PRDT-таблицу на один сектор.

use core::sync::atomic::{compiler_fence, Ordering};

use crate::{paging, pci};

const AHCI_CLASS: u8 = 0x01;
const AHCI_SUBCLASS: u8 = 0x06;
const AHCI_PROG_IF: u8 = 0x01;

const HBA_CAP: u32 = 0x00;
const HBA_GHC: u32 = 0x04;
const HBA_PI: u32 = 0x0C;

const PORT_BASE: u32 = 0x100;
const PORT_STRIDE: u32 = 0x80;

const PX_CLB: u32 = 0x00;
const PX_CLBU: u32 = 0x04;
const PX_FB: u32 = 0x08;
const PX_FBU: u32 = 0x0C;
const PX_IS: u32 = 0x10;
const PX_IE: u32 = 0x14;
const PX_CMD: u32 = 0x18;
const PX_TFD: u32 = 0x20;
const PX_SIG: u32 = 0x24;
const PX_SSTS: u32 = 0x28;
const PX_SERR: u32 = 0x30;
const PX_CI: u32 = 0x38;

/// Команды всегда выполняются через командный слот 0: драйвер не
/// использует очередь, а `PxCI` — битовая карта занятых слотов порта,
/// поэтому её значение не зависит от номера порта.
const CMD_SLOT: u32 = 1;

const GHC_AE: u32 = 0x8000_0000;

const CMD_ST: u32 = 0x0000_0001;
const CMD_FRE: u32 = 0x0000_0010;
const CMD_CR: u32 = 0x0000_8000;

const TFD_ERR: u32 = 0x0000_0001;
const TFD_DF: u32 = 0x0000_0020;

const IS_TFES: u32 = 1 << 30;

const SIGNATURE_ATA: u32 = 0x0000_0101;

/// Сколько портов контроллера поддерживаем. В `.bss` на каждый порт
/// приходится 1.5 KiB служебных структур.
pub const MAX_PORTS: usize = 32;

const TIMEOUT: usize = 5_000_000;

#[derive(Debug, Clone, Copy)]
pub enum SataError {
    NoPort,
    Timeout,
    DeviceError,
    InvalidLba,
}

#[repr(C, align(1024))]
#[derive(Clone, Copy)]
struct CommandList {
    bytes: [u8; 1024],
}

#[repr(C, align(256))]
#[derive(Clone, Copy)]
struct FisArea {
    bytes: [u8; 256],
}

#[repr(C, align(128))]
#[derive(Clone, Copy)]
struct CommandTable {
    bytes: [u8; 256],
}

static mut COMMAND_LISTS: [CommandList; MAX_PORTS] =
    [CommandList { bytes: [0; 1024] }; MAX_PORTS];
static mut FIS_AREAS: [FisArea; MAX_PORTS] =
    [FisArea { bytes: [0; 256] }; MAX_PORTS];
static mut COMMAND_TABLES: [CommandTable; MAX_PORTS] =
    [CommandTable { bytes: [0; 256] }; MAX_PORTS];

static mut ABAR: u32 = 0;
static mut FOUND: bool = false;
static mut IMPLEMENTED: u32 = 0;
static mut PORT_PRESENT: [bool; MAX_PORTS] = [false; MAX_PORTS];
static mut PORT_SECTORS: [u32; MAX_PORTS] = [0; MAX_PORTS];
static mut PORT_MODEL: [[u8; 40]; MAX_PORTS] = [[b' '; 40]; MAX_PORTS];
static mut PORT_COUNT: usize = 0;
static mut PORT_INDEX: [u8; MAX_PORTS] = [0; MAX_PORTS];

unsafe fn read32(address: u32) -> u32 {
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

unsafe fn write32(address: u32, value: u32) {
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn port_address(port: u8) -> u32 {
    unsafe { ABAR + PORT_BASE + port as u32 * PORT_STRIDE }
}

/// Разбирает 512 байт ответа IDENTIFY DEVICE.
fn parse_identify(data: &[u8; 512]) -> Option<(u32, [u8; 40])> {
    let word = |index: usize| -> u16 {
        u16::from_le_bytes([data[index * 2], data[index * 2 + 1]])
    };

    if word(0) == 0 || word(0) == 0xFFFF || word(49) & 0x0200 == 0 {
        return None;
    }

    let sectors = (word(60) as u32) | ((word(61) as u32) << 16);

    let mut model = [b' '; 40];

    for index in 27..47 {
        let value = word(index);
        let offset = (index - 27) * 2;
        model[offset] = (value >> 8) as u8;
        model[offset + 1] = value as u8;
    }

    let mut length = model.len();

    while length > 0 && (model[length - 1] == b' ' || model[length - 1] == 0) {
        length -= 1;
    }

    for byte in model[length..].iter_mut() {
        *byte = b' ';
    }

    Some((sectors, model))
}

/// Ждёт, пока порт не выполняет ни одной команды.
///
/// CR и FR остаются установленными всё время, пока порт активен, поэтому
/// готовность определяется только пустым PxCI.
unsafe fn wait_ready(port: u8) -> Result<(), SataError> {
    let base = port_address(port);

    for _ in 0..TIMEOUT {
        let active = unsafe { read32(base + PX_CI) };

        if active == 0 {
            return Ok(());
        }
    }

    Err(SataError::Timeout)
}

/// Инициализирует командные структуры порта.
unsafe fn start_port(port: u8) -> Result<(), SataError> {
    let base = port_address(port);

    unsafe {
        let mut command = read32(base + PX_CMD);

        if command & CMD_ST != 0 {
            command &= !CMD_ST;
            write32(base + PX_CMD, command);
        }

        // Дожидаемся остановки канала, иначе список команд ещё используется.
        for _ in 0..TIMEOUT {
            command = read32(base + PX_CMD);

            if command & CMD_CR == 0 {
                break;
            }
        }

        let list = core::ptr::addr_of_mut!(COMMAND_LISTS[port as usize]) as u32;
        let fis = core::ptr::addr_of_mut!(FIS_AREAS[port as usize]) as u32;

        write32(base + PX_CLB, list);
        write32(base + PX_CLBU, 0);
        write32(base + PX_FB, fis);
        write32(base + PX_FBU, 0);

        write32(base + PX_IS, 0xFFFF_FFFF);
        write32(base + PX_SERR, 0xFFFF_FFFF);
        write32(base + PX_IE, 0);

        command = read32(base + PX_CMD);
        command |= CMD_FRE;
        command &= !CMD_ST;
        write32(base + PX_CMD, command);

        command |= CMD_ST;
        write32(base + PX_CMD, command);
    }

    unsafe { wait_ready(port) }
}

/// Выполняет одну ATA-команду через PRDT.
///
/// `buffer` — физический (тождественно отображённый) адрес буфера,
/// `bytes` — размер передачи, `write` — направление.
unsafe fn issue(
    port: u8,
    command: u8,
    lba: u32,
    count: u16,
    buffer: u32,
    bytes: u32,
    write: bool,
) -> Result<(), SataError> {
    if port as usize >= MAX_PORTS {
        return Err(SataError::NoPort);
    }

    let base = port_address(port);

    unsafe {
        wait_ready(port)?;

        let list = core::ptr::addr_of_mut!(COMMAND_LISTS[port as usize])
            .cast::<u8>();
        let table = core::ptr::addr_of_mut!(COMMAND_TABLES[port as usize])
            .cast::<u8>();

        // Обнуляем заголовок команды и командную таблицу.
        for index in 0..32 {
            list.add(index).write_volatile(0);
        }

        for index in 0..256 {
            table.add(index).write_volatile(0);
        }

        // Command FIS (Register H2D): 20 байт.
        let fis = table;
        fis.add(0).write_volatile(0x27);
        fis.add(1).write_volatile(0x80);
        fis.add(2).write_volatile(command);
        fis.add(3).write_volatile(0);
        fis.add(4).write_volatile(lba as u8);
        fis.add(5).write_volatile((lba >> 8) as u8);
        fis.add(6).write_volatile((lba >> 16) as u8);
        fis.add(7).write_volatile(0x40);
        fis.add(8).write_volatile(0);
        fis.add(9).write_volatile(0);
        fis.add(10).write_volatile(0);
        fis.add(11).write_volatile(0);
        fis.add(12).write_volatile(count as u8);
        fis.add(13).write_volatile((count >> 8) as u8);
        fis.add(14).write_volatile(0);
        fis.add(15).write_volatile(0);

        let prdt_count = if bytes == 0 { 0u32 } else { 1u32 };

        if prdt_count != 0 {
            let prdt = table.add(0x80);

            // Data base address.
            for index in 0..4 {
                prdt.add(index)
                    .write_volatile((buffer >> (index * 8)) as u8);
            }

            let byte_count = bytes.min(4 * 1024 * 1024) - 1;
            // DBC находится по смещению 0x0C: 22 бита длины, бит 31 — I.
            prdt.add(12).write_volatile(byte_count as u8);
            prdt.add(13).write_volatile((byte_count >> 8) as u8);
            prdt.add(14).write_volatile((byte_count >> 16) as u8);
            prdt.add(15).write_volatile(0); // без прерывания
        }

        let table_address = table as u32;
        let write_flag = if write { 1u32 << 6 } else { 0 };
        let prdt_bits = prdt_count << 16;
        let header = 5 | write_flag | prdt_bits;

        list.add(0).write_volatile(header as u8);
        list.add(1).write_volatile((header >> 8) as u8);
        list.add(2).write_volatile((header >> 16) as u8);
        list.add(3).write_volatile((header >> 24) as u8);

        for index in 0..4 {
            list.add(4 + index)
                .write_volatile((bytes >> (index * 8)) as u8);
        }

        for index in 0..4 {
            list.add(8 + index)
                .write_volatile((table_address >> (index * 8)) as u8);
        }

        compiler_fence(Ordering::SeqCst);

        write32(base + PX_IS, 0xFFFF_FFFF);
        write32(base + PX_SERR, 0xFFFF_FFFF);

        compiler_fence(Ordering::SeqCst);
        write32(base + PX_CI, CMD_SLOT);

        let mut completed = false;

        for _ in 0..TIMEOUT {
            let active = read32(base + PX_CI);

            if active & CMD_SLOT == 0 {
                completed = true;
                break;
            }
        }

        if !completed {
            return Err(SataError::Timeout);
        }

        let interrupt = read32(base + PX_IS);
        let task_file = read32(base + PX_TFD);

        if interrupt & IS_TFES != 0
            || task_file & (TFD_ERR | TFD_DF) != 0
        {
            return Err(SataError::DeviceError);
        }
    }

    Ok(())
}

fn identify(port: u8) -> Option<(u32, [u8; 40])> {
    let mut data = [0u8; 512];

    let result = unsafe {
        issue(
            port,
            0xEC,
            0,
            0,
            data.as_mut_ptr() as u32,
            data.len() as u32,
            false,
        )
    };

    if result.is_err() {
        return None;
    }

    parse_identify(&data)
}

/// Находит AHCI-контроллер и перечисляет подключённые ATA-диски.
pub fn init() -> usize {
    unsafe {
        PORT_COUNT = 0;
        ABAR = 0;
        FOUND = false;
        IMPLEMENTED = 0;

        for index in 0..MAX_PORTS {
            PORT_PRESENT[index] = false;
            PORT_SECTORS[index] = 0;
        }
    }

    let mut found = None;

    for index in 0..pci::count() {
        let Some(device) = pci::device(index) else {
            break;
        };

        if device.class == AHCI_CLASS
            && device.subclass == AHCI_SUBCLASS
            && device.prog_if == AHCI_PROG_IF
        {
            found = Some(device);
            break;
        }
    }

    let Some(device) = found else {
        return 0;
    };

    unsafe {
        FOUND = true;
    }

    let bar = device.bar[5];

    if bar & 1 != 0 || bar == 0 {
        return 0;
    }

    let abar = bar & !0xF;

    if abar == 0 || !paging::map_identity_large(abar) {
        return 0;
    }

    // Включаем доступ к MMIO и bus mastering.
    let command = pci::read_u16(device.bus, device.slot, device.func, 0x04);
    pci::write_u16(
        device.bus,
        device.slot,
        device.func,
        0x04,
        command | 0x0006,
    );

    unsafe {
        ABAR = abar;

        let cap = read32(abar + HBA_CAP);
        let _ = cap;
        let ghc = read32(abar + HBA_GHC);
        write32(abar + HBA_GHC, ghc | GHC_AE);

        let implemented = read32(abar + HBA_PI);
        IMPLEMENTED = implemented;

        for port in 0..MAX_PORTS {
            if implemented & (1 << port) == 0 {
                continue;
            }

            let base = port_address(port as u8);
            let ssts = read32(base + PX_SSTS);

            // DET = 3 (устройство подключено), IPM = 1 (активно).
            if ssts & 0x0F != 3 || (ssts >> 8) & 0x0F != 1 {
                continue;
            }

            if read32(base + PX_SIG) != SIGNATURE_ATA {
                continue;
            }

            if start_port(port as u8).is_err() {
                continue;
            }

            let info = identify(port as u8);
            let count = PORT_COUNT;

            PORT_PRESENT[port] = true;
            PORT_INDEX[count] = port as u8;
            PORT_COUNT += 1;

            if let Some((sectors, model)) = info {
                PORT_SECTORS[port] = sectors;
                PORT_MODEL[port] = model;
            }

            // Отключаем прерывания: драйвер полностью опросный.
            write32(base + PX_IE, 0);
            write32(base + PX_IS, 0xFFFF_FFFF);
        }
    }

    unsafe { PORT_COUNT }
}

/// Сколько ATA-дисков найдено на SATA-контроллере.
pub fn count() -> usize {
    unsafe { PORT_COUNT }
}

/// Номер порта AHCI для записи `index`.
pub fn port_number(index: usize) -> Option<u8> {
    unsafe {
        if index < PORT_COUNT {
            Some(PORT_INDEX[index])
        } else {
            None
        }
    }
}

pub fn sectors(index: usize) -> u32 {
    match port_number(index) {
        Some(port) => unsafe { PORT_SECTORS[port as usize] },
        None => 0,
    }
}

pub fn model(index: usize) -> [u8; 40] {
    match port_number(index) {
        Some(port) => unsafe { PORT_MODEL[port as usize] },
        None => [b' '; 40],
    }
}

pub fn read_sector(
    index: usize,
    lba: u32,
    buffer: &mut [u8; 512],
) -> Result<(), SataError> {
    if lba > 0x0FFF_FFFF {
        return Err(SataError::InvalidLba);
    }

    let port = port_number(index).ok_or(SataError::NoPort)?;

    unsafe {
        issue(
            port,
            0x25, // READ DMA EXT
            lba,
            1,
            buffer.as_mut_ptr() as u32,
            buffer.len() as u32,
            false,
        )
    }
}

pub fn write_sector(
    index: usize,
    lba: u32,
    buffer: &[u8; 512],
) -> Result<(), SataError> {
    if lba > 0x0FFF_FFFF {
        return Err(SataError::InvalidLba);
    }

    let port = port_number(index).ok_or(SataError::NoPort)?;

    unsafe {
        issue(
            port,
            0x35, // WRITE DMA EXT
            lba,
            1,
            buffer.as_ptr() as u32,
            buffer.len() as u32,
            true,
        )
    }
}

pub fn flush(index: usize) -> Result<(), SataError> {
    let port = port_number(index).ok_or(SataError::NoPort)?;

    unsafe { issue(port, 0xEA, 0, 0, 0, 0, false) }
}

/// Найден ли AHCI-контроллер на шине PCI.
pub fn controller_found() -> bool {
    unsafe { FOUND }
}

/// Базовый адрес регистров (BAR5) или 0.
pub fn abar() -> u32 {
    unsafe { ABAR }
}

/// Битовая карта реализованных портов (CAP/PI).
pub fn implemented_ports() -> u32 {
    unsafe { IMPLEMENTED }
}

/// Состояние порта: (SSTS, SIG). По ним видно, подключён ли диск
/// и какое у него устройство (ATA или ATAPI).
pub fn port_state(port: u8) -> Option<(u32, u32)> {
    if port as usize >= MAX_PORTS {
        return None;
    }

    if unsafe { ABAR } == 0 {
        return None;
    }

    let base = port_address(port);

    unsafe {
        Some((read32(base + PX_SSTS), read32(base + PX_SIG)))
    }
}
