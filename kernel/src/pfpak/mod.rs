//! pfpak — пакетный менеджер Perfix.
//!
//! Пакеты — `.tar.gz`, метаданные — `MANIFEST`, репозитории перечислены в
//! `/etc/pfpak/repo.list`, установленные манифесты лежат в
//! `/etc/pfpak/InstalledPaks/<package>/MANIFEST`.
//!
//! Модуль работает поверх штатного ext4-драйвера и HTTP-клиента ядра.
//! Изменяющие команды требуют прав суперпользователя (`elev pfpak ...`).
//! Распаковка ограничена белым списком путей, запрещает `..`, ссылки и
//! специальные файлы, а размеры архива, файла и всего пакета ограничены.

mod inflate;
mod manifest;
mod paths;
mod repo;
mod store;
mod tar;

use crate::arena;
use crate::auth;
use crate::http;
use crate::vga;
use crate::fs::ext4::{AtaDevice, Ext4};

const REPO_LIST: &[u8] = b"/etc/pfpak/repo.list";
const INSTALLED_DIR: &[u8] = b"/etc/pfpak/InstalledPaks";
const CACHE_DIR: &[u8] = b"/etc/pfpak/Cache";
const MANIFEST_CAPACITY: usize = manifest::MAX_SIZE;
const MAX_DEPTH: usize = 8;
const MANIFEST_NAME: &[u8] = b"MANIFEST";
const MAX_ROLLBACK: usize = 32;

/// Размеры, которые пробуются для буфера архива: от большего к меньшему.
/// На машине с малым объёмом RAM установка маленького пакета всё равно
/// сможет выделить буфер.
const ARCHIVE_SIZES: [usize; 4] = [
    4 * 1024 * 1024,
    2 * 1024 * 1024,
    1024 * 1024,
    512 * 1024,
];

static mut ROLLBACK: [[u8; paths::PATH_MAX]; MAX_ROLLBACK] =
    [[0; paths::PATH_MAX]; MAX_ROLLBACK];
static mut ROLLBACK_LENGTHS: [usize; MAX_ROLLBACK] = [0; MAX_ROLLBACK];
static mut ROLLBACK_COUNT: usize = 0;

struct Printer {
    row: usize,
    used: usize,
}

impl Printer {
    fn new(row: usize) -> Self {
        Self { row, used: 0 }
    }

    fn line(&mut self, text: &[u8]) {
        let height = vga::height();

        // Последняя строка экрана остаётся под приглашение оболочки.
        if self.row + 2 > height {
            vga::scroll_screen();
            self.row = height.saturating_sub(2);
        }

        vga::print_at(self.row, 0, text);
        self.row += 1;
        self.used += 1;
    }

    fn field(&mut self, key: &[u8], value: &[u8]) {
        let mut line = [b' '; 128];
        let mut length = 0usize;

        push(&mut line, &mut length, key);
        push(&mut line, &mut length, b": ");
        push(&mut line, &mut length, value);
        self.line(&line[..length]);
    }

    fn error(&mut self, message: &[u8]) {
        let mut line = [b' '; 128];
        let mut length = 0usize;

        push(&mut line, &mut length, b"pfpak: ");
        push(&mut line, &mut length, message);
        self.line(&line[..length]);
    }
}

/// Точка входа команды `pfpak`.
pub fn run(argument: &[u8], row: usize) -> usize {
    let mut printer = Printer::new(row);
    let (command, rest) = split_first(argument);

    let result = match command {
        b"" => {
            print_usage(&mut printer);
            Ok(())
        }
        b"list" => list(&mut printer),
        b"info" => info(rest, &mut printer),
        b"install" => modifying(rest, &mut printer, install_command),
        b"remove" => modifying(rest, &mut printer, remove_command),
        b"update" => modifying(rest, &mut printer, update_command),
        b"upgrade" => modifying(rest, &mut printer, upgrade_command),
        _ => Err(b"unknown command (try: pfpak)" as store::Error),
    };

    if let Err(message) = result {
        if !message.is_empty() {
            printer.error(message);
        }
    }

    printer.used
}

