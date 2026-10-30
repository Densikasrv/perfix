//! Выключение питания через ACPI.

use core::arch::asm;

fn outw(port: u16, value: u16) {
    unsafe {
        asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn spin() {
    for _ in 0..200_000 {
        unsafe {
            asm!("pause", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Пытается выключить питание через ACPI S5.
///
/// Используются стандартные порты QEMU, VirtualBox, Bochs и ACPI PM1a_CNT.
/// Возвращает `false`, если выключение не сработало.
pub fn shutdown() -> bool {
    // ACPI SLP_TYP = 5 (S5) | SLP_EN.
    const S5: u16 = (5 << 10) | (1 << 13);
    const S5_ALT: u16 = 0x2000;

    for &port in &[0x0604u16, 0x4004, 0xB004] {
        outw(port, S5);
        spin();
    }

    for &port in &[0x0604u16, 0x4004, 0xB004] {
        outw(port, S5_ALT);
        spin();
    }

    false
}

/// Останавливает процессор, если ACPI poweroff недоступен.
pub fn halt() -> ! {
    unsafe {
        asm!("cli", options(nomem, nostack));
    }

    loop {
        unsafe {
            asm!("hlt", options(nomem, nostack));
        }
    }
}
