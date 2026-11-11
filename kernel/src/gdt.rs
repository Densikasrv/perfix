use core::arch::asm;

#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtEntry {
    limit_low: u16,
    base_low: u16,
    base_middle: u8,
    access: u8,
    granularity: u8,
    base_high: u8,
}

impl GdtEntry {
    const fn new(
        base: u32,
        limit: u32,
        access: u8,
        granularity: u8,
    ) -> Self {
        Self {
            limit_low: limit as u16,
            base_low: base as u16,
            base_middle: (base >> 16) as u8,
            access,
            granularity: ((limit >> 16) as u8 & 0x0F)
                | (granularity & 0xF0),
            base_high: (base >> 24) as u8,
        }
    }

    const fn null() -> Self {
        Self::new(0, 0, 0, 0)
    }
}

/// Первые три записи — null, код и данные ядра; остальные отдаются
/// под TLS через `set_thread_area`.
const GDT_ENTRIES: usize = 16;
const FIRST_TLS_ENTRY: usize = 3;

#[repr(C, align(8))]
struct Gdt {
    entries: [GdtEntry; GDT_ENTRIES],
}

static mut GDT: Gdt = Gdt {
    entries: [
        GdtEntry::null(),
        GdtEntry::new(0, 0xFFFFF, 0x9A, 0xCF),
        GdtEntry::new(0, 0xFFFFF, 0x92, 0xCF),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
        GdtEntry::null(),
    ],
};

static mut NEXT_TLS_ENTRY: usize = FIRST_TLS_ENTRY;

unsafe extern "C" {
    fn gdt_flush(pointer: *const GdtPointer);
}

pub unsafe fn init() {
    let pointer = GdtPointer {
        limit: (core::mem::size_of::<Gdt>() - 1) as u16,
        base: core::ptr::addr_of!(GDT) as u32,
    };

    unsafe {
        gdt_flush(&pointer);
    }

    // Ensure the compiler does not move memory operations across setup.
    unsafe {
        asm!("", options(nostack, preserves_flags));
    }
}

/// Реализация Linux-вызова `set_thread_area` (i386, номер 243).
///
/// Статические glibc-программы используют её, чтобы получить запись GDT
/// под TLS и загрузить в GS сегмент с базой блока TLS.
pub fn set_thread_area(pointer: u32) -> i32 {
    if pointer == 0 {
        return -1;
    }

    unsafe {
        let info = pointer as *mut u32;
        let requested = info.read_volatile();

        let index = if requested == u32::MAX {
            let index = NEXT_TLS_ENTRY;

            if index >= GDT_ENTRIES {
                return -1;
            }

            NEXT_TLS_ENTRY += 1;
            index
        } else {
            let index = requested as usize;

            if index < FIRST_TLS_ENTRY || index >= GDT_ENTRIES {
                return -1;
            }

            index
        };

        let base = info.add(1).read_volatile();
        let limit = info.add(2).read_volatile() & 0x000F_FFFF;
        let flags = info.add(3).read_volatile();

        let seg_32bit = flags & 1 != 0;
        let limit_in_pages = flags & (1 << 4) != 0;
        let not_present = flags & (1 << 5) != 0;

        // Present | DPL=3 | S=1 | тип «данные, чтение/запись».
        let access = if not_present { 0xF2 & !0x80 } else { 0xF2 };
        let granularity = ((if limit_in_pages { 0x8 } else { 0 })
            | (if seg_32bit { 0x4 } else { 0 }))
            << 4;

        let gdt = core::ptr::addr_of_mut!(GDT).cast::<GdtEntry>();
        gdt.add(index)
            .write(GdtEntry::new(base, limit, access, granularity));

        info.write_volatile(index as u32);
    }

    0
}