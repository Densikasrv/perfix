//! Выполнение команд консольной оболочки для окна pterm.
//!
//! pdesk — отдельный crate и не имеет доступа к файловой системе, поэтому
//! команды выполняет ядро:
//!
//! * обычные команды работают в виртуальной консоли 80x40, её текст затем
//!   копируется в окно pterm;
//! * полноэкранные команды (tedit, mouse, divzero, pagefault, halt)
//!   получают настоящий экран целиком, после чего pdesk перерисовывается;
//! * `guinit` из pterm запрещён, чтобы не запустить оболочку рекурсивно.

use crate::{shell, vga};

/// Виртуальная консоль, в которую пишет вывод команд pterm.
static mut CONSOLE: vga::CaptureConsole = vga::CaptureConsole::new();

/// Команды, которым нужен настоящий экран целиком.
const CONSOLE_COMMANDS: [&[u8]; 5] = [b"tedit", b"mouse", b"divzero", b"pagefault", b"halt"];

/// Выполняет команду оболочки.
///
/// Вывод записывается в `output` строками через `\n`, новое приглашение
/// (раскладка и текущий каталог) — в `prompt`.
pub fn run_command(command: &[u8], output: &mut [u8], prompt: &mut [u8]) -> pdesk::CommandResult {
    let mut result = pdesk::CommandResult::default();

    // Каталог мог измениться, поэтому приглашение обновляем всегда.
    let mut buffer = [0u8; 80];
    let prompt_size = shell::prompt_text(&mut buffer);
    let copy = prompt_size.min(prompt.len());
    prompt[..copy].copy_from_slice(&buffer[..copy]);
    result.prompt_length = copy;

    let command = trim(command);

    if command.is_empty() {
        return result;
    }

    let name = command_name(command);

    if name == b"guinit" {
        result.length = copy_message(output, b"guinit: pdesk is already running");
        return result;
    }

    if is_console_command(name) {
        // Отдаём команде настоящий экран: pdesk перерисуется после возврата.
        vga::clear_screen();

        let mut row = 0usize;
        shell::execute(command, &mut row);

        vga::clear_screen();
        result.fullscreen = true;
        return result;
    }

    let console = unsafe { &mut *core::ptr::addr_of_mut!(CONSOLE) };

    vga::begin_capture(console);

    let mut row = 0usize;
    shell::execute(command, &mut row);

    vga::end_capture();

    result.length = copy_screen(console.cells(), output);
    result
}

/// Отбрасывает пробелы и табуляции по краям команды.
fn trim(input: &[u8]) -> &[u8] {
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

/// Возвращает имя команды — текст до первого пробела.
fn command_name(input: &[u8]) -> &[u8] {
    let mut end = 0usize;

    while end < input.len() && input[end] != b' ' && input[end] != b'\t' {
        end += 1;
    }

    &input[..end]
}

fn is_console_command(name: &[u8]) -> bool {
    CONSOLE_COMMANDS.iter().any(|command| *command == name)
}

/// Копирует строку сообщения в буфер вывода.
fn copy_message(output: &mut [u8], message: &[u8]) -> usize {
    let length = message.len().min(output.len());
    output[..length].copy_from_slice(&message[..length]);
    length
}

/// Копирует непустые строки виртуальной консоли в буфер вывода.
///
/// Пустые строки в начале и в конце отбрасываются, хвостовые пробелы
/// в строках — тоже: так в окно попадает только осмысленный текст.
fn copy_screen(cells: &[u8], output: &mut [u8]) -> usize {
    let width = vga::CAPTURE_WIDTH;
    let height = vga::CAPTURE_HEIGHT;

    let mut first = height;
    let mut last = 0usize;

    for row in 0..height {
        let line = &cells[row * width..(row + 1) * width];

        if line.iter().any(|&byte| byte != b' ') {
            if first == height {
                first = row;
            }

            last = row;
        }
    }

    if first == height {
        return 0;
    }

    let mut length = 0usize;

    for row in first..=last {
        let line = &cells[row * width..(row + 1) * width];
        let mut end = width;

        while end > 0 && line[end - 1] == b' ' {
            end -= 1;
        }

        if row > first {
            if length >= output.len() {
                break;
            }

            output[length] = b'\n';
            length += 1;
        }

        for &byte in &line[..end] {
            if length >= output.len() {
                break;
            }

            output[length] = byte;
            length += 1;
        }
    }

    length
}
