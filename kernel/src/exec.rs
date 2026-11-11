//! Запуск сторонних статических ELF32-программ.
//!
//! Поддерживаются только `ET_EXEC`, `EM_386`, little-endian, без
//! интерпретатора (не PIE и не dynamic). Сегменты `PT_LOAD` копируются в
//! выделенные физические кадры, а виртуальные адреса программы временно
//! отображаются на них через таблицы страниц. После завершения программы
//! исходное тождественное отображение восстанавливается.
//!
//! Программа пока выполняется в ring 0 и общается с ядром через `int 0x80`.
//! Номера вызовов совпадают с Linux i386:
//!
//! * `eax = 1` (exit) / `eax = 252` (exit_group), `ebx = код возврата`;
//! * `eax = 4` (write), `ebx = fd`, `ecx = буфер`, `edx = длина`.
//!
//! Неизвестные вызовы возвращают `-1`.

use core::arch::asm;

use crate::arena;
use crate::fs::{self, FileLocation};
use crate::paging;
use crate::sysinfo;
use crate::vga;

/// Максимальный размер ELF-файла и стека программы.
///
/// Сами буферы больше не лежат в `.bss`: под них выделяется столько
/// памяти, сколько нужно, из [`arena`], поэтому на машине с малым
/// объёмом RAM загрузка маленькой программы не требует мегабайтов.
const ELF_BUFFER_SIZE: usize = 4 * 1024 * 1024;
const PROGRAM_STACK_SIZE: usize = 256 * 1024;

const PAGE_SIZE: u32 = 4096;

/// Программа не должна занимать первые мегабайты, где живут ядро,
/// загрузочные модули и статические буферы.
const PROGRAM_MIN_VADDR: u32 = 0x0100_0000; // 16 MiB
/// Выше начинается область framebuffer/MMIO, которую нельзя перекрывать.
const PROGRAM_MAX_VADDR: u32 = 0xE000_0000;

/// Куча для программ: glibc использует `brk` на старте, чтобы разместить
/// блок TLS. Физические страницы выделяются по мере роста `brk`, а не все
/// 8 МиБ сразу — на машинах с малым объёмом RAM это критично.
const HEAP_VADDR: u32 = 0x4000_0000;
const HEAP_SIZE: u32 = 8 * 1024 * 1024;

const FRAME_CEILING: u32 = 0xFC00_0000; // 252 MiB
/// Запас в верхней части RAM; кадры выделяются ниже него.
const FRAME_TOP_RESERVE: u32 = 0x0010_0000; // 1 MiB

const ELF_HEADER_SIZE: usize = 52;
const PROGRAM_HEADER_SIZE: usize = 32;

static mut RUNNING: bool = false;
static mut OUT_ROW: usize = 0;
static mut OUT_COL: usize = 0;
static mut FRAME_NEXT: u32 = 0;
static mut FRAME_FLOOR_CURRENT: u32 = 0;
static mut PROGRAM_BREAK: u32 = 0;
/// До какого адреса куча уже отображена и обнулена.
static mut HEAP_MAPPED_END: u32 = 0;

unsafe extern "C" {
    /// Переключает стек и передаёт управление точке входа программы.
    fn elf_enter(entry: u32, stack_top: u32);
    /// Восстанавливает стек ядра и возвращает управление в [`run`].
    fn elf_restore_kernel() -> !;
}

/// Выполняется ли сейчас загруженная программа.
pub fn active() -> bool {
    unsafe { RUNNING }
}

/// Обрабатывает исключение CPU во время работы программы.
pub fn handle_exception(vector: u32, error: u32, eip: u32) -> ! {
    let mut message = [b' '; 96];
    let mut len = 0usize;

    push(&mut message, &mut len, b"exec: program crashed: exception ");
    push_hex(&mut message, &mut len, vector, 2);
    push(&mut message, &mut len, b" at EIP 0x");
    push_hex(&mut message, &mut len, eip, 8);

    if vector == 14 {
        push(&mut message, &mut len, b" error 0x");
        push_hex(&mut message, &mut len, error, 4);
    }

    emit(&message[..len]);
    emit(b"\n");

    unsafe { elf_restore_kernel() }
}

