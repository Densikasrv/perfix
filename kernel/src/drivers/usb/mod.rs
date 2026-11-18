//! Поддержка USB-контроллеров, HID-клавиатур и мышей.
//!
//! Первый этап — контроллер UHCI (USB 1.1), который есть на типичных
//! машинах с чипсетами Intel/VIA и в QEMU (`piix3-usb-uhci`). Устройства
//! работают в HID boot protocol, поэтому для клавиатуры и мыши достаточно
//! восьми байт отчёта, а разбор раскладки остаётся общим с PS/2.
//!
//! Второй этап — EHCI (USB 2.0): высокоскоростные устройства. Порт с
//! full/low-speed устройством передаётся companion-контроллеру UHCI,
//! поэтому оба драйвера работают вместе.
//!
//! Кроме HID, перечисляются vendor-specific устройства: сохраняются
//! VID/PID и bulk-эндпоинты. Так опознаётся Wi-Fi адаптер TL-WN727N
//! (Realtek RTL8188EUS, `0bda:8179`); для него реализованы generic
//! bulk-передачи, но 802.11/WPA2 пока нет.
//!
//! Драйвер опросный: прерывания не используются, как и в остальных
//! устройствах Perfix. `poll` проверяет завершённые transfer descriptor'ы
//! и раскладывает события в небольшие очереди, которые забирают
//! `keyboard::read_key` и `mouse::poll`.

mod device;
mod ehci;
mod hid;
mod ohci;
mod uhci;

/// Сведения об одном подключённом USB-устройстве.
///
/// Используются драйверами и командой `usb`: контроллеры UHCI и EHCI
/// хранят устройства в своих таблицах, а этот тип даёт общий вид на них.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) struct DeviceInfo {
    pub kind: device::Kind,
    pub vendor: u16,
    pub product: u16,
    pub address: u8,
    pub bulk_in: Option<(u8, u16)>,
    pub bulk_out: Option<(u8, u16)>,
    pub low_speed: bool,
}

/// Очередь скан-кодов клавиатуры. Байты хранятся в формате PS/2
/// (с префиксом 0xE0 для расширенных клавиш), чтобы не дублировать
/// разбор раскладок и модификаторов.
const KEY_QUEUE_SIZE: usize = 128;

static mut KEY_QUEUE: [u8; KEY_QUEUE_SIZE] = [0; KEY_QUEUE_SIZE];
static mut KEY_HEAD: usize = 0;
static mut KEY_TAIL: usize = 0;
static mut KEY_COUNT: usize = 0;
static mut KEY_TOTAL: usize = 0;

/// Очередь относительных перемещений мыши.
const MOUSE_QUEUE_SIZE: usize = 32;

static mut MOUSE_QUEUE: [(i32, i32, u8); MOUSE_QUEUE_SIZE] =
    [(0, 0, 0); MOUSE_QUEUE_SIZE];
static mut MOUSE_HEAD: usize = 0;
static mut MOUSE_COUNT: usize = 0;
static mut MOUSE_TOTAL: usize = 0;

/// Инициализирует контроллеры UHCI и EHCI.
///
/// Возвращает `true`, если хотя бы один контроллер найден и запущен.
/// Отсутствие USB не является ошибкой: PS/2-клавиатура и мышь
/// продолжают работать.
pub fn init() -> bool {
    let uhci_ready = uhci::init();
    let ehci_ready = ehci::init();
    let ohci_ready = ohci::init();

    uhci_ready || ehci_ready || ohci_ready
}

/// Опрашивает контроллер и подключённые устройства.
///
/// Вызывается из обработчиков ввода; повторный вызов безопасен.
pub fn poll() {
    uhci::poll();
    ehci::poll();
    ohci::poll();
}

/// Число подключённых USB-устройств (UHCI и EHCI вместе).
pub(crate) fn device_count() -> usize {
    uhci::device_count() + ehci::device_count() + ohci::device_count()
}

/// Сведения об устройстве по порядковому номеру.
pub(crate) fn device(index: usize) -> Option<DeviceInfo> {
    let uhci_count = uhci::device_count();

    if index < uhci_count {
        return uhci::device(index);
    }

    let index = index - uhci_count;
    let ehci_count = ehci::device_count();

    if index < ehci_count {
        ehci::device(index)
    } else {
        ohci::device(index - ehci_count)
    }
}

/// Находит устройство по VID/PID и возвращает его порядковый номер.
#[allow(dead_code)]
pub(crate) fn find(vendor: u16, product: u16) -> Option<usize> {
    for index in 0..device_count() {
        if let Some(info) = device(index) {
            if info.vendor == vendor && info.product == product {
                return Some(index);
            }
        }
    }

    None
}

