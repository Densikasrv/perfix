//! Драйвер контроллера UHCI (USB 1.1).
//!
//! Схема работы такая же, как у остальных драйверов Perfix: никаких
//! прерываний, только опрос. Контроллер сам обходит frame list раз в
//! миллисекунду и выполняет прикреплённые к нему очереди transfer
//! descriptor'ов (TD). Драйвер раскладывает TD в `.bss`; страницы ядра
//! отображены тождественно, поэтому виртуальный адрес совпадает с
//! физическим, который нужен контроллеру.
//!
//! Поддерживаются только корневые порты и HID boot protocol. Этого
//! достаточно для USB-клавиатуры и мыши, подключённых напрямую.

use core::sync::atomic::{fence, Ordering};

use crate::pci;
use crate::port::{inw, outl, outw};
use crate::timer;

use super::device::{self, BulkEndpoint, Identity, Kind};
use super::hid;

// Регистры UHCI.
const USBCMD: u16 = 0x00;
const USBSTS: u16 = 0x02;
const USBINTR: u16 = 0x04;
const USBFRNUM: u16 = 0x06;
const USBFLBASEADD: u16 = 0x08;
const USBSOF: u16 = 0x0c;
const USBPORTSC1: u16 = 0x10;

const USBCMD_RS: u16 = 0x0001;
const USBCMD_HCRESET: u16 = 0x0002;
const USBCMD_CF: u16 = 0x0040;
const USBCMD_MAXP: u16 = 0x0080;

const USBPORTSC_CCS: u16 = 0x0001;
const USBPORTSC_CSC: u16 = 0x0002;
const USBPORTSC_PE: u16 = 0x0004;
const USBPORTSC_PEC: u16 = 0x0008;
const USBPORTSC_OCC: u16 = 0x0800;
const USBPORTSC_LSDA: u16 = 0x0100;
const USBPORTSC_PR: u16 = 0x0200;

/// Биты изменения состояния порта, которые сбрасываются записью единицы.
const PORT_CHANGE_BITS: u16 =
    USBPORTSC_CSC | USBPORTSC_PEC | USBPORTSC_OCC;

// Флаги указателей UHCI.
const PTR_TERM: u32 = 0x0001;
const PTR_QH: u32 = 0x0002;

// TD status.
const TD_SPD: u32 = 1 << 29;
const TD_MAXERR_SHIFT: u32 = 27;
const TD_LS: u32 = 1 << 26;
const TD_ACTIVE: u32 = 1 << 23;
const TD_STALLED: u32 = 1 << 22;
const TD_DBUFERR: u32 = 1 << 21;
const TD_BABBLE: u32 = 1 << 20;
const TD_CRCTIMEO: u32 = 1 << 18;
const TD_BITSTUFF: u32 = 1 << 17;
const TD_ACTLEN_MASK: u32 = 0x7ff;
/// Ошибки, при которых очередь останавливается. ACTIVE сюда не входит.
/// NAK не считается ошибкой: TD остаётся активным и повторяется.
const TD_ERRORS: u32 =
    TD_STALLED | TD_DBUFERR | TD_BABBLE | TD_CRCTIMEO | TD_BITSTUFF;

// TD token.
const TD_PID_OUT: u32 = 0xe1;
const TD_PID_IN: u32 = 0x69;
const TD_PID_SETUP: u32 = 0x2d;
const TD_TOKEN_TOGGLE: u32 = 1 << 19;

const MAX_CONTROLLERS: usize = 6;
const PORTS_PER_CONTROLLER: usize = 2;
const MAX_PORTS: usize = MAX_CONTROLLERS * PORTS_PER_CONTROLLER;
const FRAME_COUNT: usize = 1024;
const CONTROL_TD_COUNT: usize = 40;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Qh {
    link: u32,
    element: u32,
    reserved: [u32; 6],
}