/// Завершает программу по системному вызову `exit`.
pub fn exit_program(_code: u32) -> ! {
    unsafe { elf_restore_kernel() }
}

/// Выводит буфер программы в консоль (в pterm — в виртуальный экран).
pub fn write_output(pointer: u32, length: u32) {
    if pointer == 0 || length == 0 {
        return;
    }

    // Ограничиваем один вызов, чтобы ошибка в программе не заставила
    // ядро читать гигантский диапазон памяти.
    let length = length.min(1024 * 1024) as usize;
    let bytes = unsafe {
        core::slice::from_raw_parts(pointer as *const u8, length)
    };

    emit(bytes);
}

/// Реализация `writev(fd, iovec *iov, iovcnt)` (номер 146).
///
/// glibc использует его для сообщений об ошибках на старте. Каждый элемент
/// `iovec` — пара `{ base: u32, len: u32 }`. Возвращает число байт.
pub fn writev_output(vector: u32, count: u32) -> u32 {
    if vector == 0 || count == 0 {
        return 0;
    }

    let count = count.min(64);
    let mut total = 0u32;

    for index in 0..count {
        let entry = vector.wrapping_add(index * 8) as *const u32;

        let (base, length) = unsafe {
            (entry.read_unaligned(), entry.add(1).read_unaligned())
        };

        if base != 0 && length != 0 {
            write_output(base, length);
            total = total.saturating_add(length.min(1024 * 1024));
        }
    }

    total
}

/// Реализация `uname(struct utsname *)` (номер 122).
///
/// Статические glibc-программы проверяют версию ядра на старте и без
/// разумного ответа печатают «kernel too old» и завершаются с кодом 127.
pub fn uname_output(pointer: u32) -> i32 {
    if pointer == 0 {
        return -1;
    }

    const FIELD: usize = 65;

    let fields: [&[u8]; 6] = [
        b"Linux",
        b"perfix",
        b"6.1.0",
        b"Perfix 0.2.6",
        b"i686",
        b"(none)",
    ];

    unsafe {
        let output = pointer as *mut u8;

        for (index, value) in fields.iter().enumerate() {
            let start = output.add(index * FIELD);

            core::ptr::write_bytes(start, 0, FIELD);
            core::ptr::copy_nonoverlapping(
                value.as_ptr(),
                start,
                value.len().min(FIELD - 1),
            );
        }
    }

    0
}

/// Загружает и запускает ELF-файл. Возвращает строку, на которой нужно
/// показать следующее приглашение.
pub fn run(location: &FileLocation, start_row: usize) -> usize {
    unsafe {
        OUT_ROW = start_row;
        OUT_COL = 0;
    }

    // Буфер ELF и стек программы живут только на время запуска. Метка
    // позволяет вернуть всю арену одним вызовом при любом исходе.
    let mark = arena::mark();
    let result = run_loaded(location, start_row);
    arena::release(mark);
    result
}

