//! Настройка MTRR для линейного framebuffer.
//!
//! На реальном железе (например, на ноутбуках с Intel 915GM) BIOS
//! оставляет область видеопамяти некэшируемой. Каждая запись в такой
//! framebuffer уходит отдельной транзакцией, поэтому перерисовка окна
//! при перетаскивании становится очень медленной.
//!
//! Здесь framebuffer помечается типом write-combining: процессор
//! объединяет соседние записи в одну пакетную передачу. Чтение из
//! видеопамяти при этом всё равно остаётся некэшируемым, но pdesk
//! старается читать её только при первом захвате фона.
//!
//! Всё делается консервативно: если подходящего свободного MTRR нет
//! или BIOS занял область несовместимым типом, настройка пропускается.
//! Ошибка настройки не должна мешать загрузке.

use core::arch::asm;

use crate::cpu;

/// MSR с параметрами MTRR.
const MTRR_CAP: u32 = 0x00fe;
/// MSR с типом памяти по умолчанию и битами разрешения.
const MTRR_DEF_TYPE: u32 = 0x02ff;
/// Первая пара MSR variable range: base и mask.
const MTRR_PHYS_BASE: u32 = 0x0200;

const MTRR_ENABLE: u64 = 1 << 11;
const MTRR_WC_SUPPORTED: u64 = 1 << 10;

const TYPE_UC: u64 = 0;
const TYPE_WC: u64 = 1;
const TYPE_WT: u64 = 4;

/// Больше 256 МиБ видеопамяти на 32-битной машине не бывает; ограничение
/// защищает от случайного перекрытия всей физической памяти.
const MAX_WC_SIZE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
struct State {
    active: bool,
    base: u64,
    size: u64,
}

static mut STATE: State = State {
    active: false,
    base: 0,
    size: 0,
};

unsafe fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;

    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags)
        );
    }

    ((high as u64) << 32) | low as u64
}

/// Сбрасывает кэши после изменения MTRR.
unsafe fn wbinvd() {
    unsafe {
        asm!("wbinvd", options(nostack, preserves_flags));
    }
}

unsafe fn wrmsr(msr: u32, value: u64) {
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack, preserves_flags)
        );
    }
}

fn has_mtrr() -> bool {
    let (_, _, _, edx) = cpu::cpuid(1);

    edx & (1 << 12) != 0
}

/// Сколько физических адресных битов поддерживает процессор.
fn physical_address_bits() -> u32 {
    let (max_leaf, _, _, _) = cpu::cpuid(0x8000_0000);

    if max_leaf >= 0x8000_0008 {
        let (eax, _, _, _) = cpu::cpuid(0x8000_0008);
        let bits = eax & 0xff;

        if (32..=52).contains(&bits) {
            return bits;
        }
    }

    36
}

/// Кодирует диапазон `[base, base + size)` в MSR mask.
fn mask_for(size: u64, address_bits: u32) -> u64 {
    let address_mask = if address_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << address_bits) - 1
    };

    ((!(size - 1)) & address_mask & !0xfff) | MTRR_ENABLE
}

/// Разбирает включённый variable range.
///
/// Возвращает `(base, size, type)`. Размер вычисляется по младшему
/// установленному биту маски.
fn decode_range(base_msr: u64, mask_msr: u64) -> Option<(u64, u64, u64)> {
    if mask_msr & MTRR_ENABLE == 0 {
        return None;
    }

    let base = base_msr & !0xfff;
    let mask = mask_msr & !0xfff;

    if mask == 0 {
        return None;
    }

    // Младший установленный бит маски задаёт размер диапазона.
    let size = 1u64 << mask.trailing_zeros();

    Some((base, size, base_msr & 0xff))
}

/// Возвращает `true`, если диапазоны пересекаются.
fn overlaps(a_base: u64, a_size: u64, b_base: u64, b_size: u64) -> bool {
    a_base < b_base + b_size && b_base < a_base + a_size
}

