//! Драйвер контроллера OHCI (USB 1.1).
//!
//! OHCI используют многие машины на чипсетах NVIDIA, в том числе
//! MacBook Pro 2010 (MacBookPro7,1): встроенные клавиатура и тачпад —
//! full-speed USB-устройства, которые EHCI передаёт companion-контроллеру
//! OHCI. Без этого драйвера на таких машинах не работает USB-ввод.
//!
//! Кроме корневых портов драйвер перечисляет USB-хабы. На MacBook
//! клавиатура и тачпад подключены через внутренние хабы (SMSC и
//! Broadcom), а тачпад является вторым HID-интерфейсом того же
//! устройства, что и клавиатура. Поэтому для одного устройства
//! поддерживается несколько HID-интерфейсов.
//!
//! Как и остальные драйверы Perfix, OHCI работает без прерываний.
//! Управляющие и bulk-передачи идут через списки endpoint descriptor'ов,
//! а HID-отчёты читаются через periodic schedule. Все структуры лежат в
//! `.bss`; страницы ядра отображены тождественно, поэтому адрес структуры
//! совпадает с физическим адресом для контроллера.

use core::sync::atomic::{fence, Ordering};

use crate::{paging, pci, timer};

use super::device::{self, BulkEndpoint, Identity, Kind};
use super::hid;

// Регистры OHCI (BAR0).
const HC_CONTROL: u32 = 0x04;
const HC_COMMAND_STATUS: u32 = 0x08;
const HC_INTERRUPT_STATUS: u32 = 0x0c;
const HC_INTERRUPT_ENABLE: u32 = 0x10;
const HC_INTERRUPT_DISABLE: u32 = 0x14;
const HC_HCCA: u32 = 0x18;
const HC_CONTROL_HEAD_ED: u32 = 0x20;
const HC_CONTROL_CURRENT_ED: u32 = 0x24;
const HC_BULK_HEAD_ED: u32 = 0x28;
const HC_BULK_CURRENT_ED: u32 = 0x2c;
const HC_FM_INTERVAL: u32 = 0x34;
const HC_PERIODIC_START: u32 = 0x40;
const HC_LS_THRESHOLD: u32 = 0x44;
const HC_RH_DESCRIPTOR_A: u32 = 0x48;
const HC_RH_PORT_STATUS: u32 = 0x54;

// HcControl.
const CTRL_PLE: u32 = 1 << 2;
const CTRL_CLE: u32 = 1 << 4;
const CTRL_BLE: u32 = 1 << 5;
const CTRL_HCFS_OPERATIONAL: u32 = 2 << 6;

// HcCommandStatus.
const CMD_HCR: u32 = 1 << 0;
const CMD_CLF: u32 = 1 << 1;
const CMD_BLF: u32 = 1 << 2;

const INTR_WDH: u32 = 1 << 1;

// Состояние корневого порта.
const RH_PS_CCS: u32 = 0x0000_0001;
const RH_PS_PES: u32 = 0x0000_0002;
const RH_PS_PRS: u32 = 0x0000_0010;
const RH_PS_PPS: u32 = 0x0000_0100;
const RH_PS_LSDA: u32 = 0x0000_0200;
const RH_PS_CSC: u32 = 0x0001_0000;
const RH_PS_PESC: u32 = 0x0002_0000;
const RH_PS_PSSC: u32 = 0x0004_0000;
const RH_PS_OCIC: u32 = 0x0008_0000;
const RH_PS_PRSC: u32 = 0x0010_0000;
const RH_CHANGE_BITS: u32 =
    RH_PS_CSC | RH_PS_PESC | RH_PS_PSSC | RH_PS_OCIC | RH_PS_PRSC;

const RH_A_NDP: u32 = 0x7f;
const RH_A_NPS: u32 = 1 << 8;
const RH_A_POTPGT_SHIFT: u32 = 24;

// TD control/status.
/// Начальное значение CC — «не обработан».
const TD_NOT_ACCESSED: u32 = 0xf << 28;
const TD_EC: u32 = 3 << 26;
const TD_DI: u32 = 7 << 21;
const TD_DP_SETUP: u32 = 0 << 19;
const TD_DP_OUT: u32 = 1 << 19;
const TD_DP_IN: u32 = 2 << 19;
const TD_R: u32 = 1 << 18;
/// Поле T (data toggle) занимает два бита: DATA0 = 0b10, DATA1 = 0b11.
const TD_T_DATA0: u32 = 0x0200_0000;
const TD_T_DATA1: u32 = 0x0300_0000;

// Класс хаба и его запросы.
const HUB_CLASS: u8 = 0x09;
const HUB_GET_STATUS: u8 = 0x00;
const HUB_SET_FEATURE: u8 = 0x03;
const HUB_CLEAR_FEATURE: u8 = 0x01;
const HUB_PORT_POWER: u16 = 8;
const HUB_PORT_RESET: u16 = 4;
const HUB_C_PORT_CONNECTION: u16 = 16;
const HUB_C_PORT_RESET: u16 = 20;
const HUB_PORT_CONNECTED: u16 = 1 << 0;
const HUB_PORT_LOW_SPEED: u16 = 1 << 9;
const HUB_CHANGE_CONNECTION: u16 = 1 << 0;
const HUB_CHANGE_RESET: u16 = 1 << 4;

const MAX_CONTROLLERS: usize = 4;
/// OHCI-контроллеры обычно имеют 2–4 порта; берём с запасом.
const MAX_PORTS_PER_CONTROLLER: usize = 8;
const MAX_PORTS: usize = MAX_CONTROLLERS * MAX_PORTS_PER_CONTROLLER;
const PERIODIC_SLOTS: usize = 32;
const CONTROL_TD_COUNT: usize = 4;
/// Последний TD в списке — пустой «dummy», на который указывает TailP.
const CONTROL_DUMMY: usize = CONTROL_TD_COUNT - 1;

/// Сколько устройств (включая хабы) может быть подключено одновременно.
const MAX_DEVICES: usize = 24;
/// Сколько HID-интерфейсов берём у одного устройства.
const MAX_INTERFACES: usize = 3;
const MAX_RES: usize = MAX_DEVICES * MAX_INTERFACES;
/// Размер буфера одного HID-отчёта (см. `hid::MAX_REPORT`).
const REPORT_BUFFER: usize = hid::MAX_REPORT;
/// Порты хаба: у внутренних хабов MacBook их 3–4.
const MAX_HUB_PORTS: usize = 8;
const NO_HUB: u8 = 0xff;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Ed {
    control: u32,
    tail: u32,
    head: u32,
    next: u32,
}

