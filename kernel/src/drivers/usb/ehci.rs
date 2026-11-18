//! Драйвер контроллера EHCI (USB 2.0).
//!
//! EHCI отвечает за высокоскоростные устройства (high speed, 480 Мбит/с).
//! Устройства на полной и низкой скорости контроллер EHCI не обслуживает:
//! они передаются companion-контроллерам UHCI/OHCI. Драйвер выставляет
//! Port Owner для таких портов, а дальше с ними работает `uhci.rs`.
//!
//! Как и остальные драйверы Perfix, EHCI работает без прерываний.
//! Управляющие передачи идут через asynchronous schedule, а HID-отчёты
//! читаются через periodic schedule. Всё расписание и дескрипторы лежат
//! в `.bss`; страницы ядра отображены тождественно, поэтому адрес
//! структуры совпадает с физическим адресом, который нужен контроллеру.

use core::sync::atomic::{fence, Ordering};

use crate::{paging, pci, timer};

use super::device::{self, BulkEndpoint, Identity, Kind};
use super::hid;

const EHCI_CLASS: u8 = 0x0c;
const EHCI_SUBCLASS: u8 = 0x03;
const EHCI_PROG_IF: u8 = 0x20;

// Capability registers (от BAR0).
const CAPLENGTH: u32 = 0x00;
const HCSPARAMS: u32 = 0x04;
const HCCPARAMS: u32 = 0x08;

// Operational registers (от BAR0 + CAPLENGTH).
const USBCMD: u32 = 0x00;
const USBSTS: u32 = 0x04;
const USBINTR: u32 = 0x08;
const FRINDEX: u32 = 0x0c;
const CTRLDSSEGMENT: u32 = 0x10;
const PERIODICLISTBASE: u32 = 0x14;
const ASYNCLISTADDR: u32 = 0x18;
const CONFIGFLAG: u32 = 0x40;
const PORTSC: u32 = 0x44;

const CMD_RS: u32 = 1 << 0;
const CMD_HCRESET: u32 = 1 << 1;
const CMD_PSE: u32 = 1 << 4;
const CMD_ASE: u32 = 1 << 5;

const STS_INT: u32 = 1 << 0;
const STS_ERRINT: u32 = 1 << 1;
const STS_PCD: u32 = 1 << 2;
const STS_HALT: u32 = 1 << 12;

const PORT_CCS: u32 = 1 << 0;
const PORT_CSC: u32 = 1 << 1;
const PORT_PED: u32 = 1 << 2;
const PORT_PEDC: u32 = 1 << 3;
const PORT_OCC: u32 = 1 << 5;
const PORT_PR: u32 = 1 << 8;
const PORT_PP: u32 = 1 << 12;
const PORT_POWNER: u32 = 1 << 13;
const PORT_SPEED_SHIFT: u32 = 26;
const PORT_SPEED_MASK: u32 = 0x3 << PORT_SPEED_SHIFT;
const PORT_SPEED_HIGH: u32 = 2;
const PORT_CHANGE_BITS: u32 = PORT_CSC | PORT_PEDC | PORT_OCC;

/// Указатель-терминатор EHCI.
const TERMINATE: u32 = 1;
/// Тег очереди в horizontal link pointer.
const Q_TYPE_QH: u32 = 2;

// qTD token.
const QTD_ACTIVE: u32 = 1 << 7;
const QTD_HALT: u32 = 1 << 6;
const QTD_DBE: u32 = 1 << 5;
const QTD_BABBLE: u32 = 1 << 4;
const QTD_XACT: u32 = 1 << 3;
const QTD_TOGGLE: u32 = 1 << 31;
const QTD_PID_OUT: u32 = 0;
const QTD_PID_IN: u32 = 1;
const QTD_PID_SETUP: u32 = 2;
const QTD_ERRORS: u32 = QTD_HALT | QTD_DBE | QTD_BABBLE | QTD_XACT;
const QTD_LENGTH_SHIFT: u32 = 16;
const QTD_CERR: u32 = 3 << 10;

/// Сколько портов EHCI поддерживаем. В QEMU и типичных чипсетах не больше
/// шести, но статические массивы дешёвые.
const MAX_PORTS: usize = 6;
/// Управляющая передача — setup, данные, status.
const CONTROL_QTD_COUNT: usize = 3;
/// Список кадров periodic schedule.
const FRAME_COUNT: usize = 1024;

#[repr(C, align(32))]
#[derive(Clone, Copy)]
struct Qh {
    next: u32,
    epchar: u32,
    epcap: u32,
    current: u32,
    next_qtd: u32,
    alt_next: u32,
    token: u32,
    buffer: [u32; 5],
    /// Старшие 32 бита адресов буферов (Appendix B EHCI).
    /// Контроллеры без 64-битной адресации их не читают, а с ней —
    /// требуют нули.
    buffer_high: [u32; 5],
}

impl Qh {
    const EMPTY: Self = Self {
        next: TERMINATE,
        epchar: 0,
        epcap: 0,
        current: 0,
        next_qtd: TERMINATE,
        alt_next: TERMINATE,
        token: 0,
        buffer: [0; 5],
        buffer_high: [0; 5],
    };
}

