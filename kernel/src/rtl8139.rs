//! Драйвер сетевой карты Realtek RTL8139 (RTL8139D).
//!
//! Карта работает в классическом режиме (не C+): передача идёт через
//! четыре дескриптора, а приём — через кольцевой буфер. Прерывания не
//! используются; вместо них драйвер опрашивает регистры, что хорошо
//! подходит однопоточному ядру. DMA-буферы лежат в `.bss`, которая
//! отображается тождественно, поэтому виртуальный адрес совпадает с
//! физическим.

use crate::pci;
use crate::port::{inb, inl, outb, outl, outw};

const VENDOR_REALTEK: u16 = 0x10EC;
const DEVICE_RTL8139: u16 = 0x8139;
const DEVICE_RTL810X: u16 = 0x8136;

// Регистры.
const IDR0: u16 = 0x00;
const TSD0: u16 = 0x10;
const TSAD0: u16 = 0x20;
const RBSTART: u16 = 0x30;
const CR: u16 = 0x37;
const CAPR: u16 = 0x38;
const IMR: u16 = 0x3C;
const ISR: u16 = 0x3E;
const TCR: u16 = 0x40;
const RCR: u16 = 0x44;
const MSR: u16 = 0x58;

// Command Register.
const CR_RST: u8 = 1 << 4;
const CR_RE: u8 = 1 << 3;
const CR_TE: u8 = 1 << 2;
const CR_BUFE: u8 = 1 << 0;

// Transmit Status Register.
const TSD_TOK: u32 = 1 << 15;
const TSD_TUN: u32 = 1 << 14;

// Interrupt Status Register.
const INT_CLEAR: u16 = 0xFFFF;

// Media Status Register.
const MSR_SPD_10: u8 = 1 << 3;
const MSR_LINK_BAD: u8 = 1 << 2;

/// Кольцо приёма: 8 КиБ полезных данных плюс 16 байт заголовка.
const RX_RING_SIZE: usize = 8 * 1024 + 16;
/// Запас на случай, если карта допишет пакет за границей кольца.
const RX_BUFFER_SIZE: usize = RX_RING_SIZE + 1536;

const TX_BUFFER_SIZE: usize = 2048;

#[repr(align(16))]
struct Aligned<const N: usize>([u8; N]);

static mut RX_BUFFER: Aligned<RX_BUFFER_SIZE> = Aligned([0; RX_BUFFER_SIZE]);
static mut TX_BUFFER: Aligned<TX_BUFFER_SIZE> = Aligned([0; TX_BUFFER_SIZE]);

static mut IO_BASE: u16 = 0;
static mut MAC: [u8; 6] = [0; 6];
static mut RX_OFFSET: usize = 0;
static mut READY: bool = false;
/// Номер следующего дескриптора передачи: карта обслуживает их
/// по кругу, поэтому драйвер тоже обязан чередовать TSD0..TSD3.
static mut TX_DESC: usize = 0;

static mut RX_OK: u32 = 0;
static mut RX_BAD: u32 = 0;
static mut TX_OK: u32 = 0;
static mut TX_FAIL: u32 = 0;

fn base() -> u16 {
    unsafe { IO_BASE }
}

fn ready() -> bool {
    unsafe { READY }
}

/// MAC-адрес, прочитанный из карты.
pub fn mac() -> [u8; 6] {
    unsafe { MAC }
}

/// Счётчики принятых/переданных кадров и ошибок.
pub fn stats() -> (u32, u32, u32, u32) {
    unsafe { (RX_OK, RX_BAD, TX_OK, TX_FAIL) }
}

/// Есть ли линк.
pub fn link_up() -> bool {
    if !ready() {
        return false;
    }

    inb(base() + MSR) & MSR_LINK_BAD == 0
}

/// Работает ли линк на 10 Мбит/с (иначе 100).
pub fn link_is_10mbit() -> bool {
    inb(base() + MSR) & MSR_SPD_10 != 0
}

fn delay() {
    for _ in 0..200_000 {
        core::hint::spin_loop();
    }
}

fn reset() -> bool {
    outb(base() + CR, CR_RST);

    let mut timeout = 1_000_000u32;

    while inb(base() + CR) & CR_RST != 0 {
        if timeout == 0 {
            return false;
        }

        timeout -= 1;
        core::hint::spin_loop();
    }

    // После сброса карте нужно время на автозагрузку EEPROM.
    delay();
    true
}

