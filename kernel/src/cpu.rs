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