#[repr(C, align(32))]
#[derive(Clone, Copy)]
struct Qtd {
    next: u32,
    alt_next: u32,
    token: u32,
    buffer: [u32; 5],
    buffer_high: [u32; 5],
}

impl Qtd {
    const EMPTY: Self = Self {
        next: TERMINATE,
        alt_next: TERMINATE,
        token: 0,
        buffer: [0; 5],
        buffer_high: [0; 5],
    };
}

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct FrameList {
    entries: [u32; FRAME_COUNT],
}

static mut FRAME_LIST: FrameList = FrameList {
    entries: [TERMINATE; FRAME_COUNT],
};

static mut ASYNC_HEAD: Qh = Qh::EMPTY;
static mut CONTROL_QH: Qh = Qh::EMPTY;
static mut CONTROL_QTD: [Qtd; CONTROL_QTD_COUNT] =
    [Qtd::EMPTY; CONTROL_QTD_COUNT];

#[repr(C, align(64))]
struct SetupPacket {
    bytes: [u8; 8],
}

static mut SETUP_PACKET: SetupPacket = SetupPacket { bytes: [0; 8] };

static mut DEVICE_QH: [Qh; MAX_PORTS] = [Qh::EMPTY; MAX_PORTS];
static mut DEVICE_QTD: [Qtd; MAX_PORTS] = [Qtd::EMPTY; MAX_PORTS];
static mut DEVICE_BUFFER: [[u8; 8]; MAX_PORTS] = [[0; 8]; MAX_PORTS];

#[derive(Clone, Copy)]
struct Device {
    present: bool,
    kind: Kind,
    address: u8,
    endpoint: u8,
    max_packet: u16,
    toggle: bool,
    pending_rearm: bool,
    previous_modifiers: u8,
    previous_keys: [u8; 6],
    /// Идентификаторы устройства (VID/PID).
    identity: Identity,
    /// Bulk-эндпоинты vendor-specific устройства (например, Wi-Fi).
    bulk_in: Option<BulkEndpoint>,
    bulk_out: Option<BulkEndpoint>,
    /// Состояние DATA toggle для будущих bulk-передач.
    bulk_in_toggle: bool,
    bulk_out_toggle: bool,
    retry_at: u64,
}

impl Device {
    const EMPTY: Self = Self {
        present: false,
        kind: Kind::None,
        address: 0,
        endpoint: 0,
        max_packet: 8,
        toggle: false,
        pending_rearm: false,
        previous_modifiers: 0,
        previous_keys: [0; 6],
        identity: Identity::EMPTY,
        bulk_in: None,
        bulk_out: None,
        bulk_in_toggle: false,
        bulk_out_toggle: false,
        retry_at: 0,
    };
}

static mut DEVICES: [Device; MAX_PORTS] = [Device::EMPTY; MAX_PORTS];

static mut MMIO_BASE: u32 = 0;
static mut OP_BASE: u32 = 0;
static mut PORTS: usize = 0;
static mut READY: bool = false;
static mut NEXT_SCAN: u64 = 0;
static mut FOUND: bool = false;

unsafe fn read32(address: u32) -> u32 {
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

unsafe fn write32(address: u32, value: u32) {
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn op(offset: u32) -> u32 {
    unsafe { OP_BASE + offset }
}

unsafe fn read_op(offset: u32) -> u32 {
    unsafe { read32(op(offset)) }
}

unsafe fn write_op(offset: u32, value: u32) {
    unsafe { write32(op(offset), value) };
}

fn ready() -> bool {
    unsafe { READY }
}

/// Был ли найден контроллер EHCI (даже если сейчас не готов).
pub fn found() -> bool {
    unsafe { FOUND }
}

fn qh_address(index: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(DEVICE_QH[index]) as u32 }
}

fn qtd_address(index: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(DEVICE_QTD[index]) as u32 }
}

fn control_qtd_address(index: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(CONTROL_QTD[index]) as u32 }
}

fn delay_millis(millis: u32) {
    let deadline = timer::deadline_millis(millis);

    while !timer::expired(deadline) {
        core::hint::spin_loop();
    }
}

fn control_epchar(address: u8) -> u32 {
    // High speed, endpoint 0, DTC (toggle из qTD), max packet 64, RL=4.
    (address as u32 & 0x7f)
        | (2 << 12)
        | (1 << 14)
        | (64 << 16)
        | (4 << 28)
}

fn bulk_epchar(address: u8, endpoint: u8, max_packet: u16) -> u32 {
    // High speed, bulk endpoint, DTC=0 (toggle берётся из qTD), RL=4.
    let max_packet = (max_packet as u32 & 0x7ff).max(1);

    (address as u32 & 0x7f)
        | ((endpoint as u32 & 0x0f) << 8)
        | (2 << 12)
        | (max_packet << 16)
        | (4 << 28)
}

fn interrupt_epchar(address: u8, endpoint: u8, max_packet: u16) -> u32 {
    let max_packet = (max_packet as u32 & 0x7ff).max(1);

    (address as u32 & 0x7f)
        | ((endpoint as u32 & 0x0f) << 8)
        | (2 << 12)
        | (1 << 14)
        | (max_packet << 16)
}

