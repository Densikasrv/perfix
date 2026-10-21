use crate::exfat;
use crate::vga;
use crate::version;

pub fn show_screen(message: Option<&[u8]>) {
    vga::clear_screen();

    vga::print_at(0, 0, b"Welcome to Perfix Shell!");
    vga::print_at(
        2,
        0,
        b"Commands: help, about, version, clear, divzero, pagefault,",
    );
    vga::print_at(3, 0, b"          diskinfo, mount, ls [path], cat <file>");

    if let Some(text) = message {
        vga::print_at(4, 0, text);
    }

    prompt(6);
}

/// Длина приглашения в символах: `perfix[EN]> ` и `perfix[RU]> `.
pub const PROMPT_WIDTH: usize = 12;

pub fn prompt(row: usize) {
    let text: &[u8] = if crate::keyboard::is_russian() {
        b"perfix[RU]> "
    } else {
        b"perfix[EN]> "
    };

    vga::print_at(row, 0, text);
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
        14
    } else if input == b"version" {
        5
    } else if input == b"diskinfo" {
        9
    } else if input == b"mount" {
        14
    } else if input == b"charset" {
        6
    } else if input == b"rootdir" {
        16
    } else if command_arg(input, b"ls").is_some() {
        16
    } else if command_arg(input, b"cat").is_some() {
        exfat::CAT_LINES
    } else {
        1
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

    let lines = command_lines(input);

    // Освобождаем место ДО вывода всей команды.
    while *row + 1 + lines > 24 && *row > 0 {
        vga::scroll_screen();
        *row -= 1;
    }

    let output_row = *row + 1;

    if input == b"help" {
        vga::print_at(output_row, 0, b"Available commands:");
        vga::print_at(output_row + 1, 0, b"help      - show commands");
        vga::print_at(output_row + 2, 0, b"about     - about Perfix");
        vga::print_at(output_row + 3, 0, b"version   - kernel information");
        vga::print_at(output_row + 4, 0, b"clear     - clear the screen");
        vga::print_at(output_row + 5, 0, b"divzero   - test CPU exception");
        vga::print_at(output_row + 6, 0, b"pagefault - test page fault");
        vga::print_at(output_row + 7, 0, b"diskinfo  - read exFAT boot sector");
        vga::print_at(output_row + 8, 0, b"mount     - show exFAT volume info");
        vga::print_at(output_row + 9, 0, b"ls [path] - list exFAT directory");
        vga::print_at(output_row + 10, 0, b"cat <file>- print file contents");
        vga::print_at(output_row + 11, 0, b"charset   - show CP866 glyphs");
        vga::print_at(output_row + 12, 0, b"write <f> <text> - create/overwrite file");
        vga::print_at(output_row + 13, 0, b"rm <file> - delete file");
    } else if input == b"about" {
        vga::print_at(
            output_row,
            0,
            b"Perfix is a bare-metal kernel written in Rust.",
        );
    } else if input == b"version" {
        vga::print_at(output_row, 0, version::KERNEL_NAME);
        vga::print_at(output_row + 1, 0, b"Kernel version: ");
        vga::print_at(output_row + 1, 16, version::KERNEL_VERSION);
        vga::print_at(output_row + 2, 0, b"Architecture: ");
        vga::print_at(output_row + 2, 13, version::ARCHITECTURE);
        vga::print_at(output_row + 3, 0, b"Language: ");
        vga::print_at(output_row + 3, 10, version::LANGUAGE);
        vga::print_at(output_row + 4, 0, b"Status: ");
        vga::print_at(output_row + 4, 8, version::STATUS);
    } else if input == b"diskinfo" {
        exfat::print_boot_sector(output_row);
    } else if input == b"mount" {
        exfat::print_volume_info(output_row);
    } else if input == b"charset" {
        vga::print_charset(output_row);
    } else if input == b"rootdir" || command_arg(input, b"ls").is_some() {
        let root: &[u8] = b"/";
        let path = command_arg(input, b"ls").unwrap_or(root);
        exfat::shell_list(path, output_row);
    } else if let Some(path) = command_arg(input, b"cat") {
        exfat::shell_cat(path, output_row);
    } else if let Some(argument) = command_arg(input, b"write") {
        exfat::shell_write(argument, output_row);
    } else if let Some(argument) = command_arg(input, b"rm") {
        exfat::shell_remove(argument, output_row);
    } else if input.is_empty() {
        *row += 1;
        prompt(*row);
        return;
    } else {
        vga::print_at(output_row, 0, b"Unknown command. Type help.");
    }

    *row = output_row + lines;
    prompt(*row);
}