impl Ed {
    const EMPTY: Self = Self {
        control: 0,
        tail: 0,
        head: 0,
        next: 0,
    };
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Td {
    info: u32,
    cbp: u32,
    next: u32,
    be: u32,
}

impl Td {
    const EMPTY: Self = Self {
        info: 0,
        cbp: 0,
        next: 0,
        be: 0,
    };
}

#[repr(C, align(256))]
#[derive(Clone, Copy)]
struct Hcca {
    interrupt: [u32; PERIODIC_SLOTS],
    frame: u32,
    done: u32,
    reserved: [u8; 116],
}

impl Hcca {
    const EMPTY: Self = Self {
        interrupt: [0; PERIODIC_SLOTS],
        frame: 0,
        done: 0,
        reserved: [0; 116],
    };
}

#[derive(Clone, Copy)]
struct HidInterface {
    kind: Kind,
    interface: u8,
    endpoint: u8,
    max_packet: u16,
    toggle: bool,
    previous_modifiers: u8,
    previous_keys: [u8; 6],
    layout: hid::Layout,
    /// Сколько байт запрашивать у endpoint'а за один отчёт.
    report_len: usize,
}

impl HidInterface {
    const EMPTY: Self = Self {
        kind: Kind::None,
        interface: 0,
        endpoint: 0,
        max_packet: 8,
        toggle: false,
        previous_modifiers: 0,
        previous_keys: [0; 6],
        layout: hid::Layout::EMPTY,
        report_len: 8,
    };
}

#[derive(Clone, Copy)]
struct Device {
    present: bool,
    controller: usize,
    /// 0xFF — корневой порт, иначе индекс слота родительского хаба.
    parent: u8,
    /// Номер корневого порта (с нуля) или порта хаба (с единицы).
    port: u8,
    address: u8,
    low_speed: bool,
    control_max_packet: u16,
    kind: Kind,
    identity: Identity,
    bulk_in: Option<BulkEndpoint>,
    bulk_out: Option<BulkEndpoint>,
    bulk_in_toggle: bool,
    bulk_out_toggle: bool,
    hub_ports: u8,
    interfaces: [HidInterface; MAX_INTERFACES],
    interface_count: u8,
}

impl Device {
    const EMPTY: Self = Self {
        present: false,
        controller: 0,
        parent: NO_HUB,
        port: 0,
        address: 0,
        low_speed: false,
        control_max_packet: 8,
        kind: Kind::None,
        identity: Identity::EMPTY,
        bulk_in: None,
        bulk_out: None,
        bulk_in_toggle: false,
        bulk_out_toggle: false,
        hub_ports: 0,
        interfaces: [HidInterface::EMPTY; MAX_INTERFACES],
        interface_count: 0,
    };
}

static mut HCCA: [Hcca; MAX_CONTROLLERS] = [Hcca::EMPTY; MAX_CONTROLLERS];
static mut CONTROL_ED: [Ed; MAX_CONTROLLERS] = [Ed::EMPTY; MAX_CONTROLLERS];
static mut CONTROL_TD: [[Td; CONTROL_TD_COUNT]; MAX_CONTROLLERS] =
    [[Td::EMPTY; CONTROL_TD_COUNT]; MAX_CONTROLLERS];
static mut SETUP_PACKETS: [[u8; 8]; MAX_CONTROLLERS] =
    [[0; 8]; MAX_CONTROLLERS];

static mut DEVICE_ED: [Ed; MAX_RES] = [Ed::EMPTY; MAX_RES];
static mut DEVICE_TD: [Td; MAX_RES] = [Td::EMPTY; MAX_RES];
static mut DEVICE_DUMMY: [Td; MAX_RES] = [Td::EMPTY; MAX_RES];
static mut DEVICE_BUFFER: [[u8; REPORT_BUFFER]; MAX_RES] =
    [[0; REPORT_BUFFER]; MAX_RES];

static mut DEVICES: [Device; MAX_DEVICES] = [Device::EMPTY; MAX_DEVICES];
static mut ROOT_RETRY: [u64; MAX_PORTS] = [0; MAX_PORTS];
static mut CHILD_RETRY: [[u64; MAX_HUB_PORTS + 1]; MAX_DEVICES] =
    [[0; MAX_HUB_PORTS + 1]; MAX_DEVICES];

static mut MMIO_BASES: [u32; MAX_CONTROLLERS] = [0; MAX_CONTROLLERS];
static mut PORTS: [usize; MAX_CONTROLLERS] = [0; MAX_CONTROLLERS];
static mut CONTROLLER_COUNT: usize = 0;
static mut READY: bool = false;
static mut NEXT_SCAN: u64 = 0;
static mut FOUND: bool = false;

fn ready() -> bool {
    unsafe { READY }
}

/// Был ли найден контроллер OHCI (даже если сейчас не готов).
pub fn found() -> bool {
    unsafe { FOUND }
}

unsafe fn read32(address: u32) -> u32 {
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

unsafe fn write32(address: u32, value: u32) {
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn reg(controller: usize, offset: u32) -> u32 {
    unsafe { MMIO_BASES[controller] + offset }
}

unsafe fn read_reg(controller: usize, offset: u32) -> u32 {
    unsafe { read32(reg(controller, offset)) }
}

unsafe fn write_reg(controller: usize, offset: u32, value: u32) {
    unsafe { write32(reg(controller, offset), value) }
}

fn port_register(controller: usize, local: usize) -> u32 {
    reg(controller, HC_RH_PORT_STATUS + (local as u32) * 4)
}

fn delay_millis(millis: u32) {
    let deadline = timer::deadline_millis(millis);

    while !timer::expired(deadline) {
        core::hint::spin_loop();
    }
}

fn control_ed_address(controller: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(CONTROL_ED[controller]) as u32 }
}

fn control_td_address(controller: usize, index: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(CONTROL_TD[controller][index]) as u32 }
}

fn resource_index(device: usize, interface: usize) -> usize {
    device * MAX_INTERFACES + interface
}

fn device_ed_address(resource: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(DEVICE_ED[resource]) as u32 }
}

fn device_td_address(resource: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(DEVICE_TD[resource]) as u32 }
}

fn device_dummy_address(resource: usize) -> u32 {
    unsafe { core::ptr::addr_of_mut!(DEVICE_DUMMY[resource]) as u32 }
}

