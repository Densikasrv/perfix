//! Установщик Perfix на раздел ext4.
//!
//! Установочный ISO загружает в память три Multiboot-модуля:
//! ядро (`perfix.elf`), первый сектор GRUB (`boot.img`) и сгенерированный
//! `core.img`. Установщик копирует ядро в корень выбранного ext4-раздела
//! как `/perfix.elf`, а затем ставит GRUB в MBR и в промежуток после него.
//!
//! Поддерживаются только MBR-диски: таблица разделов и сигнатура диска
//! сохраняются, поэтому существующая разметка не меняется.

use crate::disk::{self, DiskPartition, FsKind};
use crate::fs::ext4::{AtaDevice, Ext4};

/// Сколько Multiboot-модулей запоминаем.
const MAX_MODULES: usize = 8;
/// Сколько ext4-разделов показываем как цели установки.
const MAX_TARGETS: usize = 8;
/// Начальный LBA для `core.img` (сразу после MBR).
const CORE_IMAGE_LBA: u32 = 1;
/// Смещение поля core image LBA в `boot.img`.
const BOOT_IMAGE_CORE_LBA_OFFSET: usize = 0x5c;
/// Смещение байта загрузочного диска в `boot.img`.
const BOOT_IMAGE_DRIVE_OFFSET: usize = 0x64;

/// Раздел, доступный для установки.
#[derive(Clone, Copy)]
pub struct InstallTarget {
    pub number: usize,
    pub sectors: u32,
    pub ext4: bool,
}

impl InstallTarget {
    pub const EMPTY: Self = Self {
        number: 0,
        sectors: 0,
        ext4: false,
    };
}

/// Причина, по которой установка не удалась.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InstallError {
    NoPayload,
    PartitionNotFound,
    NotExt4,
    NotMbr,
    NoGap,
    MountFailed,
    KernelWriteFailed,
    BootWriteFailed,
    CoreWriteFailed,
    MarkerWriteFailed,
    AccountsWriteFailed,
    SystemFilesWriteFailed,
    InvalidCredentials,
    FormatFailed,
}

impl InstallError {
    pub fn message(self) -> &'static [u8] {
        match self {
            InstallError::NoPayload => b"installer payload is missing (boot as ISO)",
            InstallError::PartitionNotFound => b"partition not found",
            InstallError::NotExt4 => b"only ext4 partitions are supported",
            InstallError::NotMbr => b"only MBR disks are supported",
            InstallError::NoGap => b"not enough space after MBR for core.img",
            InstallError::MountFailed => b"could not mount ext4 partition for writing",
            InstallError::KernelWriteFailed => b"could not write /perfix.elf",
            InstallError::BootWriteFailed => b"could not write boot.img to MBR",
            InstallError::CoreWriteFailed => b"could not write core.img after MBR",
            InstallError::MarkerWriteFailed => b"could not write /etc/InstallSuccess",
            InstallError::AccountsWriteFailed => {
                b"could not create /etc/users or /home/<user>"
            }
            InstallError::SystemFilesWriteFailed => {
                b"could not write /etc/pfpak/repo.list or CA certificates"
            }
            InstallError::InvalidCredentials => {
                b"invalid user name or empty password"
            }
            InstallError::FormatFailed => b"could not format partition as ext4",
        }
    }
}

#[derive(Clone, Copy)]
struct Module {
    start: u32,
    end: u32,
}

static mut MODULES: [Module; MAX_MODULES] = [
    Module {
        start: 0,
        end: 0,
    };
    MAX_MODULES
];
static mut MODULE_COUNT: usize = 0;
/// Признак того, что Perfix уже установлен на диск.
static mut INSTALLED: bool = false;

/// Путь к маркеру успешной установки в корне ext4-раздела.
const SUCCESS_MARKER: &[u8] = b"/etc/InstallSuccess";
/// Содержимое маркера установки.
const SUCCESS_TEXT: &[u8] = b"Perfix installation completed\n";
/// Список репозиториев pfpak, создаваемый на установленной системе.
const REPO_LIST_PATH: &[u8] = b"/etc/pfpak/repo.list";
/// Репозиторий pfpak по умолчанию.
///
/// GitHub отдаёт содержимое репозитория по адресу `/tree/<ветка>/<путь>`;
/// pfpak распознаёт такую форму и сам переводит её в raw-адрес.
const REPO_LIST_CONTENT: &[u8] =
    b"https://raw.githubusercontent.com/Densikasrv/pfpak-repo/tree/main/pfpak RELEASE:fatcat\n";