impl Qh {
    const EMPTY: Self = Self {
        link: PTR_TERM,
        element: PTR_TERM,
        reserved: [0; 6],
    };
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Td {
    link: u32,
    status: u32,
    token: u32,
    buffer: u32,
    reserved: [u32; 4],
}

impl Td {
    const EMPTY: Self = Self {
        link: PTR_TERM,
        status: 0,
        token: 0,
        buffer: 0,
        reserved: [0; 4],
    };
}

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct FrameList {
    entries: [u32; FRAME_COUNT],
}

static mut FRAME_LISTS: [FrameList; MAX_CONTROLLERS] =
    [FrameList { entries: [PTR_TERM; FRAME_COUNT] }; MAX_CONTROLLERS];

static mut CONTROL_QH: [Qh; MAX_CONTROLLERS] = [Qh::EMPTY; MAX_CONTROLLERS];
static mut CONTROL_TD: [[Td; CONTROL_TD_COUNT]; MAX_CONTROLLERS] =
    [[Td::EMPTY; CONTROL_TD_COUNT]; MAX_CONTROLLERS];
static mut SETUP_PACKETS: [[u8; 8]; MAX_CONTROLLERS] =
    [[0; 8]; MAX_CONTROLLERS];

static mut DEVICE_QH: [Qh; MAX_PORTS] = [Qh::EMPTY; MAX_PORTS];
static mut DEVICE_TD: [Td; MAX_PORTS] = [Td::EMPTY; MAX_PORTS];
static mut DEVICE_DUMMY: [Td; MAX_PORTS] = [Td::EMPTY; MAX_PORTS];
static mut DEVICE_BUFFER: [[u8; 8]; MAX_PORTS] = [[0; 8]; MAX_PORTS];

#[derive(Clone, Copy)]
struct Device {
    present: bool,
    kind: Kind,
    address: u8,
    low_speed: bool,
    endpoint: u8,
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
    /// Момент, раньше которого не стоит повторять перечисление.
    retry_at: u64,
}

impl Device {
    const EMPTY: Self = Self {
        present: false,
        kind: Kind::None,
        address: 0,
        low_speed: false,
        endpoint: 0,
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

static mut IO_BASES: [u16; MAX_CONTROLLERS] = [0; MAX_CONTROLLERS];
static mut CONTROLLER_COUNT: usize = 0;
static mut READY: bool = false;
static mut NEXT_SCAN: u64 = 0;

fn reg(controller: usize, offset: u16) -> u16 {
    unsafe { IO_BASES[controller] + offset }
}

fn controller_of(port: usize) -> usize {
    port / PORTS_PER_CONTROLLER
}

fn local_port(port: usize) -> usize {
    port % PORTS_PER_CONTROLLER
}

fn ready() -> bool {
    unsafe { READY }
}

/// Был ли найден контроллер UHCI.
pub fn found() -> bool {
    unsafe { READY }
}

fn qh_address(index: usize) -> u32 {
    unsafe {
        core::ptr::addr_of_mut!(DEVICE_QH[index]) as u32
    }
}

fn dummy_address(index: usize) -> u32 {
    unsafe {
        core::ptr::addr_of_mut!(DEVICE_DUMMY[index]) as u32
    }
}

fn control_td_address(controller: usize, index: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(CONTROL_TD[controller][index]) as u32 }
}

fn expected_length(length: usize) -> u32 {
    (((length.wrapping_sub(1)) as u32) & TD_ACTLEN_MASK) << 21
}

fn make_token(pid: u32, address: u8, endpoint: u8, toggle: bool, length: usize) -> u32 {
    pid | (((address as u32) & 0x7f) << 8)
        | (((endpoint as u32) & 0xf) << 15)
        | (if toggle { TD_TOKEN_TOGGLE } else { 0 })
        | expected_length(length)
}

fn actual_length(status: u32) -> usize {
    (((status & TD_ACTLEN_MASK) + 1) & TD_ACTLEN_MASK) as usize
}

fn delay_millis(millis: u32) {
    let deadline = timer::deadline_millis(millis);

    while !timer::expired(deadline) {
        core::hint::spin_loop();
    }
}

/// Сбрасывает контроллер и создаёт расписание: control → HID-очереди
/// только тех портов, которые принадлежат этому контроллеру.
fn reset_controller(controller: usize) {
    let first_port = controller * PORTS_PER_CONTROLLER;

    unsafe {
        outw(reg(controller, USBCMD), USBCMD_HCRESET);

        let deadline = timer::deadline_millis(100);

        while inw(reg(controller, USBCMD)) & USBCMD_HCRESET != 0 {
            if timer::expired(deadline) {
                break;
            }

            core::hint::spin_loop();
        }

        outw(reg(controller, USBCMD), 0);
        outw(reg(controller, USBSTS), 0x003f);
        outw(reg(controller, USBINTR), 0);
        outw(reg(controller, USBSOF), 64);
        outw(reg(controller, USBFRNUM), 0);

        CONTROL_QH[controller] = Qh::EMPTY;
        CONTROL_QH[controller].link = qh_address(first_port) | PTR_QH;

        for index in 0..CONTROL_TD_COUNT {
            CONTROL_TD[controller][index] = Td::EMPTY;
        }

        SETUP_PACKETS[controller] = [0; 8];

        for local in 0..PORTS_PER_CONTROLLER {
            let port = first_port + local;

            DEVICE_QH[port] = Qh::EMPTY;

            // Соседние очереди идут цепочкой, последняя завершается.
            DEVICE_QH[port].link = if local + 1 < PORTS_PER_CONTROLLER {
                qh_address(port + 1) | PTR_QH
            } else {
                PTR_TERM
            };

            DEVICE_TD[port] = Td::EMPTY;
            DEVICE_DUMMY[port] = Td::EMPTY;
            DEVICE_BUFFER[port] = [0; 8];
            DEVICES[port] = Device::EMPTY;
        }

        let first_qh =
            core::ptr::addr_of_mut!(CONTROL_QH[controller]) as u32 | PTR_QH;
        let entries = core::ptr::addr_of_mut!(FRAME_LISTS[controller].entries)
            .cast::<u32>();

        for index in 0..FRAME_COUNT {
            entries.add(index).write(first_qh);
        }

        fence(Ordering::SeqCst);

        outl(
            reg(controller, USBFLBASEADD),
            core::ptr::addr_of!(FRAME_LISTS[controller]) as u32,
        );
        outw(
            reg(controller, USBCMD),
            USBCMD_RS | USBCMD_CF | USBCMD_MAXP,
        );
    }

    delay_millis(10);
}

/// Инициализирует все найденные контроллеры UHCI.
///
/// На современных чипсетах их несколько: EHCI передаёт full/low-speed
/// устройства соответствующему companion-контроллеру.
pub fn init() -> bool {
    let mut count = 0usize;

    for index in 0..pci::count() {
        let Some(device) = pci::device(index) else {
            continue;
        };

        // class 0x0C / subclass 0x03 / prog_if 0x00 — UHCI.
        if device.class != 0x0c
            || device.subclass != 0x03
            || device.prog_if != 0x00
        {
            continue;
        }

        let bar = device.bar[4];

        if bar & 1 == 0 {
            continue;
        }

        let io_base = (bar & 0xffff_fffc) as u16;

        if io_base == 0 || count >= MAX_CONTROLLERS {
            continue;
        }

        // Разрешаем доступ к портам ввода-вывода и bus mastering.
        let command = pci::read_u16(device.bus, device.slot, device.func, 0x04);
        pci::write_u16(
            device.bus,
            device.slot,
            device.func,
            0x04,
            command | 0x0005,
        );

        // Забираем порты у BIOS legacy support (PIRQ/SMI off).
        pci::write_u16(device.bus, device.slot, device.func, 0xc0, 0x8f00);

        unsafe {
            IO_BASES[count] = io_base;
        }

        count += 1;
    }

    unsafe {
        CONTROLLER_COUNT = count;
        READY = false;
        NEXT_SCAN = 0;
    }

    for controller in 0..count {
        reset_controller(controller);
    }

    unsafe {
        READY = count > 0;
    }

    if count > 0 {
        scan_ports();
    }

    count > 0
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

/// Сбрасывает корневой порт. Возвращает признак low-speed устройства.
fn reset_port(controller: usize, port: usize) -> Option<bool> {
    let status_register =
        reg(controller, USBPORTSC1 + (local_port(port) as u16) * 2);

    let mut status = inw(status_register);

    if status & USBPORTSC_CCS == 0 {
        return None;
    }

    // Сбрасываем накопленные флаги изменений.
    outw(status_register, status);

    // Удерживаем reset не менее 50 мс.
    outw(status_register, (status & !PORT_CHANGE_BITS) | USBPORTSC_PR);
    delay_millis(50);

    status = inw(status_register);
    let low_speed = status & USBPORTSC_LSDA != 0;

    // PE выставляет программное обеспечение после сброса порта.
    outw(
        status_register,
        (status & !PORT_CHANGE_BITS & !USBPORTSC_PR) | USBPORTSC_PE,
    );

    // После снятия reset устройству нужно время на восстановление.
    delay_millis(10);

    let deadline = timer::deadline_millis(100);

    loop {
        status = inw(status_register);

        if status & USBPORTSC_PE != 0 {
            outw(status_register, status);
            return Some(low_speed);
        }

        if timer::expired(deadline) {
            return None;
        }

        core::hint::spin_loop();
    }
}

/// Удаляет устройство из расписания и освобождает его адрес.
fn remove_device(port: usize) {
    unsafe {
        let qh = core::ptr::addr_of_mut!(DEVICE_QH[port]);
        let td = core::ptr::addr_of_mut!(DEVICE_TD[port]);

        (*qh).element = PTR_TERM;
        (*td).status = 0;

        fence(Ordering::SeqCst);

        DEVICES[port] = Device::EMPTY;
    }
}

fn control_transfer(
    controller: usize,
    address: u8,
    low_speed: bool,
    setup: &[u8; 8],
    data: Option<*mut u8>,
    data_length: usize,
    data_in: bool,
    max_packet: u8,
) -> Option<usize> {
    let max_packet = match max_packet {
        8 | 16 | 32 | 64 => max_packet as usize,
        _ => 8,
    };

    let data_tds = if data_length == 0 {
        0
    } else {
        (data_length + max_packet - 1) / max_packet
    };

    // setup + data + status + dummy
    let total = data_tds + 3;

    if total > CONTROL_TD_COUNT {
        return None;
    }

    unsafe {
        SETUP_PACKETS[controller] = *setup;
    }

    let base_status = TD_ACTIVE
        | (3 << TD_MAXERR_SHIFT)
        | if low_speed { TD_LS } else { 0 };

    let setup_td = control_td_address(controller, 0);
    let dummy_td = control_td_address(controller, 2 + data_tds);

    let setup_buffer =
        unsafe { core::ptr::addr_of!(SETUP_PACKETS[controller]) as u32 };

    unsafe {
        // SETUP transaction.
        let td = core::ptr::addr_of_mut!(CONTROL_TD[controller][0]);
        (*td).link = if total > 1 {
            control_td_address(controller, 1)
        } else {
            PTR_TERM
        };
        (*td).status = base_status;
        (*td).token = make_token(TD_PID_SETUP, address, 0, false, 8);
        (*td).buffer = setup_buffer;

        // DATA stage: по одному TD на максимальный пакет.
        let mut offset = 0usize;
        let mut toggle = true; // data stage начинается с DATA1

        for index in 0..data_tds {
            let remaining = data_length - offset;
            let length = remaining.min(max_packet);
            let mut status = base_status;

            // SPD позволяет завершить TD коротким пакетом. На последнем
            // пакете он не нужен.
            if data_in && remaining > max_packet {
                status |= TD_SPD;
            }

            let td = core::ptr::addr_of_mut!(CONTROL_TD[controller][1 + index]);
            let next = 2 + index;

            (*td).link = control_td_address(controller, next);
            (*td).status = status;
            (*td).token = make_token(
                if data_in { TD_PID_IN } else { TD_PID_OUT },
                address,
                0,
                toggle,
                length,
            );
            (*td).buffer = match data {
                Some(pointer) => pointer as u32 + offset as u32,
                None => 0,
            };

            offset += length;
            toggle = !toggle;
        }

        // STATUS stage идёт в обратную сторону, всегда DATA1.
        let status = core::ptr::addr_of_mut!(CONTROL_TD[controller][1 + data_tds]);
        (*status).link = dummy_td;
        (*status).status = base_status;
        (*status).token = make_token(
            if data_in { TD_PID_OUT } else { TD_PID_IN },
            address,
            0,
            true,
            0,
        );
        (*status).buffer = 0;

        // Неактивный хвост: UHCI останавливается на нём, а не на
        // только что завершённом TD.
        let dummy = core::ptr::addr_of_mut!(CONTROL_TD[controller][2 + data_tds]);
        (*dummy).link = PTR_TERM;
        (*dummy).status = 0;
        (*dummy).token = 0;
        (*dummy).buffer = 0;

        fence(Ordering::SeqCst);

        let qh = core::ptr::addr_of_mut!(CONTROL_QH[controller]);
        (*qh).element = setup_td;
        fence(Ordering::SeqCst);

        let deadline = timer::deadline_millis(500);

        loop {
            let mut complete = true;

            for index in 0..total {
                let td = core::ptr::addr_of!(CONTROL_TD[controller][index]);
                let status = core::ptr::read_volatile(core::ptr::addr_of!((*td).status));

                if status & TD_ACTIVE != 0 {
                    complete = false;
                } else if status & TD_ERRORS != 0 {
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

        // Суммируем фактические длины этапа DATA.
        let mut actual = 0usize;
        let mut remaining = data_length;

        for index in 0..data_tds {
            let expected = remaining.min(max_packet);
            remaining -= expected;

            let td = core::ptr::addr_of!(CONTROL_TD[controller][1 + index]);
            let status = core::ptr::read_volatile(core::ptr::addr_of!((*td).status));
            let length = actual_length(status);

            actual += length.min(expected);

            // Короткий пакет завершает transfer.
            if length < expected {
                break;
            }
        }

        Some(actual)
    }
}

fn control_read(
    controller: usize,
    address: u8,
    low_speed: bool,
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    buffer: &mut [u8],
    max_packet: u8,
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
        controller,
        address,
        low_speed,
        &setup,
        Some(buffer.as_mut_ptr()),
        buffer.len(),
        true,
        max_packet,
    )
}

fn control_write(
    controller: usize,
    address: u8,
    low_speed: bool,
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    data: &[u8],
    max_packet: u8,
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
        controller,
        address,
        low_speed,
        &setup,
        if data.is_empty() {
            None
        } else {
            Some(data.as_ptr() as *mut u8)
        },
        data.len(),
        false,
        max_packet,
    )
}

/// Выполняет bulk-передачу на эндпоинт, найденный при перечислении.
///
/// Отдельной очереди для bulk в UHCI не требуется: адрес устройства и
/// номер эндпоинта хранятся в token каждого TD, поэтому используется та же
/// control-очередь. Передача синхронная и выполняется только когда другие
/// control-передачи завершены (драйвер опросный и однопоточный).
#[allow(dead_code)]
fn bulk_transfer(
    controller: usize,
    address: u8,
    low_speed: bool,
    endpoint: u8,
    max_packet: u16,
    toggle: bool,
    data_in: bool,
    buffer: &mut [u8],
) -> Option<usize> {
    let max_packet = match max_packet {
        8 | 16 | 32 | 64 => max_packet as usize,
        _ => 64,
    };

    let length = buffer.len();

    if length == 0 {
        return Some(0);
    }

    let data_tds = (length + max_packet - 1) / max_packet;

    // data TD'ы и неактивный хвост.
    if data_tds + 1 > CONTROL_TD_COUNT {
        return None;
    }

    let base_status = TD_ACTIVE
        | (3 << TD_MAXERR_SHIFT)
        | TD_SPD
        | if low_speed { TD_LS } else { 0 };

    unsafe {
        let mut offset = 0usize;
        let mut current_toggle = toggle;

        for index in 0..data_tds {
            let remaining = length - offset;
            let packet = remaining.min(max_packet);
            let td = core::ptr::addr_of_mut!(CONTROL_TD[controller][index]);

            (*td).link = if index + 1 < data_tds {
                control_td_address(controller, index + 1)
            } else {
                control_td_address(controller, data_tds)
            };
            (*td).status = base_status;
            (*td).token = make_token(
                if data_in { TD_PID_IN } else { TD_PID_OUT },
                address,
                endpoint,
                current_toggle,
                packet,
            );
            (*td).buffer = buffer.as_mut_ptr() as u32 + offset as u32;

            offset += packet;
            current_toggle = !current_toggle;
        }

        // Неактивный хвост, чтобы контроллер не начал передачу заново.
        let dummy = core::ptr::addr_of_mut!(CONTROL_TD[controller][data_tds]);
        (*dummy).link = PTR_TERM;
        (*dummy).status = 0;
        (*dummy).token = 0;
        (*dummy).buffer = 0;

        fence(Ordering::SeqCst);

        let qh = core::ptr::addr_of_mut!(CONTROL_QH[controller]);
        (*qh).element = control_td_address(controller, 0);
        fence(Ordering::SeqCst);

        let deadline = timer::deadline_millis(500);

        loop {
            let mut actual = 0usize;
            let mut offset = 0usize;
            let mut finished = true;

            for index in 0..data_tds {
                let td = core::ptr::addr_of!(CONTROL_TD[controller][index]);
                let status = core::ptr::read_volatile(core::ptr::addr_of!((*td).status));

                if status & TD_ACTIVE != 0 {
                    finished = false;
                    break;
                }

                if status & TD_ERRORS != 0 {
                    return None;
                }

                let remaining = length - offset;
                let expected = remaining.min(max_packet);
                let got = actual_length(status).min(expected);

                actual += got;
                offset += expected;

                // Короткий пакет завершает передачу раньше последнего TD.
                if got < expected {
                    return Some(actual);
                }
            }

            if finished {
                return Some(actual);
            }

            if timer::expired(deadline) {
                return None;
            }

            core::hint::spin_loop();
        }
    }
}

fn arm_device(port: usize, device: &Device) -> bool {    unsafe {
        let td = core::ptr::addr_of_mut!(DEVICE_TD[port]);
        let dummy = core::ptr::addr_of_mut!(DEVICE_DUMMY[port]);
        let qh = core::ptr::addr_of_mut!(DEVICE_QH[port]);
        let buffer = core::ptr::addr_of_mut!(DEVICE_BUFFER[port]) as u32;

        // Терминальный TD остаётся неактивным.
        (*dummy).link = PTR_TERM;
        (*dummy).status = 0;
        (*dummy).token = 0;
        (*dummy).buffer = 0;

        let mut status = TD_ACTIVE | (3 << TD_MAXERR_SHIFT) | TD_SPD;

        if device.low_speed {
            status |= TD_LS;
        }

        (*td).link = dummy_address(port);
        (*td).status = status;
        (*td).token = make_token(
            TD_PID_IN,
            device.address,
            device.endpoint,
            device.toggle,
            8,
        );
        (*td).buffer = buffer;

        fence(Ordering::SeqCst);
        (*qh).element = core::ptr::addr_of_mut!(DEVICE_TD[port]) as u32;
        fence(Ordering::SeqCst);
    }

    true
}

/// Перечисляет устройство на порту и настраивает HID-отчёт.
fn enumerate(controller: usize, port: usize, low_speed: bool) -> Option<Kind> {
    let address = allocate_address(port)?;

    // GET_DESCRIPTOR (device), первые 8 байт, адрес 0.
    let mut header = [0u8; 8];

    if control_read(controller, 0, low_speed, 0x80, 6, 0x0100, 0, &mut header, 8)? < 8 {
        return None;
    }

    let max_packet = match header[7] {
        8 | 16 | 32 | 64 => header[7],
        _ => 8,
    };

    // SET_ADDRESS отправляется по нулевому адресу, новый адрес — в wValue.
    control_write(
        controller,
        0,
        low_speed,
        0x00,
        5,
        address as u16,
        0,
        &[],
        max_packet,
    )?;

    delay_millis(2);

    // Полный device descriptor нужен только для проверки, что устройство
    // действительно ответило по новому адресу.
    let mut device_descriptor = [0u8; 18];

    control_read(
        controller,
        address,
        low_speed,
        0x80,
        6,
        0x0100,
        0,
        &mut device_descriptor,
        max_packet,
    )?;

    // Заголовок configuration descriptor, чтобы узнать полную длину.
    let mut configuration_header = [0u8; 9];

    control_read(
        controller,
        address,
        low_speed,
        0x80,
        6,
        0x0200,
        0,
        &mut configuration_header,
        max_packet,
    )?;

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
        controller,
        address,
        low_speed,
        0x80,
        6,
        0x0200,
        0,
        &mut configuration[..total_length],
        max_packet,
    )?;

    let hid = hid::find(&configuration[..total_length]);
    let (bulk_in, bulk_out) = device::find_bulk(&configuration[..total_length]);
    let identity = device::identity(&device_descriptor);

    // SET_CONFIGURATION всегда нужен, даже для неподдерживаемого
    // устройства: иначе оно останется неактивным.
    control_write(controller, address, low_speed, 0x00, 9, 1, 0, &[], max_packet)?;

    let Some(hid) = hid else {
        // Vendor-specific устройство: Wi-Fi адаптер опознаём по VID/PID.
        // Его bulk-эндпоинты запоминаются для следующего этапа.
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
            device.low_speed = low_speed;
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
        controller,
        address,
        low_speed,
        0x21,
        0x0b,
        0,
        hid.interface as u16,
        &[],
        max_packet,
    );

    control_write(
        controller,
        address,
        low_speed,
        0x21,
        0x0a,
        0,
        hid.interface as u16,
        &[],
        max_packet,
    );

    let device = Device {
        present: true,
        kind: hid.kind,
        address,
        low_speed,
        endpoint: hid.endpoint,
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

    Some(hid.kind)
}

fn scan_ports() {
    if !ready() {
        return;
    }

    let ports = unsafe { CONTROLLER_COUNT * PORTS_PER_CONTROLLER };

    for port in 0..ports {
        let controller = controller_of(port);
        let status_register = reg(
            controller,
            USBPORTSC1 + (local_port(port) as u16) * 2,
        );

        let status = inw(status_register);

        // Несуществующие порты обычно читаются как 0xFFFF.
        if status == 0xffff {
            continue;
        }

        // Запись единиц в change-биты сбрасывает их.
        outw(status_register, status);

        let connected = status & USBPORTSC_CCS != 0;

        if !connected {
            if unsafe { DEVICES[port].present } {
                remove_device(port);
            }

            continue;
        }

        if unsafe { DEVICES[port].present } {
            // Если порт отключился, но остался подключённым, перечислим
            // устройство заново.
            if status & USBPORTSC_PE == 0 {
                remove_device(port);
            } else {
                continue;
            }
        }

        if unsafe {
            DEVICES[port].retry_at != 0
                && !timer::expired(DEVICES[port].retry_at)
        } {
            continue;
        }

        let Some(low_speed) = reset_port(controller, port) else {
            unsafe {
                DEVICES[port].retry_at = timer::deadline_millis(1000);
            }

            continue;
        };

        if enumerate(controller, port, low_speed).is_none() {
            unsafe {
                DEVICES[port] = Device::EMPTY;
                DEVICES[port].retry_at = timer::deadline_millis(1000);
            }
        }
    }
}

fn poll_device(port: usize) {
    let controller = controller_of(port);
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

    let td = unsafe { core::ptr::addr_of_mut!(DEVICE_TD[port]) };

    let status = unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!((*td).status))
    };

    if pending_rearm {
        // Ждём, пока контроллер сдвинет указатель очереди с TD на
        // терминальный TD, и только потом разрешаем повторную передачу.
        let qh = unsafe { core::ptr::addr_of_mut!(DEVICE_QH[port]) };
        let element = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!((*qh).element))
        };