/// Заполняет ED для endpoint'а.
fn init_ed(
    ed: *mut Ed,
    address: u8,
    endpoint: u8,
    direction: u32,
    low_speed: bool,
    max_packet: u16,
) {
    let max_packet = (max_packet as u32 & 0x7ff).max(1);

    unsafe {
        (*ed).control = (address as u32 & 0x7f)
            | ((endpoint as u32 & 0x0f) << 7)
            | ((direction & 0x03) << 11)
            | (if low_speed { 1 << 13 } else { 0 })
            | (max_packet << 16);
        (*ed).tail = 0;
        (*ed).head = 0;
        (*ed).next = 0;
    }
}

/// Заполняет TD. `rounding` разрешает короткий пакет.
fn init_td(
    td: *mut Td,
    buffer: u32,
    length: usize,
    has_buffer: bool,
    dp: u32,
    toggle: u32,
    rounding: bool,
) {
    let psw = TD_NOT_ACCESSED
        | TD_EC
        | TD_DI
        | dp
        | toggle
        | if rounding { TD_R } else { 0 };

    unsafe {
        (*td).next = 0;

        if has_buffer && length > 0 {
            (*td).cbp = buffer;
            (*td).be = buffer + length as u32 - 1;
        } else {
            (*td).cbp = 0;
            (*td).be = 0;
        }

        (*td).info = psw;
    }
}

fn td_condition(td: *const Td) -> u32 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*td).info)) >> 28 }
}

fn td_complete(td: *const Td) -> bool {
    // 0..=0xB — реальные condition code, 0xC..0xF — TD не обработан.
    td_condition(td) <= 0x0b
}

/// Вычисляет длину фактически переданных данных.
///
/// По завершении TD контроллер может обнулить CBP; тогда длина считается
/// по BE (так же поступает Linux в ohci-q.c).
fn transferred_length(td: *const Td, base: u32, limit: usize) -> usize {
    let cbp =
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*td).cbp)) };
    let be = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*td).be)) };

    if cbp == 0 {
        if be >= base {
            ((be - base + 1) as usize).min(limit)
        } else {
            0
        }
    } else {
        (cbp.saturating_sub(base) as usize).min(limit)
    }
}

fn service_done_head(controller: usize) {
    unsafe {
        let status = read_reg(controller, HC_INTERRUPT_STATUS);

        if status & INTR_WDH != 0 {
            write_reg(controller, HC_INTERRUPT_STATUS, INTR_WDH);
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(HCCA[controller].done),
                0,
            );
        }
    }
}

/// Включает или выключает списки control/bulk, сохраняя periodic.
fn set_lists(controller: usize, control: bool, bulk: bool) {
    let mut value = CTRL_HCFS_OPERATIONAL | CTRL_PLE;

    if control {
        value |= CTRL_CLE;
    }

    if bulk {
        value |= CTRL_BLE;
    }

    unsafe {
        write_reg(controller, HC_CONTROL, value);
    }
}

/// Сбрасывает контроллер и создаёт пустое расписание.
fn reset_controller(controller: usize) {
    unsafe {
        write_reg(controller, HC_INTERRUPT_DISABLE, 0x7fff_ffff);
        write_reg(controller, HC_COMMAND_STATUS, CMD_HCR);

        let deadline = timer::deadline_millis(200);

        while read_reg(controller, HC_COMMAND_STATUS) & CMD_HCR != 0 {
            if timer::expired(deadline) {
                break;
            }

            core::hint::spin_loop();
        }

        write_reg(controller, HC_INTERRUPT_STATUS, 0x7fff_ffff);
        write_reg(
            controller,
            HC_HCCA,
            core::ptr::addr_of_mut!(HCCA[controller]) as u32,
        );
        write_reg(controller, HC_FM_INTERVAL, 0x277f);
        write_reg(controller, HC_PERIODIC_START, 0x2380);
        write_reg(controller, HC_LS_THRESHOLD, 0x0628);
        write_reg(controller, HC_CONTROL_HEAD_ED, 0);
        write_reg(controller, HC_CONTROL_CURRENT_ED, 0);
        write_reg(controller, HC_BULK_HEAD_ED, 0);
        write_reg(controller, HC_BULK_CURRENT_ED, 0);

        HCCA[controller] = Hcca::EMPTY;

        for device in 0..MAX_DEVICES {
            for interface in 0..MAX_INTERFACES {
                let resource = resource_index(device, interface);
                DEVICE_ED[resource] = Ed::EMPTY;
                DEVICE_TD[resource] = Td::EMPTY;
                DEVICE_DUMMY[resource] = Td::EMPTY;
                DEVICE_BUFFER[resource] = [0; REPORT_BUFFER];
            }

            DEVICES[device] = Device::EMPTY;
            CHILD_RETRY[device] = [0; MAX_HUB_PORTS + 1];
        }

        for local in 0..MAX_PORTS {
            ROOT_RETRY[local] = 0;
        }

        let descriptor = read_reg(controller, HC_RH_DESCRIPTOR_A);
        let port_count = (descriptor & RH_A_NDP) as usize;

        PORTS[controller] = port_count.min(MAX_PORTS_PER_CONTROLLER);

        let power_on = ((descriptor >> RH_A_POTPGT_SHIFT) & 0xff) * 2;
        let no_power_switching = descriptor & RH_A_NPS != 0;

        for local in 0..PORTS[controller] {
            if !no_power_switching {
                write32(port_register(controller, local), RH_PS_PPS);
            }
        }

        if power_on > 0 {
            delay_millis(power_on.max(20));
        } else {
            delay_millis(20);
        }

        // Periodic schedule пуст, но PLE уже можно включить.
        write_reg(controller, HC_CONTROL, CTRL_HCFS_OPERATIONAL | CTRL_PLE);
        write_reg(controller, HC_INTERRUPT_ENABLE, INTR_WDH);

        fence(Ordering::SeqCst);
    }

    delay_millis(10);
}

