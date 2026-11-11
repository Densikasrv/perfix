//! Сведения о памяти, полученные из Multiboot info.

static mut RAM_KB: u64 = 0;
/// Конец области, занятой Multiboot-модулями (0, если их нет).
static mut MODULES_END: u32 = 0;

fn read_u32(address: u32, offset: usize) -> u32 {
    unsafe {
        (address as *const u8)
            .add(offset)
            .cast::<u32>()
            .read_unaligned()
    }
}

fn read_u64(address: u32, offset: usize) -> u64 {
    unsafe {
        (address as *const u8)
            .add(offset)
            .cast::<u64>()
            .read_unaligned()
    }
}

/// Разбирает Multiboot info и запоминает объём RAM.
pub fn init(multiboot_info: u32) {
    if multiboot_info == 0 {
        return;
    }

    let flags = read_u32(multiboot_info, 0);
    let mut total_kb = 0u64;

    // Memory map: суммируем только доступные (type 1) области.
    if flags & (1 << 6) != 0 {
        let length = read_u32(multiboot_info, 44) as u64;
        let address = read_u32(multiboot_info, 48);
        let mut offset = 0u64;
        let mut entries = 0usize;

        while address != 0
            && offset + 24 <= length
            && entries < 4096
        {
            let entry = match address.checked_add(offset as u32) {
                Some(entry) => entry,
                None => break,
            };

            let size = read_u32(entry, 0) as u64;

            if size < 20 {
                break;
            }

            let region_length = read_u64(entry, 12);
            let region_type = read_u32(entry, 20);

            if region_type == 1 {
                total_kb = total_kb.saturating_add(region_length / 1024);
            }

            offset = match offset.checked_add(size + 4) {
                Some(next) => next,
                None => break,
            };
            entries += 1;
        }
    }

    // Запасной вариант: mem_lower + mem_upper.
    if total_kb == 0 && flags & 1 != 0 {
        let lower = read_u32(multiboot_info, 4) as u64;
        let upper = read_u32(multiboot_info, 8) as u64;
        total_kb = lower.saturating_add(upper);
    }

    let mut modules_end = 0u32;

    // Модули загрузчик кладёт в память сразу за ядром. Их нельзя
    // затирать ареной или кадрами программы, иначе перестанет работать
    // установщик, которому нужен образ ядра из модуля.
    if flags & (1 << 3) != 0 {
        let count = read_u32(multiboot_info, 20);
        let address = read_u32(multiboot_info, 24);

        for index in 0..count.min(64) {
            let entry = match address.checked_add(index * 16) {
                Some(entry) => entry,
                None => break,
            };
            let end = read_u32(entry, 4);

            modules_end = modules_end.max(end);
        }
    }

    unsafe {
        RAM_KB = total_kb;
        MODULES_END = modules_end;
    }
}

/// Конец области Multiboot-модулей (0, если модулей нет).
pub fn modules_end() -> u32 {
    unsafe { MODULES_END }
}

/// Объём RAM в килобайтах (0, если неизвестен).
pub fn ram_kb() -> u64 {
    unsafe { RAM_KB }
}