/// Обычный PEM-набор корневых сертификатов для программ на диске.
const CA_BUNDLE_PATH: &[u8] = b"/etc/ssl/certs/ca-certificates.crt";

fn mbi_u32(multiboot_info: u32, offset: usize) -> u32 {
    unsafe {
        (multiboot_info as *const u8)
            .add(offset)
            .cast::<u32>()
            .read_unaligned()
    }
}

/// Запоминает Multiboot-модули, переданные загрузчиком.
///
/// Вызывается после включения страничной адресации: модули могут быть
/// загружены выше первых 4 МиБ, но identity mapping покрывает их.
///
/// Имена модулей в Multiboot 1 содержат только командную строку, а не имя
/// файла, поэтому модули различаются по содержимому и размеру.
pub fn init(multiboot_info: u32) {
    if multiboot_info == 0 {
        return;
    }

    const FLAG_MODULES: u32 = 1 << 3;

    if mbi_u32(multiboot_info, 0) & FLAG_MODULES == 0 {
        return;
    }

    let count = mbi_u32(multiboot_info, 20) as usize;
    let address = mbi_u32(multiboot_info, 24);
    let stored = count.min(MAX_MODULES);

    for index in 0..stored {
        let entry = address.wrapping_add((index * 16) as u32);

        unsafe {
            MODULES[index] = Module {
                start: mbi_u32(entry, 0),
                end: mbi_u32(entry, 4),
            };
        }
    }

    unsafe {
        MODULE_COUNT = stored;
    }
}

fn module_bytes(index: usize) -> &'static [u8] {
    unsafe {
        let module = MODULES[index];
        let length = module.end.saturating_sub(module.start) as usize;

        core::slice::from_raw_parts(module.start as *const u8, length)
    }
}

fn find_module<F>(predicate: F) -> Option<&'static [u8]>
where
    F: Fn(&[u8]) -> bool,
{
    let count = unsafe { MODULE_COUNT };

    for index in 0..count {
        let bytes = module_bytes(index);

        if predicate(bytes) {
            return Some(bytes);
        }
    }

    None
}

fn is_elf(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == b"\x7fELF"
}

fn kernel_payload() -> Option<&'static [u8]> {
    find_module(is_elf)
}

fn boot_image_payload() -> Option<&'static [u8]> {
    find_module(|bytes| bytes.len() == 512)
}

fn core_image_payload() -> Option<&'static [u8]> {
    find_module(|bytes| bytes.len() > 512 && !is_elf(bytes))
}

fn has_payload() -> bool {
    kernel_payload().is_some()
        && boot_image_payload().is_some()
        && core_image_payload().is_some()
}

/// Загружены ли мы с установочного носителя.
///
/// На живом носителе вход в систему не запрашивается: установщик должен
/// работать от имени root, даже если на диске уже есть чужая установка.
pub fn is_live_media() -> bool {
    has_payload()
}

/// Установлен ли Perfix на диск.
pub fn is_installed() -> bool {
    unsafe { INSTALLED }
}

/// Доступен ли установщик: на установленной системе его нет.
pub fn available() -> bool {
    !is_installed()
}