/// Инициализирует все найденные контроллеры OHCI.
pub fn init() -> bool {
    let mut count = 0usize;

    for index in 0..pci::count() {
        let Some(device) = pci::device(index) else {
            continue;
        };

        // class 0x0C / subclass 0x03 / prog_if 0x10 — OHCI.
        if device.class != 0x0c
            || device.subclass != 0x03
            || device.prog_if != 0x10
        {
            continue;
        }

        let bar = device.bar[0];

        if bar & 1 != 0 || bar == 0 {
            continue;
        }

        let base = bar & !0x0f;

        if base == 0 || !paging::map_identity_large(base) {
            continue;
        }

        if count >= MAX_CONTROLLERS {
            continue;
        }

        // Memory space и bus mastering.
        let command = pci::read_u16(device.bus, device.slot, device.func, 0x04);
        pci::write_u16(
            device.bus,
            device.slot,
            device.func,
            0x04,
            command | 0x0006,
        );

        unsafe {
            MMIO_BASES[count] = base;
        }

        count += 1;
    }

    unsafe {
        CONTROLLER_COUNT = count;
        READY = false;
        NEXT_SCAN = 0;
        FOUND = count > 0;
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

fn allocate_address(device: usize) -> Option<u8> {
    unsafe {
        for address in 1..=127u8 {
            let mut used = false;

            for index in 0..MAX_DEVICES {
                if index != device
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

fn free_device() -> Option<usize> {
    unsafe {
        for index in 0..MAX_DEVICES {
            if !DEVICES[index].present {
                return Some(index);
            }
        }
    }

    None
}

/// Ищет устройство, подключённое к заданному порту.
fn find_device(controller: usize, parent: u8, port: u8) -> Option<usize> {
    unsafe {
        for index in 0..MAX_DEVICES {
            let device = &DEVICES[index];

            if device.present
                && device.controller == controller
                && device.parent == parent
                && device.port == port
            {
                return Some(index);
            }
        }
    }

    None
}

/// Сбрасывает корневой порт. Возвращает признак low-speed устройства.
fn reset_port(controller: usize, local: usize) -> Option<bool> {
    let register = port_register(controller, local);

    unsafe {
        let mut status = read32(register);

        if status & RH_PS_CCS == 0 {
            return None;
        }

        if status & RH_PS_PPS == 0 {
            write32(register, RH_PS_PPS);
            delay_millis(20);
        }

        // Начинаем reset.
        write32(register, RH_PS_PPS | RH_PS_PRS);

        let deadline = timer::deadline_millis(200);

        loop {
            status = read32(register);

            if status & RH_PS_PRS == 0 {
                break;
            }

            if timer::expired(deadline) {
                return None;
            }

            core::hint::spin_loop();
        }

        // Порт после reset нужно включить программно.
        if status & RH_PS_PES == 0 {
            write32(register, RH_PS_PPS | RH_PS_PES);
        }

        let deadline = timer::deadline_millis(100);

        loop {
            status = read32(register);

            if status & RH_PS_PES != 0 {
                break;
            }

            if timer::expired(deadline) {
                return None;
            }

            core::hint::spin_loop();
        }

        // Сбрасываем накопленные change-биты.
        write32(register, status & RH_CHANGE_BITS);

        Some(status & RH_PS_LSDA != 0)
    }
}

/// Выполняет управляющую передачу через control list.
fn control_transfer(
    controller: usize,
    address: u8,
    low_speed: bool,
    setup: &[u8; 8],
    data: Option<*mut u8>,
    data_length: usize,
    data_in: bool,
    max_packet: u16,
) -> Option<usize> {
    if data_length > 4096 {
        return None;
    }

    let td_count = if data_length == 0 { 2 } else { 3 };

    if td_count > CONTROL_DUMMY {
        return None;
    }

    unsafe {
        SETUP_PACKETS[controller] = *setup;

        let setup_td = control_td_address(controller, 0);
        let setup_buffer =
            core::ptr::addr_of!(SETUP_PACKETS[controller]) as u32;

        init_td(
            core::ptr::addr_of_mut!(CONTROL_TD[controller][0]),
            setup_buffer,
            8,
            true,
            TD_DP_SETUP,
            TD_T_DATA0,
            false,
        );

        let mut data_td = 0u32;

        if data_length > 0 {
            data_td = control_td_address(controller, 1);

            init_td(
                core::ptr::addr_of_mut!(CONTROL_TD[controller][1]),
                data.unwrap_or(core::ptr::null_mut()) as u32,
                data_length,
                true,
                if data_in { TD_DP_IN } else { TD_DP_OUT },
                TD_T_DATA1,
                data_in,
            );
        }

        let status_index = td_count - 1;
        let status_td = control_td_address(controller, status_index);

        init_td(
            core::ptr::addr_of_mut!(CONTROL_TD[controller][status_index]),
            0,
            0,
            false,
            if data_in && data_length > 0 {
                TD_DP_OUT
            } else {
                TD_DP_IN
            },
            TD_T_DATA1,
            false,
        );

        // Связываем TD в цепочку.
        if data_length > 0 {
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(CONTROL_TD[controller][0].next),
                data_td,
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(CONTROL_TD[controller][1].next),
                status_td,
            );
        } else {
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(CONTROL_TD[controller][0].next),
                status_td,
            );
        }

        let dummy_td = control_td_address(controller, CONTROL_DUMMY);

        // Последний реальный TD ссылается на пустой dummy, а TailP
        // указывает на него: контроллер останавливается, не выполняя dummy.
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(CONTROL_TD[controller][status_index].next),
            dummy_td,
        );
        CONTROL_TD[controller][CONTROL_DUMMY] = Td::EMPTY;

        let ed = core::ptr::addr_of_mut!(CONTROL_ED[controller]);
        init_ed(ed, address, 0, 0, low_speed, max_packet);
        (*ed).tail = dummy_td;
        (*ed).head = setup_td;

        fence(Ordering::SeqCst);

        set_lists(controller, false, false);
        write_reg(
            controller,
            HC_CONTROL_HEAD_ED,
            control_ed_address(controller),
        );
        write_reg(controller, HC_CONTROL_CURRENT_ED, 0);
        set_lists(controller, true, false);
        write_reg(controller, HC_COMMAND_STATUS, CMD_CLF);

        let deadline = timer::deadline_millis(500);

        loop {
            let head =
                core::ptr::read_volatile(core::ptr::addr_of!((*ed).head));

            if head & 1 != 0 {
                set_lists(controller, false, false);
                return None;
            }

            if head == 0 || head & !0x0f == dummy_td {
                break;
            }

            if timer::expired(deadline) {
                set_lists(controller, false, false);
                return None;
            }

            core::hint::spin_loop();
        }

        set_lists(controller, false, false);

        for index in 0..td_count {
            let td = core::ptr::addr_of!(CONTROL_TD[controller][index]);

            if !td_complete(td) || td_condition(td) != 0 {
                return None;
            }
        }

        if data_length == 0 {
            return Some(0);
        }

        let td = core::ptr::addr_of!(CONTROL_TD[controller][1]);
        let base = data.unwrap_or(core::ptr::null_mut()) as u32;

        Some(transferred_length(td, base, data_length))
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
    max_packet: u16,
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
    max_packet: u16,
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

fn clear_halt(device: &Device, endpoint: u8) {
    let _ = control_write(
        device.controller,
        device.address,
        device.low_speed,
        0x02,
        1,
        0,
        endpoint as u16,
        &[],
        device.control_max_packet,
    );
}

/// Выполняет bulk-передачу через bulk list.
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
    let length = buffer.len();

    if length == 0 {
        return Some(0);
    }

    if length > 0x7fff {
        return None;
    }

    unsafe {
        let td = core::ptr::addr_of_mut!(CONTROL_TD[controller][0]);
        init_td(
            td,
            buffer.as_mut_ptr() as u32,
            length,
            true,
            if data_in { TD_DP_IN } else { TD_DP_OUT },
            if toggle { TD_T_DATA1 } else { TD_T_DATA0 },
            data_in,
        );

        let dummy = control_td_address(controller, CONTROL_DUMMY);

        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(CONTROL_TD[controller][0].next),
            dummy,
        );
        CONTROL_TD[controller][CONTROL_DUMMY] = Td::EMPTY;

        let ed = core::ptr::addr_of_mut!(CONTROL_ED[controller]);
        init_ed(
            ed,
            address,
            endpoint,
            if data_in { 2 } else { 1 },
            low_speed,
            max_packet,
        );
        (*ed).tail = dummy;
        (*ed).head = control_td_address(controller, 0);

        fence(Ordering::SeqCst);

        set_lists(controller, false, false);
        write_reg(
            controller,
            HC_BULK_HEAD_ED,
            control_ed_address(controller),
        );
        write_reg(controller, HC_BULK_CURRENT_ED, 0);
        set_lists(controller, false, true);
        write_reg(controller, HC_COMMAND_STATUS, CMD_BLF);

        let deadline = timer::deadline_millis(500);

        loop {
            let head =
                core::ptr::read_volatile(core::ptr::addr_of!((*ed).head));

            if head & 1 != 0 {
                set_lists(controller, false, false);
                return None;
            }

            if head == 0 || head & !0x0f == dummy {
                break;
            }

            if timer::expired(deadline) {
                set_lists(controller, false, false);
                return None;
            }

            core::hint::spin_loop();
        }

        set_lists(controller, false, false);

        if !td_complete(td) || td_condition(td) != 0 {
            return None;
        }

        if data_in {
            let base = buffer.as_mut_ptr() as u32;

            Some(transferred_length(td, base, length))
        } else {
            Some(length)
        }
    }
}

/// Настраивает periodic ED для чтения одного HID-отчёта.
fn arm_interface(resource: usize, device: &Device, hid: &HidInterface) {
    unsafe {
        let td = core::ptr::addr_of_mut!(DEVICE_TD[resource]);
        let ed = core::ptr::addr_of_mut!(DEVICE_ED[resource]);
        let buffer =
            core::ptr::addr_of_mut!(DEVICE_BUFFER[resource]) as u32;

        init_td(
            td,
            buffer,
            hid.report_len,
            true,
            TD_DP_IN,
            if hid.toggle { TD_T_DATA1 } else { TD_T_DATA0 },
            true,
        );

        let dummy = device_dummy_address(resource);
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(DEVICE_TD[resource].next),
            dummy,
        );
        DEVICE_DUMMY[resource] = Td::EMPTY;

        init_ed(
            ed,
            device.address,
            hid.endpoint,
            2,
            device.low_speed,
            hid.max_packet,
        );
        (*ed).tail = dummy;
        (*ed).head = device_td_address(resource);

        fence(Ordering::SeqCst);
    }
}

/// Перестраивает цепочку periodic ED и прописывает её во все слоты.
fn rebuild_periodic(controller: usize) {
    unsafe {
        let mut head = 0u32;

        for device in (0..MAX_DEVICES).rev() {
            let device_info = &DEVICES[device];

            if !device_info.present
                || device_info.controller != controller
                || device_info.interface_count == 0
            {
                continue;
            }

            for interface in (0..device_info.interface_count as usize).rev() {
                let resource = resource_index(device, interface);
                DEVICE_ED[resource].next = head;
                head = device_ed_address(resource);
            }
        }

        for slot in 0..PERIODIC_SLOTS {
            HCCA[controller].interrupt[slot] = head;
        }

        fence(Ordering::SeqCst);
    }
}

fn clear_device_resources(device: usize) {
    unsafe {
        for interface in 0..MAX_INTERFACES {
            let resource = resource_index(device, interface);
            DEVICE_ED[resource] = Ed::EMPTY;
            DEVICE_TD[resource] = Td::EMPTY;
            DEVICE_DUMMY[resource] = Td::EMPTY;
            DEVICE_BUFFER[resource] = [0; REPORT_BUFFER];
        }

        DEVICES[device] = Device::EMPTY;
        CHILD_RETRY[device] = [0; MAX_HUB_PORTS + 1];
    }
}

/// Удаляет устройство и всех его потомков (если это был хаб).
fn remove_device(device: usize) {
    let controller = unsafe { DEVICES[device].controller };

    loop {
        let mut child = None;

        unsafe {
            for index in 0..MAX_DEVICES {
                if DEVICES[index].present && DEVICES[index].parent == device as u8 {
                    child = Some(index);
                    break;
                }
            }
        }

        match child {
            Some(index) => remove_device(index),
            None => break,
        }
    }

    clear_device_resources(device);

    rebuild_periodic(controller);
}

fn hub_port_status(device: &Device, port: u8) -> Option<(u16, u16)> {
    let mut data = [0u8; 4];

    control_read(
        device.controller,
        device.address,
        device.low_speed,
        0xa3,
        HUB_GET_STATUS,
        0,
        port as u16,
        &mut data,
        device.control_max_packet,
    )?;

    Some((
        u16::from_le_bytes([data[0], data[1]]),
        u16::from_le_bytes([data[2], data[3]]),
    ))
}

fn hub_set_feature(device: &Device, port: u8, feature: u16) -> bool {
    control_write(
        device.controller,
        device.address,
        device.low_speed,
        0x23,
        HUB_SET_FEATURE,
        feature,
        port as u16,
        &[],
        device.control_max_packet,
    )
    .is_some()
}

fn hub_clear_feature(device: &Device, port: u8, feature: u16) -> bool {
    control_write(
        device.controller,
        device.address,
        device.low_speed,
        0x23,
        HUB_CLEAR_FEATURE,
        feature,
        port as u16,
        &[],
        device.control_max_packet,
    )
    .is_some()
}

/// Сбрасывает порт хаба и возвращает признак low-speed устройства.
fn hub_reset_port(device: &Device, port: u8) -> Option<bool> {
    if !hub_set_feature(device, port, HUB_PORT_RESET) {
        return None;
    }

    delay_millis(50);

    let deadline = timer::deadline_millis(300);

    loop {
        let (status, change) = hub_port_status(device, port)?;

        if change & HUB_CHANGE_RESET != 0 {
            let _ = hub_clear_feature(device, port, HUB_C_PORT_RESET);

            if status & HUB_PORT_CONNECTED == 0 {
                return None;
            }

            delay_millis(10);

            // После сброса читаем состояние ещё раз: низкоскоростной
            // признак выставляется только для подключённого устройства.
            let (status, _) = hub_port_status(device, port)?;

            return Some(status & HUB_PORT_LOW_SPEED != 0);
        }

        if timer::expired(deadline) {
            return None;
        }

        core::hint::spin_loop();
    }
}

fn is_hub(configuration: &[u8], descriptor: &[u8]) -> bool {
    if descriptor.len() >= 5 && descriptor[4] == HUB_CLASS {
        return true;
    }

    let mut offset = 0usize;

    while offset + 2 <= configuration.len() {
        let length = configuration[offset] as usize;
        let kind = configuration[offset + 1];

        if length < 2 || offset + length > configuration.len() {
            break;
        }

        if kind == 0x04 && length >= 9 && configuration[offset + 5] == HUB_CLASS
        {
            return true;
        }

        offset += length;
    }

    false
}

/// Перечисляет устройство в слоте `slot`.
fn enumerate(
    controller: usize,
    slot: usize,
    low_speed: bool,
    parent: u8,
    port: u8,
) -> Option<()> {
    let address = allocate_address(slot)?;

    let mut header = [0u8; 8];

    if control_read(
        controller,
        0,
        low_speed,
        0x80,
        6,
        0x0100,
        0,
        &mut header,
        8,
    )? < 8
    {
        return None;
    }

    let max_packet = match header[7] {
        8 | 16 | 32 | 64 => header[7] as u16,
        _ => 8,
    };

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

    let control_max_packet = u16::from_le_bytes([
        device_descriptor[7],
        device_descriptor[8],
    ]);

    let control_max_packet = match control_max_packet {
        8 | 16 | 32 | 64 => control_max_packet,
        _ => max_packet,
    };

    let empty_endpoint = hid::Endpoint {
        kind: Kind::None,
        interface: 0,
        endpoint: 0,
        max_packet: 8,
        boot: false,
    };
    let mut endpoints = [empty_endpoint; MAX_INTERFACES];
    let interface_count = hid::find_all(
        &configuration[..total_length],
        &mut endpoints,
    );
    let mut interfaces = [HidInterface::EMPTY; MAX_INTERFACES];

    for index in 0..interface_count {
        interfaces[index] = HidInterface {
            kind: endpoints[index].kind,
            interface: endpoints[index].interface,
            endpoint: endpoints[index].endpoint,
            max_packet: endpoints[index].max_packet,
            toggle: false,
            previous_modifiers: 0,
            previous_keys: [0; 6],
            layout: hid::Layout::boot(endpoints[index].kind),
            report_len: 8,
        };
    }

    let (bulk_in, bulk_out) =
        device::find_bulk(&configuration[..total_length]);
    let identity = device::identity(&device_descriptor);
    let hub = is_hub(&configuration[..total_length], &device_descriptor);

    control_write(
        controller,
        address,
        low_speed,
        0x00,
        9,
        1,
        0,
        &[],
        control_max_packet,
    )?;

    let mut hub_ports = 0u8;
    let mut hub_power_good = 0u16;

    if hub {
        let mut hub_descriptor = [0u8; 9];

        if control_read(
            controller,
            address,
            low_speed,
            0xa0,
            6,
            0x2900,
            0,
            &mut hub_descriptor,
            control_max_packet,
        )
        .is_some()
            && hub_descriptor[0] >= 7
        {
            hub_ports = hub_descriptor[2].min(MAX_HUB_PORTS as u8);
            hub_power_good = hub_descriptor[5] as u16 * 2;
        }

        if hub_ports == 0 {
            hub_ports = 1;
        }
    }

    // Устройства без boot protocol (например, тачпад MacBook) отдают
    // отчёт в формате своего report descriptor — разбираем его.
    for index in 0..interface_count {
        if endpoints[index].boot {
            continue;
        }

        let mut descriptor = [0u8; 256];

        let Some(length) = control_read(
            controller,
            address,
            low_speed,
            0x81,
            6,
            0x2200,
            endpoints[index].interface as u16,
            &mut descriptor,
            control_max_packet,
        ) else {
            continue;
        };

        if length == 0 {
            continue;
        }

        let length = length.min(descriptor.len());

        if let Some(layout) = hid::parse_report_descriptor(
            &descriptor[..length],
            endpoints[index].kind,
        ) {
            interfaces[index].layout = layout;
            interfaces[index].report_len = layout.report_len();
        }
    }

    let kind = if interface_count > 0 {
        interfaces[0].kind
    } else if device::is_wifi(identity) {
        Kind::Wifi
    } else {
        Kind::Unsupported
    };

    let device = Device {
        present: true,
        controller,
        parent,
        port,
        address,
        low_speed,
        control_max_packet,
        kind,
        identity,
        bulk_in,
        bulk_out,
        bulk_in_toggle: false,
        bulk_out_toggle: false,
        hub_ports,
        interfaces,
        interface_count: interface_count as u8,
    };

    unsafe {
        DEVICES[slot] = device;
    }

    if hub {
        let device = unsafe { DEVICES[slot] };

        for index in 0..hub_ports {
            let _ = hub_set_feature(&device, index + 1, HUB_PORT_POWER);
        }

        delay_millis(hub_power_good.max(100) as u32);
    }

    for index in 0..interface_count {
        let endpoint = interfaces[index];

        // Boot protocol запрашиваем только у интерфейсов, которые его
        // поддерживают: остальные работают с report descriptor.
        if endpoints[index].boot {
            control_write(
                controller,
                address,
                low_speed,
                0x21,
                0x0b,
                0,
                endpoint.interface as u16,
                &[],
                control_max_packet,
            );
        }

        // Отсутствие периодических пустых отчётов.
        control_write(
            controller,
            address,
            low_speed,
            0x21,
            0x0a,
            0,
            endpoint.interface as u16,
            &[],
            control_max_packet,
        );
    }

    if interface_count > 0 {
        let device = unsafe { DEVICES[slot] };

        for index in 0..interface_count {
            let resource = resource_index(slot, index);
            arm_interface(resource, &device, &device.interfaces[index]);
        }
    }

    rebuild_periodic(controller);

    Some(())
}

fn scan_root_ports() {
    let count = unsafe { CONTROLLER_COUNT };

    for controller in 0..count {
        let ports = unsafe { PORTS[controller] };

        for local in 0..ports {
            let register = port_register(controller, local);
            let status = unsafe { read32(register) };

            if status == 0xffff_ffff {
                continue;
            }

            if status & RH_CHANGE_BITS != 0 {
                unsafe {
                    write32(register, status & RH_CHANGE_BITS);
                }
            }

            let existing = find_device(controller, NO_HUB, local as u8);
            let retry_index = controller * MAX_PORTS_PER_CONTROLLER + local;

            if status & RH_PS_CCS == 0 {
                if let Some(device) = existing {
                    remove_device(device);
                }

                unsafe {
                    ROOT_RETRY[retry_index] = 0;
                }

                continue;
            }

            if existing.is_some() {
                continue;
            }

            if unsafe {
                ROOT_RETRY[retry_index] != 0
                    && !timer::expired(ROOT_RETRY[retry_index])
            } {
                continue;
            }

            let Some(slot) = free_device() else {
                continue;
            };

            match reset_port(controller, local) {
                Some(low_speed) => {
                    if enumerate(controller, slot, low_speed, NO_HUB, local as u8)
                        .is_none()
                    {
                        unsafe {
                            clear_device_resources(slot);
                            ROOT_RETRY[retry_index] =
                                timer::deadline_millis(1000);
                        }

                        rebuild_periodic(controller);
                    } else {
                        unsafe {
                            ROOT_RETRY[retry_index] = 0;
                        }
                    }
                }
                None => {
                    unsafe {
                        ROOT_RETRY[retry_index] =
                            timer::deadline_millis(1000);
                    }
                }
            }
        }
    }
}

fn scan_hubs() {
    for slot in 0..MAX_DEVICES {
        let (present, is_hub, controller, ports) = unsafe {
            (
                DEVICES[slot].present,
                DEVICES[slot].hub_ports != 0,
                DEVICES[slot].controller,
                DEVICES[slot].hub_ports,
            )
        };

        if !present || !is_hub {
            continue;
        }

        for port in 1..=ports {
            let device = unsafe { DEVICES[slot] };
            let Some((status, change)) = hub_port_status(&device, port) else {
                continue;
            };

            if change & HUB_CHANGE_CONNECTION != 0 {
                let _ = hub_clear_feature(
                    &device,
                    port,
                    HUB_C_PORT_CONNECTION,
                );
            }

            let existing = find_device(controller, slot as u8, port);

            if status & HUB_PORT_CONNECTED == 0 {
                if let Some(index) = existing {
                    remove_device(index);
                }

                unsafe {
                    CHILD_RETRY[slot][port as usize] = 0;
                }

                continue;
            }

            if existing.is_some() {
                continue;
            }

            if unsafe {
                CHILD_RETRY[slot][port as usize] != 0
                    && !timer::expired(CHILD_RETRY[slot][port as usize])
            } {
                continue;
            }

            let Some(child) = free_device() else {
                continue;
            };

            match hub_reset_port(&device, port) {
                Some(low_speed) => {
                    if enumerate(
                        controller,
                        child,
                        low_speed,
                        slot as u8,
                        port,
                    )
                    .is_none()
                    {
                        unsafe {
                            clear_device_resources(child);
                            CHILD_RETRY[slot][port as usize] =
                                timer::deadline_millis(1000);
                        }

                        rebuild_periodic(controller);
                    } else {
                        unsafe {
                            CHILD_RETRY[slot][port as usize] = 0;
                        }
                    }
                }
                None => {
                    unsafe {
                        CHILD_RETRY[slot][port as usize] =
                            timer::deadline_millis(1000);
                    }
                }
            }
        }
    }
}

fn scan_ports() {
    if !ready() {
        return;
    }

    scan_root_ports();
    scan_hubs();
}

fn poll_interface(device: usize, interface: usize) {
    let resource = resource_index(device, interface);

    let ed = unsafe { core::ptr::addr_of_mut!(DEVICE_ED[resource]) };
    let head =
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*ed).head)) };

    if head & 1 != 0 {
        // Контроллер остановил ED из-за ошибки. Сбрасываем halt endpoint'а.
        unsafe {
            let info = DEVICES[device];
            clear_halt(&info, info.interfaces[interface].endpoint);
            DEVICES[device].interfaces[interface].toggle = false;
            let info = DEVICES[device];
            arm_interface(resource, &info, &info.interfaces[interface]);
        }

        return;
    }

    let dummy = device_dummy_address(resource);

    if head & !0x0f != dummy {
        return;
    }

    let td = unsafe { core::ptr::addr_of_mut!(DEVICE_TD[resource]) };

    if !td_complete(td) || td_condition(td) != 0 {
        unsafe {
            DEVICES[device].interfaces[interface].toggle = false;
            let info = DEVICES[device];
            arm_interface(resource, &info, &info.interfaces[interface]);
        }

        return;
    }

    let report_len = unsafe { DEVICES[device].interfaces[interface].report_len };
    let base = unsafe { core::ptr::addr_of!(DEVICE_BUFFER[resource]) as u32 };

    let length = transferred_length(td, base, report_len);

    if length > 0 {
        let mut report = [0u8; REPORT_BUFFER];

        unsafe {
            let buffer =
                core::ptr::addr_of!(DEVICE_BUFFER[resource]) as *const u8;

            for index in 0..length {
                report[index] = core::ptr::read_volatile(buffer.add(index));
            }
        }

        unsafe {
            let hid = &mut DEVICES[device].interfaces[interface];

            match hid.kind {
                Kind::Keyboard => hid::keyboard_report_layout(
                    &mut hid.previous_modifiers,
                    &mut hid.previous_keys,
                    &report[..length],
                    &hid.layout,
                ),
                Kind::Mouse => {
                    hid::mouse_report_layout(&report[..length], &hid.layout)
                }
                _ => {}
            }

            hid.toggle = !hid.toggle;
        }
    }

    unsafe {
        let info = DEVICES[device];
        arm_interface(resource, &info, &info.interfaces[interface]);
    }
}

