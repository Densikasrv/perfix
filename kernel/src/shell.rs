use crate::fs::{self, FileLocation, FileSystem, CAT_LINES};
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

/// Буфер для копирования файлов командами `cp` и `mv`.
///
/// Ядро отображает первые 4 MiB физической памяти, а BSS начинается
/// чуть ниже 2 MiB. Буфер обязан помещаться в это отображение, иначе
/// обращение к нему вызовет page fault.
const COPY_BUFFER_SIZE: usize = 2 * 1024 * 1024;
static mut COPY_BUFFER: [u8; COPY_BUFFER_SIZE] = [0; COPY_BUFFER_SIZE];

/// Сообщение о попытке обратиться к `/dsk` как к обычному каталогу.
const RESERVED_DSK: &[u8] = b"dsk is a reserved system path";

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

/// Вызывает операцию на файловой системе выбранного раздела.
fn for_selected_fs<F>(row: usize, mut f: F) -> usize
where
    F: FnMut(&dyn fs::DirectoryFileSystem, usize) -> usize,
{
    match crate::disk::selected_kind() {
        crate::disk::FsKind::Exfat => f(fs::directory_filesystems()[0], row),
        crate::disk::FsKind::Ext4 => f(fs::directory_filesystems()[1], row),
        crate::disk::FsKind::Other => {
            vga::print_at(row, 0, b"Unsupported filesystem");
            1
        }
    }
}

/// Выполняет операцию по пути на первой ФС, где путь существует.
/// Для операций создания (create = true) используется первая ФС.
fn with_path_fs<F>(
    path: &[u8],
    row: usize,
    create: bool,
    mut f: F,
) -> usize
where
    F: FnMut(&dyn fs::DirectoryFileSystem) -> usize,
{
    if create {
        vga::print_at(row, 0, b"Create operation not supported here");
        return 1;
    }

    let filesystems = fs::directory_filesystems();

    let filesystem = match crate::disk::selected_kind() {
        crate::disk::FsKind::Exfat => filesystems[0],
        crate::disk::FsKind::Ext4 => filesystems[1],
        crate::disk::FsKind::Other => {
            vga::print_at(row, 0, b"Unsupported filesystem");
            return 1;
        }
    };

    match filesystem.path_is_directory(path) {
        Ok(true) => f(filesystem),
        Ok(false) => {
            vga::print_at(row, 0, b"Not a directory");
            1
        }
        Err(_) => {
            vga::print_at(row, 0, b"Path not found");
            1
        }
    }
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
    // В окне pterm приглашение рисует сама оболочка, а консольный вывод
    // только собирается в виртуальный экран.
    if vga::is_capturing() {
        return;
    }

    let mut buffer = [0u8; 80];
    let len = build_prompt(&mut buffer);

    vga::print_at(row, 0, &buffer[..len]);
}

