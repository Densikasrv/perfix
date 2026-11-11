//! Информация о процессоре через CPUID.

pub fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let mut eax = leaf;
    let mut ebx = 0u32;
    let mut ecx = 0u32;
    let mut edx = 0u32;

    unsafe {
        core::arch::asm!(
            "cpuid",
            inout("eax") eax,
            inout("ebx") ebx,
            inout("ecx") ecx,
            inout("edx") edx,
        );
    }

    (eax, ebx, ecx, edx)
}

/// Включает FPU, а также SSE и AVX, если процессор их поддерживает.
///
/// Ядро собрано с `-mmx,-sse`, но сторонние программы могут использовать
/// эти инструкции. Без установленных битов CR0/CR4/OSXSAVE они вызывают
/// #UD (#NM для FPU), поэтому разрешаем их до запуска чего-либо ещё.
///
/// Все обращения к CR0/CR4 выполняются только для поддерживаемых
/// возможностей: биты OSFXSR/OSXMMEXCPT на процессоре без SSE
/// зарезервированы, и запись единиц в них вызывает #GP. Так же и
/// OSXSAVE/XCR0 включаются лишь при наличии XSAVE и AVX.
pub fn init_features() {
    unsafe {
        let (_, _, ecx_features, edx_features) = cpuid(1);

        let has_fpu = edx_features & (1 << 0) != 0;
        let has_fxsr = edx_features & (1 << 24) != 0;
        let has_sse = edx_features & (1 << 25) != 0;

        // x87: сбрасываем EM/TS (эмуляция и ленивое переключение
        // контекста) и выставляем MP/NE, чтобы FPU был доступен
        // программе напрямую. На CPU без FPU регистры не трогаем.
        if has_fpu {
            let mut cr0: u32;

            core::arch::asm!(
                "mov {0:e}, cr0",
                out(reg) cr0,
                options(nostack, preserves_flags)
            );

            cr0 &= !((1 << 2) | (1 << 3));
            cr0 |= (1 << 1) | (1 << 5);

            core::arch::asm!(
                "mov cr0, {0:e}",
                in(reg) cr0,
                options(nostack, preserves_flags)
            );

            core::arch::asm!("fninit", options(nostack));
        }

        let mut cr4: u32;

        core::arch::asm!(
            "mov {0:e}, cr4",
            out(reg) cr4,
            options(nostack, preserves_flags)
        );

        // OSFXSR — разрешает SSE-регистры, OSXMMEXCPT — исключения SSE.
        if has_fxsr {
            cr4 |= 1 << 9;
        }

        if has_sse {
            cr4 |= 1 << 10;
        }

        // Бит 26 — аппаратная поддержка XSAVE, бит 28 — AVX;
        // бит 27 (OSXSAVE) выставляет уже операционная система.
        let has_xsave = ecx_features & (1 << 26) != 0;
        let has_avx = ecx_features & (1 << 28) != 0;

        // AVX имеет смысл только поверх SSE.
        let use_avx = has_fxsr && has_sse && has_xsave && has_avx;

        if use_avx {
            // OSXSAVE разрешает XSAVE и XCR0.
            cr4 |= 1 << 18;
        }

        core::arch::asm!(
            "mov cr4, {0:e}",
            in(reg) cr4,
            options(nostack, preserves_flags)
        );

        if use_avx {
            // Проверяем, что XCR0 умеет хранить состояния x87, SSE и AVX.
            let (xcr0_supported, _, _, _) = cpuid(0xD);
            let xcr0 = xcr0_supported & 0x7;

            if xcr0 == 0x7 {
                core::arch::asm!(
                    "xsetbv",
                    in("ecx") 0u32,
                    in("eax") 0x7u32,
                    in("edx") 0u32,
                    options(nostack, preserves_flags)
                );
            }
        }
    }
}

/// Записывает vendor string CPU (например, GenuineIntel) и возвращает длину.
pub fn vendor(out: &mut [u8]) -> usize {
    let (_, ebx, ecx, edx) = cpuid(0);
    let mut raw = [0u8; 12];

    raw[0..4].copy_from_slice(&ebx.to_le_bytes());
    raw[4..8].copy_from_slice(&edx.to_le_bytes());
    raw[8..12].copy_from_slice(&ecx.to_le_bytes());

    copy_trimmed(&raw, out)
}

/// Записывает brand string CPU и возвращает длину.
///
/// Если расширенные CPUID-листья недоступны, возвращает vendor string.
pub fn brand(out: &mut [u8]) -> usize {
    let (max_leaf, _, _, _) = cpuid(0x8000_0000);

    if max_leaf < 0x8000_0004 {
        return vendor(out);
    }

    let mut raw = [0u8; 48];

    for index in 0..3 {
        let (eax, ebx, ecx, edx) = cpuid(0x8000_0002 + index);
        let base = index as usize * 16;

        raw[base..base + 4].copy_from_slice(&eax.to_le_bytes());
        raw[base + 4..base + 8].copy_from_slice(&ebx.to_le_bytes());
        raw[base + 8..base + 12].copy_from_slice(&ecx.to_le_bytes());
        raw[base + 12..base + 16].copy_from_slice(&edx.to_le_bytes());
    }

    let length = copy_trimmed(&raw, out);

    if length == 0 {
        vendor(out)
    } else {
        length
    }
}

fn copy_trimmed(source: &[u8], out: &mut [u8]) -> usize {
    let mut start = 0usize;
    let mut end = source.len();

    while start < end && (source[start] == 0 || source[start] == b' ') {
        start += 1;
    }

    while end > start && (source[end - 1] == 0 || source[end - 1] == b' ') {
        end -= 1;
    }

    let mut written = 0usize;
    let mut previous_space = false;

    for &byte in &source[start..end] {
        if byte == b' ' {
            if previous_space {
                continue;
            }

            previous_space = true;
        } else {
            previous_space = false;
        }

        if written >= out.len() {
            break;
        }

        out[written] = byte;
        written += 1;
    }

    written
}