/// Инициализирует первую найденную RTL8139/RTL810x.
pub fn init() -> bool {
    let device = pci::find(VENDOR_REALTEK, DEVICE_RTL8139)
        .or_else(|| pci::find(VENDOR_REALTEK, DEVICE_RTL810X));

    let Some(device) = device else {
        return false;
    };

    // Включаем доступ к портам ввода-вывода и bus mastering.
    let command = pci::read_u16(device.bus, device.slot, device.func, 0x04);
    pci::write_u16(
        device.bus,
        device.slot,
        device.func,
        0x04,
        command | 0x0005,
    );

    let bar = device.bar[0];

    // Драйвер работает через порты; BAR0 у RTL8139 — I/O BAR.
    if bar & 1 == 0 {
        return false;
    }

    let io = (bar & 0xFFFC) as u16;

    if io == 0 {
        return false;
    }

    unsafe {
        IO_BASE = io;
        RX_OFFSET = 0;
        READY = false;
    }

    if !reset() {
        return false;
    }

    outw(base() + ISR, INT_CLEAR);
    outw(base() + IMR, 0);

    let mut mac = [0u8; 6];

    for (index, byte) in mac.iter_mut().enumerate() {
        *byte = inb(base() + IDR0 + index as u16);
    }

    // Некоторые карты сообщают нулевой MAC при выключенном autoload —
    // в этом случае драйвер не сможет работать корректно.
    if mac == [0; 6] {
        return false;
    }

    // Буфер выровнен благодаря repr(align(16)), поэтому виртуальный
    // адрес совпадает с физическим и годится для RBSTART.
    let rx_address =
        unsafe { core::ptr::addr_of!(RX_BUFFER.0) as *const u8 as u32 };

    // 8 КиБ кольцо, порог FIFO — целый пакет, разрешаем broadcast,
    // multicast и адресованные нам кадры, WRAP для перехода через край.
    let rcr = (7 << 13) | (7 << 8) | (1 << 7) | (1 << 3) | (1 << 2) | (1 << 1);
    // CRC добавляется картой; межкадровый интервал и DMA по умолчанию.
    let tcr = (3 << 24) | (1 << 16) | (7 << 8);

    outl(base() + RBSTART, rx_address);
    outw(base() + CAPR, 0);
    outl(base() + RCR, rcr);
    outl(base() + TCR, tcr);
    outb(base() + CR, CR_RE | CR_TE);
    outw(base() + ISR, INT_CLEAR);

    unsafe {
        MAC = mac;
        RX_OFFSET = 0;
        TX_DESC = 0;
        READY = true;
    }

    true
}

fn update_capr(offset: usize) {
    // CAPR хранит позицию чтения минус 16 байт.
    outw(base() + CAPR, (offset as u16).wrapping_sub(16));
}

fn ring_byte(offset: usize) -> u8 {
    unsafe { RX_BUFFER.0[offset % RX_RING_SIZE] }
}

fn ring_u16(offset: usize) -> u16 {
    ring_byte(offset) as u16 | ((ring_byte(offset + 1) as u16) << 8)
}

/// Принимает один кадр. Возвращает его длину без CRC.
pub fn recv(out: &mut [u8]) -> Option<usize> {
    if !ready() {
        return None;
    }

    // За один вызов продвигаемся не более чем на несколько пакетов,
    // чтобы не застрять в цикле при повреждённом заголовке.
    for _ in 0..8 {
        if inb(base() + CR) & CR_BUFE != 0 {
            return None;
        }

        let offset = unsafe { RX_OFFSET };
        let status = ring_u16(offset);
        let length = ring_u16(offset + 2) as usize;

        if length == 0 || length > RX_RING_SIZE {
            // Синхронизация потеряна — начинаем кольцо заново.
            unsafe {
                RX_OFFSET = 0;
            }
            update_capr(0);
            return None;
        }

        let advance = (length + 4 + 3) & !3;
        let mut next = offset + advance;

        while next >= RX_RING_SIZE {
            next -= RX_RING_SIZE;
        }

        unsafe {
            RX_OFFSET = next;
        }
        update_capr(next);

        // Бит ROK: пакет принят без ошибок.
        if status & 1 == 0 {
            unsafe {
                RX_BAD += 1;
            }
            continue;
        }

        // В length входит 4-байтовый CRC.
        let payload = length.saturating_sub(4);
        let copy = payload.min(out.len());

        for (index, byte) in out.iter_mut().take(copy).enumerate() {
            *byte = ring_byte(offset + 4 + index);
        }

        unsafe {
            RX_OK += 1;
        }

        return Some(copy);
    }

    None
}

/// Передаёт Ethernet-кадр. Карта сама добавляет CRC.
pub fn send(frame: &[u8]) -> bool {
    if !ready() || frame.len() > TX_BUFFER_SIZE {
        return false;
    }

    let mut length = frame.len();

    // Минимальный размер кадра Ethernet без CRC — 60 байт.
    if length < 60 {
        length = 60;
    }

    let address = unsafe {
        let buffer = &mut *core::ptr::addr_of_mut!(TX_BUFFER.0);
        buffer[..frame.len()].copy_from_slice(frame);

        for byte in buffer.iter_mut().take(length).skip(frame.len()) {
            *byte = 0;
        }

        buffer.as_ptr() as u32
    };

    let descriptor = unsafe { TX_DESC };
    let tsd = TSD0 + (descriptor as u16) * 4;
    let tsad = TSAD0 + (descriptor as u16) * 4;

    outl(base() + tsad, address);
    // Запись размера запускает передачу и сбрасывает бит OWN.
    outl(base() + tsd, length as u32);

    // Следующий кадр пойдёт через соседний дескриптор.
    unsafe {
        TX_DESC = (descriptor + 1) % 4;
    }

    let mut timeout = 20_000_000u32;

    loop {
        let status = inl(base() + tsd);

        if status & TSD_TOK != 0 {
            unsafe {
                TX_OK += 1;
            }
            return true;
        }

        if status & TSD_TUN != 0 || timeout == 0 {
            unsafe {
                TX_FAIL += 1;
            }
            return false;
        }

        timeout -= 1;
        core::hint::spin_loop();
    }
}