fn find_controller() -> Option<pci::Device> {
    for index in 0..pci::count() {
        let Some(device) = pci::device(index) else {
            continue;
        };

        if device.class == EHCI_CLASS
            && device.subclass == EHCI_SUBCLASS
            && device.prog_if == EHCI_PROG_IF
        {
            return Some(*device);
        }
    }

    None
}

/// Забирает управление у BIOS через extended capability BIOS/OS handoff.
fn bios_handoff(bus: u8, slot: u8, func: u8, base: u32) {
    let hcc_params = unsafe { read32(base + HCCPARAMS) };
    let mut offset = ((hcc_params >> 8) & 0xff) as u8;

    for _ in 0..16 {
        if offset < 0x40 {
            return;
        }

        let header = pci::read_u32(bus, slot, func, offset);

        // Capability ID 1 — BIOS/OS handoff.
        if header & 0xff == 0x01 {
            if header & (1 << 16) != 0 {
                // Просим BIOS отдать контроллер: OS semaphore bit 24.
                pci::write_u8(bus, slot, func, offset + 3, 1);

                let deadline = timer::deadline_millis(1000);

                loop {
                    let value = pci::read_u32(bus, slot, func, offset);

                    if value & (1 << 16) == 0 {
                        break;
                    }

                    if timer::expired(deadline) {
                        // BIOS не ответил: забираем управление силой.
                        pci::write_u8(bus, slot, func, offset + 2, 0);
                        break;
                    }

                    delay_millis(10);
                }
            }

            // Отключаем SMI, которые BIOS мог включить.
            pci::write_u32(bus, slot, func, offset + 4, 0);
            return;
        }

        let next = ((header >> 8) & 0xff) as u8;

        if next == 0 || next == offset {
            return;
        }

        offset = next;
    }
}

fn reset_controller() {
    unsafe {
        // Останавливаем контроллер.
        write_op(USBCMD, 0);

        let deadline = timer::deadline_millis(100);

        while read_op(USBSTS) & STS_HALT == 0 {
            if timer::expired(deadline) {
                break;
            }

            core::hint::spin_loop();
        }

        // Полный сброс.
        write_op(USBCMD, CMD_HCRESET);
        delay_millis(20);

        let deadline = timer::deadline_millis(200);

        while read_op(USBCMD) & CMD_HCRESET != 0 {
            if timer::expired(deadline) {
                break;
            }

            core::hint::spin_loop();
        }

        write_op(USBINTR, 0);
        write_op(USBSTS, STS_INT | STS_ERRINT | STS_PCD | 0x3c);
        write_op(FRINDEX, 0);
        write_op(CTRLDSSEGMENT, 0);

        // Голова asynchronous schedule. H-бит обязателен для первой QH.
        ASYNC_HEAD = Qh::EMPTY;
        ASYNC_HEAD.epchar = 1 << 15;
        ASYNC_HEAD.next = core::ptr::addr_of_mut!(CONTROL_QH) as u32 | Q_TYPE_QH;

        CONTROL_QH = Qh::EMPTY;
        CONTROL_QH.next = core::ptr::addr_of_mut!(ASYNC_HEAD) as u32 | Q_TYPE_QH;
        CONTROL_QH.epchar = control_epchar(0);
        CONTROL_QH.next_qtd = TERMINATE;
        CONTROL_QH.alt_next = TERMINATE;

        for index in 0..CONTROL_QTD_COUNT {
            CONTROL_QTD[index] = Qtd::EMPTY;
        }

        SETUP_PACKET.bytes = [0; 8];

        // Periodic schedule: цепочка interrupt-очередей по одной на порт.
        for port in 0..MAX_PORTS {
            DEVICE_QH[port] = Qh::EMPTY;
            DEVICE_QH[port].next = if port + 1 < PORTS {
                qh_address(port + 1) | Q_TYPE_QH
            } else {
                TERMINATE
            };
            DEVICE_QH[port].next_qtd = TERMINATE;
            DEVICE_QH[port].alt_next = TERMINATE;
            DEVICE_QTD[port] = Qtd::EMPTY;
            DEVICE_BUFFER[port] = [0; 8];
            DEVICES[port] = Device::EMPTY;
        }

        let first = if PORTS > 0 {
            qh_address(0) | Q_TYPE_QH
        } else {
            TERMINATE
        };

        let entries =
            core::ptr::addr_of_mut!(FRAME_LIST.entries).cast::<u32>();

        for index in 0..FRAME_COUNT {
            core::ptr::write_volatile(entries.add(index), first);
        }

        fence(Ordering::SeqCst);

        write_op(ASYNCLISTADDR, core::ptr::addr_of_mut!(ASYNC_HEAD) as u32);
        write_op(
            PERIODICLISTBASE,
            core::ptr::addr_of_mut!(FRAME_LIST) as u32,
        );

        // Все порты переходят под управление EHCI.
        write_op(CONFIGFLAG, 1);

        fence(Ordering::SeqCst);

        write_op(USBCMD, CMD_RS | CMD_PSE | CMD_ASE);
    }

    delay_millis(20);
}