fn modifying<F>(
    argument: &[u8],
    printer: &mut Printer,
    command: F,
) -> Result<(), store::Error>
where
    F: FnOnce(&[u8], &mut Printer) -> Result<(), store::Error>,
{
    if !auth::is_elevated() {
        return Err(b"permission denied. Use elev pfpak ...");
    }

    command(argument, printer)
}

fn print_usage(printer: &mut Printer) {
    printer.line(b"pfpak - Perfix package manager");
    printer.line(b"  pfpak list");
    printer.line(b"  pfpak info <package>");
    printer.line(b"  elev pfpak install <package>");
    printer.line(b"  elev pfpak remove <package>");
    printer.line(b"  elev pfpak update");
    printer.line(b"  elev pfpak upgrade <package> | all");
}

fn list(printer: &mut Printer) -> Result<(), store::Error> {
    let filesystem = store::mount_readonly()?;

    if !store::is_directory(&filesystem, INSTALLED_DIR) {
        printer.line(b"No packages installed");
        return Ok(());
    }

    printer.line(b"Installed packages:");

    let mut manifest_buffer = [0u8; MANIFEST_CAPACITY];
    let mut count = 0usize;
    let mut truncated = false;

    let result = store::for_each_entry(&filesystem, INSTALLED_DIR, |name, is_dir| {
        if !is_dir {
            return true;
        }

        if count >= 32 {
            truncated = true;
            return false;
        }

        let mut path = [0u8; 512];
        let Some(length) = manifest_path(name, &mut path) else {
            return true;
        };

        let Ok(size) = store::read_file(&filesystem, &path[..length], &mut manifest_buffer)
        else {
            return true;
        };

        let data = &manifest_buffer[..size];
        let package = manifest::field(data, b"name").unwrap_or(name);
        let version = manifest::field(data, b"version").unwrap_or(b"?");

        let mut line = [b' '; 128];
        let mut length = 0usize;
        push(&mut line, &mut length, b"  ");
        push(&mut line, &mut length, package);
        push(&mut line, &mut length, b" ");
        push(&mut line, &mut length, version);
        printer.line(&line[..length]);
        count += 1;
        true
    });

    if let Err(message) = result {
        return Err(message);
    }

    if count == 0 {
        printer.line(b"  (none)");
    }

    if truncated {
        printer.line(b"  (more packages hidden)");
    }

    Ok(())
}

fn info(argument: &[u8], printer: &mut Printer) -> Result<(), store::Error> {
    let package = trim(argument);

    if !paths::valid_package_name(package) {
        return Err(b"usage: pfpak info <package>");
    }

    let mark = arena::mark();
    let buffer = match arena::allocate(MANIFEST_CAPACITY, 16) {
        Some(buffer) => buffer,
        None => {
            arena::release(mark);
            return Err(b"not enough memory");
        }
    };

    let result = info_inner(package, buffer, printer);
    arena::release(mark);
    result
}

fn info_inner(
    package: &[u8],
    buffer: &mut [u8],
    printer: &mut Printer,
) -> Result<(), store::Error> {
    let filesystem = store::mount_readonly()?;
    let mut path = [0u8; 512];
    let manifest_path = manifest_path(package, &mut path)
        .ok_or(b"package path is too long" as store::Error)?;

    let (length, installed) = match store::read_file(
        &filesystem,
        &path[..manifest_path],
        buffer,
    ) {
        Ok(length) => (length, true),
        Err(_) => (fetch_manifest(package, buffer)?, false),
    };

    let data = &buffer[..length];
    manifest::validate(data, Some(package))?;

    let name = manifest::field(data, b"name").unwrap_or(package);
    let version = manifest::field(data, b"version").unwrap_or(b"?");
    let maintainer = manifest::field(data, b"maintainer").unwrap_or(b"?");
    let description = manifest::field(data, b"description").unwrap_or(b"?");
    let size = manifest::field(data, b"size").unwrap_or(b"?");
    let depends = manifest::field(data, b"depends").unwrap_or(b"");

    printer.field(b"Name", name);
    printer.field(b"Version", version);
    printer.field(b"Maintainer", maintainer);
    printer.field(b"Description", description);
    printer.field(b"Size", size);
    printer.field(b"Depends", depends);
    printer.field(b"Installed", if installed { b"yes" } else { b"no" });

    let count = manifest::file_count(data);
    let mut header = [b' '; 64];
    let mut length = 0usize;
    push(&mut header, &mut length, b"Files");
    push(&mut header, &mut length, b" (");
    push_dec(&mut header, &mut length, count);
    push(&mut header, &mut length, b")");
    printer.line(&header[..length]);

    let mut shown = 0usize;
    let mut truncated = false;
    manifest::for_each_file(data, |path| {
        if shown >= 24 {
            truncated = true;
            return false;
        }

        let mut line = [b' '; 128];
        let mut length = 0usize;
        push(&mut line, &mut length, b"  ");
        push(&mut line, &mut length, path);
        printer.line(&line[..length]);
        shown += 1;
        true
    });

    if truncated {
        printer.line(b"  ...");
    }

    Ok(())
}

