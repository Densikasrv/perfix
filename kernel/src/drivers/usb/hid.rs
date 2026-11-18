//! Разбор HID-отчётов клавиатуры и мыши.
//!
//! Boot-клавиатура отдаёт eight-byte отчёт: байт модификаторов и до шести
//! одновременно нажатых usage-кодов. Драйвер хранит предыдущее состояние
//! и превращает изменения в обычные PS/2 make/break-коды, поэтому
//! `keyboard.rs` продолжает отвечать за раскладку и модификаторы.
//!
//! Для интерфейсов без boot protocol (например, тачпада MacBook)
//! разбирается HID report descriptor: номер отчёта, смещения кнопок,
//! относительных координат и клавиш.

use super::device::Kind;

/// Максимальный размер одного входного отчёта, который читает драйвер.
pub(crate) const MAX_REPORT: usize = 16;

/// Boot-интерфейс клавиатуры или мыши.
#[derive(Clone, Copy)]
pub(crate) struct Endpoint {
    pub kind: Kind,
    pub interface: u8,
    pub endpoint: u8,
    pub max_packet: u16,
    /// Интерфейс поддерживает HID boot protocol.
    pub boot: bool,
}

/// Разобранная раскладка входного отчёта HID.
///
/// Boot-устройства всегда отдают фиксированный отчёт, а устройствам
/// с report protocol (например, тачпаду MacBook) нужен разбор
/// report descriptor: номер отчёта, смещения кнопок и координат.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub kind: Kind,
    pub report_id: u8,
    /// Полный размер входного отчёта в битах.
    pub report_bits: u16,
    pub buttons_bit: u16,
    pub buttons_count: u8,
    pub x_bit: u16,
    pub x_bits: u8,
    pub y_bit: u16,
    pub y_bits: u8,
    pub modifiers_bit: u16,
    pub keys_bit: u16,
    pub keys_count: u8,
}

impl Layout {
    pub const EMPTY: Self = Self {
        kind: Kind::None,
        report_id: 0,
        report_bits: 0,
        buttons_bit: 0,
        buttons_count: 0,
        x_bit: 0,
        x_bits: 0,
        y_bit: 0,
        y_bits: 0,
        modifiers_bit: 0,
        keys_bit: 0,
        keys_count: 0,
    };

    /// Раскладка boot-отчёта: 8 байт у клавиатуры, 3 байта у мыши.
    pub const fn boot(kind: Kind) -> Self {
        match kind {
            Kind::Keyboard => Self {
                kind: Kind::Keyboard,
                report_bits: 64,
                modifiers_bit: 0,
                keys_bit: 16,
                keys_count: 6,
                ..Self::EMPTY
            },
            Kind::Mouse => Self {
                kind: Kind::Mouse,
                report_bits: 24,
                buttons_bit: 0,
                buttons_count: 3,
                x_bit: 8,
                x_bits: 8,
                y_bit: 16,
                y_bits: 8,
                ..Self::EMPTY
            },
            _ => Self::EMPTY,
        }
    }

    /// Сколько байт достаточно прочитать для одного отчёта.
    pub const fn report_len(&self) -> usize {
        let bytes = (self.report_bits as usize + 7) / 8;
        let bytes = if bytes < 3 { 3 } else { bytes };

        if bytes > MAX_REPORT {
            MAX_REPORT
        } else {
            bytes
        }
    }
}