/// Инициализирует первый контроллер EHCI.
pub fn init() -> bool {
    let Some(device) = find_controller() else {
        return false;
    };

    unsafe {
        FOUND = true;
        READY = false;
        MMIO_BASE = 0;
        OP_BASE = 0;
        PORTS = 0;
        NEXT_SCAN = 0;
    }

    let bar = device.bar[0];

    if bar & 1 != 0 || bar == 0 {
        return false;
    }

    let base = bar & !0x0f;

    if base == 0 || !paging::map_identity_large(base) {
        return false;
    }

    // Доступ к MMIO и bus mastering.
    let command = pci::read_u16(device.bus, device.slot, device.func, 0x04);
    pci::write_u16(
        device.bus,
        device.slot,
        device.func,
        0x04,
        command | 0x0006,
    );

    bios_handoff(device.bus, device.slot, device.func, base);

    unsafe {
        MMIO_BASE = base;

        let cap_length = read32(base + CAPLENGTH) & 0xff;
        let hcs_params = read32(base + HCSPARAMS);

        if cap_length == 0 || cap_length > 0x40 {
            return false;
        }

        OP_BASE = base + cap_length;
        PORTS = ((hcs_params & 0x0f) as usize).min(MAX_PORTS);
    }

    reset_controller();

    unsafe {
        READY = true;
    }

    scan_ports();

    true
}

fn allocate_address(port: usize) -> Option<u8> {
    unsafe {
        for address in 1..=127u8 {
            let mut used = false;

            for index in 0..MAX_PORTS {
                if index != port
                    && DEVICES[index].present
                    && DEVICES[index].address == address
                {
                    used = true;
                    break;
                }
            }

            if !used {
                return Some(address);
            }
        }
    }

    None
}

/// Заполняет page-указатели qTD так, чтобы передача могла пересекать
/// границы страниц.
unsafe fn fill_buffers(qtd: *mut Qtd, address: u32, length: usize) {
    unsafe {
        (*qtd).buffer = [0; 5];
        (*qtd).buffer_high = [0; 5];

        if length == 0 {
            return;
        }

        (*qtd).buffer[0] = address;

        let mut page = (address & !0xfff).wrapping_add(0x1000);
        let end = address.wrapping_add(length as u32);
        let mut index = 1;

        while page < end && index < 5 {
            (*qtd).buffer[index] = page;
            page = page.wrapping_add(0x1000);
            index += 1;
        }
    }
}

fn qtd_token(pid: u32, toggle: bool, length: usize) -> u32 {
    QTD_ACTIVE
        | QTD_CERR
        | (((length as u32) & 0x7fff) << QTD_LENGTH_SHIFT)
        | (pid << 8)
        | if toggle { QTD_TOGGLE } else { 0 }
}

/// Выполняет управляющую передачу через asynchronous schedule.
fn control_transfer(
    address: u8,
    setup: &[u8; 8],
    data: Option<*mut u8>,
    data_length: usize,
    data_in: bool,
) -> Option<usize> {
    if data_length > 256 {
        return None;
    }

    let qtd_count = if data_length == 0 { 2 } else { 3 };

    unsafe {
        SETUP_PACKET.bytes = *setup;

        for index in 0..CONTROL_QTD_COUNT {
            CONTROL_QTD[index] = Qtd::EMPTY;
        }

        // SETUP stage: DATA0.
        let setup_qtd = core::ptr::addr_of_mut!(CONTROL_QTD[0]);
        (*setup_qtd).token = qtd_token(QTD_PID_SETUP, false, 8);
        fill_buffers(
            setup_qtd,
            core::ptr::addr_of!(SETUP_PACKET.bytes) as u32,
            8,
        );

        if data_length > 0 {
            let data_qtd = core::ptr::addr_of_mut!(CONTROL_QTD[1]);
            (*data_qtd).token = qtd_token(
                if data_in { QTD_PID_IN } else { QTD_PID_OUT },
                true,
                data_length,
            );
            fill_buffers(data_qtd, data.unwrap_or(core::ptr::null_mut()) as u32, data_length);
        }

        // STATUS stage всегда DATA1 и в обратную сторону.
        let status_qtd =
            core::ptr::addr_of_mut!(CONTROL_QTD[qtd_count - 1]);
        (*status_qtd).token =
            qtd_token(if data_in { QTD_PID_OUT } else { QTD_PID_IN }, true, 0);

        for index in 0..qtd_count {
            let pointer = if index + 1 < qtd_count {
                control_qtd_address(index + 1)
            } else {
                TERMINATE
            };

            CONTROL_QTD[index].next = pointer;
            // Короткий пакет не должен обрывать цепочку перед status.
            CONTROL_QTD[index].alt_next = pointer;
        }

        fence(Ordering::SeqCst);

        CONTROL_QH.epchar = control_epchar(address);
        CONTROL_QH.current = 0;
        CONTROL_QH.next_qtd = control_qtd_address(0);
        CONTROL_QH.alt_next = TERMINATE;
        CONTROL_QH.token = 0;

        fence(Ordering::SeqCst);

        let deadline = timer::deadline_millis(500);

        loop {
            let mut complete = true;

            for index in 0..qtd_count {
                let token = core::ptr::read_volatile(
                    core::ptr::addr_of!(CONTROL_QTD[index].token),
                );

                if token & QTD_ACTIVE != 0 {
                    complete = false;
                } else if token & QTD_ERRORS != 0 {
                    return None;
                }
            }

            if complete {
                break;
            }

            if timer::expired(deadline) {
                return None;
            }

            core::hint::spin_loop();
        }

        if data_length == 0 {
            return Some(0);
        }

        let token = core::ptr::read_volatile(
            core::ptr::addr_of!(CONTROL_QTD[1].token),
        );
        let remaining = ((token >> QTD_LENGTH_SHIFT) & 0x7fff) as usize;

        Some(data_length.saturating_sub(remaining))
    }
}