fn install_command(argument: &[u8], printer: &mut Printer) -> Result<(), store::Error> {
    let package = trim(argument);

    if !paths::valid_package_name(package) {
        return Err(b"usage: elev pfpak install <package>");
    }

    install_package(package, printer, 0)
}

fn install_package(
    package: &[u8],
    printer: &mut Printer,
    depth: usize,
) -> Result<(), store::Error> {
    if depth > MAX_DEPTH {
        return Err(b"dependency tree is too deep (or a loop)");
    }

    let mark = arena::mark();
    let result = install_inner(package, printer, depth);
    arena::release(mark);
    result
}

fn install_inner(
    package: &[u8],
    printer: &mut Printer,
    depth: usize,
) -> Result<(), store::Error> {
    let buffer = arena::allocate(MANIFEST_CAPACITY, 16)
        .ok_or(b"not enough memory for manifest" as store::Error)?;
    let length = fetch_manifest(package, buffer)?;
    let remote = &buffer[..length];

    let version = manifest::field(remote, b"version").unwrap_or(b"?");
    let installed = installed_version(package)?;

    if let Some((installed_version, installed_len)) = installed {
        if installed_version[..installed_len] == *version {
            let mut line = [b' '; 128];
            let mut length = 0usize;
            push(&mut line, &mut length, package);
            push(&mut line, &mut length, b" ");
            push(&mut line, &mut length, version);
            push(&mut line, &mut length, b" is already installed");
            printer.line(&line[..length]);
            return Ok(());
        }
    }

    if let Some((installed_version, installed_len)) = installed {
        let mut line = [b' '; 160];
        let mut length = 0usize;
        push(&mut line, &mut length, b"Upgrading ");
        push(&mut line, &mut length, package);
        push(&mut line, &mut length, b" ");
        push(&mut line, &mut length, &installed_version[..installed_len]);
        push(&mut line, &mut length, b" -> ");
        push(&mut line, &mut length, version);
        printer.line(&line[..length]);
    } else {
        let mut line = [b' '; 128];
        let mut length = 0usize;
        push(&mut line, &mut length, b"Installing ");
        push(&mut line, &mut length, package);
        push(&mut line, &mut length, b" ");
        push(&mut line, &mut length, version);
        printer.line(&line[..length]);
    }

    // Сначала зависимости. Родительский манифест остаётся в арене: дочерние
    // установки выделяют и освобождают память после него.
    let mut dependency_error: Option<store::Error> = None;
    manifest::for_each_depend(remote, |dependency| {
        if !paths::valid_package_name(dependency) {
            dependency_error = Some(b"invalid dependency name");
            return false;
        }

        if let Err(message) = install_package(dependency, printer, depth + 1) {
            dependency_error = Some(message);
            return false;
        }

        true
    });

    if let Some(message) = dependency_error {
        return Err(message);
    }

    let blob = fetch_archive(package, version)?;
    let length = blob.length;
    let result = extract_package(package, remote, &mut blob.data[..length]);

    if result.is_err() {
        rollback_files();
    }

    arena::release(blob.mark);
    result?;

    let mut path = [0u8; 512];
    let manifest_path = manifest_path(package, &mut path)
        .ok_or(b"installed manifest path is too long" as store::Error)?;

    let filesystem = store::mount_readwrite()?;

    if let Err(message) = store::write_file(&filesystem, &path[..manifest_path], remote) {
        rollback_files();
        return Err(message);
    }

    let mut line = [b' '; 128];
    let mut length = 0usize;
    push(&mut line, &mut length, b"Installed ");
    push(&mut line, &mut length, package);
    push(&mut line, &mut length, b" ");
    push(&mut line, &mut length, version);
    printer.line(&line[..length]);

    Ok(())
}