/// Копирует текущее приглашение в буфер и возвращает его длину.
///
/// Используется pdesk: приглашение в pterm показывает раскладку и
/// текущий каталог так же, как консольная оболочка.
pub fn prompt_text(out: &mut [u8; 80]) -> usize {
    build_prompt(out)
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

/// Является ли нормализованный путь системным пространством `/dsk`.
fn is_reserved_dsk(path: &[u8]) -> bool {
    path == b"/dsk" || path.starts_with(b"/dsk/")
}

/// Строит абсолютный путь из текущего каталога и аргумента.
///
/// Публичная обёртка нужна GUI-редактору, который работает через pdesk
/// и не имеет доступа к внутреннему состоянию оболочки.
pub(crate) fn resolve_path(input: &[u8], out: &mut [u8; PATH_MAX]) -> Option<usize> {
    absolute_path(input, out)
}

/// Зарезервирован ли путь под системное пространство `/dsk`.
pub(crate) fn reserved_path(path: &[u8]) -> bool {
    is_reserved_dsk(path)
}

fn parse_partition_number(input: &[u8]) -> Option<usize> {
    if input.is_empty() {
        return None;
    }

    let mut value = 0usize;

    for &byte in input {
        if !byte.is_ascii_digit() {
            return None;
        }

        value = value.checked_mul(10)?;
        value = value.checked_add((byte - b'0') as usize)?;
    }

    Some(value)
}

/// Разбирает аргумент в расположение файла.
///
/// Путь `/dsk/N/...` адресует раздел `N`; остальные пути относятся
/// к текущему выбранному разделу.
fn parse_file_location(
    argument: &[u8],
    out: &mut [u8; PATH_MAX],
) -> Result<FileLocation, &'static [u8]> {
    let kind;
    let start_lba;
    let sectors;
    let path_len;

    if argument.starts_with(b"/dsk/") {
        let rest = &argument[5..];
        let mut digits_end = 0usize;

        while digits_end < rest.len() && rest[digits_end].is_ascii_digit() {
            digits_end += 1;
        }

        if digits_end == 0 {
            return Err(RESERVED_DSK);
        }

        let number = parse_partition_number(&rest[..digits_end])
            .ok_or(b"Partition not found" as &'static [u8])?;
        let tail = &rest[digits_end..];

        if !tail.is_empty() && tail[0] != b'/' {
            return Err(RESERVED_DSK);
        }

        // Собираем путь внутри раздела и нормализуем его.
        let mut raw = [0u8; PATH_MAX];
        raw[0] = b'/';
        let remainder = if tail.is_empty() { tail } else { &tail[1..] };

        if remainder.len() + 1 > raw.len() {
            return Err(b"Path is too long" as &'static [u8]);
        }

        raw[1..1 + remainder.len()].copy_from_slice(remainder);
        path_len = absolute_path(&raw[..1 + remainder.len()], out)
            .ok_or(b"Path is too long" as &'static [u8])?;

        // Внутри раздела каталог `dsk` тоже зарезервирован.
        if is_reserved_dsk(&out[..path_len]) {
            return Err(RESERVED_DSK);
        }

        let mut partitions = [crate::disk::DiskPartition {
            number: 0,
            kind: crate::disk::FsKind::Other,
            start_lba: 0,
            sectors: 0,
        }; 4];
        let count = crate::disk::read_partitions(&mut partitions);
        let partition = partitions[..count]
            .iter()
            .find(|p| p.number == number)
            .ok_or(b"Partition not found" as &'static [u8])?;

        if partition.kind == crate::disk::FsKind::Other {
            return Err(b"Unsupported filesystem" as &'static [u8]);
        }

        kind = partition.kind;
        start_lba = partition.start_lba;
        sectors = partition.sectors;
    } else {
        path_len = absolute_path(argument, out)
            .ok_or(b"Path is too long" as &'static [u8])?;

        if is_reserved_dsk(&out[..path_len]) {
            return Err(RESERVED_DSK);
        }

        kind = crate::disk::selected_kind();

        if kind == crate::disk::FsKind::Other {
            return Err(b"Unsupported filesystem" as &'static [u8]);
        }

        start_lba = crate::disk::selected_lba();
        sectors = crate::disk::selected_sectors();
    }

    FileLocation::new(kind, start_lba, sectors, &out[..path_len])
        .ok_or(b"Path is too long" as &'static [u8])
}

/// Выполняет `cp` или `mv`. Возвращает число занятых строк вывода.
fn transfer_command(argument: &[u8], row: usize, moving: bool) -> usize {
    let (source_arg, destination_arg) = split_first_space(argument);

    if source_arg.is_empty() || destination_arg.is_empty() {
        if moving {
            vga::print_at(row, 0, b"Usage: mv <source> <destination>");
        } else {
            vga::print_at(row, 0, b"Usage: cp <source> <destination>");
        }

        return 1;
    }

    let mut source_path = [0u8; PATH_MAX];
    let mut destination_path = [0u8; PATH_MAX];

    let source = match parse_file_location(source_arg, &mut source_path) {
        Ok(location) => location,
        Err(message) => {
            vga::print_at(row, 0, message);
            return 1;
        }
    };

    let destination =
        match parse_file_location(destination_arg, &mut destination_path) {
            Ok(location) => location,
            Err(message) => {
                vga::print_at(row, 0, message);
                return 1;
            }
        };

    if source.same_file(&destination) {
        vga::print_at(row, 0, b"source and destination are the same");
        return 1;
    }

    // Ядро однопоточное, поэтому статический буфер можно использовать
    // напрямую без блокировки.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(COPY_BUFFER) };

    let result = if moving {
        fs::move_file(&source, &destination, buffer)
    } else {
        fs::copy_file(&source, &destination, buffer)
    };

    let mut message = [b' '; 80];
    let mut len = 0usize;

    match result {
        Ok(size) => {
            push_message(
                &mut message,
                &mut len,
                if moving { b"mv: moved " } else { b"cp: copied " },
            );
            push_dec_message(&mut message, &mut len, size as usize);
            push_message(&mut message, &mut len, b" bytes");
        }
        Err(error) => {
            push_message(
                &mut message,
                &mut len,
                if moving { b"mv: " } else { b"cp: " },
            );
            push_message(&mut message, &mut len, error.message());
        }
    }

    vga::print_at(row, 0, &message[..len]);
    1
}

/// Сколько строк вывода резервировать под команду до её выполнения.
/// Дописывает число с ведущим нулём (для даты и времени).
fn push_fixed2<const N: usize>(out: &mut [u8; N], len: &mut usize, value: u32) {
    if value < 10 {
        push_message(out, len, b"0");
    }

    push_dec_message(out, len, value as usize);
}

/// Дописывает часовой пояс в формате GMT+3:00.
fn push_timezone<const N: usize>(out: &mut [u8; N], len: &mut usize, minutes: i32) {
    push_message(out, len, b"GMT");

    if minutes < 0 {
        push_message(out, len, b"-");
    } else {
        push_message(out, len, b"+");
    }

    let total = minutes.abs();
    push_dec_message(out, len, (total / 60) as usize);
    push_message(out, len, b":");
    push_fixed2(out, len, (total % 60) as u32);
}

/// Разбирает часовой пояс: GMT+3, GMT-5, GMT+3:30, +2.
fn parse_timezone(input: &[u8]) -> Option<i32> {
    let input = trim_ascii_space(input);
    let mut rest = input;

    if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case(b"GMT") {
        rest = &rest[3..];
    }

    if rest.is_empty() {
        return None;
    }

    let sign = if rest[0] == b'-' { -1 } else { 1 };

    if rest[0] == b'+' || rest[0] == b'-' {
        rest = &rest[1..];
    }

    let mut hours = 0i32;
    let mut minutes = 0i32;
    let mut part = 0usize;
    let mut digits = 0usize;

    for &byte in rest {
        if byte.is_ascii_digit() {
            if part == 0 {
                hours = hours
                    .saturating_mul(10)
                    .saturating_add((byte - b'0') as i32);
            } else {
                minutes = minutes
                    .saturating_mul(10)
                    .saturating_add((byte - b'0') as i32);
            }

            digits += 1;
        } else if byte == b':' && part == 0 && digits > 0 {
            part = 1;
            digits = 0;
        } else {
            return None;
        }
    }

    if digits == 0 || hours > 14 || minutes > 59 {
        return None;
    }

    Some(sign * (hours * 60 + minutes))
}

/// Разбирает дату и время в формате dd/mm/yyyy/hh/mm/ss.
fn parse_datetime(input: &[u8]) -> Option<(u32, u32, u32, u32, u32, u32)> {
    let mut values = [0u32; 6];
    let mut index = 0usize;
    let mut digits = 0usize;
    let mut value = 0u32;

    for &byte in input {
        if byte.is_ascii_digit() {
            value = value
                .saturating_mul(10)
                .saturating_add((byte - b'0') as u32);
            digits += 1;
        } else if byte == b'/' {
            if digits == 0 || index >= 5 {
                return None;
            }

            values[index] = value;
            index += 1;
            value = 0;
            digits = 0;
        } else {
            return None;
        }
    }

    if digits == 0 || index != 5 {
        return None;
    }

    values[5] = value;

    // В команде порядок dd/mm/yyyy/hh/mm/ss, наружу отдаём год/месяц/день.
    Some((
        values[2], values[1], values[0], values[3], values[4], values[5],
    ))
}

fn command_lines(input: &[u8]) -> usize {
    if input == b"help" {
        if crate::installer::available() {
            34
        } else {
            32
        }
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

    if is_reserved_dsk(&path[..target_len]) {
        vga::print_at(row, 0, RESERVED_DSK);
        return;
    }

    let mut cd_handled = false;
    for filesystem in fs::directory_filesystems() {
    match filesystem.path_is_directory(&path[..target_len]) {
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

/// Освобождает на экране место под `extra` строк, прокручивая его сразу
/// на нужное количество строк (а не по одной).
fn make_room(row: &mut usize, extra: usize) {
    let height = vga::height();

    if height == 0 {
        return;
    }

    let limit = height - 1;

    if *row + extra > limit {
        let needed = (*row + extra - limit).min(*row);

        vga::scroll_screen_lines(needed);
        *row -= needed;
    }
}

pub fn execute(input: &[u8], row: &mut usize) {
    if input == b"clear" {
        show_screen(None);
        *row = 6;
        return;
    }

    if input == b"guinit" {
        // Ранняя версия pdesk: запускается вручную, а не при загрузке.
        match crate::gui::run() {
            Ok(()) => show_screen(Some(b"pdesk: session finished")),
            Err(message) => show_screen(Some(message)),
        }

        *row = 6;
        return;
    }

    if input == b"mouse" {
        // Освобождаем место под две строки демонстрации.
        make_room(row, 3);

        let used = crate::mouse::demo(*row + 1);
        *row += 1 + used;

        if *row >= vga::height() {
            vga::scroll_screen();
            *row = vga::height() - 1;
        }

        prompt(*row);
        return;
    }

    if let Some(argument) = command_arg(input, b"timezone") {
        // Освобождаем место под строку вывода и приглашение.
        make_room(row, 2);

        let output_row = *row + 1;
        let mut line = [b' '; 80];
        let mut len = 0usize;

        if argument.is_empty() {
            push_message(&mut line, &mut len, b"timezone: ");
            push_timezone(&mut line, &mut len, crate::rtc::timezone_minutes());
        } else if let Some(minutes) = parse_timezone(argument) {
            crate::rtc::set_timezone_minutes(minutes);
            push_message(&mut line, &mut len, b"timezone set: ");
            push_timezone(&mut line, &mut len, crate::rtc::timezone_minutes());
        } else {
            push_message(&mut line, &mut len, b"Usage: timezone <GMT+3>");
        }

        vga::print_at(output_row, 0, &line[..len]);
        *row = output_row + 1;
        prompt(*row);
        return;
    }

    if let Some(argument) = command_arg(input, b"time") {
        // Освобождаем место под строку вывода и приглашение.
        make_room(row, 2);

        let output_row = *row + 1;
        let mut line = [b' '; 80];
        let mut len = 0usize;

        match parse_datetime(argument) {
            Some((year, month, day, hour, minute, second)) => {
                let (utc_year, utc_month, utc_day, utc_hour, utc_minute) =
                    crate::rtc::local_to_utc(year, month, day, hour, minute);

                if crate::rtc::set_datetime(
                    utc_year, utc_month, utc_day, utc_hour, utc_minute, second,
                ) {
                    push_message(&mut line, &mut len, b"time set: ");
                    push_fixed2(&mut line, &mut len, day);
                    push_message(&mut line, &mut len, b"/");
                    push_fixed2(&mut line, &mut len, month);
                    push_message(&mut line, &mut len, b"/");
                    push_dec_message(&mut line, &mut len, year as usize);
                    push_message(&mut line, &mut len, b" ");
                    push_fixed2(&mut line, &mut len, hour);
                    push_message(&mut line, &mut len, b":");
                    push_fixed2(&mut line, &mut len, minute);
                    push_message(&mut line, &mut len, b":");
                    push_fixed2(&mut line, &mut len, second);
                } else {
                    push_message(&mut line, &mut len, b"time: invalid date or time");
                }
            }
            None => push_message(&mut line, &mut len, b"Usage: time <dd/mm/yyyy/hh/mm/ss>"),
        }

        vga::print_at(output_row, 0, &line[..len]);
        *row = output_row + 1;
        prompt(*row);
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
            if is_reserved_dsk(&path[..path_len]) {
                push_message(&mut message, &mut len, b"tedit: ");
                push_message(&mut message, &mut len, RESERVED_DSK);
            } else {
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
            }
        } else {
            push_message(&mut message, &mut len, b"tedit: path is too long");
        }

        show_screen(Some(&message[..len]));
        *row = 6;
        return;
    }

    let lines = command_lines(input);

    // Освобождаем место ДО вывода всей команды — одним сдвигом экрана.
    make_room(row, 1 + lines);

    let output_row = *row + 1;

    if input == b"help" {
        vga::print_at(output_row, 0, b"Available commands:");
        vga::print_at(output_row + 1, 0, b"help      - show commands");
        vga::print_at(output_row + 2, 0, b"about     - about Perfix");
        vga::print_at(output_row + 3, 0, b"version   - OS information");
        vga::print_at(output_row + 4, 0, b"clear     - clear the screen");
        vga::print_at(output_row + 5, 0, b"divzero   - test CPU exception");
        vga::print_at(output_row + 6, 0, b"pagefault - test page fault");
        vga::print_at(output_row + 7, 0, b"diskinfo  - read boot sector");
        vga::print_at(output_row + 8, 0, b"mount     - show volume info");
        vga::print_at(output_row + 9, 0, b"ls [path] - list directory contents");
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
        vga::print_at(output_row + 20, 0, b"listpart  - list partitions");
        vga::print_at(output_row + 21, 0, b"cdpart <n>- select partition");
        vga::print_at(output_row + 22, 0, b"fetch     - system information");
        vga::print_at(output_row + 23, 0, b"halt      - turn off the computer");
        vga::print_at(output_row + 24, 0, b"calc      - simple calculator");
        vga::print_at(output_row + 25, 0, b"cp <src> <dst> - copy file");
        vga::print_at(output_row + 26, 0, b"mv <src> <dst> - move file");
        vga::print_at(output_row + 27, 0, b"mouse     - PS/2 mouse demo");
        vga::print_at(output_row + 28, 0, b"guinit    - start pdesk (GUI)");
        vga::print_at(output_row + 29, 0, b"timezone <GMT+N> - set time zone");
        vga::print_at(output_row + 30, 0, b"time <dd/mm/yyyy/hh/mm/ss> - set clock");
        vga::print_at(output_row + 31, 0, b"mkfs.ext4 <n> - format partition as ext4");

        if crate::installer::available() {
            vga::print_at(output_row + 32, 0, b"install [n] [format] - install Perfix");
            vga::print_at(output_row + 33, 0, b"setup [n] [format]   - alias for install");
        }
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
    }; crate::fs::exfat::MAX_PARTITIONS];

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

            let mut line = [b' '; 80];
            let mut len = 0usize;

            push_message(&mut line, &mut len, b"Partition ");
            push_dec_message(&mut line, &mut len, p.number);
            push_message(&mut line, &mut len, b"  ");
            push_message(&mut line, &mut len, name);
            push_message(&mut line, &mut len, b"  LBA ");
            push_dec_message(&mut line, &mut len, p.start_lba as usize);
            push_message(&mut line, &mut len, b"  sectors ");
            push_dec_message(&mut line, &mut len, p.sectors as usize);

            vga::print_at(output_row + 1 + i, 0, &line[..len]);
        }
    }

    *row = output_row + count + 1;
    prompt(*row);
    return;


    } else if let Some(argument) = command_arg(input, b"mkfs.ext4") {
        let used = crate::installer::mkfs_shell_command(argument, output_row);
        *row = output_row + used;
        prompt(*row);
        return;
    } else if crate::installer::available()
        && command_arg(input, b"setup").is_some()
    {
        let argument = command_arg(input, b"setup").unwrap_or(b"");
        let used = crate::installer::shell_command(argument, output_row);
        *row = output_row + used;
        prompt(*row);
        return;
    } else if crate::installer::available()
        && command_arg(input, b"install").is_some()
    {
        let argument = command_arg(input, b"install").unwrap_or(b"");
        let used = crate::installer::shell_command(argument, output_row);
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

    let mut inode_hex = [b'0'; 8];
    let digits = b"0123456789ABCDEF";
    let mut value = inode_number;

    for i in (0..8).rev() {
        inode_hex[i] = digits[(value & 0xF) as usize];
        value >>= 4;
    }

    vga::print_at(
        output_row + 7 + entries,
        20,
        b"inode 0x",
    );
    vga::print_at(
        output_row + 7 + entries,
        28,
        &inode_hex,
    );

    if inode_number == 11 {
    let test_row = output_row + 7 + entries;

    match crate::disk::read_ext4_inode(number, inode_number) {
        Ok(_info) => {
            vga::print_at(test_row, 40, b"INODE READ OK");

            let mut test_block = [0u8; 4096];

            match crate::disk::read_ext4_directory_block(
                number,
                inode_number,
                &mut test_block,
            ) {
                Ok(_size) => {
                    vga::print_at(test_row, 54, b"DIR READ OK");
                }
                Err(message) => {
                    vga::print_at(test_row, 54, message);
                }
            }
        }
        Err(message) => {
            vga::print_at(test_row, 40, message);
        }
    }
}
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
            if is_reserved_dsk(&path[..target_len]) {
                vga::print_at(output_row, 0, RESERVED_DSK);
                *row = output_row + 1;
                prompt(*row);
                return;
            }

            let used = with_path_fs(&path[..target_len], output_row, false, |fs| fs.shell_list(&path[..target_len], output_row));
            *row = output_row + used;
            prompt(*row);
            return;
        }

        path_too_long(output_row);
    } else if let Some(argument) = command_arg(input, b"cat") {
        let mut path = [0u8; PATH_MAX];

        let len = if argument.is_empty() {
            0
        } else {
            match absolute_path(argument, &mut path) {
                Some(len) => len,
                None => {
                    path_too_long(output_row);
                    return;
                }
            }
        };

        if len > 0 && is_reserved_dsk(&path[..len]) {
            vga::print_at(output_row, 0, RESERVED_DSK);
            *row = output_row + 1;
            prompt(*row);
            return;
        }

        let filesystems = fs::directory_filesystems();

        let filesystem = match crate::disk::selected_kind() {
            crate::disk::FsKind::Exfat => filesystems[0],
            crate::disk::FsKind::Ext4 => filesystems[1],
            crate::disk::FsKind::Other => {
                vga::print_at(output_row, 0, b"Unsupported filesystem");
                prompt(output_row + 1);
                return;
            }
        };

        let used = filesystem.shell_cat(&path[..len], output_row);
        *row = output_row + used;
        prompt(*row);
        return;
    } else if let Some(argument) = command_arg(input, b"write") {
        let (name, text) = split_first_space(argument);
        let mut path = [0u8; PATH_MAX];

        let path_len = if name.is_empty() {
            Some(0)
        } else {
            absolute_path(name, &mut path)
        };

        if let Some(len) = path_len {
            let write_path = &path[..len];

            if is_reserved_dsk(write_path) {
                vga::print_at(output_row, 0, RESERVED_DSK);
                *row = output_row + 1;
                prompt(*row);
                return;
            }

            match crate::disk::selected_kind() {
                crate::disk::FsKind::Exfat => {
                    for_each_fs(output_row, |fs, row| {
                        fs.shell_write(write_path, text, row)
                    });
                }
                crate::disk::FsKind::Ext4 => {
                    let filesystem = fs::directory_filesystems()[1];
                    let used = filesystem.shell_write(
                        write_path,
                        text,
                        output_row,
                    );
                    *row = output_row + used;
                    prompt(*row);
                    return;
                }
                crate::disk::FsKind::Other => {
                    vga::print_at(output_row, 0, b"Unsupported filesystem");
                    prompt(output_row + 1);
                    return;
                }
            }
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"rm") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            for_selected_fs(output_row, |fs, row| fs.shell_remove(b"", row));
        } else if let Some(len) = absolute_path(argument, &mut path) {
            if is_reserved_dsk(&path[..len]) {
                vga::print_at(output_row, 0, RESERVED_DSK);
                *row = output_row + 1;
                prompt(*row);
                return;
            }

            for_selected_fs(output_row, |fs, row| {
                fs.shell_remove(&path[..len], row)
            });
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"mkdir") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            for_selected_fs(output_row, |fs, row| fs.shell_mkdir(b"", row));
        } else if let Some(len) = absolute_path(argument, &mut path) {
            if is_reserved_dsk(&path[..len]) {
                vga::print_at(output_row, 0, RESERVED_DSK);
                *row = output_row + 1;
                prompt(*row);
                return;
            }

            for_selected_fs(output_row, |fs, row| {
                fs.shell_mkdir(&path[..len], row)
            });
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"rmdir") {
        let mut path = [0u8; PATH_MAX];

        if argument.is_empty() {
            for_selected_fs(output_row, |fs, row| fs.shell_rmdir(b"", row));
        } else if let Some(len) = absolute_path(argument, &mut path) {
            if is_reserved_dsk(&path[..len]) {
                vga::print_at(output_row, 0, RESERVED_DSK);
                *row = output_row + 1;
                prompt(*row);
                return;
            }

            for_selected_fs(output_row, |fs, row| {
                fs.shell_rmdir(&path[..len], row)
            });
        } else {
            path_too_long(output_row);
        }
    } else if let Some(argument) = command_arg(input, b"cp") {
        let used = transfer_command(argument, output_row, false);
        *row = output_row + used;
        prompt(*row);
        return;
    } else if let Some(argument) = command_arg(input, b"mv") {
        let used = transfer_command(argument, output_row, true);
        *row = output_row + used;
        prompt(*row);
        return;
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