/// Включает write-combining для диапазона framebuffer.
///
/// `address` и `size` берутся из Multiboot. Диапазон расширяется до
/// степени двойки с выравниванием вниз, как того требует MTRR.
pub fn init(address: u64, size: u64) -> bool {
    if address == 0 || size == 0 || address > u32::MAX as u64 {
        return false;
    }

    if !has_mtrr() {
        return false;
    }

    let cap = unsafe { rdmsr(MTRR_CAP) };

    if cap & MTRR_WC_SUPPORTED == 0 {
        return false;
    }

    let variable_count = (cap & 0xff) as usize;

    if variable_count == 0 {
        return false;
    }

    let def_type = unsafe { rdmsr(MTRR_DEF_TYPE) };

    // Если MTRR выключены, включение типа по умолчанию может изменить
    // отображение всей памяти. Не трогаем такую конфигурацию.
    if def_type & MTRR_ENABLE == 0 {
        return false;
    }

    let mut wc_size = 1u64;

    while wc_size < size {
        wc_size <<= 1;
    }

    if wc_size > MAX_WC_SIZE {
        return false;
    }

    let wc_base = address & !(wc_size - 1);
    let wc_end = wc_base + wc_size;

    if wc_end > (1u64 << physical_address_bits()) {
        return false;
    }

    let mut entries = [(0u64, 0u64); 16];
    let count = variable_count.min(entries.len());

    for index in 0..count {
        entries[index] = unsafe {
            (
                rdmsr(MTRR_PHYS_BASE + (index as u32) * 2),
                rdmsr(MTRR_PHYS_BASE + (index as u32) * 2 + 1),
            )
        };
    }

    let mut exact: Option<usize> = None;
    let mut containing: Option<(usize, u64, u64, u64)> = None;
    let mut conflict = false;

    for index in 0..count {
        let Some((base, range, kind)) = decode_range(entries[index].0, entries[index].1) else {
            continue;
        };

        if !overlaps(base, range, wc_base, wc_size) {
            continue;
        }

        if base == wc_base && range == wc_size {
            exact = Some(index);
            continue;
        }

        if kind == TYPE_UC || kind == TYPE_WT {
            if kind == TYPE_UC
                && containing.is_none()
                && base <= wc_base
                && base + range >= wc_end
                && range <= MAX_WC_SIZE
            {
                containing = Some((index, base, range, kind));
            } else {
                conflict = true;
            }
        }
        // WB и WC совместимы: WC имеет приоритет над WB.
    }

    if conflict {
        return false;
    }

    if let Some(index) = exact {
        let kind = entries[index].0 & 0xff;

        if kind == TYPE_WC {
            set_state(wc_base, wc_size);
            return true;
        }

        if kind == TYPE_UC {
            let value = (entries[index].0 & !0xff) | TYPE_WC;

            unsafe {
                wrmsr(MTRR_PHYS_BASE + (index as u32) * 2, value);
                wbinvd();
            }

            set_state(wc_base, wc_size);
            return true;
        }

        // WT и прочие типы не переопределяются безопасно.
        return false;
    }

    if let Some((index, base, range, _)) = containing {
        // BIOS часто помечает весь графический апертур как UC. Если
        // диапазон начинается с memory BAR видеокарты (или с самого
        // framebuffer), он заведомо относится к видеопамяти и его можно
        // перевести в WC целиком.
        if base == wc_base || graphics_bar_start(base) {
            let value = (entries[index].0 & !0xff) | TYPE_WC;

            unsafe {
                wrmsr(MTRR_PHYS_BASE + (index as u32) * 2, value);
                wbinvd();
            }

            set_state(base, range);
            return true;
        }

        return false;
    }

    // Все пересечения — WB (или их нет): добавляем отдельный WC range.
    let mut free = None;

    for index in 0..count {
        if entries[index].1 & MTRR_ENABLE == 0 {
            free = Some(index);
            break;
        }
    }

    let Some(index) = free else {
        return false;
    };

    let base_value = wc_base | TYPE_WC;
    let mask_value = mask_for(wc_size, physical_address_bits());

    unsafe {
        wrmsr(MTRR_PHYS_BASE + (index as u32) * 2, base_value);
        wrmsr(MTRR_PHYS_BASE + (index as u32) * 2 + 1, mask_value);
        wbinvd();
    }

    // Не даём процессору выполнять спекулятивные обращения по старому
    // отображению сразу после изменения MTRR. `mfence` появился только
    // вместе с SSE2 (Pentium 4), поэтому на Pentium II и более старых
    // процессорах используем CPUID — он тоже сериализует конвейер и
    // доступ к памяти.
    serialize();

    set_state(wc_base, wc_size);
    true
}

/// Полный барьер после изменения MTRR.
///
/// `cpuid` доступен на всех процессорах, где вообще есть MTRR, и
/// является сериализующей инструкцией, в отличие от `mfence`.
fn serialize() {
    let _ = crate::cpu::cpuid(0);
}