fn run_loaded(location: &FileLocation, start_row: usize) -> usize {
    let size = match fs::file_size(location) {
        Ok(size) => size,
        Err(error) => return fail(start_row, error.message()),
    };

    if size as usize > ELF_BUFFER_SIZE {
        return fail(start_row, b"ELF file is too large (max 4 MiB)");
    }

    let buffer = match arena::allocate(size as usize, 16) {
        Some(buffer) => buffer,
        None => return fail(start_row, b"not enough memory to load program"),
    };

    let file_size = match fs::read_file(location, &mut buffer[..size as usize]) {
        Ok(size) => size,
        Err(error) => return fail(start_row, error.message()),
    };

    let data = &buffer[..file_size];
    let header = match parse_header(data) {
        Ok(header) => header,
        Err(message) => return fail(start_row, message),
    };

    if let Err(message) = validate_segments(data, &header) {
        return fail(start_row, message);
    }

    let stack_base = match arena::allocate(PROGRAM_STACK_SIZE, 16) {
        Some(stack) => stack.as_mut_ptr() as u32,
        None => return fail(start_row, b"not enough memory to load program"),
    };

    if !prepare_frames() {
        return fail(start_row, b"not enough memory to load program");
    }

    if let Err(message) = load_segments(data, &header) {
        paging::restore_program_pages();
        return fail(start_row, message);
    }

    if let Err(message) = map_heap() {
        paging::restore_program_pages();
        return fail(start_row, message);
    }

    let stack = build_stack(location.path(), &header, data, stack_base);

    unsafe {
        RUNNING = true;
        elf_enter(header.entry, stack);
        RUNNING = false;
    }

    paging::restore_program_pages();

    if unsafe { OUT_COL } > 0 {
        advance_line();
    }

    unsafe { OUT_ROW }
}

struct Header {
    entry: u32,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
    ]))
}

