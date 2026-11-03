use crate::fs::{self, FileSystem, CAT_LINES};
use crate::power;
use crate::vga;
use crate::version;

pub fn show_screen(message: Option<&[u8]>) {
    vga::clear_screen();
    vga::hide_cursor();

    vga::print_at(0, 0, b"Welcome to Perfix Shell!");
    vga::print_at(
        2,
        0,
        b"To see the list of commands, write help.",
    );
    vga::print_at(3, 0, b"To change the keyboard layout to russian, press Alt+Shift.");

    if let Some(text) = message {
        vga::print_at(4, 0, text);
    }

    prompt(6);
}

/// Максимальная длина абсолютного пути и текущего каталога.
const PATH_MAX: usize = 512;
/// Максимальная длина вводимой команды.
pub const INPUT_CAPACITY: usize = 256;
/// Сколько последних команд хранит история.
const HISTORY_DEPTH: usize = 16;

/// Первая зарегистрированная файловая система (по умолчанию exFAT).
fn primary_fs() -> &'static dyn FileSystem {
    fs::filesystems()[0]
}

/// Вызывает метод на каждой ФС, накапливая смещение строк по возвращённому количеству.
fn for_each_fs<F>(row: usize, mut f: F) -> usize
where
    F: FnMut(&dyn FileSystem, usize) -> usize,
{
    let mut offset = 0usize;
    for fs in fs::filesystems() {
        offset += f(fs, row + offset);
    }
    offset
}

/// Выполняет операцию по пути на первой ФС, где путь существует.
/// Для операций создания (create = true) используется первая ФС.
fn with_path_fs<F>(path: &[u8], row: usize, create: bool, mut f: F) -> usize
where
    F: FnMut(&dyn FileSystem) -> usize,
{
    if !create {
        for fs in fs::filesystems() {
            if (*fs).path_is_directory(path).is_ok() {
                return f(fs);
            }
        }
        vga::print_at(row, 0, b"Path not found on any filesystem");
        return 1;
    }
    f(primary_fs())
}

static mut HISTORY: [[u8; INPUT_CAPACITY]; HISTORY_DEPTH] =
    [[0; INPUT_CAPACITY]; HISTORY_DEPTH];
static mut HISTORY_LEN: [usize; HISTORY_DEPTH] = [0; HISTORY_DEPTH];
static mut HISTORY_COUNT: usize = 0;

pub fn history_len() -> usize {
    unsafe { HISTORY_COUNT }
}

/// Возвращает команду истории: 0 — самая старая, `history_len() - 1` — последняя.
pub fn history_get(index: usize) -> Option<&'static [u8]> {
    unsafe {
        if index >= HISTORY_COUNT {
            return None;
        }

        Some(&HISTORY[index][..HISTORY_LEN[index]])
    }
}

pub fn history_add(command: &[u8]) {
    if command.is_empty() {
        return;
    }

    let length = command.len().min(INPUT_CAPACITY);

    unsafe {
        let count = HISTORY_COUNT;

        if count > 0 {
            let last = count - 1;

            if HISTORY_LEN[last] == length
                && HISTORY[last][..length] == command[..length]
            {
                return;
            }
        }

        let slot = if count < HISTORY_DEPTH {
            count
        } else {
            // Сдвигаем историю на одну команду вверх.
            for index in 1..HISTORY_DEPTH {
                HISTORY_LEN[index - 1] = HISTORY_LEN[index];

                for byte in 0..INPUT_CAPACITY {
                    HISTORY[index - 1][byte] = HISTORY[index][byte];
                }
            }

            HISTORY_DEPTH - 1
        };

        HISTORY[slot][..length].copy_from_slice(&command[..length]);
        HISTORY_LEN[slot] = length;
        HISTORY_COUNT = (count + 1).min(HISTORY_DEPTH);
    }
}

static mut CWD: [u8; PATH_MAX] = {
    let mut buffer = [0u8; PATH_MAX];
    buffer[0] = b'/';
    buffer
};
static mut CWD_LEN: usize = 1;

/// Текущий каталог оболочки в виде абсолютного пути.
fn cwd() -> &'static [u8] {
    unsafe { &CWD[..CWD_LEN] }
}