/// Совпадает ли начало диапазона с memory BAR видеокарты.
///
/// BIOS нередко помечает как UC всю графическую апертуру, а не только
/// видимую часть framebuffer. Такой диапазон безопасно перевести в WC
/// целиком: он принадлежит видеокарте и не пересекается с RAM.
fn graphics_bar_start(base: u64) -> bool {
    if base > u32::MAX as u64 {
        return false;
    }

    for index in 0..crate::pci::count() {
        let Some(device) = crate::pci::device(index) else {
            continue;
        };

        if device.class != 0x03 {
            continue;
        }

        for &bar in device.bar.iter() {
            if bar & 1 != 0 {
                continue;
            }

            if (bar & !0x0f) as u64 == base {
                return true;
            }
        }
    }

    false
}

fn set_state(base: u64, size: u64) {
    unsafe {
        STATE = State {
            active: true,
            base,
            size,
        };
    }
}

/// Включён ли write-combining для framebuffer.
pub fn active() -> bool {
    unsafe { STATE.active }
}

/// Диапазон, переведённый в write-combining: `(base, size)`.
pub fn range() -> (u64, u64) {
    unsafe { (STATE.base, STATE.size) }
}

/// Тип памяти, действующий для framebuffer, для диагностики.
pub fn status() -> &'static [u8] {
    if active() {
        b"write-combining"
    } else {
        b"uncached"
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

fn push_decimal(out: &mut [u8; 80], len: &mut usize, value: u64) {
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

fn push_hex(out: &mut [u8; 80], len: &mut usize, value: u64) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    push_bytes(out, len, b"0x");

    for shift in [28, 24, 20, 16, 12, 8, 4, 0] {
        push_bytes(out, len, &[HEX[((value >> shift) & 0xf) as usize]]);
    }
}

fn type_name(kind: u64) -> &'static [u8] {
    match kind {
        TYPE_UC => b"UC",
        TYPE_WC => b"WC",
        TYPE_WT => b"WT",
        5 => b"WP",
        6 => b"WB",
        _ => b"?",
    }
}

/// Печатает состояние MTRR и variable ranges.
pub fn print_info(start_row: usize) -> usize {
    let mut row = start_row;
    let mut line = [b' '; 80];
    let mut len = 0usize;

    if !has_mtrr() {
        crate::vga::print_at(row, 0, b"MTRR: not supported by CPU");
        return 1;
    }

    let cap = unsafe { rdmsr(MTRR_CAP) };
    let def_type = unsafe { rdmsr(MTRR_DEF_TYPE) };

    push_bytes(&mut line, &mut len, b"MTRR: cap ");
    push_hex(&mut line, &mut len, cap);
    push_bytes(&mut line, &mut len, b"  def ");
    push_hex(&mut line, &mut len, def_type);
    push_bytes(&mut line, &mut len, b"  framebuffer ");
    push_bytes(&mut line, &mut len, status());
    crate::vga::print_at(row, 0, &line[..len]);
    row += 1;

    if active() {
        let (base, size) = range();
        len = 0;
        push_bytes(&mut line, &mut len, b"WC range: ");
        push_hex(&mut line, &mut len, base);
        push_bytes(&mut line, &mut len, b" + ");
        push_decimal(&mut line, &mut len, size / 1024);
        push_bytes(&mut line, &mut len, b" KiB");
        crate::vga::print_at(row, 0, &line[..len]);
        row += 1;
    }

    let count = (cap & 0xff) as usize;

    for index in 0..count.min(8) {
        let base = unsafe { rdmsr(MTRR_PHYS_BASE + (index as u32) * 2) };
        let mask = unsafe { rdmsr(MTRR_PHYS_BASE + (index as u32) * 2 + 1) };

        if mask & MTRR_ENABLE == 0 {
            continue;
        }

        let (start, size, kind) = decode_range(base, mask).unwrap_or((0, 0, 0));

        len = 0;
        push_bytes(&mut line, &mut len, b"r");
        push_decimal(&mut line, &mut len, index as u64);
        push_bytes(&mut line, &mut len, b": ");
        push_hex(&mut line, &mut len, start);
        push_bytes(&mut line, &mut len, b" ");
        push_decimal(&mut line, &mut len, size / 1024);
        push_bytes(&mut line, &mut len, b" KiB ");
        push_bytes(&mut line, &mut len, type_name(kind));
        crate::vga::print_at(row, 0, &line[..len]);
        row += 1;
    }

    row - start_row
}
