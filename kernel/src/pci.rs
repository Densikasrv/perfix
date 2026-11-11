//! Обнаружение устройств на шине PCI.
//!
//! Конфигурационное пространство читается через пару портов `0xCF8`/`0xCFC`
//! (механизм #1). Найденные устройства складываются в статическую таблицу:
//! она нужна команде `lspci`, а в дальнейшем — драйверам сетевых карт
//! RTL8139 и e1000, которым потребуются BAR и номер IRQ.
//!
//! Сканируются все шины, но пустые пропускаются по устройству 0, поэтому
//! на типичной машине с одной шиной проход занимает доли миллисекунды.

use core::arch::asm;

use crate::vga;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// Максимум запоминаемых устройств. Двух килобайт хватает с запасом.
const MAX_DEVICES: usize = 64;

const VENDOR_NONE: u16 = 0xFFFF;

/// Описание одного найденного устройства PCI.
#[derive(Clone, Copy)]
pub struct Device {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
    pub irq: u8,
    pub bar: [u32; 6],
}

impl Device {
    const EMPTY: Self = Self {
        bus: 0,
        slot: 0,
        func: 0,
        vendor: 0,
        device: 0,
        class: 0,
        subclass: 0,
        prog_if: 0,
        header_type: 0,
        irq: 0,
        bar: [0; 6],
    };

    /// Полный адрес вида `bus:slot.func`.
    pub fn address(&self) -> (u8, u8, u8) {
        (self.bus, self.slot, self.func)
    }
}

static mut DEVICES: [Device; MAX_DEVICES] = [Device::EMPTY; MAX_DEVICES];
static mut COUNT: usize = 0;

unsafe fn outl(port: u16, value: u32) {
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;

    unsafe {
        asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }

    value
}

fn config_address(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Читает 32 бита конфигурационного пространства.
pub fn read_u32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    unsafe {
        outl(CONFIG_ADDRESS, config_address(bus, slot, func, offset));
        inl(CONFIG_DATA)
    }
}

/// Записывает 32 бита конфигурационного пространства.
pub fn write_u32(bus: u8, slot: u8, func: u8, offset: u8, value: u32) {
    unsafe {
        outl(CONFIG_ADDRESS, config_address(bus, slot, func, offset));
        outl(CONFIG_DATA, value);
    }
}

/// Читает 16 бит конфигурационного пространства.
pub fn read_u16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let shift = (offset & 2) * 8;
    (read_u32(bus, slot, func, offset) >> shift) as u16
}

/// Читает 8 бит конфигурационного пространства.
pub fn read_u8(bus: u8, slot: u8, func: u8, offset: u8) -> u8 {
    let shift = (offset & 3) * 8;
    (read_u32(bus, slot, func, offset) >> shift) as u8
}

fn push(device: Device) {
    unsafe {
        if COUNT < MAX_DEVICES {
            DEVICES[COUNT] = device;
            COUNT += 1;
        }
    }
}

fn probe_function(bus: u8, slot: u8, func: u8) {
    let vendor = read_u16(bus, slot, func, 0);

    if vendor == VENDOR_NONE {
        return;
    }

    let mut bar = [0u32; 6];

    for (index, item) in bar.iter_mut().enumerate() {
        *item = read_u32(bus, slot, func, (0x10 + index * 4) as u8);
    }

    push(Device {
        bus,
        slot,
        func,
        vendor,
        device: read_u16(bus, slot, func, 2),
        class: read_u8(bus, slot, func, 0x0B),
        subclass: read_u8(bus, slot, func, 0x0A),
        prog_if: read_u8(bus, slot, func, 0x09),
        header_type: read_u8(bus, slot, func, 0x0E),
        irq: read_u8(bus, slot, func, 0x3C),
        bar,
    });
}

/// Сканирует шину и заполняет таблицу найденных устройств.
pub fn init() {
    unsafe {
        COUNT = 0;
    }

    for bus in 0..=255u16 {
        let bus = bus as u8;

        // Если на шине нет устройства 0, других устройств обычно тоже нет:
        // не тратим время на 32 слота впустую.
        if bus != 0 && read_u16(bus, 0, 0, 0) == VENDOR_NONE {
            continue;
        }

        for slot in 0..32u8 {
            if read_u16(bus, slot, 0, 0) == VENDOR_NONE {
                continue;
            }

            probe_function(bus, slot, 0);

            // Старший бит header type означает многофункциональное
            // устройство: проверяем остальные функции.
            if read_u8(bus, slot, 0, 0x0E) & 0x80 != 0 {
                for func in 1..8u8 {
                    if read_u16(bus, slot, func, 0) != VENDOR_NONE {
                        probe_function(bus, slot, func);
                    }
                }
            }
        }
    }
}

/// Сколько устройств найдено.
pub fn count() -> usize {
    unsafe { COUNT }
}

