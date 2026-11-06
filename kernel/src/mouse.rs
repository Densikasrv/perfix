//! Драйвер PS/2-мыши через контроллер i8042.
//!
//! Прерывания в Perfix не используются, поэтому мышь работает в режиме
//! опроса: драйвер проверяет статусный порт 0x64 и забирает байты из
//! порта 0x60 только тогда, когда они пришли от AUX-устройства.
//! Пакет PS/2-мыши состоит из трёх байт:
//!
//! 1. флаги: кнопки, знаки и переполнение смещений;
//! 2. смещение по X (знаковый байт);
//! 3. смещение по Y (знаковый байт, положительное значение — вверх).

use core::arch::asm;

use crate::keyboard::{self, Key};
use crate::vga;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const COMMAND_PORT: u16 = 0x64;

const STATUS_OUTPUT_FULL: u8 = 0x01;
const STATUS_INPUT_FULL: u8 = 0x02;
const STATUS_AUX_DATA: u8 = 0x20;

const COMMAND_READ_CONFIG: u8 = 0x20;
const COMMAND_WRITE_CONFIG: u8 = 0x60;
const COMMAND_ENABLE_AUX: u8 = 0xa8;
const COMMAND_WRITE_AUX: u8 = 0xd4;

const MOUSE_SET_DEFAULTS: u8 = 0xf6;
const MOUSE_ENABLE_REPORTING: u8 = 0xf4;
const MOUSE_ACK: u8 = 0xfa;

/// Предельное количество итераций ожидания контроллера.
const TIMEOUT: u32 = 100_000;
/// Размер пакета PS/2-мыши в байтах.
const PACKET_SIZE: usize = 3;
/// Сколько пакетов максимум разбирается за один вызов `poll`.
const PACKETS_PER_POLL: usize = 16;

/// Левая кнопка мыши.
pub const BUTTON_LEFT: u8 = 0x01;
/// Правая кнопка мыши.
pub const BUTTON_RIGHT: u8 = 0x02;
/// Средняя кнопка мыши.
pub const BUTTON_MIDDLE: u8 = 0x04;

/// Текущее состояние мыши в пиксельных координатах экрана.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseState {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
}

static mut INITIALIZED: bool = false;
static mut POSITION_X: i32 = 0;
static mut POSITION_Y: i32 = 0;
static mut BUTTONS: u8 = 0;
/// Кнопки, которые были нажаты с момента прошлого опроса.
///
/// pdesk опрашивает мышь раз в кадр, а перерисовка может занять заметное
/// время. Если нажатие и отпускание пришли одним пакетом, клик бы потерялся,
/// поэтому нажатия запоминаются до следующего чтения состояния.
static mut BUTTON_LATCH: u8 = 0;
static mut BOUNDS_WIDTH: i32 = 1024;
static mut BOUNDS_HEIGHT: i32 = 768;
static mut PACKET: [u8; PACKET_SIZE] = [0; PACKET_SIZE];
static mut PACKET_LEN: usize = 0;