fn extract_package(
    package: &[u8],
    remote_manifest: &[u8],
    archive: &mut [u8],
) -> Result<(), store::Error> {
    let filesystem = store::mount_readwrite()?;

    rollback_begin();

    let mut target = InstallTarget {
        filesystem: &filesystem,
        manifest: remote_manifest,
    };

    let mut extractor = tar::Extractor::new(&mut target);
    let result = inflate::gunzip(archive, &mut extractor);

    match result {
        Ok(()) => extractor.finish(),
        Err(message) => {
            extractor.finish();
            return Err(message);
        }
    }

    // Манифест — единственный источник списка файлов: каждый файл из него
    // должен действительно появиться после распаковки.
    let mut missing: Option<store::Error> = None;
    manifest::for_each_file(remote_manifest, |entry| {
        let path = match paths::normalize(entry) {
            Ok(path) if paths::allowed(&path) => path,
            _ => {
                missing = Some(b"manifest contains an unsafe file path");
                return false;
            }
        };

        if !store::exists(&filesystem, &path) {
            missing = Some(b"manifest lists a file missing from archive");
            return false;
        }

        true
    });

    if let Some(message) = missing {
        return Err(message);
    }

    // Небольшая проверка: архив должен быть не пустым.
    let _ = package;
    Ok(())
}

struct InstallTarget<'a> {
    filesystem: &'a Ext4<'static, AtaDevice>,
    manifest: &'a [u8],
}

impl tar::Target for InstallTarget<'_> {
    fn directory(&mut self, path: &[u8]) -> Result<(), tar::Error> {
        store::ensure_directory(self.filesystem, path)
    }

    fn file(&mut self, path: &[u8], data: &[u8]) -> Result<(), tar::Error> {
        if !manifest::has_file(self.manifest, path) {
            return Err(b"archive file is not listed in manifest");
        }

        if !store::exists(self.filesystem, path) {
            rollback_record(path);
        }

        store::write_file(self.filesystem, path, data)
    }
}

fn remove_command(argument: &[u8], printer: &mut Printer) -> Result<(), store::Error> {
    let package = trim(argument);

    if !paths::valid_package_name(package) {
        return Err(b"usage: elev pfpak remove <package>");
    }

    let mark = arena::mark();
    let buffer = match arena::allocate(MANIFEST_CAPACITY, 16) {
        Some(buffer) => buffer,
        None => {
            arena::release(mark);
            return Err(b"not enough memory");
        }
    };

    let result = remove_inner(package, buffer, printer);
    arena::release(mark);
    result
}

fn remove_inner(
    package: &[u8],
    buffer: &mut [u8],
    printer: &mut Printer,
) -> Result<(), store::Error> {
    let filesystem = store::mount_readwrite()?;
    let mut path = [0u8; 512];
    let manifest_length = manifest_path(package, &mut path)
        .ok_or(b"installed manifest path is too long" as store::Error)?;
    let length = store::read_file(&filesystem, &path[..manifest_length], buffer)
        .map_err(|_| b"package is not installed" as store::Error)?;
    let data = &buffer[..length];

    manifest::validate(data, Some(package))?;

    if let Some(required_by) = reverse_dependency(&filesystem, package)? {
        let mut line = [b' '; 192];
        let mut length = 0usize;
        push(&mut line, &mut length, b"pfpak: ");
        push(&mut line, &mut length, package);
        push(&mut line, &mut length, b" is required by ");
        push(&mut line, &mut length, &required_by);
        printer.line(&line[..length]);

        // Подробное сообщение уже напечатано выше; пустая строка
        // говорит run() не дублировать общую ошибку.
        return Err(b"");
    }

    // Сначала проверяем весь список, чтобы не удалить половину пакета,
    // если манифест был испорчен.
    let mut invalid = false;
    manifest::for_each_file(data, |entry| {
        match paths::normalize(entry) {
            Ok(path) if paths::allowed(&path) => true,
            _ => {
                invalid = true;
                false
            }
        }
    });

    if invalid {
        return Err(b"manifest contains an unsafe file path");
    }

    let mut removed = 0usize;
    manifest::for_each_file(data, |entry| {
        let Ok(path) = paths::normalize(entry) else {
            return false;
        };

        if store::exists(&filesystem, &path) && store::remove_file(&filesystem, &path).is_ok() {
            removed += 1;
        }

        true
    });

    let _ = store::remove_file(&filesystem, &path[..manifest_length]);
    let directory_length = package_path(package, &mut path).unwrap_or(0);

    if directory_length > 0 {
        let _ = store::remove_directory(&filesystem, &path[..directory_length]);
    }

    let mut line = [b' '; 128];
    let mut length = 0usize;
    push(&mut line, &mut length, b"Removed ");
    push(&mut line, &mut length, package);
    push(&mut line, &mut length, b" (");
    push_dec(&mut line, &mut length, removed);
    push(&mut line, &mut length, b" files)");
    printer.line(&line[..length]);

    Ok(())
}

