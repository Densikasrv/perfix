//! Команда `fetch`: краткая сводка о системе.

use crate::cpu;
use crate::exfat;
use crate::sysinfo;
use crate::version;
use crate::vga;

const LABEL_COLUMN: usize = 2;
const VALUE_COLUMN: usize = 16;

fn push<const N: usize>(out: &mut [u8; N], len: &mut usize, bytes: &[u8]) {
    for &byte in bytes {
        if *len >= out.len() {
            return;
        }

        out[*len] = byte;
        *len += 1;
    }
}

fn push_dec<const N: usize>(out: &mut [u8; N], len: &mut usize, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut count = 0usize;

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
        push(out, len, &[digits[count]]);
    }
}

fn push_size<const N: usize>(out: &mut [u8; N], len: &mut usize, bytes: u64) {
    let (unit, suffix): (u64, &[u8]) = if bytes >= 1 << 30 {
        (1 << 30, b" GiB")
    } else if bytes >= 1 << 20 {
        (1 << 20, b" MiB")
    } else if bytes >= 1 << 10 {
        (1 << 10, b" KiB")
    } else {
        (1, b" B")
    };

    push_dec(out, len, bytes / unit);

    if unit >= 1 << 20 {
        let tenths = (bytes % unit) * 10 / unit;
        push(out, len, b".");
        push(out, len, &[b'0' + tenths as u8]);
    }

    push(out, len, suffix);
}

fn field(row: usize, label: &[u8], value: &[u8]) {
    vga::print_at(row, LABEL_COLUMN, label);
    vga::print_at(row, VALUE_COLUMN, value);
}

/// Выводит сводку и возвращает количество строк.
pub fn show(row: usize) -> usize {
    let mut line = [b' '; 96];
    let mut len = 0usize;

    push(&mut line, &mut len, version::KERNEL_NAME);
    push(&mut line, &mut len, b" ");
    push(&mut line, &mut len, version::KERNEL_VERSION);
    field(row, b"OS:", &line[..len]);

    let mut cpu_name = [0u8; 64];
    let cpu_len = cpu::brand(&mut cpu_name);
    field(row + 1, b"CPU:", &cpu_name[..cpu_len]);

    let mut ram = [b' '; 40];
    let mut ram_len = 0usize;
    let ram_kb = sysinfo::ram_kb();

    if ram_kb == 0 {
        push(&mut ram, &mut ram_len, b"unknown");
    } else {
        push_dec(&mut ram, &mut ram_len, (ram_kb + 512) / 1024);
        push(&mut ram, &mut ram_len, b" MiB");
    }

    field(row + 2, b"RAM:", &ram[..ram_len]);

    let mut space = [b' '; 80];
    let mut space_len = 0usize;

    match exfat::space_info() {
        Ok((total, free)) => {
            push_size(&mut space, &mut space_len, free);
            push(&mut space, &mut space_len, b" free / ");
            push_size(&mut space, &mut space_len, total);
            push(&mut space, &mut space_len, b" total");
        }
        Err(_) => {
            push(&mut space, &mut space_len, b"unavailable");
        }
    }

    field(row + 3, b"exFAT Space:", &space[..space_len]);

    4
}
