use crate::exfat;
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
        23
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
        exfat::CAT_LINES
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

    match exfat::path_is_directory(&path[..target_len]) {
        Ok(true) => {
            set_cwd(&path[..target_len]);

            let mut line = [b' '; 80];
            let mut len = 0usize;
            push_message(&mut line, &mut len, b"cwd: ");
            push_message(&mut line, &mut len, &path[..target_len]);
            vga::print_at(row, 0, &line[..len]);
        }
        Ok(false) => {
            vga::print_at(row, 0, b"Not a directory");
        }
        Err(error) => {
            let mut line = [b' '; 80];
            let mut len = 0usize;
            push_message(&mut line, &mut len, b"cd: ");
            push_message(&mut line, &mut len, error.message());
            vga::print_at(row, 0, &line[..len]);
        }
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
    } else if input == b"about" {
        vga::print_at(
            output_row,
            0,
            b"Perfix is an Operating System written in Rust.",
        );
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
        exfat::print_boot_sector(output_row);
    } else if input == b"mount" {
        exfat::print_volume_info(output_row);
    } else if input == b"charset" {
        vga::print_charset(output_row);
    } else if input == b"listpart" {
        let used = exfat::shell_listpart(output_row);
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

        let mut line = [b' '; 80];
        let mut len = 0usize;

        if !valid || number == 0 {
            push_message(&mut line, &mut len, b"Usage: cdpart <n>");
        } else {
            match exfat::select_partition(number - 1) {
                Ok(partition) => {
                    set_cwd(b"/");
                    push_message(&mut line, &mut len, b"partition ");
                    push_dec_message(&mut line, &mut len, number);
                    push_message(&mut line, &mut len, b" selected, LBA ");
                    push_dec_message(
                        &mut line,
                        &mut len,
                        partition.base_lba as usize,
                    );
                }
                Err(error) => {
                    push_message(&mut line, &mut len, b"cdpart: ");
                    push_message(&mut line, &mut len, error.message());
                }
            }
        }

        vga::print_at(output_row, 0, &line[..len]);
    } else if input == b"pwd" {
        vga::print_at(output_row, 0, cwd());
    } else if let Some(argument) = command_arg(input, b"cd") {
        change_directory(argument, output_row);
    } else if input == b"rootdir" {
        let used = exfat::shell_list(b"/", output_row);
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
            let used = exfat::shell_list(&path[..target_len], output_row);
            *row = output_row + used;
            prompt(*row);
            return;
        }

        path_too_long(output_row);
    } else if let Some(argument) = command_arg(input, b"cat") {
        if argument.is_empty() {
            let used = exfat::shell_cat(b"", output_row);
            *row = output_row + used;
            prompt(*row);
            return;
        }

        let mut path = [0u8; PATH_MAX];

        if let Some(len) = absolute_path(argument, &mut path) {
            let used = exfat::shell_cat(&path[..len], output_row);
            *row = output_row + used;
            prompt(*row);
            return;
        }

        path_too_long(output_row);
    } else if let Some(argument) = command_arg(input, b"write") {
        let (name, text) = split_first_space(argument);
        let mut path = [0u8; PATH_MAX];

        if name.is_empty() {
            exfat::shell_write(b"", text, output_row);
        } else if let Some(len) = absolute_path(name, &mut path) {
            exfat::shell_write(&path[..len], text, output_row);
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"rm") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            exfat::shell_remove(b"", output_row);
        } else if let Some(len) = absolute_path(argument, &mut path) {
            exfat::shell_remove(&path[..len], output_row);
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"mkdir") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            exfat::shell_mkdir(b"", output_row);
        } else if let Some(len) = absolute_path(argument, &mut path) {
            exfat::shell_mkdir(&path[..len], output_row);
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"rmdir") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            exfat::shell_rmdir(b"", output_row);
        } else if let Some(len) = absolute_path(argument, &mut path) {
            exfat::shell_rmdir(&path[..len], output_row);
        } else {
            path_too_long(output_row);
        }
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

