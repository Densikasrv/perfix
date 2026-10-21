use crate::vga;
use crate::version;

pub fn show_screen(message: Option<&[u8]>) {
    vga::clear_screen();

    vga::print_at(0, 0, b"Welcome to Perfix Shell!");
    vga::print_at(
    2,
    0,
    b"Commands: help, about, version, clear, divzero, pagefault, diskinfo",
);

    if let Some(text) = message {
        vga::print_at(4, 0, text);
    }

    prompt(6);
}

fn prompt(row: usize) {
    vga::print_at(row, 0, b"perfix> ");
}


pub fn execute(input: &[u8], row: &mut usize) {
    if input == b"clear" {
        show_screen(None);
        *row = 6;
        return;
    }

    if input == b"divzero" {
        crate::idt::test_divzero();
        return;
    }

    if input == b"pagefault" {
        crate::paging::test_page_fault();
        return;
    }

    let lines = if input == b"help" {
        8
    } else if input == b"version" {
        5
    } else if input == b"diskinfo" {
        9
    } else if input == b"rootdir" {
        6
    } else {
        1
    };

    // Освобождаем место ДО вывода всей команды.
    while *row + 1 + lines > 24 {
        vga::scroll_screen();
        *row -= 1;
    }

    let output_row = *row + 1;

    if input == b"diskinfo" {
        crate::ata::test_disk(output_row);
    } else if input == b"rootdir" {
        crate::ata::test_root_directory(output_row);
    } else if input == b"help" {
        vga::print_at(output_row, 0, b"Available commands:");
        vga::print_at(output_row + 1, 0, b"help    - show commands");
        vga::print_at(output_row + 2, 0, b"about   - about Perfix");
        vga::print_at(output_row + 3, 0, b"version - kernel information");
        vga::print_at(output_row + 4, 0, b"clear   - clear the screen");
        vga::print_at(output_row + 5, 0, b"divzero - test CPU exception");
        vga::print_at(output_row + 6, 0, b"pagefault - test page fault");
        vga::print_at(output_row + 7, 0, b"diskinfo - read exFAT boot sector");
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