fn set_cwd(path: &[u8]) {
    if path.is_empty() || path.len() > PATH_MAX || path[0] != b'/' {
        return;
    }

    unsafe {
        CWD[..path.len()].copy_from_slice(path);
        CWD_LEN = path.len();
    }
}

fn build_prompt(out: &mut [u8; 80]) -> usize {
    let mut len = 0usize;

    push_message(out, &mut len, b"perfix[");
    push_message(
        out,
        &mut len,
        if crate::keyboard::is_russian() {
            b"RU"
        } else {
            b"EN"
        },
    );
    push_message(out, &mut len, b"] ");

    let cwd = cwd();

    if cwd.len() <= 34 {
        push_message(out, &mut len, cwd);
    } else {
        push_message(out, &mut len, b"...");
        push_message(out, &mut len, &cwd[cwd.len() - 31..]);
    }

    push_message(out, &mut len, b"> ");
    len
}

/// Длина приглашения с учётом текущего каталога.
pub fn prompt_width() -> usize {
    let mut buffer = [0u8; 80];

    build_prompt(&mut buffer)
}

pub fn prompt(row: usize) {
    let mut buffer = [0u8; 80];
    let len = build_prompt(&mut buffer);

    vga::print_at(row, 0, &buffer[..len]);
}

/// Отделяет аргумент команды: `ls /SYSTEM` -> `/SYSTEM`.
fn command_arg<'a>(input: &'a [u8], command: &[u8]) -> Option<&'a [u8]> {
    let rest = input.strip_prefix(command)?;

    if rest.is_empty() {
        return Some(rest);
    }

    if rest[0] != b' ' && rest[0] != b'\t' {
        return None;
    }

    Some(trim_ascii_space(rest))
}

fn trim_ascii_space(input: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = input.len();

    while start < end && (input[start] == b' ' || input[start] == b'\t') {
        start += 1;
    }

    while end > start && (input[end - 1] == b' ' || input[end - 1] == b'\t') {
        end -= 1;
    }

    &input[start..end]
}

/// Сколько строк вывода резервировать под команду до её выполнения.
fn command_lines(input: &[u8]) -> usize {
    if input == b"help" {
        25
    } else if input == b"version" {
        5
    } else if input == b"diskinfo" {
        9
    } else if input == b"mount" {
        14
    } else if input == b"charset" {
        6
    } else if input == b"fetch" {
        4
    } else if input == b"listpart" {
        17
    } else if input == b"rootdir" {
        42
    } else if command_arg(input, b"ls").is_some() {
        42
    } else if command_arg(input, b"cat").is_some() {
        CAT_LINES
    } else {
        1
    }
}

/// Добавляет байты в буфер сообщения, пока в нём есть место.
fn push_message<const N: usize>(out: &mut [u8; N], len: &mut usize, bytes: &[u8]) {
    for &byte in bytes {
        if *len >= out.len() {
            return;
        }

        out[*len] = byte;
        *len += 1;
    }
}

/// Строит нормализованный абсолютный путь из текущего каталога и аргумента.
///
/// Понимает `.`, `..`, повторные и завершающие `/`.
fn absolute_path(input: &[u8], out: &mut [u8; PATH_MAX]) -> Option<usize> {
    let mut len;

    if input.first() == Some(&b'/') {
        out[0] = b'/';
        len = 1;
    } else {
        let cwd = cwd();

        if cwd.len() > out.len() {
            return None;
        }

        out[..cwd.len()].copy_from_slice(cwd);
        len = cwd.len();
    }

    for component in input.split(|&byte| byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }

        if component == b".." {
            while len > 1 && out[len - 1] != b'/' {
                len -= 1;
            }

            if len > 1 {
                len -= 1;
            }

            continue;
        }

        let separator = if len > 1 { 1 } else { 0 };

        if len + separator + component.len() > out.len() {
            return None;
        }

        if separator == 1 {
            out[len] = b'/';
            len += 1;
        }

        out[len..len + component.len()].copy_from_slice(component);
        len += component.len();
    }

    Some(len)
}