/// Проверяет маркер успешной установки при загрузке с диска.
///
/// ISO тоже загружается с диска, поэтому сначала проверяется наличие
/// установочных модулей: если они есть, это установочный носитель и
/// установщик должен оставаться доступным.
pub fn detect_installed() {
    if has_payload() {
        return;
    }

    let mut partitions = [DiskPartition {
        number: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; MAX_TARGETS];
    let count = disk::read_partitions(&mut partitions);

    for partition in partitions[..count].iter() {
        if partition.kind != FsKind::Ext4 {
            continue;
        }

        let Ok(filesystem) =
            Ext4::mount(&AtaDevice, partition.start_lba, partition.sectors)
        else {
            continue;
        };

        if filesystem.path_exists(SUCCESS_MARKER).unwrap_or(false) {
            unsafe {
                INSTALLED = true;
            }
            return;
        }
    }
}

fn read_ext4_partitions(out: &mut [DiskPartition]) -> usize {
    disk::read_partitions(out)
}

/// Список разделов, доступных для установки.
///
/// ext4 можно ставить сразу; остальные разделы доступны только с
/// явным форматированием.
pub fn targets(out: &mut [InstallTarget]) -> usize {
    let mut partitions = [DiskPartition {
        number: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; MAX_TARGETS];
    let count = read_ext4_partitions(&mut partitions);
    let mut found = 0usize;

    for partition in partitions[..count].iter() {
        if found >= out.len() {
            break;
        }

        if is_extended_container(partition.number) {
            continue;
        }

        out[found] = InstallTarget {
            number: partition.number,
            sectors: partition.sectors,
            ext4: partition.kind == FsKind::Ext4,
        };
        found += 1;
    }

    found
}

/// Типы MBR, которые обозначают контейнер расширенных разделов.
fn is_extended_container(number: usize) -> bool {
    if number == 0 || number > 4 {
        return false;
    }

    let mut mbr = [0u8; 512];
    if crate::ata::read_sector(0, &mut mbr).is_err() {
        return false;
    }

    let offset = 446 + (number - 1) * 16;
    let partition_type = mbr[offset + 4];

    partition_type == 0x05 || partition_type == 0x0F || partition_type == 0x85
}

/// Ищет раздел по номеру. При `require_ext4` проверяет файловую систему.
fn find_partition(
    number: usize,
    require_ext4: bool,
) -> Result<DiskPartition, InstallError> {
    let mut partitions = [DiskPartition {
        number: 0,
        kind: FsKind::Other,
        start_lba: 0,
        sectors: 0,
    }; MAX_TARGETS];
    let count = read_ext4_partitions(&mut partitions);

    let partition = partitions[..count]
        .iter()
        .find(|partition| partition.number == number)
        .copied()
        .ok_or(InstallError::PartitionNotFound)?;

    if require_ext4 && partition.kind != FsKind::Ext4 {
        return Err(InstallError::NotExt4);
    }

    Ok(partition)
}

/// Форматирует раздел в ext4.
///
/// Форматировать можно только обычный раздел MBR; контейнер расширенных
/// разделов и GPT отклоняются.
pub fn format_partition(number: usize) -> Result<(), InstallError> {
    let mut mbr = [0u8; 512];
    crate::ata::read_sector(0, &mut mbr).map_err(|_| InstallError::NotMbr)?;

    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return Err(InstallError::NotMbr);
    }

    if number == 0 || number > 4 {
        return Err(InstallError::PartitionNotFound);
    }

    let entry = 446 + (number - 1) * 16;
    let partition_type = mbr[entry + 4];
    let start_lba = read_u32(&mbr, entry + 8);
    let sectors = read_u32(&mbr, entry + 12);

    if partition_type == 0xEE {
        return Err(InstallError::NotMbr);
    }

    if partition_type == 0
        || sectors == 0
        || partition_type == 0x05
        || partition_type == 0x0F
        || partition_type == 0x85
    {
        return Err(InstallError::PartitionNotFound);
    }

    let partition = find_partition(number, false)?;
    if partition.start_lba != start_lba || partition.sectors != sectors {
        return Err(InstallError::PartitionNotFound);
    }

    crate::fs::ext4::format_ext4(&AtaDevice, start_lba, sectors)
        .map_err(|_| InstallError::FormatFailed)?;
    let _ = crate::ata::flush();

    Ok(())
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Проверяет, что MBR не защитный (GPT) и что `core.img` помещается
/// в промежуток после MBR, не задевая существующие разделы.
fn validate_mbr_gap(mbr: &[u8; 512], core_end: u32) -> Result<(), InstallError> {
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return Err(InstallError::NotMbr);
    }

    for index in 0..4 {
        let offset = 446 + index * 16;
        let partition_type = mbr[offset + 4];
        let start_lba = read_u32(mbr, offset + 8);
        let sectors = read_u32(mbr, offset + 12);

        if partition_type == 0 || sectors == 0 {
            continue;
        }

        if partition_type == 0xEE {
            return Err(InstallError::NotMbr);
        }

        if start_lba < core_end {
            return Err(InstallError::NoGap);
        }
    }

    Ok(())
}

fn write_core_image(core: &[u8]) -> Result<(), InstallError> {
    let sectors = (core.len() + 511) / 512;
    let mut offset = 0usize;

    for index in 0..sectors {
        let mut buffer = [0u8; 512];
        let remaining = core.len() - offset;
        let take = remaining.min(buffer.len());
        buffer[..take].copy_from_slice(&core[offset..offset + take]);

        crate::ata::write_sector(
            CORE_IMAGE_LBA + index as u32,
            &buffer,
        )
        .map_err(|_| InstallError::CoreWriteFailed)?;

        offset += take;
    }

    Ok(())
}

/// Устанавливает Perfix, при необходимости сначала форматируя раздел.
///
/// Имя пользователя и пароли записываются в `/etc/users`, а домашний
/// каталог создаётся как `/home/<username>`.
pub fn install_with_format(
    partition_number: usize,
    format: bool,
    username: &[u8],
    password: &[u8],
    super_password: &[u8],
) -> Result<(), InstallError> {
    let kernel = kernel_payload().ok_or(InstallError::NoPayload)?;
    let boot_image = boot_image_payload().ok_or(InstallError::NoPayload)?;
    let core_image = core_image_payload().ok_or(InstallError::NoPayload)?;

    if boot_image.len() < 512 {
        return Err(InstallError::NoPayload);
    }

    let partition = find_partition(partition_number, !format)?;

    if is_extended_container(partition_number) {
        return Err(InstallError::PartitionNotFound);
    }
    let core_sectors = ((core_image.len() + 511) / 512) as u32;
    let core_end = CORE_IMAGE_LBA
        .checked_add(core_sectors)
        .ok_or(InstallError::NoGap)?;

    // Читаем текущий MBR заранее: установка на GPT-диск или диск без
    // достаточного промежутка после MBR должна быть отклонена до записи.
    let mut mbr = [0u8; 512];
    crate::ata::read_sector(0, &mut mbr).map_err(|_| InstallError::NotMbr)?;
    validate_mbr_gap(&mbr, core_end)?;

    if core_end > partition.start_lba {
        return Err(InstallError::NoGap);
    }

    // Разрушительное форматирование выполняется только после всех
    // проверок, чтобы ошибка установки не уничтожила файловую систему.
    if format {
        crate::fs::ext4::format_ext4(
            &AtaDevice,
            partition.start_lba,
            partition.sectors,
        )
        .map_err(|_| InstallError::FormatFailed)?;
    }

    // Сначала ядро: если запись не удалась, загрузчик не тронут.
    let filesystem = Ext4::mount_read_write(
        &AtaDevice,
        partition.start_lba,
        partition.sectors,
    )
    .map_err(|_| InstallError::MountFailed)?;

    filesystem
        .write_file_path(b"/perfix.elf", kernel)
        .map_err(|_| InstallError::KernelWriteFailed)?;

    let mut boot_sector = [0u8; 512];
    boot_sector.copy_from_slice(&boot_image[..512]);

    // Сохраняем disk signature, зарезервированные байты и таблицу разделов.
    boot_sector[440..512].copy_from_slice(&mbr[440..512]);
    boot_sector[510] = 0x55;
    boot_sector[511] = 0xAA;

    // core.img лежит сразу после MBR; загрузка идёт с текущего диска.
    boot_sector[BOOT_IMAGE_CORE_LBA_OFFSET..BOOT_IMAGE_CORE_LBA_OFFSET + 8]
        .copy_from_slice(&(CORE_IMAGE_LBA as u64).to_le_bytes());
    boot_sector[BOOT_IMAGE_DRIVE_OFFSET] = 0xff;

    crate::ata::write_sector(0, &boot_sector)
        .map_err(|_| InstallError::BootWriteFailed)?;

    write_core_image(core_image)?;

    // Учётные записи и домашний каталог. Без них установленная система
    // загрузилась бы в режиме root без входа.
    filesystem
        .ensure_directory_path(b"/etc")
        .map_err(|_| InstallError::AccountsWriteFailed)?;

    if !crate::auth::valid_username(username) {
        return Err(InstallError::InvalidCredentials);
    }

    let mut accounts = [0u8; 512];
    let accounts_len = crate::auth::build_accounts(
        username,
        password,
        super_password,
        &mut accounts,
    )
    .ok_or(InstallError::InvalidCredentials)?;

    filesystem
        .write_file_path(b"/etc/users", &accounts[..accounts_len])
        .map_err(|_| InstallError::AccountsWriteFailed)?;

    filesystem
        .ensure_directory_path(b"/home")
        .map_err(|_| InstallError::AccountsWriteFailed)?;

    let mut home = [0u8; 64];
    let mut home_len = 0usize;

    for &byte in b"/home/" {
        home[home_len] = byte;
        home_len += 1;
    }

    for &byte in username {
        home[home_len] = byte;
        home_len += 1;
    }

    filesystem
        .ensure_directory_path(&home[..home_len])
        .map_err(|_| InstallError::AccountsWriteFailed)?;

    // Репозиторий pfpak по умолчанию. Без этого файла установленная
    // система не знает, откуда ставить пакеты, хотя ISO его и не требует.
    filesystem
        .ensure_directory_path(b"/etc/pfpak")
        .map_err(|_| InstallError::SystemFilesWriteFailed)?;

    filesystem
        .write_file_path(REPO_LIST_PATH, REPO_LIST_CONTENT)
        .map_err(|_| InstallError::SystemFilesWriteFailed)?;

    // Корневые сертификаты: ядро доверяет встроенному набору, а этот
    // PEM-бандл нужен остальным программам на установленной системе.
    filesystem
        .ensure_directory_path(b"/etc/ssl/certs")
        .map_err(|_| InstallError::SystemFilesWriteFailed)?;

    let bundle_size = crate::tls::ca_bundle_size();
    if bundle_size == 0 {
        return Err(InstallError::SystemFilesWriteFailed);
    }

    let mark = crate::arena::mark();
    let bundle = match crate::arena::allocate(bundle_size, 16) {
        Some(bundle) => bundle,
        None => {
            crate::arena::release(mark);
            return Err(InstallError::SystemFilesWriteFailed);
        }
    };

    let bundle_len = match crate::tls::write_ca_bundle(bundle) {
        Some(length) => length,
        None => {
            crate::arena::release(mark);
            return Err(InstallError::SystemFilesWriteFailed);
        }
    };

    let result = filesystem.write_file_path(CA_BUNDLE_PATH, &bundle[..bundle_len]);
    crate::arena::release(mark);
    result.map_err(|_| InstallError::SystemFilesWriteFailed)?;

    // Маркер успешной установки: при загрузке с диска система увидит его
    // и уберёт установщик из CLI и GUI.
    filesystem
        .write_file_path(SUCCESS_MARKER, SUCCESS_TEXT)
        .map_err(|_| InstallError::MarkerWriteFailed)?;

    let _ = crate::ata::flush();

    Ok(())
}

fn push_decimal(out: &mut [u8; 80], length: &mut usize, value: usize) {
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

        if *length < out.len() {
            out[*length] = digits[count];
            *length += 1;
        }
    }
}