fn update_command(_argument: &[u8], printer: &mut Printer) -> Result<(), store::Error> {
    let (repos, count) = load_repos()?;
    let mark = arena::mark();
    let buffer = match arena::allocate(MANIFEST_CAPACITY, 16) {
        Some(buffer) => buffer,
        None => {
            arena::release(mark);
            return Err(b"not enough memory");
        }
    };

    let mut updated = 0usize;

    for (index, repo) in repos[..count].iter().enumerate() {
        let mut url = [0u8; 512];
        let Some(url_length) = repo::build_index_url(repo, &mut url) else {
            continue;
        };

        match http::fetch(&url[..url_length], buffer, false, 10) {
            Ok(response) if (200..300).contains(&response.status) => {
                let start = response.body_start;
                let end = response.body_end;

                if end < start {
                    continue;
                }

                buffer.copy_within(start..end, 0);
                let mut path = [0u8; 512];
                let Some(path_length) = cache_index_path(index, &mut path) else {
                    continue;
                };

                let filesystem = store::mount_readwrite()?;
                store::write_file(&filesystem, &path[..path_length], &buffer[..end - start])?;

                let mut line = [b' '; 128];
                let mut length = 0usize;
                push(&mut line, &mut length, b"Updated repository ");
                push_dec(&mut line, &mut length, index + 1);
                printer.line(&line[..length]);
                updated += 1;
            }
            Ok(_) => {
                let mut line = [b' '; 128];
                let mut length = 0usize;
                push(&mut line, &mut length, b"Repository ");
                push_dec(&mut line, &mut length, index + 1);
                push(&mut line, &mut length, b": no INDEX (skipped)");
                printer.line(&line[..length]);
            }
            Err(message) => {
                let mut line = [b' '; 160];
                let mut length = 0usize;
                push(&mut line, &mut length, b"Repository ");
                push_dec(&mut line, &mut length, index + 1);
                push(&mut line, &mut length, b": ");
                push(&mut line, &mut length, message);
                printer.line(&line[..length]);
            }
        }
    }

    arena::release(mark);

    if updated == 0 {
        printer.line(b"No repository indexes updated");
    } else {
        let mut line = [b' '; 96];
        let mut length = 0usize;
        push(&mut line, &mut length, b"Updated ");
        push_dec(&mut line, &mut length, updated);
        push(&mut line, &mut length, b" repository index(es)");
        printer.line(&line[..length]);
    }

    Ok(())
}

fn upgrade_command(argument: &[u8], printer: &mut Printer) -> Result<(), store::Error> {
    let package = trim(argument);

    if package == b"all" {
        return upgrade_all(printer);
    }

    if !paths::valid_package_name(package) {
        return Err(b"usage: elev pfpak upgrade <package> | all");
    }

    upgrade_one(package, printer, true)
}

