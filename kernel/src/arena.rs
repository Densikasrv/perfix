//! Линейный распределитель памяти для временных буферов.
//!
//! Арена занимает свободную физическую память сразу за образом ядра и
//! растёт вверх. Загрузчик программ берёт из неё буфер ELF-файла и стек
//! программы, а после завершения программы возвращает память одним
//! [`release`]. Кадры для страниц программы, наоборот, выделяются сверху
//! вниз (см. `exec`), поэтому арена и кадры движутся навстречу друг другу.
//!
//! Память арены доступна через тождественное отображение, созданное в
//! `paging::init`, поэтому отдельные таблицы страниц ей не нужны.

const PAGE_SIZE: u32 = 4096;

/// Запас в верхней части RAM, который не отдаётся ни арене, ни кадрам.
/// Нужен для структур загрузчика/прошивки и округлений.
const TOP_RESERVE: u32 = 0x0010_0000; // 1 MiB

/// Верхняя граница адресуемой памяти, как в `exec` и `paging`.
const MEMORY_CEILING: u32 = 0xFC00_0000; // 252 MiB

static mut BASE: u32 = 0;
static mut NEXT: u32 = 0;
static mut LIMIT: u32 = 0;

unsafe extern "C" {
    /// Символ из `linker.ld`: первый байт после `.bss` ядра.
    static kernel_end: u8;
}

/// Вычисляет границы арены по фактическому объёму RAM.
pub fn init() {
    let image_end = unsafe { &kernel_end as *const u8 as u32 };
    // Загрузчик размещает Multiboot-модули сразу за ядром; арена не
    // должна их перезаписать, иначе сломается установщик.
    let end = image_end.max(crate::sysinfo::modules_end());
    let base = (end + 15) & !15;
    let ram = crate::sysinfo::ram_kb().saturating_mul(1024);
    let ceiling = if ram == 0 {
        MEMORY_CEILING as u64
    } else {
        ram.min(MEMORY_CEILING as u64)
    };
    let limit = (ceiling as u32)
        .saturating_sub(TOP_RESERVE)
        & !(PAGE_SIZE - 1);

    unsafe {
        BASE = base;
        NEXT = base;
        LIMIT = limit;
    }
}

/// Текущая граница занятой арены.
pub fn next() -> u32 {
    unsafe { NEXT }
}

/// Запоминает состояние арены для последующего [`release`].
pub fn mark() -> u32 {
    unsafe { NEXT }
}

/// Освобождает всё, что было выделено после `mark`.
///
/// Освобождение работает по принципу стека: вызывающая сторона обязана
/// освобождать выделения в обратном порядке.
pub fn release(mark: u32) {
    unsafe {
        if mark >= BASE && mark <= NEXT {
            NEXT = mark;
        }
    }
}

/// Выделяет `size` байт с выравниванием `align` (степень двойки).
///
/// Возвращает срез, лежащий в тождественно отображённой памяти. Память не
/// обнуляется. При нехватке места возвращает `None`, ничего не меняя.
pub fn allocate(size: usize, align: usize) -> Option<&'static mut [u8]> {
    if size == 0 {
        return Some(&mut []);
    }

    if size > u32::MAX as usize {
        return None;
    }

    let align = align.max(1) as u32;

    if !align.is_power_of_two() {
        return None;
    }

    unsafe {
        let start = NEXT.checked_add(align - 1)? & !(align - 1);
        let end = start.checked_add(size as u32)?;

        if end > LIMIT {
            return None;
        }

        NEXT = end;
        Some(core::slice::from_raw_parts_mut(
            start as *mut u8,
            size,
        ))
    }
}