fn control_read(
    address: u8,
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    buffer: &mut [u8],
) -> Option<usize> {
    let setup = [
        request_type,
        request,
        value as u8,
        (value >> 8) as u8,
        index as u8,
        (index >> 8) as u8,
        buffer.len() as u8,
        (buffer.len() >> 8) as u8,
    ];

    control_transfer(
        address,
        &setup,
        Some(buffer.as_mut_ptr()),
        buffer.len(),
        true,
    )
}

fn control_write(
    address: u8,
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    data: &[u8],
) -> Option<usize> {
    let setup = [
        request_type,
        request,
        value as u8,
        (value >> 8) as u8,
        index as u8,
        (index >> 8) as u8,
        data.len() as u8,
        (data.len() >> 8) as u8,
    ];

    control_transfer(
        address,
        &setup,
        if data.is_empty() {
            None
        } else {
            Some(data.as_ptr() as *mut u8)
        },
        data.len(),
        false,
    )
}

/// Выполняет bulk-передачу через asynchronous schedule.
///
/// Один qTD описывает до 16 КиБ (пять page-указателей), для Wi-Fi-кадра
/// этого достаточно. Как и control-передача, bulk синхронный: драйвер
/// опросный, поэтому отдельный qTD в асинхронном расписании не нужен.
#[allow(dead_code)]
fn bulk_transfer(
    address: u8,
    endpoint: u8,
    max_packet: u16,
    toggle: bool,
    data_in: bool,
    buffer: &mut [u8],
) -> Option<usize> {
    let length = buffer.len();

    if length == 0 {
        return Some(0);
    }

    // Пять page-указателей qTD покрывают не больше 16 КиБ.
    if length > 0x4000 {
        return None;
    }

    unsafe {
        let qtd = core::ptr::addr_of_mut!(CONTROL_QTD[0]);
        *qtd = Qtd::EMPTY;
        (*qtd).token = qtd_token(
            if data_in { QTD_PID_IN } else { QTD_PID_OUT },
            toggle,
            length,
        );
        fill_buffers(qtd, buffer.as_mut_ptr() as u32, length);

        fence(Ordering::SeqCst);

        CONTROL_QH.epchar = bulk_epchar(address, endpoint, max_packet);
        CONTROL_QH.current = 0;
        CONTROL_QH.next_qtd = control_qtd_address(0);
        CONTROL_QH.alt_next = TERMINATE;
        CONTROL_QH.token = 0;

        fence(Ordering::SeqCst);

        let deadline = timer::deadline_millis(500);

        loop {
            let token =
                core::ptr::read_volatile(core::ptr::addr_of!(CONTROL_QTD[0].token));

            if token & QTD_ACTIVE == 0 {
                if token & QTD_ERRORS != 0 {
                    return None;
                }

                let remaining =
                    ((token >> QTD_LENGTH_SHIFT) & 0x7fff) as usize;

                return Some(length.saturating_sub(remaining));
            }

            if timer::expired(deadline) {
                return None;
            }

            core::hint::spin_loop();
        }
    }
}

/// Настраивает qTD и QH для периодического чтения HID-отчёта.
fn arm_device(port: usize, device: &Device) {
    unsafe {
        let qtd = core::ptr::addr_of_mut!(DEVICE_QTD[port]);
        let qh = core::ptr::addr_of_mut!(DEVICE_QH[port]);
        let buffer = core::ptr::addr_of_mut!(DEVICE_BUFFER[port]) as u32;

        *qtd = Qtd::EMPTY;
        (*qtd).token = qtd_token(QTD_PID_IN, device.toggle, 8);
        fill_buffers(qtd, buffer, 8);

        (*qh).epchar =
            interrupt_epchar(device.address, device.endpoint, device.max_packet);
        // Одна транзакция за микрофрейм, опрос раз в кадр.
        (*qh).epcap = (1 << 30) | 0x01;
        (*qh).current = 0;
        (*qh).next_qtd = qtd_address(port);
        (*qh).alt_next = TERMINATE;
        (*qh).token = 0;

        fence(Ordering::SeqCst);
    }
}

fn remove_device(port: usize) {
    unsafe {
        let qh = core::ptr::addr_of_mut!(DEVICE_QH[port]);
        let qtd = core::ptr::addr_of_mut!(DEVICE_QTD[port]);

        (*qh).next_qtd = TERMINATE;
        (*qh).token = 0;
        (*qtd).token = 0;

        fence(Ordering::SeqCst);

        DEVICES[port] = Device::EMPTY;
    }
}

