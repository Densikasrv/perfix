
use core::arch::asm;

const PAGE_SIZE: u32 = 4096;
const PAGE_PRESENT_WRITABLE: u32 = 0x003;
const PAGE_LARGE: u32 = 0x083; // Present | Writable | Page Size (4 MiB)
const LARGE_PAGE_SIZE: u32 = 0x0040_0000;
const PAGE_COUNT: usize = 1024;

#[repr(C, align(4096))]
struct PageTable {
    entries: [u32; PAGE_COUNT],
}

static mut PAGE_DIRECTORY: PageTable = PageTable {
    entries: [0; PAGE_COUNT],
};

static mut PAGE_TABLE: PageTable = PageTable {
    entries: [0; PAGE_COUNT],
};

pub unsafe fn init(fb_address: u64, fb_size: u64) {
    let directory =
        core::ptr::addr_of_mut!(PAGE_DIRECTORY).cast::<u32>();
    let table =
        core::ptr::addr_of_mut!(PAGE_TABLE).cast::<u32>();

    // Identity-map the first 4 MiB: virtual address = physical address.
    for i in 0..PAGE_COUNT {
        let address = (i as u32) * PAGE_SIZE;

        unsafe {
            table.add(i).write_volatile(
                address | PAGE_PRESENT_WRITABLE,
            );
            directory.add(i).write_volatile(0);
        }
    }

    // The first directory entry points to our page table.
    unsafe {
        directory.write_volatile(
            (table as u32) | PAGE_PRESENT_WRITABLE,
        );
    }

    // Map the linear framebuffer with 4 MiB pages (requires PSE).
    if fb_address != 0 && fb_size != 0 && fb_address <= u32::MAX as u64 {
        let start = (fb_address & !(LARGE_PAGE_SIZE as u64 - 1)) as u32;
        let end_address = fb_address
            .saturating_add(fb_size)
            .min(u32::MAX as u64);
        let end = (end_address
            .saturating_add(LARGE_PAGE_SIZE as u64 - 1)
            & !(LARGE_PAGE_SIZE as u64 - 1)) as u32;

        let mut address = start;

        while address < end {
            let index = (address >> 22) as usize;

            // Entry 0 already points to the 4 KiB page table.
            if index > 0 && index < PAGE_COUNT {
                unsafe {
                    directory
                        .add(index)
                        .write_volatile(address | PAGE_LARGE);
                }
            }

            match address.checked_add(LARGE_PAGE_SIZE) {
                Some(next) => address = next,
                None => break,
            }
        }
    }

    // Enable Page Size Extensions before turning paging on.
    unsafe {
        let cr4: u32;
        asm!("mov {0:e}, cr4", out(reg) cr4, options(nostack, preserves_flags));
        asm!("mov cr4, {0:e}", in(reg) cr4 | 0x10, options(nostack, preserves_flags));
    }

    // Tell the CPU where the page directory is.
    unsafe {
        asm!(
            "mov cr3, {0:e}",
            in(reg) directory as u32,
            options(nostack, preserves_flags)
        );
    }

    // Enable paging by setting CR0.PG (bit 31).
    let cr0: u32;

    unsafe {
        asm!(
            "mov {0:e}, cr0",
            out(reg) cr0,
            options(nostack, preserves_flags)
        );

        asm!(
            "mov cr0, {0:e}",
            in(reg) cr0 | 0x8000_0000,
            options(nostack, preserves_flags)
        );
    }
}

pub fn test_page_fault() -> ! {
    unsafe {
        core::ptr::read_volatile(0x0040_0000 as *const u32);
    }

    loop {
        unsafe {
            asm!("cli", "hlt", options(nomem, nostack));
        }
    }
}