        if element & !0x0f != unsafe { core::ptr::addr_of_mut!(DEVICE_TD[port]) as u32 } {
            unsafe {
                DEVICES[port].pending_rearm = false;
                let device = DEVICES[port];
                arm_device(port, &device);
            }
        } else {
            // Контроллер ещё не сдвинул указатель. Переводим очередь на
            // неактивный хвост вручную; на следующем опросе TD можно
            // безопасно активировать снова.
            unsafe {
                let qh = core::ptr::addr_of_mut!(DEVICE_QH[port]);
                (*qh).element = dummy_address(port);
                fence(Ordering::SeqCst);
            }
        }

        return;
    }

    if status & TD_ACTIVE != 0 {
        return;
    }

    if status & TD_ERRORS != 0 {
        // STALL обычно означает, что устройство не приняло запрос;
        // сбрасываем halt и пробуем снова со следующего кадра.
        if status & TD_STALLED != 0 {
            unsafe {
                let device = DEVICES[port];
                let _ = control_write(
                    controller,
                    device.address,
                    device.low_speed,
                    0x02,
                    1,
                    0,
                    device.endpoint as u16,
                    &[],
                    8,
                );
                DEVICES[port].toggle = false;
                DEVICES[port].pending_rearm = true;
            }
        } else {
            unsafe {
                DEVICES[port].pending_rearm = true;
            }
        }

        return;
    }