/// Сбрасывает корневой порт.
///
/// Возвращает `Some(true)` для high-speed устройства, `Some(false)` если
/// устройство передано companion-контроллеру, `None` при ошибке.
fn reset_port(port: usize) -> Option<bool> {
    let register = PORTSC + port as u32 * 4;

    unsafe {
        let mut status = read_op(register);

        if status & PORT_CCS == 0 {
            return None;
        }

        // Питание порта.
        if status & PORT_PP == 0 {
            write_op(register, (status & !PORT_CHANGE_BITS) | PORT_PP);
            delay_millis(20);
            status = read_op(register);
        }

        // Сбрасываем накопленные change-биты.
        write_op(register, status & !PORT_PR);
        status = read_op(register);

        // Удерживаем reset не меньше 50 мс.
        write_op(register, (status & !PORT_CHANGE_BITS) | PORT_PR);
        delay_millis(50);

        status = read_op(register);
        write_op(register, status & !PORT_CHANGE_BITS & !PORT_PR);

        // Ждём завершения сброса и появления PED.
        let deadline = timer::deadline_millis(200);

        loop {
            status = read_op(register);

            if status & PORT_PR == 0
                && (status & PORT_PED != 0
                    || (status & PORT_SPEED_MASK) >> PORT_SPEED_SHIFT
                        == PORT_SPEED_HIGH)
            {
                break;
            }

            if timer::expired(deadline) {
                break;
            }

            core::hint::spin_loop();
        }

        delay_millis(10);

        status = read_op(register);
        write_op(register, status & !PORT_CHANGE_BITS & !PORT_PR);

        let speed = (status & PORT_SPEED_MASK) >> PORT_SPEED_SHIFT;
        let high_speed = status & PORT_PED != 0 || speed == PORT_SPEED_HIGH;

        if high_speed {
            return Some(true);
        }

        // Full/low-speed обслуживает companion-контроллер.
        write_op(
            register,
            (status & !PORT_CHANGE_BITS & !PORT_PR) | PORT_POWNER,
        );

        Some(false)
    }
}

/// Перечисляет high-speed HID-устройство.
fn enumerate(port: usize) -> Option<Kind> {
    let address = allocate_address(port)?;

    // Первые 8 байт device descriptor по нулевому адресу.
    let mut header = [0u8; 8];

    if control_read(0, 0x80, 6, 0x0100, 0, &mut header)? < 8 {
        return None;
    }

    // SET_ADDRESS.
    control_write(0, 0x00, 5, address as u16, 0, &[])?;
    delay_millis(2);

    let mut device_descriptor = [0u8; 18];

    if control_read(address, 0x80, 6, 0x0100, 0, &mut device_descriptor)? < 18 {
        return None;
    }

    // Заголовок configuration descriptor.
    let mut configuration_header = [0u8; 9];

    if control_read(address, 0x80, 6, 0x0200, 0, &mut configuration_header)?
        < 9
    {
        return None;
    }

    let total_length = u16::from_le_bytes([
        configuration_header[2],
        configuration_header[3],
    ]) as usize;

    if total_length < 9 {
        return None;
    }

    let total_length = total_length.min(256);
    let mut configuration = [0u8; 256];

    control_read(
        address,
        0x80,
        6,
        0x0200,
        0,
        &mut configuration[..total_length],
    )?;

    let endpoint = hid::find(&configuration[..total_length]);
    let (bulk_in, bulk_out) = device::find_bulk(&configuration[..total_length]);
    let identity = device::identity(&device_descriptor);

    // SET_CONFIGURATION нужен даже неподдерживаемому устройству.
    control_write(address, 0x00, 9, 1, 0, &[])?;

    let Some(endpoint) = endpoint else {
        // Vendor-specific устройство: Wi-Fi адаптер опознаём по VID/PID,
        // bulk-эндпоинты запоминаем для следующего этапа.
        let kind = if device::is_wifi(identity) {
            Kind::Wifi
        } else {
            Kind::Unsupported
        };

        unsafe {
            let device = &mut DEVICES[port];
            device.present = true;
            device.kind = kind;
            device.address = address;
            device.identity = identity;
            device.bulk_in = bulk_in;
            device.bulk_out = bulk_out;
            device.bulk_in_toggle = false;
            device.bulk_out_toggle = false;
            device.retry_at = 0;
        }

        return Some(kind);
    };

    // HID boot protocol и отсутствие периодических пустых отчётов.
    control_write(
        address,
        0x21,
        0x0b,
        0,
        endpoint.interface as u16,
        &[],
    )?;

    control_write(
        address,
        0x21,
        0x0a,
        0,
        endpoint.interface as u16,
        &[],
    )?;

    let device = Device {
        present: true,
        kind: endpoint.kind,
        address,
        endpoint: endpoint.endpoint,
        max_packet: endpoint.max_packet,
        toggle: false,
        pending_rearm: false,
        previous_modifiers: 0,
        previous_keys: [0; 6],
        identity,
        bulk_in,
        bulk_out,
        bulk_in_toggle: false,
        bulk_out_toggle: false,
        retry_at: 0,
    };

    unsafe {
        DEVICES[port] = device;
    }

    arm_device(port, &device);

    Some(endpoint.kind)
}