fn push_text(out: &mut [u8; 80], length: &mut usize, text: &[u8]) {
    for &byte in text {
        if *length >= out.len() {
            return;
        }

        out[*length] = byte;
        *length += 1;
    }
}

fn parse_number(input: &[u8]) -> Option<usize> {
    if input.is_empty() {
        return None;
    }

    let mut value = 0usize;

    for &byte in input {
        if !byte.is_ascii_digit() {
            return None;
        }

        value = value
            .checked_mul(10)?
            .checked_add((byte - b'0') as usize)?;
    }

    Some(value)
}

fn print_targets(row: usize) -> usize {
    let mut list = [InstallTarget::EMPTY; MAX_TARGETS];
    let count = targets(&mut list);

    let payload_ready = kernel_payload().is_some()
        && boot_image_payload().is_some()
        && core_image_payload().is_some();

    crate::vga::print_at(
        row,
        0,
        if payload_ready {
            b"Installer payload: ready"
        } else {
            b"Installer payload: missing (boot from ISO)"
        },
    );
    crate::vga::print_at(row + 1, 0, b"Install targets (format option erases data):");

    if count == 0 {
        crate::vga::print_at(row + 2, 0, b"No partitions found");
        return 3;
    }

    for (index, target) in list[..count].iter().enumerate() {
        let mut line = [b' '; 80];
        let mut length = 0usize;

        push_text(&mut line, &mut length, b"Partition ");
        push_decimal(&mut line, &mut length, target.number);
        push_text(
            &mut line,
            &mut length,
            if target.ext4 { b"  ext4   " } else { b"  other  " },
        );
        push_decimal(&mut line, &mut length, (target.sectors / 2048) as usize);
        push_text(&mut line, &mut length, b" MiB");

        crate::vga::print_at(row + 2 + index, 0, &line[..length]);
    }

    count + 2
}