/// Находит все HID-интерфейсы клавиатуры и мыши.
///
/// На MacBook клавиатура и тачпад — два интерфейса одного USB-устройства,
/// поэтому возвращается не первый найденный интерфейс, а все.
/// Флаг `boot` показывает, можно ли переключить интерфейс в boot protocol.
pub(crate) fn find_all(
    configuration: &[u8],
    out: &mut [Endpoint],
) -> usize {
    let mut offset = 0usize;
    let mut interface: Option<(u8, Option<Kind>, bool)> = None;
    let mut count = 0usize;

    while offset + 2 <= configuration.len() {
        let length = configuration[offset] as usize;
        let descriptor_type = configuration[offset + 1];

        if length < 2 || offset + length > configuration.len() {
            break;
        }

        match descriptor_type {
            // Interface.
            0x04 if length >= 9 => {
                let number = configuration[offset + 2];
                let class = configuration[offset + 5];
                let subclass = configuration[offset + 6];
                let protocol = configuration[offset + 7];

                let kind = if class == 0x03
                    && (subclass == 0x01 || subclass == 0x00)
                {
                    match protocol {
                        1 => Some(Kind::Keyboard),
                        2 => Some(Kind::Mouse),
                        _ => None,
                    }
                } else {
                    None
                };

                interface = Some((
                    number,
                    kind,
                    subclass == 0x01 && protocol != 0,
                ));
            }

            // Endpoint.
            0x05 if length >= 7 => {
                if let Some((number, Some(kind), boot)) = interface {
                    let address = configuration[offset + 2];
                    let attributes = configuration[offset + 3];
                    let max_packet = u16::from_le_bytes([
                        configuration[offset + 4],
                        configuration[offset + 5],
                    ]) & 0x07ff;

                    if address & 0x80 != 0
                        && attributes & 0x03 == 0x03
                        && count < out.len()
                    {
                        out[count] = Endpoint {
                            kind,
                            interface: number,
                            endpoint: address & 0x0f,
                            max_packet,
                            boot,
                        };
                        count += 1;
                    }
                }
            }

            _ => {}
        }

        offset += length;
    }

    count
}

/// Ищет в конфигурации первый HID boot-интерфейс клавиатуры или мыши.
pub(crate) fn find(configuration: &[u8]) -> Option<Endpoint> {
    let mut offset = 0usize;
    let mut interface: Option<(u8, bool, Kind)> = None;

    while offset + 2 <= configuration.len() {
        let length = configuration[offset] as usize;
        let descriptor_type = configuration[offset + 1];

        if length < 2 || offset + length > configuration.len() {
            break;
        }

        match descriptor_type {
            // Interface.
            0x04 if length >= 9 => {
                let number = configuration[offset + 2];
                let class = configuration[offset + 5];
                let subclass = configuration[offset + 6];
                let protocol = configuration[offset + 7];

                let kind = if class == 0x03 && subclass == 0x01 {
                    match protocol {
                        1 => Kind::Keyboard,
                        2 => Kind::Mouse,
                        _ => Kind::Unsupported,
                    }
                } else {
                    Kind::Unsupported
                };

                interface = Some((number, kind != Kind::Unsupported, kind));
            }

            // Endpoint.
            0x05 if length >= 7 => {
                if let Some((number, is_boot, kind)) = interface {
                    let address = configuration[offset + 2];
                    let attributes = configuration[offset + 3];
                    let max_packet = u16::from_le_bytes([
                        configuration[offset + 4],
                        configuration[offset + 5],
                    ]) & 0x07ff;

                    if is_boot
                        && kind != Kind::Unsupported
                        && address & 0x80 != 0
                        && attributes & 0x03 == 0x03
                    {
                        return Some(Endpoint {
                            kind,
                            interface: number,
                            endpoint: address & 0x0f,
                            max_packet,
                            boot: true,
                        });
                    }
                }
            }

            _ => {}
        }

        offset += length;
    }

    None
}