/// Делит аргумент `write` на путь и текст.
fn split_first_space(input: &[u8]) -> (&[u8], &[u8]) {
    match input.iter().position(|&byte| byte == b' ') {
        Some(index) => (&input[..index], trim_ascii_space(&input[index + 1..])),
        None => (input, b""),
    }
}

fn path_too_long(row: usize) {
    vga::print_at(row, 0, b"Path is too long");
}

fn change_directory(argument: &[u8], row: usize) {
    let mut path = [0u8; PATH_MAX];

    let target_len = if argument.is_empty() {
        path[0] = b'/';
        Some(1)
    } else {
        absolute_path(argument, &mut path)
    };

    let Some(target_len) = target_len else {
        path_too_long(row);
        return;
    };

    let mut cd_handled = false;
    for fs in fs::filesystems() {
        match fs.path_is_directory(&path[..target_len]) {
            Ok(true) => {
                set_cwd(&path[..target_len]);

                let mut line = [b' '; 80];
                let mut len = 0usize;
                push_message(&mut line, &mut len, b"cwd: ");
                push_message(&mut line, &mut len, &path[..target_len]);
                vga::print_at(row, 0, &line[..len]);
                cd_handled = true;
                break;
            }
            Ok(false) => {
                vga::print_at(row, 0, b"Not a directory");
                cd_handled = true;
                break;
            }
            Err(_) => continue,
        }
    }
    if !cd_handled {
        vga::print_at(row, 0, b"Path not found on any filesystem");
    }
}

fn push_dec_message<const N: usize>(out: &mut [u8; N], len: &mut usize, value: usize) {
    let mut digits = [0u8; 20];
    let mut count = 0usize;
    let mut value = value;

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
        push_message(out, len, &[digits[count]]);
    }
}