fn scan_ports() {
    if !ready() {
        return;
    }

    let ports = unsafe { PORTS };

    for port in 0..ports {
        let register = PORTSC + port as u32 * 4;

        unsafe {
            let status = read_op(register);

            if status == 0xffff_ffff {
                continue;
            }

            // Запись единиц сбрасывает change-биты, остальные биты
            // сохраняются (PED всё равно read-only).
            if status & PORT_CHANGE_BITS != 0 {
                write_op(register, status);
            }

            if status & PORT_POWNER != 0 {
                if DEVICES[port].present {
                    remove_device(port);
                }

                continue;
            }

            let connected = status & PORT_CCS != 0;

            if !connected {
                if DEVICES[port].present {
                    remove_device(port);
                }

                continue;
            }

            if DEVICES[port].present {
                if status & PORT_PED != 0 {
                    continue;
                }

                remove_device(port);
            }

            if DEVICES[port].retry_at != 0
                && !timer::expired(DEVICES[port].retry_at)
            {
                continue;
            }

            match reset_port(port) {
                Some(true) => {
                    if enumerate(port).is_none() {
                        DEVICES[port] = Device::EMPTY;
                        DEVICES[port].retry_at =
                            timer::deadline_millis(1000);
                    }
                }
                Some(false) => {}
                None => {
                    DEVICES[port] = Device::EMPTY;
                    DEVICES[port].retry_at = timer::deadline_millis(1000);
                }
            }
        }
    }
}

fn poll_device(port: usize) {
    let (present, kind, pending_rearm) = unsafe {
        (
            DEVICES[port].present,
            DEVICES[port].kind,
            DEVICES[port].pending_rearm,
        )
    };

    if !present || kind == Kind::Unsupported || kind == Kind::None {
        return;
    }

    unsafe {
        let token = core::ptr::read_volatile(
            core::ptr::addr_of!(DEVICE_QTD[port].token),
        );

        if pending_rearm {
            let qh_token = core::ptr::read_volatile(
                core::ptr::addr_of!(DEVICE_QH[port].token),
            );

            // Ждём, пока контроллер завершит работу с overlay QH.
            if qh_token & QTD_ACTIVE != 0 {
                return;
            }

            let device = DEVICES[port];
            arm_device(port, &device);
            DEVICES[port].pending_rearm = false;
            return;
        }

        if token & QTD_ACTIVE != 0 {
            return;
        }

        if token & QTD_ERRORS != 0 {
            let device = DEVICES[port];

            if token & QTD_HALT != 0 {
                let _ = control_write(
                    device.address,
                    0x02,
                    1,
                    0,
                    device.endpoint as u16,
                    &[],
                );
            }

            DEVICES[port].toggle = false;
            DEVICES[port].pending_rearm = true;
            return;
        }

        let remaining = ((token >> QTD_LENGTH_SHIFT) & 0x7fff) as usize;
        let length = 8usize.saturating_sub(remaining).min(8);

        if length > 0 {
            let mut report = [0u8; 8];
            let buffer = core::ptr::addr_of!(DEVICE_BUFFER[port]) as *const u8;

            for index in 0..length {
                report[index] = core::ptr::read_volatile(buffer.add(index));
            }

            match kind {
                Kind::Keyboard => {
                    hid::keyboard_report(
                        &mut DEVICES[port].previous_modifiers,
                        &mut DEVICES[port].previous_keys,
                        &report[..length],
                    );
                }
                Kind::Mouse => {
                    hid::mouse_report(&report[..length]);
                }
                _ => {}
            }

            DEVICES[port].toggle = !DEVICES[port].toggle;
        }

        DEVICES[port].pending_rearm = true;
    }
}

/// Опрашивает порты и завершённые передачи.
pub fn poll() {
    if !ready() {
        return;
    }

    if timer::expired(unsafe { NEXT_SCAN }) {
        unsafe {
            NEXT_SCAN = timer::deadline_millis(100);
        }

        scan_ports();
    }

    let ports = unsafe { PORTS };

    for port in 0..ports {
        poll_device(port);
    }
}

/// Число подключённых устройств на контроллере EHCI.
pub(crate) fn device_count() -> usize {
    unsafe {
        let mut count = 0;

        for port in 0..PORTS {
            if DEVICES[port].present {
                count += 1;
            }
        }

        count
    }
}

/// Сведения об устройстве по его порядковому номеру среди подключённых.
pub(crate) fn device(index: usize) -> Option<super::DeviceInfo> {
    unsafe {
        let mut seen = 0;

        for port in 0..PORTS {
            let device = &DEVICES[port];

            if !device.present {
                continue;
            }

            if seen == index {
                return Some(super::DeviceInfo {
                    kind: device.kind,
                    vendor: device.identity.vendor,
                    product: device.identity.product,
                    address: device.address,
                    bulk_in: device.bulk_in.map(|endpoint| {
                        (endpoint.address, endpoint.max_packet)
                    }),
                    bulk_out: device.bulk_out.map(|endpoint| {
                        (endpoint.address, endpoint.max_packet)
                    }),
                    low_speed: false,
                });
            }

            seen += 1;
        }

        None
    }
}