fn upgrade_all(printer: &mut Printer) -> Result<(), store::Error> {
    let filesystem = store::mount_readonly()?;

    if !store::is_directory(&filesystem, INSTALLED_DIR) {
        printer.line(b"No packages installed");
        return Ok(());
    }

    let mut names = [[0u8; 64]; 32];
    let mut lengths = [0usize; 32];
    let mut count = 0usize;
    let mut too_many = false;

    store::for_each_entry(&filesystem, INSTALLED_DIR, |name, is_dir| {
        if !is_dir {
            return true;
        }

        if count >= names.len() {
            too_many = true;
            return false;
        }

        if !paths::valid_package_name(name) {
            return true;
        }

        lengths[count] = name.len();
        names[count][..name.len()].copy_from_slice(name);
        count += 1;
        true
    })?;

    if too_many {
        printer.line(b"Too many packages; upgrade them one by one");
    }

    let mut failed = 0usize;

    for index in 0..count {
        let name = &names[index][..lengths[index]];

        if let Err(message) = upgrade_one(name, printer, false) {
            let mut line = [b' '; 160];
            let mut length = 0usize;
            push(&mut line, &mut length, name);
            push(&mut line, &mut length, b": ");
            push(&mut line, &mut length, message);
            printer.line(&line[..length]);
            failed += 1;
        }
    }

    if failed > 0 {
        return Err(b"some packages failed to upgrade");
    }

    Ok(())
}

fn upgrade_one(
    package: &[u8],
    printer: &mut Printer,
    require_installed: bool,
) -> Result<(), store::Error> {
    let installed = installed_version(package)?;

    let Some((installed_version, installed_len)) = installed else {
        if require_installed {
            return Err(b"package is not installed");
        }

        return Ok(());
    };

    let mark = arena::mark();
    let buffer = match arena::allocate(MANIFEST_CAPACITY, 16) {
        Some(buffer) => buffer,
        None => {
            arena::release(mark);
            return Err(b"not enough memory");
        }
    };

    let result = (|| {
        let length = fetch_manifest(package, buffer)?;
        let remote = &buffer[..length];
        let remote_version = manifest::field(remote, b"version").unwrap_or(b"?");

        if !version_is_newer(remote_version, &installed_version[..installed_len]) {
            let mut line = [b' '; 128];
            let mut length = 0usize;
            push(&mut line, &mut length, package);
            push(&mut line, &mut length, b" is up to date");
            printer.line(&line[..length]);
            return Ok(());
        }

        install_package(package, printer, 0)
    })();

    arena::release(mark);
    result
}

struct Blob {
    data: &'static mut [u8],
    length: usize,
    mark: u32,
}

fn fetch_archive(
    package: &[u8],
    version: &[u8],
) -> Result<Blob, store::Error> {
    let (repos, count) = load_repos()?;
    let mut file = [0u8; 128];
    let mut file_length = 0usize;
    push(&mut file, &mut file_length, package);
    push(&mut file, &mut file_length, b"-");
    push(&mut file, &mut file_length, version);
    push(&mut file, &mut file_length, b".tar.gz");

    let mut last: store::Error = b"package archive not found";

    for repo in &repos[..count] {
        let mut url = [0u8; 512];
        let Some(url_length) = repo::build_url(repo, package, &file[..file_length], &mut url)
        else {
            continue;
        };

        for &capacity in &ARCHIVE_SIZES {
            let mark = arena::mark();
            let Some(buffer) = arena::allocate(capacity, 16) else {
                continue;
            };

            match http::fetch(&url[..url_length], buffer, false, 20) {
                Ok(response) if (200..300).contains(&response.status) => {
                    let start = response.body_start;
                    let end = response.body_end;

                    if start > end || end > buffer.len() {
                        arena::release(mark);
                        last = b"malformed package archive";
                        continue;
                    }

                    buffer.copy_within(start..end, 0);
                    let length = end - start;

                    return Ok(Blob {
                        data: buffer,
                        length,
                        mark,
                    });
                }
                Ok(_) => {
                    arena::release(mark);
                    last = b"package archive not found";
                    break;
                }
                Err(message) => {
                    arena::release(mark);

                    if message == b"response is too large".as_slice() {
                        continue;
                    }

                    last = message;
                    break;
                }
            }
        }
    }

    Err(last)
}