fn inb(port: u16) -> u8 {
    let value: u8;

    unsafe {
        asm!(
            "in al, dx",
            out("al") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }

    value
}

fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Ждёт, пока контроллер освободит входной буфер (можно писать команду).
fn wait_input_clear() -> bool {
    for _ in 0..TIMEOUT {
        if inb(STATUS_PORT) & STATUS_INPUT_FULL == 0 {
            return true;
        }
    }

    false
}

/// Ждёт байт от нужного устройства: `aux = true` — мышь, иначе клавиатура.
///
/// Байты «чужого» устройства вычитываются и отбрасываются, иначе они
/// заблокируют контроллер.
fn wait_output_full(aux: bool) -> bool {
    for _ in 0..TIMEOUT {
        let status = inb(STATUS_PORT);

        if status & STATUS_OUTPUT_FULL == 0 {
            continue;
        }

        if (status & STATUS_AUX_DATA != 0) == aux {
            return true;
        }

        inb(DATA_PORT);
    }

    false
}

fn read_byte(aux: bool) -> Option<u8> {
    if wait_output_full(aux) {
        Some(inb(DATA_PORT))
    } else {
        None
    }
}

/// Отправляет команду мыши через AUX-порт контроллера.
fn write_mouse(byte: u8) -> bool {
    if !wait_input_clear() {
        return false;
    }

    outb(COMMAND_PORT, COMMAND_WRITE_AUX);

    if !wait_input_clear() {
        return false;
    }

    outb(DATA_PORT, byte);
    true
}

/// Отправляет команду мыши и дожидается подтверждения.
fn mouse_command(byte: u8) -> bool {
    for _ in 0..3 {
        if !write_mouse(byte) {
            continue;
        }

        match read_byte(true) {
            Some(MOUSE_ACK) => return true,
            // 0xFE — «повторите команду», прочие байты игнорируем.
            Some(_) => continue,
            None => return false,
        }
    }

    false
}

fn read_config() -> Option<u8> {
    if !wait_input_clear() {
        return None;
    }

    outb(COMMAND_PORT, COMMAND_READ_CONFIG);
    read_byte(false)
}

fn write_config(value: u8) -> bool {
    if !wait_input_clear() {
        return false;
    }

    outb(COMMAND_PORT, COMMAND_WRITE_CONFIG);

    if !wait_input_clear() {
        return false;
    }

    outb(DATA_PORT, value);
    true
}


/// Инициализирует PS/2-мышь.
///
/// `screen_width` и `screen_height` задают границы, внутри которых
/// перемещается указатель. Возвращает `true`, если мышь ответила на команды.
pub fn init(screen_width: usize, screen_height: usize) -> bool {
    let width = if screen_width == 0 { 1024 } else { screen_width } as i32;
    let height = if screen_height == 0 { 768 } else { screen_height } as i32;

    unsafe {
        INITIALIZED = false;
        PACKET_LEN = 0;
        BUTTONS = 0;
        BUTTON_LATCH = 0;
        BOUNDS_WIDTH = width.max(1);
        BOUNDS_HEIGHT = height.max(1);
        POSITION_X = BOUNDS_WIDTH / 2;
        POSITION_Y = BOUNDS_HEIGHT / 2;
    }

    // Отбрасываем всё, что успело накопиться в буфере контроллера.
    for _ in 0..64 {
        if inb(STATUS_PORT) & STATUS_OUTPUT_FULL == 0 {
            break;
        }

        inb(DATA_PORT);
    }

    if !wait_input_clear() {
        return false;
    }

    // Включаем AUX-порт (мышь).
    outb(COMMAND_PORT, COMMAND_ENABLE_AUX);

    let config = match read_config() {
        Some(value) => value,
        None => return false,
    };

    // Бит 1 — прерывание IRQ12: выключаем, работаем опросом.
    // Бит 5 — тактирование AUX: сбрасываем, чтобы порт был активен.
    let config = config & !0x22;

    if !write_config(config) {
        return false;
    }

    if !mouse_command(MOUSE_SET_DEFAULTS) {
        return false;
    }

    if !mouse_command(MOUSE_ENABLE_REPORTING) {
        return false;
    }

    unsafe {
        INITIALIZED = true;
    }

    true
}

/// Возвращает `true`, если драйвер успешно инициализирован.
pub fn is_initialized() -> bool {
    unsafe { INITIALIZED }
}

/// Текущее состояние мыши.
pub fn state() -> MouseState {
    unsafe {
        MouseState {
            x: POSITION_X,
            y: POSITION_Y,
            buttons: BUTTONS,
        }
    }
}

/// Состояние мыши с учётом нажатий, пришедших между опросами.
///
/// Используется оболочкой: нажатия из «защёлки» отдаются один раз, даже
/// если кнопку уже отпустили, — так клик не теряется во время перерисовки.
pub fn take_state() -> MouseState {
    unsafe {
        let state = MouseState {
            x: POSITION_X,
            y: POSITION_Y,
            buttons: BUTTONS | BUTTON_LATCH,
        };

        BUTTON_LATCH = 0;
        state
    }
}

/// Меняет границы перемещения указателя (например, при входе в графику).
pub fn set_bounds(screen_width: usize, screen_height: usize) {
    let width = if screen_width == 0 { 1024 } else { screen_width } as i32;
    let height = if screen_height == 0 { 768 } else { screen_height } as i32;

    unsafe {
        BOUNDS_WIDTH = width.max(1);
        BOUNDS_HEIGHT = height.max(1);
        POSITION_X = POSITION_X.clamp(0, BOUNDS_WIDTH - 1);
        POSITION_Y = POSITION_Y.clamp(0, BOUNDS_HEIGHT - 1);
    }
}

/// Обрабатывает один байт пакета. Возвращает `true`, когда пакет собран.
fn feed(byte: u8) -> bool {
    unsafe {
        // Первый байт пакета всегда имеет единицу в бите 3 — по нему
        // синхронизируемся, если предыдущий пакет потерялся.
        if PACKET_LEN == 0 && byte & 0x08 == 0 {
            return false;
        }

        PACKET[PACKET_LEN] = byte;
        PACKET_LEN += 1;

        if PACKET_LEN < PACKET_SIZE {
            return false;
        }

        PACKET_LEN = 0;
        apply_packet(PACKET);
        true
    }
}

fn apply_packet(packet: [u8; 3]) {
    let flags = packet[0];

    // Биты 6 и 7 — переполнение смещений: пакет недостоверен.
    if flags & 0xc0 != 0 {
        return;
    }

    let dx = packet[1] as i8 as i32;
    let dy = packet[2] as i8 as i32;

    unsafe {
        let new_buttons = flags & 0x07;

        // Нажатия запоминаем: они могли прийти и уйти между опросами.
        BUTTON_LATCH |= new_buttons & !BUTTONS & 0x07;
        BUTTONS = new_buttons;

        POSITION_X = (POSITION_X + dx).clamp(0, BOUNDS_WIDTH - 1);
        // Экранная ось Y направлена вниз, а мышь сообщает смещение вверх.
        POSITION_Y = (POSITION_Y - dy).clamp(0, BOUNDS_HEIGHT - 1);
    }
}

/// Читает из контроллера все готовые пакеты мыши.
///
/// Возвращает новое состояние, если хотя бы один пакет был обработан.
pub fn poll() -> Option<MouseState> {
    if !is_initialized() {
        return None;
    }

    let mut updated = None;

    for _ in 0..PACKETS_PER_POLL {
        let status = inb(STATUS_PORT);

        // Данные клавиатуры оставляем обработчику клавиатуры.
        if status & STATUS_OUTPUT_FULL == 0 || status & STATUS_AUX_DATA == 0 {
            break;
        }

        let byte = inb(DATA_PORT);

        if feed(byte) {
            updated = Some(state());
        }
    }

    updated
}

/// Принимает байт мыши, прочитанный из контроллера другим модулем.
///
/// Клавиатура обязана забирать AUX-байты из контроллера, иначе они
/// блокируют выдачу данных клавиатуры; она передаёт их сюда.
pub(crate) fn handle_byte(byte: u8) {
    if !is_initialized() {
        return;
    }

    feed(byte);
}

/// Демонстрация работы драйвера: выводит состояние мыши в двух строках,
/// начиная с `row`, пока не будет нажата любая клавиша.
///
/// Возвращает число использованных строк экрана.
pub fn demo(row: usize) -> usize {
    vga::write_row(
        row,
        b"PS/2 mouse demo: move the mouse, press any key to exit.",
        vga::COLOR_NORMAL,
    );

    if !is_initialized() {
        vga::write_row(row + 1, b"PS/2 mouse not detected.", vga::COLOR_NORMAL);
        return 2;
    }

    let mut line = [b' '; 80];

    loop {
        poll();

        let state = state();
        let mut len = 0usize;

        push_message(&mut line, &mut len, b"x=");
        push_decimal(&mut line, &mut len, state.x);
        push_message(&mut line, &mut len, b"  y=");
        push_decimal(&mut line, &mut len, state.y);
        push_message(&mut line, &mut len, b"  buttons=");
        push_message(
            &mut line,
            &mut len,
            if state.buttons & BUTTON_LEFT != 0 {
                b"L"
            } else {
                b"-"
            },
        );
        push_message(
            &mut line,
            &mut len,
            if state.buttons & BUTTON_RIGHT != 0 {
                b"R"
            } else {
                b"-"
            },
        );
        push_message(
            &mut line,
            &mut len,
            if state.buttons & BUTTON_MIDDLE != 0 {
                b"M"
            } else {
                b"-"
            },
        );

        vga::write_row(row + 1, &line[..len], vga::COLOR_NORMAL);

        if !matches!(keyboard::read_key(), Key::None) {
            break;
        }
    }

    2
}

/// Дописывает байты в буфер, пока в нём есть место.
fn push_message(out: &mut [u8], len: &mut usize, text: &[u8]) {
    for &byte in text {
        if *len >= out.len() {
            return;
        }

        out[*len] = byte;
        *len += 1;
    }
}

/// Дописывает знаковое число в десятичном виде.
fn push_decimal(out: &mut [u8], len: &mut usize, value: i32) {
    let mut digits = [0u8; 11];
    let mut count = 0usize;
    let mut magnitude = value.unsigned_abs();

    if magnitude == 0 {
        push_message(out, len, b"0");
        return;
    }

    while magnitude > 0 && count < digits.len() {
        digits[count] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        count += 1;
    }

    if value < 0 {
        push_message(out, len, b"-");
    }

    while count > 0 {
        count -= 1;
        push_message(out, len, &[digits[count]]);
    }
}