/// Выполняет bulk-передачу device по порядковому номеру.
#[allow(dead_code)]
pub(crate) fn bulk(
    index: usize,
    data_in: bool,
    buffer: &mut [u8],
) -> Option<usize> {
    unsafe {
        let mut seen = 0;

        for port in 0..PORTS {
            if !DEVICES[port].present {
                continue;
            }

            if seen != index {
                seen += 1;
                continue;
            }

            let device = DEVICES[port];
            let endpoint = if data_in {
                device.bulk_in?
            } else {
                device.bulk_out?
            };
            let toggle = if data_in {
                device.bulk_in_toggle
            } else {
                device.bulk_out_toggle
            };

            let result = bulk_transfer(
                device.address,
                endpoint.address & 0x0f,
                endpoint.max_packet,
                toggle,
                data_in,
                buffer,
            );

            if result.is_some() {
                if data_in {
                    DEVICES[port].bulk_in_toggle = !toggle;
                } else {
                    DEVICES[port].bulk_out_toggle = !toggle;
                }
            }

            return result;
        }

        None
    }
}

fn push_bytes(out: &mut [u8; 80], len: &mut usize, text: &[u8]) {
    for &byte in text {
        if *len >= out.len() {
            return;
        }

        out[*len] = byte;
        *len += 1;
    }
}

fn push_decimal(out: &mut [u8; 80], len: &mut usize, value: usize) {
    let mut digits = [0u8; 10];
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

fn push_hex(out: &mut [u8; 80], len: &mut usize, value: u32) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    push_bytes(out, len, b"0x");

    for shift in [28, 24, 20, 16, 12, 8, 4, 0] {
        push_bytes(out, len, &[HEX[((value >> shift) & 0xf) as usize]]);
    }
}

fn push_hex16(out: &mut [u8; 80], len: &mut usize, value: u16) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    for shift in [12, 8, 4, 0] {
        push_bytes(out, len, &[HEX[((value >> shift) & 0xf) as usize]]);
    }
}

fn push_hex8(out: &mut [u8; 80], len: &mut usize, value: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    push_bytes(out, len, b"0x");
    push_bytes(out, len, &[HEX[(value >> 4) as usize]]);
    push_bytes(out, len, &[HEX[(value & 0xf) as usize]]);
}

/// Печатает состояние контроллера и корневых портов.
pub fn print_info(start_row: usize) -> usize {
    let mut row = start_row;

    if !ready() {
        return 0;
    }

    crate::vga::print_at(row, 0, b"USB EHCI controller: running");
    row += 1;

    let ports = unsafe { PORTS };

    for port in 0..ports {
        let device = unsafe { DEVICES[port] };
        let status = unsafe { read_op(PORTSC + port as u32 * 4) };
        let mut line = [b' '; 80];
        let mut len = 0usize;

        push_bytes(&mut line, &mut len, b"EHCI port ");
        push_decimal(&mut line, &mut len, port + 1);
        push_bytes(&mut line, &mut len, b": ");

        if !device.present {
            if status & PORT_POWNER != 0 {
                push_bytes(&mut line, &mut len, b"companion controller");
            } else {
                push_bytes(&mut line, &mut len, b"empty");
            }
        } else {
            match device.kind {
                Kind::Keyboard => push_bytes(&mut line, &mut len, b"keyboard"),
                Kind::Mouse => push_bytes(&mut line, &mut len, b"mouse"),
                Kind::Wifi => push_bytes(
                    &mut line,
                    &mut len,
                    device::name(device.identity)
                        .unwrap_or(b"Wi-Fi adapter"),
                ),
                Kind::Unsupported => {
                    push_bytes(&mut line, &mut len, b"unsupported device")
                }
                Kind::None => push_bytes(&mut line, &mut len, b"unknown"),
            }

            push_bytes(&mut line, &mut len, b", address ");
            push_decimal(&mut line, &mut len, device.address as usize);

            if device.identity.vendor != 0 || device.identity.product != 0 {
                push_bytes(&mut line, &mut len, b", ");
                push_hex16(&mut line, &mut len, device.identity.vendor);
                push_bytes(&mut line, &mut len, b":");
                push_hex16(&mut line, &mut len, device.identity.product);
            }

            push_bytes(&mut line, &mut len, b", high-speed");
        }

        push_bytes(&mut line, &mut len, b" status ");
        push_hex(&mut line, &mut len, status);

        crate::vga::print_at(row, 0, &line[..len]);
        row += 1;

        if device.present
            && (device.bulk_in.is_some() || device.bulk_out.is_some())
        {
            let mut bulk = [b' '; 80];
            let mut bulk_len = 0usize;

            push_bytes(&mut bulk, &mut bulk_len, b"  bulk");

            if let Some(endpoint) = device.bulk_in {
                push_bytes(&mut bulk, &mut bulk_len, b" in ");
                push_hex8(&mut bulk, &mut bulk_len, endpoint.address);
                push_bytes(&mut bulk, &mut bulk_len, b"/");
                push_decimal(
                    &mut bulk,
                    &mut bulk_len,
                    endpoint.max_packet as usize,
                );
            }

            if let Some(endpoint) = device.bulk_out {
                push_bytes(&mut bulk, &mut bulk_len, b" out ");
                push_hex8(&mut bulk, &mut bulk_len, endpoint.address);
                push_bytes(&mut bulk, &mut bulk_len, b"/");
                push_decimal(
                    &mut bulk,
                    &mut bulk_len,
                    endpoint.max_packet as usize,
                );
            }

            crate::vga::print_at(row, 0, &bulk[..bulk_len]);
            row += 1;
        }
    }

    row - start_row
}