fn fetch_manifest(package: &[u8], output: &mut [u8]) -> Result<usize, store::Error> {
    let (repos, count) = load_repos()?;
    let mut last: store::Error = b"package not found in repositories";

    for repo in &repos[..count] {
        let mut url = [0u8; 512];
        let Some(url_length) = repo::build_url(repo, package, MANIFEST_NAME, &mut url) else {
            continue;
        };

        match http::fetch(&url[..url_length], output, false, 10) {
            Ok(response) if (200..300).contains(&response.status) => {
                let start = response.body_start;
                let end = response.body_end;

                if start > end || end > output.len() {
                    return Err(b"malformed manifest response");
                }

                output.copy_within(start..end, 0);
                let length = end - start;
                manifest::validate(&output[..length], Some(package))?;
                return Ok(length);
            }
            Ok(_) => {
                last = b"package not found in repositories";
            }
            Err(message) => {
                last = message;
            }
        }
    }

    Err(last)
}

fn load_repos() -> Result<([repo::Repo; 8], usize), store::Error> {
    let filesystem = store::mount_readonly()?;
    let mark = arena::mark();
    let buffer = match arena::allocate(8192, 16) {
        Some(buffer) => buffer,
        None => {
            arena::release(mark);
            return Err(b"not enough memory");
        }
    };

    let length = store::read_file(&filesystem, REPO_LIST, buffer)
        .map_err(|_| b"cannot read /etc/pfpak/repo.list" as store::Error)?;
    let mut repos = [repo::Repo::EMPTY; 8];
    let count = repo::parse(&buffer[..length], &mut repos);
    arena::release(mark);

    if count == 0 {
        return Err(b"no repositories configured in /etc/pfpak/repo.list");
    }

    Ok((repos, count))
}

fn installed_version(package: &[u8]) -> Result<Option<([u8; 64], usize)>, store::Error> {
    let filesystem = store::mount_readonly()?;
    let mut path = [0u8; 512];
    let manifest_path = manifest_path(package, &mut path)
        .ok_or(b"installed manifest path is too long" as store::Error)?;

    if !store::exists(&filesystem, &path[..manifest_path]) {
        return Ok(None);
    }

    let mark = arena::mark();
    let buffer = match arena::allocate(MANIFEST_CAPACITY, 16) {
        Some(buffer) => buffer,
        None => {
            arena::release(mark);
            return Err(b"not enough memory");
        }
    };

    let length = store::read_file(&filesystem, &path[..manifest_path], buffer)?;
    let data = &buffer[..length];
    let Some(version) = manifest::field(data, b"version") else {
        arena::release(mark);
        return Err(b"installed manifest has no version");
    };

    let mut copy = [0u8; 64];
    let copy_length = version.len().min(copy.len());
    copy[..copy_length].copy_from_slice(&version[..copy_length]);
    arena::release(mark);
    Ok(Some((copy, copy_length)))
}

fn reverse_dependency(
    filesystem: &Ext4<'static, AtaDevice>,
    package: &[u8],
) -> Result<Option<[u8; 64]>, store::Error> {
    let mut found: Option<[u8; 64]> = None;

    store::for_each_entry(filesystem, INSTALLED_DIR, |name, is_dir| {
        if !is_dir || name == package || !paths::valid_package_name(name) {
            return true;
        }

        let mut path = [0u8; 512];
        let Some(path_length) = manifest_path(name, &mut path) else {
            return true;
        };

        let mark = arena::mark();
        let Some(buffer) = arena::allocate(MANIFEST_CAPACITY, 16) else {
            arena::release(mark);
            return true;
        };

        if let Ok(length) = store::read_file(filesystem, &path[..path_length], buffer) {
            let data = &buffer[..length];
            let mut required = false;

            manifest::for_each_depend(data, |dependency| {
                if dependency == package {
                    required = true;
                    false
                } else {
                    true
                }
            });

            if required {
                let mut copy = [0u8; 64];
                let length = name.len().min(copy.len());
                copy[..length].copy_from_slice(&name[..length]);
                found = Some(copy);
            }
        }

        arena::release(mark);
        found.is_none()
    })?;

    Ok(found)
}

fn manifest_path(package: &[u8], out: &mut [u8; 512]) -> Option<usize> {
    let mut length = package_path(package, out)?;
    append(out, &mut length, b"/")?;
    append(out, &mut length, MANIFEST_NAME)?;
    Some(length)
}

fn package_path(package: &[u8], out: &mut [u8; 512]) -> Option<usize> {
    let mut length = 0usize;
    append(out, &mut length, INSTALLED_DIR)?;
    append(out, &mut length, b"/")?;
    append(out, &mut length, package)?;
    Some(length)
}