pub fn execute(input: &[u8], row: &mut usize) {
    if input == b"clear" {
        show_screen(None);
        *row = 6;
        return;
    }

    if input == b"divzero" {
        crate::idt::test_divzero();
    }

    if input == b"pagefault" {
        crate::paging::test_page_fault();
    }

    if let Some(argument) = command_arg(input, b"tedit") {
        let mut message = [b' '; 80];
        let mut len = 0usize;
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            push_message(&mut message, &mut len, b"tedit: usage: tedit <file>");
        } else if let Some(path_len) = absolute_path(argument, &mut path) {
            match crate::tedit::run(&path[..path_len]) {
                Ok(outcome) => {
                    push_message(&mut message, &mut len, b"tedit: ");

                    if outcome.saved {
                        push_message(&mut message, &mut len, b"saved ");
                    } else {
                        push_message(&mut message, &mut len, b"closed ");
                    }

                    push_message(&mut message, &mut len, &path[..path_len]);
                }
                Err(error) => {
                    push_message(&mut message, &mut len, b"tedit: ");
                    push_message(&mut message, &mut len, error.message());
                }
            }
        } else {
            push_message(&mut message, &mut len, b"tedit: path is too long");
        }

        show_screen(Some(&message[..len]));
        *row = 6;
        return;
    }

    let lines = command_lines(input);

    // Освобождаем место ДО вывода всей команды.
    while *row + 1 + lines > vga::height() - 1 && *row > 0 {
        vga::scroll_screen();
        *row -= 1;
    }

    let output_row = *row + 1;

    if input == b"help" {
        vga::print_at(output_row, 0, b"Available commands:");
        vga::print_at(output_row + 1, 0, b"help      - show commands");
        vga::print_at(output_row + 2, 0, b"about     - about Perfix");
        vga::print_at(output_row + 3, 0, b"version   - OS information");
        vga::print_at(output_row + 4, 0, b"clear     - clear the screen");
        vga::print_at(output_row + 5, 0, b"divzero   - test CPU exception");
        vga::print_at(output_row + 6, 0, b"pagefault - test page fault");
        vga::print_at(output_row + 7, 0, b"diskinfo  - read exFAT boot sector");
        vga::print_at(output_row + 8, 0, b"mount     - show exFAT volume info");
        vga::print_at(output_row + 9, 0, b"ls [path] - list exFAT directory");
        vga::print_at(output_row + 10, 0, b"cat <file>- print file contents");
        vga::print_at(output_row + 11, 0, b"charset   - show CP866 glyphs");
        vga::print_at(output_row + 12, 0, b"write <f> <text> - create/overwrite file");
        vga::print_at(output_row + 13, 0, b"tedit <file> - nano-like text editor");
        vga::print_at(output_row + 14, 0, b"rm <file> - delete file");
        vga::print_at(output_row + 15, 0, b"mkdir <dir> - create directory");
        vga::print_at(output_row + 16, 0, b"rmdir <dir> - remove empty directory");
        vga::print_at(output_row + 17, 0, b"cd <dir>  - change directory");
        vga::print_at(output_row + 18, 0, b"pwd       - print working directory");
        vga::print_at(output_row + 19, 0, b"Up/Down   - command history");
        vga::print_at(output_row + 20, 0, b"listpart  - list exFAT partitions");
        vga::print_at(output_row + 21, 0, b"cdpart <n>- select partition");
        vga::print_at(output_row + 22, 0, b"fetch     - system information");
        vga::print_at(output_row + 23, 0, b"halt      - turn off the computer");
        vga::print_at(output_row + 24, 0, b"calc      - simple calculator");
    } else if input == b"about" {
        vga::print_at(output_row, 0, b"Perfix");
        vga::print_at(output_row + 1, 0, b"Copyright (C) 2026 Densikasrv");
        vga::print_at(output_row + 3, 0, b"This program comes with ABSOLUTELY NO WARRANTY.");
        vga::print_at(output_row + 4, 0, b"This is free software, and you are welcome to");
        vga::print_at(output_row + 5, 0, b"redistribute it under certain conditions.");
        
        *row = output_row + 6;
        prompt(*row);
        return;
    } else if input == b"version" {
        vga::print_at(output_row, 0, version::KERNEL_NAME);
        vga::print_at(output_row + 1, 0, b"OS version: ");
        vga::print_at(output_row + 1, 16, version::KERNEL_VERSION);
        vga::print_at(output_row + 2, 0, b"Architecture: ");
        vga::print_at(output_row + 2, 13, version::ARCHITECTURE);
        vga::print_at(output_row + 3, 0, b"Language: ");
        vga::print_at(output_row + 3, 10, version::LANGUAGE);
        vga::print_at(output_row + 4, 0, b"Status: ");
        vga::print_at(output_row + 4, 8, version::STATUS);
    } else if input == b"fetch" {
        let used = crate::fetch::show(output_row);
        *row = output_row + used;
        prompt(*row);
        return;
    } else if input == b"diskinfo" {
        let used = crate::disk::print_disk_info(output_row);
        *row = output_row + used;
        prompt(*row);
        return;
    } else if input == b"mount" {
        for_each_fs(output_row, |fs, r| fs.print_volume_info(r));
    } else if input == b"charset" {
        vga::print_charset(output_row);
    
} else if input == b"listpart" {
    let mut partitions = [crate::disk::DiskPartition {
        number: 0,
        kind: crate::disk::FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; 4];

    let count = crate::disk::read_partitions(&mut partitions);

    if count == 0 {
        vga::print_at(output_row, 0, b"No partitions found");
    } else {
        vga::print_at(output_row, 0, b"Disk partitions:");

        for (i, p) in partitions[..count].iter().enumerate() {
            let name = match p.kind {
                crate::disk::FsKind::Exfat => b"exFAT".as_slice(),
                crate::disk::FsKind::Ext4 => b"ext4".as_slice(),
                crate::disk::FsKind::Other => b"Other".as_slice(),
            };

            vga::print_at(output_row + 1 + i, 0, b"Partition");

            let mut digits = [0u8; 10];
            let mut n = p.number;
            let mut len = 0;

            if n == 0 {
                digits[0] = b'0';
                len = 1;
            } else {
                while n > 0 {
                    digits[len] = b'0' + (n % 10) as u8;
                    n /= 10;
                    len += 1;
                }
            }

            for j in 0..len {
                vga::print_at(
                    output_row + 1 + i,
                    10 + j,
                    &[digits[len - j - 1]],
                );
            }

            vga::print_at(output_row + 1 + i, 14, name);
        }
    }

    *row = output_row + count + 1;
    prompt(*row);
    return;


    } else if let Some(argument) = command_arg(input, b"setup") {
        let mut n: i32 = 0;
        let mut i = 0;

        while i < argument.len() && argument[i].is_ascii_digit() {
            n = n.wrapping_mul(10).wrapping_add((argument[i] - b'0') as i32);
            i += 1;
        }

        if i == 0 || n < 0 {
            vga::print_at(output_row, 0, b"Usage: setup <partiton number>");
            *row = output_row + 1;
            prompt(*row);
            return;
        }

        let used = primary_fs().shell_setup(n as usize);
        *row = output_row + used;
        prompt(*row);
        return;
    } else if let Some(argument) = command_arg(input, b"cdpart") {
    let mut number = 0usize;
    let mut valid = !argument.is_empty();

    for &byte in argument {
        if !byte.is_ascii_digit() {
            valid = false;
            break;
        }

        number = number
            .saturating_mul(10)
            .saturating_add((byte - b'0') as usize);
    }
    let mut used_rows = 1usize;
    if !valid || number == 0 {
        vga::print_at(output_row, 0, b"Usage: cdpart <number>");
    } else {
        match crate::disk::select_partition(number) {
            Ok(crate::disk::FsKind::Exfat) => {
                set_cwd(b"/");
                vga::print_at(output_row, 0, b"exFAT partition selected");
            }
            Ok(crate::disk::FsKind::Ext4) => {
    set_cwd(b"/");

    vga::print_at(output_row, 0, b"ext4 partition selected");
    vga::print_at(output_row + 1, 0, b"Superblock: OK");

    match crate::disk::read_ext4_root_inode(number) {
        Ok(inode) => {
            vga::print_at(output_row + 2, 0, b"Root inode: ");
            vga::print_at(output_row + 2, 12, b"2");

            if inode.is_directory {
                vga::print_at(output_row + 3, 0, b"Type: Directory");
            } else {
                vga::print_at(output_row + 3, 0, b"Type: Not a directory");
            }

            vga::print_at(output_row + 4, 0, b"Inode read: OK");
            let mut hex = [b'0'; 16];
let digits = b"0123456789ABCDEF";

let mut value = inode.first_extent_block;

for i in (0..16).rev() {
    hex[i] = digits[(value & 0xF) as usize];
    value >>= 4;
}  

            vga::print_at(output_row + 5, 0, b"Extent: 0x");
            vga::print_at(output_row + 5, 10, &hex);
            let mut directory_block = [0u8; 4096];

match crate::disk::read_ext4_root_directory_block(
    number,
    &mut directory_block,
) {
    Ok(block_size) => {
        vga::print_at(output_row + 6, 0, b"Root directory:");

        let mut offset = 0usize;
        let mut entries = 0usize;

        while offset + 8 <= block_size && entries < 8 {
            let inode_number = u32::from_le_bytes([
                directory_block[offset],
                directory_block[offset + 1],
                directory_block[offset + 2],
                directory_block[offset + 3],
            ]);

            let record_length = u16::from_le_bytes([
                directory_block[offset + 4],
                directory_block[offset + 5],
            ]) as usize;

            let name_length = directory_block[offset + 6] as usize;

            if record_length < 8
                || offset + record_length > block_size
                || name_length > record_length - 8
            {
                break;
            }

            if inode_number != 0 && name_length > 0 {
                vga::print_at(
                    output_row + 7 + entries,
                    2,
                    &directory_block[offset + 8..offset + 8 + name_length],
                );

                entries += 1;
            }

            offset += record_length;
        }

        if entries == 0 {
            vga::print_at(output_row + 7, 2, b"Directory is empty");
        }
        used_rows = if entries == 0 {
    8
} else {
    7 + entries
};
    }
    Err(message) => {
        vga::print_at(output_row + 6, 0, message);
        used_rows = 7;
    }
}
        }
        Err(message) => {
            vga::print_at(output_row + 2, 0, message);
            used_rows = 3;
        }
    }
}
            Ok(crate::disk::FsKind::Other) => {
                vga::print_at(output_row, 0, b"Unsupported filesystem");
            }
            Err(message) => {
                vga::print_at(output_row, 0, message);
            }
        }
    }

    *row = output_row + used_rows;
    prompt(*row);
    return;
    } else if input == b"pwd" {
        vga::print_at(output_row, 0, cwd());
    } else if let Some(argument) = command_arg(input, b"cd") {
        change_directory(argument, output_row);
    } else if input == b"rootdir" {
        let used = with_path_fs(b"/", output_row, false, |fs| fs.shell_list(b"/", output_row));
        *row = output_row + used;
        prompt(*row);
        return;
    } else if let Some(argument) = command_arg(input, b"ls") {
        let mut path = [0u8; PATH_MAX];

        let target_len = if argument.is_empty() {
            let cwd = cwd();
            path[..cwd.len()].copy_from_slice(cwd);
            Some(cwd.len())
        } else {
            absolute_path(argument, &mut path)
        };

        if let Some(target_len) = target_len {
            let used = with_path_fs(&path[..target_len], output_row, false, |fs| fs.shell_list(&path[..target_len], output_row));
            *row = output_row + used;
            prompt(*row);
            return;
        }

        path_too_long(output_row);
    } else if let Some(argument) = command_arg(input, b"cat") {
        if argument.is_empty() {
            let used = with_path_fs(b"", output_row, false, |fs| fs.shell_cat(b"", output_row));
            *row = output_row + used;
            prompt(*row);
            return;
        }

        let mut path = [0u8; PATH_MAX];

        if let Some(len) = absolute_path(argument, &mut path) {
            let used = with_path_fs(&path[..len], output_row, false, |fs| fs.shell_cat(&path[..len], output_row));
            *row = output_row + used;
            prompt(*row);
            return;
        }

        path_too_long(output_row);
    } else if let Some(argument) = command_arg(input, b"write") {
        let (name, text) = split_first_space(argument);
        let mut path = [0u8; PATH_MAX];

        if name.is_empty() {
            with_path_fs(b"", output_row, true, |fs| fs.shell_write(b"", text, output_row));
        } else if let Some(len) = absolute_path(name, &mut path) {
            with_path_fs(&path[..len], output_row, true, |fs| fs.shell_write(&path[..len], text, output_row));
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"rm") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            with_path_fs(b"", output_row, false, |fs| fs.shell_remove(b"", output_row));
        } else if let Some(len) = absolute_path(argument, &mut path) {
            with_path_fs(&path[..len], output_row, false, |fs| fs.shell_remove(&path[..len], output_row));
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"mkdir") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            with_path_fs(b"", output_row, true, |fs| fs.shell_mkdir(b"", output_row));
        } else if let Some(len) = absolute_path(argument, &mut path) {
            with_path_fs(&path[..len], output_row, true, |fs| fs.shell_mkdir(&path[..len], output_row));
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"rmdir") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            with_path_fs(b"", output_row, false, |fs| fs.shell_rmdir(b"", output_row));
        } else if let Some(len) = absolute_path(argument, &mut path) {
            with_path_fs(&path[..len], output_row, false, |fs| fs.shell_rmdir(&path[..len], output_row));
        } else {
            path_too_long(output_row);
        }
    } else if input == b"halt" {
        vga::print_at(output_row, 0, b"Shutting down...");
        if !power::shutdown() {
            power::halt();
        }
    } else if let Some(argument) = command_arg(input, b"calc") {
        let arg = argument;
        let mut i = 0;

        while i < arg.len() && arg[i] == b' ' {
            i += 1;
        }
        let n1_start = i;
        while i < arg.len() && arg[i] != b' ' {
            i += 1;
        }
        let n1_end = i;

        while i < arg.len() && arg[i] == b' ' {
            i += 1;
        }
        let op_start = i;
        while i < arg.len() && arg[i] != b' ' {
            i += 1;
        }
        let op_end = i;

        while i < arg.len() && arg[i] == b' ' {
            i += 1;
        }
        let n2_start = i;
        while i < arg.len() && arg[i] != b' ' {
            i += 1;
        }
        let n2_end = i;

        let n1_str = &arg[n1_start..n1_end];
        let op_str = &arg[op_start..op_end];
        let n2_str = &arg[n2_start..n2_end];

        if n1_str.is_empty() || op_str.is_empty() || n2_str.is_empty() {
            vga::print_at(output_row, 0, b"Usage: calc <n> <+|-|*|/> <n>");
        } else {
            let n1 = match parse_i32(n1_str) {
                Some(n) => n,
                None => {
                    vga::print_at(output_row, 0, b"Invalid number.");
                    *row = output_row + 1;
                    prompt(*row);
                    return;
                }
            };
            let n2 = match parse_i32(n2_str) {
                Some(n) => n,
                None => {
                    vga::print_at(output_row, 0, b"Invalid number.");
                    *row = output_row + 1;
                    prompt(*row);
                    return;
                }
            };

            let result = match op_str {
                b"+" => n1.wrapping_add(n2),
                b"-" => n1.wrapping_sub(n2),
                b"*" => n1.wrapping_mul(n2),
                b"/" => {
                    if n2 == 0 {
                        vga::print_at(output_row, 0, b"Division by zero.");
                        *row = output_row + 1;
                        prompt(*row);
                        return;
                    }
                    n1.wrapping_div(n2)
                }
                _ => {
                    vga::print_at(output_row, 0, b"Unknown operator. Use + - * /");
                    *row = output_row + 1;
                    prompt(*row);
                    return;
                }
            };

            let mut buf = [0u8; 12];
            let result_len = format_i32(&mut buf, result);

            let mut out = [0u8; 80];
            let mut out_len = 0;

            out[out_len..out_len + n1_str.len()].copy_from_slice(n1_str);
            out_len += n1_str.len();
            out[out_len] = b' ';
            out_len += 1;
            out[out_len..out_len + op_str.len()].copy_from_slice(op_str);
            out_len += op_str.len();
            out[out_len] = b' ';
            out_len += 1;
            out[out_len..out_len + n2_str.len()].copy_from_slice(n2_str);
            out_len += n2_str.len();
            out[out_len] = b'=';
            out[out_len + 1] = b' ';
            out_len += 2;
            out[out_len..out_len + result_len].copy_from_slice(&buf[..result_len]);
            out_len += result_len;

            vga::print_at(output_row, 0, &out[..out_len]);
        }
        *row = output_row + 1;
        prompt(*row);
        return;
    } else if input.is_empty() {
        *row += 1;

        // Пустая команда не должна выводить приглашение за пределы экрана.
        if *row >= vga::height() {
            vga::scroll_screen();
            *row = vga::height() - 1;
        }

        prompt(*row);
        return;
    } else {
        vga::print_at(output_row, 0, b"Unknown command. Type help.");
    }

    *row = output_row + lines;
    prompt(*row);
}

/// Парсит срез байт как целое i32. Возвращает None при ошибке.
fn parse_i32(s: &[u8]) -> Option<i32> {
    if s.is_empty() {
        return None;
    }
    let mut i = 0;
    let mut negative = false;
    if s[i] == b'-' {
        negative = true;
        i += 1;
        if i >= s.len() {
            return None;
        }
    }
    let mut result: i32 = 0;
    while i < s.len() {
        if s[i] < b'0' || s[i] > b'9' {
            return None;
        }
        result = result.wrapping_mul(10).wrapping_add((s[i] - b'0') as i32);
        i += 1;
    }
    if negative {
        result = -result;
    }
    Some(result)
}

/// Записывает i32 в буфер как десятичные цифры, возвращает длину.
fn format_i32(out: &mut [u8], mut value: i32) -> usize {
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut negative = false;
    if value < 0 {
        negative = true;
        value = -value;
    }
    let mut digits = [0u8; 11];
    let mut len = 0;
    while value > 0 {
        digits[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    let mut out_len = 0;
    if negative {
        out[out_len] = b'-';
        out_len += 1;
    }
    while len > 0 {
        len -= 1;
        out[out_len] = digits[len];
        out_len += 1;
    }
    out_len
}