fn parse_header(data: &[u8]) -> Result<Header, &'static [u8]> {
    if data.len() < ELF_HEADER_SIZE {
        return Err(b"not an ELF file");
    }

    if &data[..4] != b"\x7fELF" {
        return Err(b"not an ELF file");
    }

    if data[4] != 1 {
        return Err(b"only 32-bit ELF is supported");
    }

    if data[5] != 1 {
        return Err(b"only little-endian ELF is supported");
    }

    let e_type = read_u16(data, 16).ok_or(b"invalid ELF header" as &'static [u8])?;
    let e_machine = read_u16(data, 18).ok_or(b"invalid ELF header" as &'static [u8])?;
    let e_version = read_u32(data, 20).ok_or(b"invalid ELF header" as &'static [u8])?;

    if e_type != 2 {
        // ET_DYN (3) — это PIE, ET_REL (1) и прочие не являются
        // готовыми исполняемыми файлами.
        return Err(b"dynamic or PIE ELF is not supported");
    }

    if e_machine != 3 {
        return Err(b"not an i386 ELF");
    }

    if e_version != 1 {
        return Err(b"invalid ELF header");
    }

    let entry = read_u32(data, 24).ok_or(b"invalid ELF header" as &'static [u8])?;
    let phoff = read_u32(data, 28).ok_or(b"invalid ELF header" as &'static [u8])? as usize;
    let phentsize = read_u16(data, 42).ok_or(b"invalid ELF header" as &'static [u8])? as usize;
    let phnum = read_u16(data, 44).ok_or(b"invalid ELF header" as &'static [u8])? as usize;

    if phnum == 0 {
        return Err(b"ELF has no loadable segments");
    }

    if phentsize < PROGRAM_HEADER_SIZE || phnum > 256 {
        return Err(b"invalid ELF header");
    }

    let table_size = phentsize
        .checked_mul(phnum)
        .ok_or(b"invalid ELF header" as &'static [u8])?;
    let table_end = phoff
        .checked_add(table_size)
        .ok_or(b"invalid ELF header" as &'static [u8])?;

    if table_end > data.len() {
        return Err(b"invalid ELF header");
    }

    Ok(Header {
        entry,
        phoff,
        phentsize,
        phnum,
    })
}

fn segment_offset(header: &Header, index: usize) -> usize {
    header.phoff + index * header.phentsize
}

/// Проверяет все `PT_LOAD` до изменения отображения памяти.
fn validate_segments(
    data: &[u8],
    header: &Header,
) -> Result<(), &'static [u8]> {
    let mut load_seen = false;
    let mut entry_inside = false;

    for index in 0..header.phnum {
        let offset = segment_offset(header, index);
        let p_type = read_u32(data, offset).ok_or(b"invalid ELF header" as &'static [u8])?;

        if p_type == 3 {
            return Err(b"dynamic or PIE ELF is not supported");
        }

        if p_type != 1 {
            continue;
        }

        load_seen = true;

        let p_offset = read_u32(data, offset + 4)
            .ok_or(b"invalid ELF header" as &'static [u8])? as usize;
        let p_vaddr = read_u32(data, offset + 8)
            .ok_or(b"invalid ELF header" as &'static [u8])?;
        let p_filesz = read_u32(data, offset + 16)
            .ok_or(b"invalid ELF header" as &'static [u8])? as usize;
        let p_memsz = read_u32(data, offset + 20)
            .ok_or(b"invalid ELF header" as &'static [u8])? as usize;
        let flags = read_u32(data, offset + 24)
            .ok_or(b"invalid ELF header" as &'static [u8])?;

        if p_filesz > p_memsz {
            return Err(b"invalid ELF segment");
        }

        let file_end = p_offset
            .checked_add(p_filesz)
            .ok_or(b"invalid ELF segment" as &'static [u8])?;

        if file_end > data.len() {
            return Err(b"ELF segment is out of range");
        }

        let memory_end = p_vaddr as u64 + p_memsz as u64;

        if p_memsz != 0
            && (p_vaddr < PROGRAM_MIN_VADDR
                || memory_end > PROGRAM_MAX_VADDR as u64)
        {
            return Err(b"ELF segment overlaps kernel or MMIO area");
        }

        if p_memsz != 0 {
            let heap_start = HEAP_VADDR as u64;
            let heap_end = (HEAP_VADDR + HEAP_SIZE) as u64;

            if (p_vaddr as u64) < heap_end && heap_start < memory_end {
                return Err(b"ELF segment overlaps program heap area");
            }
        }

        if flags & 0x1 == 0 {
            // Исполняемый сегмент обязателен, но ELF-загрузчик может
            // встретить и неисполняемые; ничего не делаем, просто
            // отмечаем точку входа отдельно.
        }

        if p_memsz != 0
            && header.entry >= p_vaddr
            && (header.entry as u64) < memory_end
        {
            entry_inside = true;
        }

        // Проверяем пересечение с уже проверенными сегментами.
        for other in 0..index {
            let other_offset = segment_offset(header, other);
            let other_type = read_u32(data, other_offset)
                .ok_or(b"invalid ELF header" as &'static [u8])?;

            if other_type != 1 {
                continue;
            }

            let other_vaddr = read_u32(data, other_offset + 8)
                .ok_or(b"invalid ELF header" as &'static [u8])?;
            let other_memsz = read_u32(data, other_offset + 20)
                .ok_or(b"invalid ELF header" as &'static [u8])? as u64;

            if other_memsz == 0 || p_memsz == 0 {
                continue;
            }

            let other_end = other_vaddr as u64 + other_memsz;

            if (p_vaddr as u64) < other_end
                && (other_vaddr as u64) < memory_end
            {
                return Err(b"ELF segments overlap each other");
            }
        }
    }

    if !load_seen {
        return Err(b"ELF has no loadable segments");
    }

    if !entry_inside {
        return Err(b"ELF entry point is out of range");
    }

    Ok(())
}

fn prepare_frames() -> bool {
    let ram = sysinfo::ram_kb().saturating_mul(1024);
    let ceiling = if ram == 0 {
        FRAME_CEILING as u64
    } else {
        ram.min(FRAME_CEILING as u64)
    };
    let top = (ceiling as u32)
        .saturating_sub(FRAME_TOP_RESERVE)
        & !(PAGE_SIZE - 1);

    unsafe {
        FRAME_NEXT = top;
    }

    // Кадры не должны залезть в арену, где уже лежат буфер ELF и стек
    // программы (она растёт вверх, кадры — вниз).
    let floor = arena::next();

    unsafe {
        FRAME_FLOOR_CURRENT = floor;
    }

    top > floor + PAGE_SIZE
}

fn allocate_frame() -> Option<u32> {
    unsafe {
        let next = FRAME_NEXT;

        if next < FRAME_FLOOR_CURRENT + PAGE_SIZE {
            return None;
        }

        let frame = next - PAGE_SIZE;
        FRAME_NEXT = frame;
        Some(frame)
    }
}

fn load_segments(
    data: &[u8],
    header: &Header,
) -> Result<(), &'static [u8]> {
    for index in 0..header.phnum {
        let offset = segment_offset(header, index);
        let p_type = read_u32(data, offset).ok_or(b"invalid ELF header" as &'static [u8])?;

        if p_type != 1 {
            continue;
        }

        let p_offset = read_u32(data, offset + 4)
            .ok_or(b"invalid ELF header" as &'static [u8])? as usize;
        let p_vaddr = read_u32(data, offset + 8)
            .ok_or(b"invalid ELF header" as &'static [u8])?;
        let p_filesz = read_u32(data, offset + 16)
            .ok_or(b"invalid ELF header" as &'static [u8])? as usize;
        let p_memsz = read_u32(data, offset + 20)
            .ok_or(b"invalid ELF header" as &'static [u8])? as usize;

        if p_memsz == 0 {
            continue;
        }

        let page_start = p_vaddr & !(PAGE_SIZE - 1);
        let memory_end = p_vaddr as u64 + p_memsz as u64;
        let page_end = ((memory_end + (PAGE_SIZE as u64 - 1))
            & !(PAGE_SIZE as u64 - 1)) as u32;

        let mut page = page_start;

        while page < page_end {
            let frame = match allocate_frame() {
                Some(frame) => frame,
                None => return Err(b"not enough memory to load program"),
            };

            if !paging::map_program_page(page, frame) {
                return Err(b"program uses too many 4 MiB regions");
            }

            // Новые страницы должны быть нулевыми: так автоматически
            // получается BSS без отдельного прохода.
            unsafe {
                let destination = page as *mut u8;

                for byte in 0..PAGE_SIZE as usize {
                    destination.add(byte).write_volatile(0);
                }
            }

            match page.checked_add(PAGE_SIZE) {
                Some(next) => page = next,
                None => break,
            }
        }

        if p_filesz != 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr().add(p_offset),
                    p_vaddr as *mut u8,
                    p_filesz,
                );
            }

            // Проверяем, что файловая система действительно отдала все
            // байты: иначе программа получит повреждённый код.
            let loaded = unsafe {
                core::slice::from_raw_parts(
                    p_vaddr as *const u8,
                    p_filesz,
                )
            };

            if loaded != &data[p_offset..p_offset + p_filesz] {
                return Err(b"failed to load program into memory");
            }
        }
    }

    Ok(())
}

/// Готовит кучу программы к работе.
///
/// Сразу отображается только первая страница: остальные добавляются по
/// мере роста `brk`. Так glibc-программа на машине с 16 МиБ RAM не
/// требует 8 МиБ физической памяти ещё до запуска.
fn map_heap() -> Result<(), &'static [u8]> {
    unsafe {
        PROGRAM_BREAK = HEAP_VADDR;
        HEAP_MAPPED_END = HEAP_VADDR;
    }

    map_heap_page(HEAP_VADDR)?;

    unsafe {
        HEAP_MAPPED_END = HEAP_VADDR + PAGE_SIZE;
    }

    Ok(())
}

/// Отображает одну страницу кучи на новый кадр и обнуляет её.
fn map_heap_page(page: u32) -> Result<(), &'static [u8]> {
    let frame = allocate_frame()
        .ok_or(b"not enough memory to load program" as &'static [u8])?;

    if !paging::map_program_page(page, frame) {
        return Err(b"program uses too many 4 MiB regions");
    }

    unsafe {
        let destination = page as *mut u8;

        for byte in 0..PAGE_SIZE as usize {
            destination.add(byte).write_volatile(0);
        }
    }

    Ok(())
}

/// Реализация Linux-вызова `brk` (номер 45) для запущенной программы.
///
/// Возвращает текущую границу кучи; при успехе — новую. Новые страницы
/// отображаются и обнуляются по требованию. При нехватке памяти граница
/// остаётся прежней (как в Linux при неудачном `brk`). Уменьшение `brk`
/// не освобождает страницы — они будут переиспользованы при следующем
/// запуске, потому что кадры выделяются заново.
pub fn set_break(address: u32) -> u32 {
    unsafe {
        let current = PROGRAM_BREAK;

        if address == 0 {
            return current;
        }

        if address < HEAP_VADDR || address > HEAP_VADDR + HEAP_SIZE {
            return current;
        }

        if address > current {
            let end = (address + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            let mut page = HEAP_MAPPED_END;

            while page < end {
                if map_heap_page(page).is_err() {
                    return current;
                }

                HEAP_MAPPED_END = page + PAGE_SIZE;
                page += PAGE_SIZE;
            }
        }

        PROGRAM_BREAK = address;
        address
    }
}

/// Тег вспомогательного вектора (`auxv`) для начального стека.
const AT_NULL: u32 = 0;
const AT_PHDR: u32 = 3;
const AT_PHENT: u32 = 4;
const AT_PHNUM: u32 = 5;
const AT_PAGESZ: u32 = 6;
const AT_ENTRY: u32 = 9;
const AT_UID: u32 = 11;
const AT_EUID: u32 = 12;
const AT_GID: u32 = 13;
const AT_EGID: u32 = 14;
const AT_HWCAP: u32 = 16;
const AT_CLKTCK: u32 = 17;
const AT_SECURE: u32 = 23;
const AT_RANDOM: u32 = 25;
const AT_EXECFN: u32 = 31;

/// Ищет виртуальный адрес таблицы program headers.
///
/// Обычно `PT_PHDR` отсутствует, и таблица лежит внутри первого
/// `PT_LOAD`, поэтому пересчитываем адрес по смещению в файле.
fn program_header_vaddr(data: &[u8], header: &Header) -> Option<u32> {
    for index in 0..header.phnum {
        let offset = segment_offset(header, index);
        let p_type = read_u32(data, offset)?;

        if p_type == 6 {
            // PT_PHDR прямо содержит виртуальный адрес таблицы.
            return read_u32(data, offset + 8);
        }

        if p_type != 1 {
            continue;
        }

        let p_offset = read_u32(data, offset + 4)? as usize;
        let p_vaddr = read_u32(data, offset + 8)?;
        let p_filesz = read_u32(data, offset + 16)? as usize;

        if let Some(delta) = header.phoff.checked_sub(p_offset) {
            if delta < p_filesz {
                return Some(p_vaddr.wrapping_add(delta as u32));
            }
        }
    }

    None
}

/// Строит начальный стек программы по ELF ABI:
/// `argc`, `argv`, `envp`, затем вспомогательный вектор `auxv`.
///
/// `auxv` нужен статическим glibc-программам: по `AT_PHDR`/`AT_PHNUM`
/// они находят `PT_TLS` и настраивают блок TLS.
fn build_stack(
    path: &[u8],
    header: &Header,
    data: &[u8],
    base: u32,
) -> u32 {
    let top = (base + PROGRAM_STACK_SIZE as u32) & !0xF;

    // Строки и случайные байты размещаем в верхней части стека.
    let name_pointer = top - 1024;
    let name_length = path.len().min(1000);
    let random_pointer = top - 32;

    let phdr = program_header_vaddr(data, header).unwrap_or(0);

    let auxv: [(u32, u32); 14] = [
        (AT_PHDR, phdr),
        (AT_PHENT, header.phentsize as u32),
        (AT_PHNUM, header.phnum as u32),
        (AT_PAGESZ, PAGE_SIZE),
        (AT_ENTRY, header.entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_SECURE, 0),
        (AT_RANDOM, random_pointer),
        (AT_HWCAP, 0),
        (AT_CLKTCK, 100),
        (AT_EXECFN, name_pointer),
    ];

    unsafe {
        core::ptr::copy_nonoverlapping(
            path.as_ptr(),
            name_pointer as *mut u8,
            name_length,
        );
        (name_pointer as *mut u8).add(name_length).write_volatile(0);

        // AT_RANDOM: 16 байт для stack guard glibc. Криптостойкость
        // здесь не требуется, главное — ненулевые значения.
        for index in 0..16u32 {
            (random_pointer as *mut u8).add(index as usize).write_volatile(
                (index as u8).wrapping_mul(37).wrapping_add(11),
            );
        }

        // argc(1) + argv[0] + NULL + envp NULL + auxv + AT_NULL.
        let words = 1 + 2 + 1 + auxv.len() * 2 + 2;
        let frame = ((name_pointer - (words as u32 * 4)) & !0xF) - 16;
        let mut cursor = frame as *mut u32;

        cursor.write_volatile(1);
        cursor = cursor.add(1);
        cursor.write_volatile(name_pointer);
        cursor = cursor.add(1);

        // Конец argv и пустой envp.
        cursor.write_volatile(0);
        cursor = cursor.add(1);
        cursor.write_volatile(0);
        cursor = cursor.add(1);

        for (kind, value) in auxv {
            cursor.write_volatile(kind);
            cursor = cursor.add(1);
            cursor.write_volatile(value);
            cursor = cursor.add(1);
        }

        cursor.write_volatile(AT_NULL);
        cursor = cursor.add(1);
        cursor.write_volatile(0);

        frame
    }
}

fn fail(row: usize, message: &[u8]) -> usize {
    vga::print_at(row, 0, b"exec: ");
    vga::print_at(row, 6, message);

    if row + 1 > vga::height().saturating_sub(1) {
        vga::scroll_screen();
        return vga::height().saturating_sub(1);
    }

    row + 1
}

fn advance_line() {
    let height = vga::height();

    if height == 0 {
        return;
    }

    unsafe {
        OUT_COL = 0;

        if OUT_ROW + 1 >= height {
            vga::scroll_screen();
            OUT_ROW = height - 1;
        } else {
            OUT_ROW += 1;
        }
    }
}

fn emit(bytes: &[u8]) {
    let width = vga::width();
    let height = vga::height();

    if width == 0 || height == 0 {
        return;
    }

    for &byte in bytes {
        match byte {
            b'\n' => advance_line(),
            b'\r' => unsafe {
                OUT_COL = 0;
            },
            _ => unsafe {
                if OUT_COL >= width {
                    advance_line();
                }

                if OUT_ROW >= height {
                    OUT_ROW = height - 1;
                }

                vga::print_at(OUT_ROW, OUT_COL, &[byte]);
                OUT_COL += 1;
            },
        }
    }
}

fn push(out: &mut [u8], len: &mut usize, bytes: &[u8]) {
    for &byte in bytes {
        if *len >= out.len() {
            return;
        }

        out[*len] = byte;
        *len += 1;
    }
}

fn push_hex(out: &mut [u8], len: &mut usize, value: u32, digits: usize) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    for index in 0..digits {
        let shift = (digits - 1 - index) * 4;
        push(out, len, &[HEX[((value >> shift) & 0xF) as usize]]);
    }
}

/// Небольшая обёртка, чтобы ядро не оптимизировало ожидание прерывания.
#[allow(dead_code)]
fn pause() {
    unsafe {
        asm!("pause", options(nomem, nostack, preserves_flags));
    }
}