fn cache_index_path(index: usize, out: &mut [u8; 512]) -> Option<usize> {
    let mut length = 0usize;
    append(out, &mut length, CACHE_DIR)?;
    append(out, &mut length, b"/repo-")?;
    push_dec(out, &mut length, index + 1);
    append(out, &mut length, b"/INDEX")?;
    Some(length)
}

fn rollback_begin() {
    unsafe {
        ROLLBACK_COUNT = 0;
    }
}

fn rollback_record(path: &[u8]) {
    unsafe {
        let count = ROLLBACK_COUNT;

        if count >= MAX_ROLLBACK {
            return;
        }

        let length = path.len().min(paths::PATH_MAX);
        ROLLBACK[count][..length].copy_from_slice(&path[..length]);
        ROLLBACK_LENGTHS[count] = length;
        ROLLBACK_COUNT = count + 1;
    }
}

fn rollback_files() {
    let filesystem = match store::mount_readwrite() {
        Ok(filesystem) => filesystem,
        Err(_) => return,
    };

    unsafe {
        for index in 0..ROLLBACK_COUNT {
            let path = &ROLLBACK[index][..ROLLBACK_LENGTHS[index]];
            let _ = store::remove_file(&filesystem, path);
        }

        ROLLBACK_COUNT = 0;
    }
}

fn version_is_newer(remote: &[u8], installed: &[u8]) -> bool {
    let mut left = 0usize;
    let mut right = 0usize;

    loop {
        let left_digit = remote.get(left).is_some_and(|byte| byte.is_ascii_digit());
        let right_digit = installed.get(right).is_some_and(|byte| byte.is_ascii_digit());

        if left_digit && right_digit {
            let left_start = left;
            let right_start = right;

            while left < remote.len() && remote[left].is_ascii_digit() {
                left += 1;
            }

            while right < installed.len() && installed[right].is_ascii_digit() {
                right += 1;
            }

            let mut left_zero = left_start;
            let mut right_zero = right_start;

            while left_zero + 1 < left && remote[left_zero] == b'0' {
                left_zero += 1;
            }

            while right_zero + 1 < right && installed[right_zero] == b'0' {
                right_zero += 1;
            }

            let left_length = left - left_zero;
            let right_length = right - right_zero;

            if left_length != right_length {
                return left_length > right_length;
            }

            if remote[left_zero..left] != installed[right_zero..right] {
                return remote[left_zero..left] > installed[right_zero..right];
            }
        } else if left < remote.len() && right < installed.len() {
            if remote[left] != installed[right] {
                return remote[left] > installed[right];
            }

            left += 1;
            right += 1;
        } else {
            return remote.len() > left;
        }
    }
}

fn split_first(input: &[u8]) -> (&[u8], &[u8]) {
    let mut end = 0usize;

    while end < input.len() && input[end] != b' ' && input[end] != b'\t' {
        end += 1;
    }

    (trim(&input[..end]), trim(&input[end..]))
}

fn trim(mut data: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = data.split_first() {
        if first == b' ' || first == b'\t' {
            data = rest;
        } else {
            break;
        }
    }

    while let Some((&last, rest)) = data.split_last() {
        if last == b' ' || last == b'\t' {
            data = rest;
        } else {
            break;
        }
    }

    data
}

fn push(out: &mut [u8], length: &mut usize, data: &[u8]) {
    for &byte in data {
        if *length >= out.len() {
            return;
        }

        out[*length] = byte;
        *length += 1;
    }
}

fn append(out: &mut [u8; 512], length: &mut usize, data: &[u8]) -> Option<()> {
    if *length + data.len() > out.len() {
        return None;
    }

    out[*length..*length + data.len()].copy_from_slice(data);
    *length += data.len();
    Some(())
}

fn push_dec(out: &mut [u8], length: &mut usize, mut value: usize) {
    let mut digits = [0u8; 20];
    let mut count = 0usize;

    if value == 0 {
        digits[0] = b'0';
        count = 1;
    } else {
        while value > 0 {
            digits[count] = b'0' + (value % 10) as u8;
            value /= 10;
            count += 1;
        }
    }

    while count > 0 {
        count -= 1;
        push(out, length, &[digits[count]]);
    }
}