    let length = actual_length(status).min(8);

    if length > 0 {
        let mut report = [0u8; 8];

        unsafe {
            let buffer = core::ptr::addr_of!(DEVICE_BUFFER[port]) as *const u8;

            for index in 0..length {
                report[index] = core::ptr::read_volatile(buffer.add(index));
            }
        }

        match kind {
            Kind::Keyboard => unsafe {
                crate::drivers::usb::hid::keyboard_report(
                    &mut DEVICES[port].previous_modifiers,
                    &mut DEVICES[port].previous_keys,
                    &report[..length],
                );
            },
            Kind::Mouse => {
                crate::drivers::usb::hid::mouse_report(&report[..length]);
            }
            _ => {}
        }
    }

    unsafe {
        let device = &mut DEVICES[port];

        // Toggle меняется после каждой успешной передачи.
        if length > 0 {
            device.toggle = !device.toggle;
        }

        device.pending_rearm = true;
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

    let ports = unsafe { CONTROLLER_COUNT * PORTS_PER_CONTROLLER };

    for port in 0..ports {
        poll_device(port);
    }
}

/// Число подключённых устройств на контроллерах UHCI.
pub(crate) fn device_count() -> usize {
    unsafe {
        let ports = CONTROLLER_COUNT * PORTS_PER_CONTROLLER;
        let mut count = 0;

        for port in 0..ports {
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
        let ports = CONTROLLER_COUNT * PORTS_PER_CONTROLLER;
        let mut seen = 0;

        for port in 0..ports {
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
                    low_speed: device.low_speed,
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
        let ports = CONTROLLER_COUNT * PORTS_PER_CONTROLLER;
        let mut seen = 0;

        for port in 0..ports {
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
                controller_of(port),
                device.address,
                device.low_speed,
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

fn push_hex(out: &mut [u8; 80], len: &mut usize, value: u16) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    push_bytes(out, len, b"0x");

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
///
/// Возвращает число занятых строк экрана.
pub fn print_info(start_row: usize) -> usize {
    let mut row = start_row;

    if !ready() {
        return 0;
    }

    let count = unsafe { CONTROLLER_COUNT };

    for controller in 0..count {
        let mut header = [b' '; 80];
        let mut len = 0usize;
        push_bytes(&mut header, &mut len, b"USB UHCI controller ");
        push_decimal(&mut header, &mut len, controller + 1);
        push_bytes(&mut header, &mut len, b": running");
        crate::vga::print_at(row, 0, &header[..len]);
        row += 1;

        for local in 0..PORTS_PER_CONTROLLER {
            let port = controller * PORTS_PER_CONTROLLER + local;
            let device = unsafe { DEVICES[port] };
            let status = inw(reg(
                controller,
                USBPORTSC1 + (local as u16) * 2,
            ));
            let mut line = [b' '; 80];
            let mut len = 0usize;

            push_bytes(&mut line, &mut len, b"port ");
            push_decimal(&mut line, &mut len, local + 1);
            push_bytes(&mut line, &mut len, b": ");

            if !device.present {
                push_bytes(&mut line, &mut len, b"empty");
            } else {
                match device.kind {
                    Kind::Keyboard => {
                        push_bytes(&mut line, &mut len, b"keyboard")
                    }
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
                    push_hex(&mut line, &mut len, device.identity.vendor);
                    push_bytes(&mut line, &mut len, b":");
                    push_hex(&mut line, &mut len, device.identity.product);
                }

                if device.low_speed {
                    push_bytes(&mut line, &mut len, b", low-speed");
                }
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
    }

    row - start_row
}