fn poll_device(device: usize) {
    let (present, controller, interface_count) = unsafe {
        (
            DEVICES[device].present,
            DEVICES[device].controller,
            DEVICES[device].interface_count,
        )
    };

    if !present || interface_count == 0 {
        return;
    }

    service_done_head(controller);

    for interface in 0..interface_count as usize {
        poll_interface(device, interface);
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

    for device in 0..MAX_DEVICES {
        poll_device(device);
    }
}

/// Число подключённых устройств на контроллерах OHCI.
pub(crate) fn device_count() -> usize {
    let mut count = 0;

    unsafe {
        for device in 0..MAX_DEVICES {
            if DEVICES[device].present {
                count += 1;
            }
        }
    }

    count
}

/// Сведения об устройстве по его порядковому номеру среди подключённых.
pub(crate) fn device(index: usize) -> Option<super::DeviceInfo> {
    unsafe {
        let mut seen = 0;

        for device in 0..MAX_DEVICES {
            let info = &DEVICES[device];

            if !info.present {
                continue;
            }

            if seen == index {
                return Some(super::DeviceInfo {
                    kind: info.kind,
                    vendor: info.identity.vendor,
                    product: info.identity.product,
                    address: info.address,
                    bulk_in: info
                        .bulk_in
                        .map(|endpoint| (endpoint.address, endpoint.max_packet)),
                    bulk_out: info
                        .bulk_out
                        .map(|endpoint| (endpoint.address, endpoint.max_packet)),
                    low_speed: info.low_speed,
                });
            }

            seen += 1;
        }
    }

    None
}

/// Выполняет bulk-передачу по порядковому номеру устройства.
#[allow(dead_code)]
pub(crate) fn bulk(
    index: usize,
    data_in: bool,
    buffer: &mut [u8],
) -> Option<usize> {
    unsafe {
        let mut seen = 0;

        for port in 0..MAX_DEVICES {
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
                device.controller,
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
    }

    None
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

fn push_interface(
    out: &mut [u8; 80],
    len: &mut usize,
    device: &Device,
) {
    if device.interface_count == 0 {
        match device.kind {
            Kind::Wifi => push_bytes(
                out,
                len,
                device::name(device.identity).unwrap_or(b"Wi-Fi adapter"),
            ),
            Kind::Unsupported => {
                if device.hub_ports != 0 {
                    push_bytes(out, len, b"hub");
                } else {
                    push_bytes(out, len, b"unsupported device");
                }
            }
            Kind::Keyboard => push_bytes(out, len, b"keyboard"),
            Kind::Mouse => push_bytes(out, len, b"mouse"),
            Kind::None => push_bytes(out, len, b"unknown"),
        }

        return;
    }

    for interface in 0..device.interface_count as usize {
        if interface > 0 {
            push_bytes(out, len, b"+");
        }

        match device.interfaces[interface].kind {
            Kind::Keyboard => push_bytes(out, len, b"keyboard"),
            Kind::Mouse => push_bytes(out, len, b"mouse"),
            _ => push_bytes(out, len, b"hid"),
        }
    }
}

/// Печатает состояние контроллеров, портов и устройств.
pub fn print_info(start_row: usize) -> usize {
    let mut row = start_row;

    if !ready() {
        return 0;
    }

    let count = unsafe { CONTROLLER_COUNT };

    for controller in 0..count {
        let mut header = [b' '; 80];
        let mut len = 0usize;
        push_bytes(&mut header, &mut len, b"USB OHCI controller ");
        push_decimal(&mut header, &mut len, controller + 1);
        push_bytes(&mut header, &mut len, b": running");
        crate::vga::print_at(row, 0, &header[..len]);
        row += 1;

        let ports = unsafe { PORTS[controller] };

        for local in 0..ports {
            let status = unsafe { read32(port_register(controller, local)) };
            let mut line = [b' '; 80];
            let mut len = 0usize;

            push_bytes(&mut line, &mut len, b"port ");
            push_decimal(&mut line, &mut len, local + 1);
            push_bytes(&mut line, &mut len, b": ");

            match find_device(controller, NO_HUB, local as u8) {
                Some(device) => {
                    let info = unsafe { DEVICES[device] };
                    push_interface(&mut line, &mut len, &info);
                    push_bytes(&mut line, &mut len, b", address ");
                    push_decimal(&mut line, &mut len, info.address as usize);
                }
                None => push_bytes(&mut line, &mut len, b"empty"),
            }

            push_bytes(&mut line, &mut len, b" status ");
            push_hex(&mut line, &mut len, status);

            crate::vga::print_at(row, 0, &line[..len]);
            row += 1;
        }

        // Устройства за хабами не привязаны к корневым портам.
        for device in 0..MAX_DEVICES {
            let info = unsafe { DEVICES[device] };

            if !info.present
                || info.controller != controller
                || info.parent == NO_HUB
            {
                continue;
            }

            let mut line = [b' '; 80];
            let mut len = 0usize;

            push_bytes(&mut line, &mut len, b"  hub ");
            push_decimal(&mut line, &mut len, info.parent as usize + 1);
            push_bytes(&mut line, &mut len, b" port ");
            push_decimal(&mut line, &mut len, info.port as usize);
            push_bytes(&mut line, &mut len, b": ");
            push_interface(&mut line, &mut len, &info);
            push_bytes(&mut line, &mut len, b", address ");
            push_decimal(&mut line, &mut len, info.address as usize);

            if info.identity.vendor != 0 || info.identity.product != 0 {
                push_bytes(&mut line, &mut len, b", ");
                push_hex16(&mut line, &mut len, info.identity.vendor);
                push_bytes(&mut line, &mut len, b":");
                push_hex16(&mut line, &mut len, info.identity.product);
            }

            if info.low_speed {
                push_bytes(&mut line, &mut len, b", low-speed");
            }

            crate::vga::print_at(row, 0, &line[..len]);
            row += 1;

            if info.bulk_in.is_some() || info.bulk_out.is_some() {
                let mut bulk = [b' '; 80];
                let mut bulk_len = 0usize;

                push_bytes(&mut bulk, &mut bulk_len, b"  bulk");

                if let Some(endpoint) = info.bulk_in {
                    push_bytes(&mut bulk, &mut bulk_len, b" in ");
                    push_hex8(&mut bulk, &mut bulk_len, endpoint.address);
                    push_bytes(&mut bulk, &mut bulk_len, b"/");
                    push_decimal(
                        &mut bulk,
                        &mut bulk_len,
                        endpoint.max_packet as usize,
                    );
                }

                if let Some(endpoint) = info.bulk_out {
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
