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

#[repr(C, align(8))]
struct Gdt {
    entries: [GdtEntry; 3],
}

static mut GDT: Gdt = Gdt {
    entries: [
        GdtEntry::null(),
        GdtEntry::new(0, 0xFFFFF, 0x9A, 0xCF),
        GdtEntry::new(0, 0xFFFFF, 0x92, 0xCF),
    ],
};

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