/// Разбирает HID report descriptor и находит раскладку отчёта.
///
/// Нужен устройствам без boot protocol: они отдают отчёт в формате,
/// описанном дескриптором (например, с номером отчёта в первом байте).
/// Поддерживаются поля кнопок и относительных X/Y мыши, а также
/// модификаторы и массив usage-кодов клавиатуры.
pub(crate) fn parse_report_descriptor(
    descriptor: &[u8],
    fallback: Kind,
) -> Option<Layout> {
    const PAGE_GENERIC: u32 = 0x01;
    const PAGE_KEYBOARD: u32 = 0x07;
    const PAGE_BUTTON: u32 = 0x09;

    let mut layout = Layout::EMPTY;
    let mut kind_stack = [Kind::None; 8];
    let mut depth = 0usize;
    let mut kind = fallback;
    let mut usage_page = 0u32;
    let mut report_size = 0u32;
    let mut report_count = 0u32;
    let mut report_id = 0u8;
    let mut logical_min = 0i32;
    let mut bit_pos = 0u32;
    let mut report_bits = 0u32;

    let mut usages = [0u32; 8];
    let mut usage_count = 0usize;
    let mut usage_min = 0u32;
    let mut usage_max = 0u32;
    let mut has_range = false;

    let mut buttons_set = false;
    let mut x_set = false;
    let mut y_set = false;
    let mut modifiers_set = false;
    let mut keys_set = false;

    let mut offset = 0usize;

    while offset < descriptor.len() {
        let prefix = descriptor[offset];
        offset += 1;

        if prefix == 0xfe {
            // Длинный item: следующий байт — размер данных.
            if offset >= descriptor.len() {
                break;
            }

            let size = descriptor[offset] as usize;
            offset += 1 + size;
            continue;
        }

        let size_code = prefix & 0x03;
        let size = if size_code == 3 {
            4
        } else {
            size_code as usize
        };

        if offset + size > descriptor.len() {
            break;
        }

        let data = &descriptor[offset..offset + size];
        offset += size;

        let mut value = 0u32;

        for (index, &byte) in data.iter().enumerate() {
            value |= (byte as u32) << (index * 8);
        }

        let item_type = (prefix >> 2) & 0x03;
        let tag = prefix >> 4;

        match item_type {
            // Global.
            1 => match tag {
                0x0 => usage_page = value,
                0x1 => {
                    logical_min = match size {
                        1 => value as u8 as i8 as i32,
                        2 => value as u16 as i16 as i32,
                        _ => value as i32,
                    }
                }
                0x7 => report_size = value,
                0x8 => {
                    report_id = value as u8;
                    bit_pos = if report_id != 0 { 8 } else { 0 };
                }
                0x9 => report_count = value,
                _ => {}
            },

            // Local.
            2 => match tag {
                0x0 => {
                    if usage_count < usages.len() {
                        usages[usage_count] =
                            (usage_page << 16) | (value & 0xffff);
                        usage_count += 1;
                    }
                }
                0x1 => {
                    usage_min = (usage_page << 16) | (value & 0xffff);
                    has_range = true;
                }
                0x2 => {
                    usage_max = (usage_page << 16) | (value & 0xffff);
                    has_range = true;
                }
                _ => {}
            },

            // Main.
            0 => match tag {
                // Collection.
                0xa => {
                    if depth < kind_stack.len() {
                        kind_stack[depth] = kind;
                        depth += 1;
                    }

                    if value == 0x01 {
                        let usage = usages.first().copied().unwrap_or(0);
                        let page = usage >> 16 & 0xffff;
                        let id = usage & 0xffff;

                        if page == PAGE_GENERIC {
                            match id {
                                0x02 => kind = Kind::Mouse,
                                0x06 => kind = Kind::Keyboard,
                                _ => {}
                            }
                        }
                    }
                }

                // End Collection.
                0xc => {
                    if depth > 0 {
                        depth -= 1;
                        kind = kind_stack[depth];
                    }
                }

                // Input.
                0x8 => {
                    let end = bit_pos + report_size * report_count;
                    report_bits = report_bits.max(end);

                    // Бит 0 в item'е Input: 1 — константа, 0 — данные.
                    let is_data = value & 0x01 == 0;

                    if is_data {
                        let target = kind;
                        let page = usage_page;

                        if target == Kind::Mouse {
                            if page == PAGE_BUTTON && !buttons_set {
                                layout.buttons_bit = bit_pos as u16;
                                layout.buttons_count = report_count.min(8) as u8;
                                buttons_set = true;
                            } else if page == PAGE_GENERIC
                                && report_size >= 1
                                && report_size <= 16
                                && value & 0x04 != 0
                            {
                                for (slot, &usage) in usages
                                    .iter()
                                    .take(usage_count)
                                    .enumerate()
                                {
                                    if usage >> 16 & 0xffff != PAGE_GENERIC {
                                        continue;
                                    }

                                    let field = bit_pos
                                        + slot as u32 * report_size;

                                    match usage & 0xffff {
                                        0x30 if !x_set => {
                                            layout.x_bit = field as u16;
                                            layout.x_bits = report_size as u8;
                                            x_set = true;
                                        }
                                        0x31 if !y_set => {
                                            layout.y_bit = field as u16;
                                            layout.y_bits = report_size as u8;
                                            y_set = true;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        } else if target == Kind::Keyboard && page == PAGE_KEYBOARD
                        {
                            let modifiers = if report_size == 1
                                && (usages.iter().take(usage_count).any(|&usage| {
                                    usage >> 16 & 0xffff == PAGE_KEYBOARD
                                        && (usage & 0xffff) >= 0xe0
                                        && (usage & 0xffff) <= 0xe7
                                }) || (has_range
                                    && usage_min >> 16 & 0xffff == PAGE_KEYBOARD
                                    && usage_min & 0xffff >= 0xe0
                                    && usage_max & 0xffff <= 0xe7))
                            {
                                true
                            } else {
                                false
                            };

                            if modifiers && !modifiers_set {
                                layout.modifiers_bit = bit_pos as u16;
                                modifiers_set = true;
                            } else if report_size == 8 && !keys_set {
                                layout.keys_bit = bit_pos as u16;
                                layout.keys_count = report_count.min(6) as u8;
                                keys_set = true;
                            }
                        }
                    }

                    bit_pos = end;
                }

                _ => {}
            },

            _ => {}
        }

        // Локальные значения действуют только до следующего main item.
        if item_type == 0 {
            usage_count = 0;
            usage_min = 0;
            usage_max = 0;
            has_range = false;
        }
    }

    let kind = if kind == Kind::Mouse || kind == Kind::Keyboard {
        kind
    } else {
        fallback
    };

    let valid = match kind {
        Kind::Mouse => x_set && y_set && logical_min <= 0,
        Kind::Keyboard => modifiers_set && keys_set,
        _ => false,
    };

    if !valid {
        return None;
    }

    layout.kind = kind;
    layout.report_id = report_id;
    layout.report_bits = report_bits.max(1) as u16;

    Some(layout)
}

fn extract_bits(report: &[u8], bit: u32, count: u8) -> u32 {
    let mut value = 0u32;

    for index in 0..count as u32 {
        let position = bit + index;
        let byte = match report.get((position / 8) as usize) {
            Some(&byte) => byte,
            None => return value,
        };

        value |= (((byte >> (position % 8)) & 1) as u32) << index;
    }

    value
}

fn sign_extend(value: u32, bits: u8) -> i32 {
    if bits == 0 || bits >= 32 {
        return value as i32;
    }

    let shift = 32 - bits as u32;

    ((value << shift) as i32) >> shift
}

/// Обрабатывает отчёт мыши по разобранной раскладке.
pub(crate) fn mouse_report_layout(report: &[u8], layout: &Layout) {
    if layout.report_id != 0 && report.first() != Some(&layout.report_id) {
        return;
    }

    if layout.x_bits == 0 || layout.y_bits == 0 {
        mouse_report(report);
        return;
    }

    let buttons = extract_bits(
        report,
        layout.buttons_bit as u32,
        layout.buttons_count.min(3),
    ) as u8;
    let dx = sign_extend(
        extract_bits(report, layout.x_bit as u32, layout.x_bits),
        layout.x_bits,
    );
    // В HID положительное Y направлено вниз, а mouse.rs ждёт «вверх».
    let dy = -sign_extend(
        extract_bits(report, layout.y_bit as u32, layout.y_bits),
        layout.y_bits,
    );

    if layout.buttons_count == 0 {
        super::push_mouse(dx, dy, 0);
    } else {
        super::push_mouse(dx, dy, buttons);
    }
}

/// Обрабатывает отчёт клавиатуры по разобранной раскладке.
pub(crate) fn keyboard_report_layout(
    previous_modifiers: &mut u8,
    previous_keys: &mut [u8; 6],
    report: &[u8],
    layout: &Layout,
) {
    if layout.report_id != 0 && report.first() != Some(&layout.report_id) {
        return;
    }

    let modifiers =
        extract_bits(report, layout.modifiers_bit as u32, 8) as u8;
    let mut keys = [0u8; 6];

    for index in 0..layout.keys_count.min(6) as u32 {
        keys[index as usize] = extract_bits(
            report,
            layout.keys_bit as u32 + index * 8,
            8,
        ) as u8;
    }

    keyboard_state(previous_modifiers, previous_keys, modifiers, &keys);
}

/// Коды модификаторов HID в порядке битов байта отчёта.
const MODIFIER_SCANCODES: [(bool, u8); 8] = [
    (false, 0x1d), // Left Ctrl
    (false, 0x2a), // Left Shift
    (false, 0x38), // Left Alt
    (true, 0x5b),  // Left GUI
    (true, 0x1d),  // Right Ctrl
    (false, 0x36), // Right Shift
    (true, 0x38),  // Right Alt
    (true, 0x5c),  // Right GUI
];

fn emit_scancode((extended, code): (bool, u8), release: bool) {
    if extended {
        super::push_scancode(0xe0);
    }

    super::push_scancode(if release { code | 0x80 } else { code });
}

/// Переводит HID usage ID в скан-код PS/2 set 1.
///
/// Возвращает `(extended, code)`: для расширенных клавиш перед кодом
/// нужно передать префикс 0xE0.
fn usage_to_scancode(usage: u8) -> Option<(bool, u8)> {
    let code = match usage {
        0x04 => (false, 0x1e), // a
        0x05 => (false, 0x30), // b
        0x06 => (false, 0x2e), // c
        0x07 => (false, 0x20), // d
        0x08 => (false, 0x12), // e
        0x09 => (false, 0x21), // f
        0x0a => (false, 0x22), // g
        0x0b => (false, 0x23), // h
        0x0c => (false, 0x17), // i
        0x0d => (false, 0x24), // j
        0x0e => (false, 0x25), // k
        0x0f => (false, 0x26), // l
        0x10 => (false, 0x32), // m
        0x11 => (false, 0x31), // n
        0x12 => (false, 0x18), // o
        0x13 => (false, 0x19), // p
        0x14 => (false, 0x10), // q
        0x15 => (false, 0x13), // r
        0x16 => (false, 0x1f), // s
        0x17 => (false, 0x14), // t
        0x18 => (false, 0x16), // u
        0x19 => (false, 0x2f), // v
        0x1a => (false, 0x11), // w
        0x1b => (false, 0x2d), // x
        0x1c => (false, 0x15), // y
        0x1d => (false, 0x2c), // z

        0x1e => (false, 0x02), // 1
        0x1f => (false, 0x03), // 2
        0x20 => (false, 0x04), // 3
        0x21 => (false, 0x05), // 4
        0x22 => (false, 0x06), // 5
        0x23 => (false, 0x07), // 6
        0x24 => (false, 0x08), // 7
        0x25 => (false, 0x09), // 8
        0x26 => (false, 0x0a), // 9
        0x27 => (false, 0x0b), // 0

        0x28 => (false, 0x1c), // Enter
        0x29 => (false, 0x01), // Escape
        0x2a => (false, 0x0e), // Backspace
        0x2b => (false, 0x0f), // Tab
        0x2c => (false, 0x39), // Space
        0x2d => (false, 0x0c), // -
        0x2e => (false, 0x0d), // =
        0x2f => (false, 0x1a), // [
        0x30 => (false, 0x1b), // ]
        0x31 => (false, 0x2b), // backslash
        0x33 => (false, 0x27), // ;
        0x34 => (false, 0x28), // '
        0x35 => (false, 0x29), // `
        0x36 => (false, 0x33), // ,
        0x37 => (false, 0x34), // .
        0x38 => (false, 0x35), // /
        0x39 => (false, 0x3a), // Caps Lock

        0x3a => (false, 0x3b), // F1
        0x3b => (false, 0x3c), // F2
        0x3c => (false, 0x3d), // F3
        0x3d => (false, 0x3e), // F4
        0x3e => (false, 0x3f), // F5
        0x3f => (false, 0x40), // F6
        0x40 => (false, 0x41), // F7
        0x41 => (false, 0x42), // F8
        0x42 => (false, 0x43), // F9
        0x43 => (false, 0x44), // F10
        0x44 => (false, 0x57), // F11
        0x45 => (false, 0x58), // F12

        0x49 => (true, 0x52),  // Insert
        0x4a => (true, 0x47),  // Home
        0x4b => (true, 0x49),  // Page Up
        0x4c => (true, 0x53),  // Delete
        0x4d => (true, 0x4f),  // End
        0x4e => (true, 0x51),  // Page Down
        0x4f => (true, 0x4d),  // Right
        0x50 => (true, 0x4b),  // Left
        0x51 => (true, 0x50),  // Down
        0x52 => (true, 0x48),  // Up

        0x53 => (false, 0x45), // Num Lock
        0x54 => (true, 0x35),  // Keypad /
        0x55 => (false, 0x37), // Keypad *
        0x56 => (false, 0x4a), // Keypad -
        0x57 => (false, 0x4e), // Keypad +
        0x58 => (true, 0x1c),  // Keypad Enter
        0x59 => (false, 0x4f), // Keypad 1
        0x5a => (false, 0x50), // Keypad 2
        0x5b => (false, 0x51), // Keypad 3
        0x5c => (false, 0x4b), // Keypad 4
        0x5d => (false, 0x4c), // Keypad 5
        0x5e => (false, 0x4d), // Keypad 6
        0x5f => (false, 0x47), // Keypad 7
        0x60 => (false, 0x48), // Keypad 8
        0x61 => (false, 0x49), // Keypad 9
        0x62 => (false, 0x52), // Keypad 0
        0x63 => (false, 0x53), // Keypad .
        _ => return None,
    };

    Some(code)
}

fn contains(keys: &[u8; 6], usage: u8) -> bool {
    keys.iter().any(|&key| key == usage)
}

fn keyboard_state(
    previous_modifiers: &mut u8,
    previous_keys: &mut [u8; 6],
    modifiers: u8,
    keys: &[u8; 6],
) {
    // Модификаторы приходят битовой маской, а не usage-кодами.
    let changed = *previous_modifiers ^ modifiers;

    for bit in 0..8 {
        if changed & (1 << bit) != 0 {
            let pressed = modifiers & (1 << bit) != 0;
            emit_scancode(MODIFIER_SCANCODES[bit], !pressed);
        }
    }

    *previous_modifiers = modifiers;

    // ErrorRollOver/PostFail/ErrorUndefined: отчёт недостоверен.
    if keys.iter().any(|&key| (1..=3).contains(&key)) {
        return;
    }

    // Сначала отпускания: если клавиша заменена другой в том же слоте,
    // иначе можно получить «залипание» модификатора раскладки.
    for &old in previous_keys.iter() {
        if old != 0 && !contains(keys, old) {
            if let Some(code) = usage_to_scancode(old) {
                emit_scancode(code, true);
            }
        }
    }

    for &new in keys.iter() {
        if new != 0 && !contains(previous_keys, new) {
            if let Some(code) = usage_to_scancode(new) {
                emit_scancode(code, false);
            }
        }
    }

    *previous_keys = *keys;
}

/// Обрабатывает отчёт boot-клавиатуры.
pub(crate) fn keyboard_report(
    previous_modifiers: &mut u8,
    previous_keys: &mut [u8; 6],
    report: &[u8],
) {
    if report.len() < 8 {
        return;
    }

    let mut keys = [0u8; 6];
    keys.copy_from_slice(&report[2..8]);

    keyboard_state(previous_modifiers, previous_keys, report[0], &keys);
}

/// Обрабатывает отчёт boot-мыши.
pub(crate) fn mouse_report(report: &[u8]) {
    if report.len() < 3 {
        return;
    }

    let buttons = report[0] & 0x07;
    let dx = report[1] as i8 as i32;
    // В HID положительное Y направлено вниз, а mouse.rs ждёт «вверх».
    let dy = -(report[2] as i8 as i32);

    super::push_mouse(dx, dy, buttons);
}