/// Читает данные через bulk IN первого bulk-эндпоинта устройства.
#[allow(dead_code)]
pub(crate) fn bulk_read(index: usize, buffer: &mut [u8]) -> Option<usize> {
    let uhci_count = uhci::device_count();

    if index < uhci_count {
        return uhci::bulk(index, true, buffer);
    }

    let index = index - uhci_count;
    let ehci_count = ehci::device_count();

    if index < ehci_count {
        ehci::bulk(index, true, buffer)
    } else {
        ohci::bulk(index - ehci_count, true, buffer)
    }
}

/// Передаёт данные через bulk OUT первого bulk-эндпоинта устройства.
#[allow(dead_code)]
pub(crate) fn bulk_write(index: usize, buffer: &mut [u8]) -> Option<usize> {
    let uhci_count = uhci::device_count();

    if index < uhci_count {
        return uhci::bulk(index, false, buffer);
    }

    let index = index - uhci_count;
    let ehci_count = ehci::device_count();

    if index < ehci_count {
        ehci::bulk(index, false, buffer)
    } else {
        ohci::bulk(index - ehci_count, false, buffer)
    }
}

/// Число событий, полученных от USB-мыши.
pub fn mouse_total() -> usize {
    unsafe { MOUSE_TOTAL }
}

/// Число скан-кодов, полученных от USB-клавиатуры.
pub fn key_total() -> usize {
    unsafe { KEY_TOTAL }
}

/// Печатает состояние USB-контроллеров и портов.
pub fn print_info(start_row: usize) -> usize {
    let mut row = start_row;
    let mut any = false;

    if uhci::found() {
        row += uhci::print_info(row);
        any = true;
    }

    if ehci::found() {
        row += ehci::print_info(row);
        any = true;
    }

    if ohci::found() {
        row += ohci::print_info(row);
        any = true;
    }

    if !any {
        crate::vga::print_at(row, 0, b"USB: no UHCI/EHCI/OHCI controller found");
        row += 1;
        return row - start_row;
    }

    let mut line = [b' '; 80];
    let mut len = 0usize;
    push_bytes(&mut line, &mut len, b"input: keys ");
    push_decimal(&mut line, &mut len, key_total());
    push_bytes(&mut line, &mut len, b", mouse events ");
    push_decimal(&mut line, &mut len, mouse_total());
    crate::vga::print_at(row, 0, &line[..len]);
    row += 1;

    row - start_row
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

/// Ставит скан-код PS/2 в очередь клавиатуры.
pub(crate) fn push_scancode(byte: u8) {
    unsafe {
        // При переполнении новые байты отбрасываются: потеря release-
        // события хуже, чем потеря нажатия, но очередь достаточно велика.
        if KEY_COUNT >= KEY_QUEUE_SIZE {
            return;
        }

        KEY_QUEUE[KEY_TAIL] = byte;
        KEY_TAIL = (KEY_TAIL + 1) % KEY_QUEUE_SIZE;
        KEY_COUNT += 1;
    }
}

/// Забирает один скан-код клавиатуры, если он есть.
pub(crate) fn pop_scancode() -> Option<u8> {
    unsafe {
        if KEY_COUNT == 0 {
            return None;
        }

        let byte = KEY_QUEUE[KEY_HEAD];
        KEY_HEAD = (KEY_HEAD + 1) % KEY_QUEUE_SIZE;
        KEY_COUNT -= 1;
        KEY_TOTAL = KEY_TOTAL.wrapping_add(1);
        Some(byte)
    }
}

/// Ставит событие мыши в очередь. `dy` положителен при движении вверх,
/// как и у PS/2-драйвера.
pub(crate) fn push_mouse(dx: i32, dy: i32, buttons: u8) {
    unsafe {
        // Для мыши важнее последние события, поэтому при переполнении
        // вытесняем самое старое.
        if MOUSE_COUNT >= MOUSE_QUEUE_SIZE {
            MOUSE_HEAD = (MOUSE_HEAD + 1) % MOUSE_QUEUE_SIZE;
            MOUSE_COUNT -= 1;
        }

        let index = (MOUSE_HEAD + MOUSE_COUNT) % MOUSE_QUEUE_SIZE;
        MOUSE_QUEUE[index] = (dx, dy, buttons & 0x07);
        MOUSE_COUNT += 1;
        MOUSE_TOTAL = MOUSE_TOTAL.wrapping_add(1);
    }
}

/// Забирает одно событие мыши, если оно есть.
pub(crate) fn pop_mouse() -> Option<(i32, i32, u8)> {
    unsafe {
        if MOUSE_COUNT == 0 {
            return None;
        }

        let event = MOUSE_QUEUE[MOUSE_HEAD];
        MOUSE_HEAD = (MOUSE_HEAD + 1) % MOUSE_QUEUE_SIZE;
        MOUSE_COUNT -= 1;
        Some(event)
    }
}
