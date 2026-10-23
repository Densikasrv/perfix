//! Сведения о памяти, полученные из Multiboot info.

static mut RAM_KB: u64 = 0;

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

    unsafe {
        RAM_KB = total_kb;
    }
}

/// Объём RAM в килобайтах (0, если неизвестен).
pub fn ram_kb() -> u64 {
    unsafe { RAM_KB }
}