/// Команда оболочки `mkfs.ext4 <номер>`.
pub fn mkfs_shell_command(argument: &[u8], row: usize) -> usize {
    let Some(number) = parse_number(argument) else {
        crate::vga::print_at(row, 0, b"Usage: mkfs.ext4 <partition number>");
        return 1;
    };

    if !crate::auth::is_elevated() {
        crate::vga::print_at(row, 0, b"Permission denied. Use elev mkfs.ext4.");
        return 1;
    }

    crate::vga::print_at(row, 0, b"Formatting partition as ext4...");

    match format_partition(number) {
        Ok(()) => {
            crate::vga::print_at(row + 1, 0, b"Format complete: ext4 filesystem created");
            2
        }
        Err(error) => {
            crate::vga::print_at(row + 1, 0, b"Format failed: ");
            crate::vga::print_at(row + 1, 15, error.message());
            2
        }
    }
}

/// Команда оболочки `install [номер]` и `setup [номер]`.
pub fn shell_command(argument: &[u8], row: usize) -> usize {
    if argument.is_empty() {
        return print_targets(row);
    }

    let mut tokens = argument
        .split(|&byte| byte == b' ' || byte == b'\t')
        .filter(|token| !token.is_empty());

    let number = match tokens.next().and_then(parse_number) {
        Some(number) => number,
        None => {
            crate::vga::print_at(
                row,
                0,
                b"Usage: install <partition number> [format]",
            );
            return 1;
        }
    };

    let mut format = false;

    for token in tokens {
        if token.eq_ignore_ascii_case(b"format") {
            format = true;
        } else {
            crate::vga::print_at(
                row,
                0,
                b"Usage: install <partition number> [format]",
            );
            return 1;
        }
    }

    if !crate::auth::is_elevated() {
        crate::vga::print_at(row, 0, b"Permission denied. Use elev install.");
        return 1;
    }

    let Some(credentials) = crate::auth::prompt_new_credentials(row) else {
        crate::vga::print_at(
            row,
            0,
            b"install: enter credentials in the console or use the GUI installer",
        );
        return 1;
    };

    let base = row + credentials.rows;

    if format {
        crate::vga::print_at(base, 0, b"Formatting partition and installing Perfix...");
    } else {
        crate::vga::print_at(base, 0, b"Installing Perfix to ext4 partition...");
    }

    match install_with_format(
        number,
        format,
        &credentials.username[..credentials.username_len],
        &credentials.password[..credentials.password_len],
        &credentials.super_password[..credentials.super_password_len],
    ) {
        Ok(()) => {
            crate::vga::print_at(
                base + 1,
                0,
                if format {
                    b"Installation complete: ext4 formatted, kernel and GRUB written"
                } else {
                    b"Installation complete: /perfix.elf and GRUB are written"
                },
            );
            crate::vga::print_at(base + 2, 0, b"Reboot the computer to start Perfix");
            credentials.rows + 3
        }
        Err(error) => {
            crate::vga::print_at(base + 1, 0, b"Install failed: ");
            crate::vga::print_at(base + 1, 15, error.message());
            credentials.rows + 2
        }
    }
}