/// Устройство по индексу.
pub fn device(index: usize) -> Option<&'static Device> {
    unsafe {
        if index < COUNT {
            Some(&DEVICES[index])
        } else {
            None
        }
    }
}

/// Находит первое устройство с заданными vendor и device.
pub fn find(vendor: u16, device: u16) -> Option<&'static Device> {
    for index in 0..count() {
        if let Some(found) = self::device(index) {
            if found.vendor == vendor && found.device == device {
                return Some(found);
            }
        }
    }

    None
}

/// Человекочитаемое имя класса устройства.
fn class_name(class: u8, subclass: u8) -> &'static str {
    match class {
        0x01 => match subclass {
            0x01 => "IDE",
            0x06 => "SATA",
            0x08 => "NVMe",
            _ => "storage",
        },
        0x02 => "Ethernet",
        0x03 => "Display",
        0x04 => "Multimedia",
        0x06 => "Bridge",
        0x0C => match subclass {
            0x03 => "USB",
            _ => "serial bus",
        },
        _ => "other",
    }
}

/// Известные сетевые карты, которые драйверы 0.2.7 будут поддерживать.
fn device_name(vendor: u16, device: u16) -> &'static str {
    match (vendor, device) {
        (0x8086, 0x100E) | (0x8086, 0x100F) | (0x8086, 0x10D3) | (0x8086, 0x153A) => "e1000",
        (0x10EC, 0x8139) => "RTL8139",
        (0x10EC, 0x8136) => "RTL810x",
        _ => "",
    }
}

fn push_bytes(out: &mut [u8; 80], len: &mut usize, bytes: &[u8]) {
    for &byte in bytes {
        if *len >= out.len() {
            return;
        }

        out[*len] = byte;
        *len += 1;
    }
}

fn push_dec(out: &mut [u8; 80], len: &mut usize, value: usize) {
    let mut digits = [0u8; 20];
    let mut count = 0usize;
    let mut value = value;

    if value == 0 {
        digits[0] = b'0';
        count = 1;
    } else {
        while value > 0 && count < digits.len() {
            digits[count] = b'0' + (value % 10) as u8;
            value /= 10;
            count += 1;
        }
    }

    while count > 0 {
        count -= 1;
        push_bytes(out, len, &[digits[count]]);
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn push_hex(out: &mut [u8; 80], len: &mut usize, value: u32, digits: usize) {
    let mut index = digits;

    while index > 0 {
        index -= 1;
        let nibble = ((value >> (index * 4)) & 0xF) as usize;
        push_bytes(out, len, &[HEX[nibble]]);
    }
}

/// Печатает список устройств начиная со строки `start_row`.
///
/// Возвращает число занятых строк.
pub fn print_list(start_row: usize) -> usize {
    let total = count();
    let mut row = start_row;

    let mut header = [b' '; 80];
    let mut header_len = 0usize;
    push_bytes(&mut header, &mut header_len, b"PCI devices: ");
    push_dec(&mut header, &mut header_len, total);
    vga::print_at(row, 0, &header[..header_len]);
    row += 1;

    if total == 0 {
        vga::print_at(row, 0, b"No PCI devices found");
        return 2;
    }

    for index in 0..total {
        let Some(device) = self::device(index) else {
            break;
        };

        let mut line = [b' '; 80];
        let mut len = 0usize;

        push_hex(&mut line, &mut len, device.bus as u32, 2);
        push_bytes(&mut line, &mut len, b":");
        push_hex(&mut line, &mut len, device.slot as u32, 2);
        push_bytes(&mut line, &mut len, b".");
        push_dec(&mut line, &mut len, device.func as usize);

        push_bytes(&mut line, &mut len, b"  ");
        push_hex(&mut line, &mut len, device.vendor as u32, 4);
        push_bytes(&mut line, &mut len, b":");
        push_hex(&mut line, &mut len, device.device as u32, 4);

        push_bytes(&mut line, &mut len, b"  ");
        push_hex(&mut line, &mut len, device.class as u32, 2);
        push_bytes(&mut line, &mut len, b":");
        push_hex(&mut line, &mut len, device.subclass as u32, 2);

        push_bytes(&mut line, &mut len, b"  IRQ ");
        push_dec(&mut line, &mut len, device.irq as usize);

        let class = class_name(device.class, device.subclass);
        let name = device_name(device.vendor, device.device);

        if !class.is_empty() || !name.is_empty() {
            push_bytes(&mut line, &mut len, b"  ");
            push_bytes(&mut line, &mut len, class.as_bytes());

            if !name.is_empty() {
                push_bytes(&mut line, &mut len, b" ");
                push_bytes(&mut line, &mut len, name.as_bytes());
            }
        }

        vga::print_at(row, 0, &line[..len]);
        row += 1;
    }

    row - start_row
}
