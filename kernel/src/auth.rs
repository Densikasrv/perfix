//! Пользователи, пароли и права доступа.
//!
//! Учётные записи хранятся в обычном текстовом файле `/etc/users` на
//! установленном ext4-разделе. Каждая строка имеет вид:
//!
//! ```text
//! <role>:<name>:<salt-hex>:<hash-hex>
//! ```
//!
//! `role` равен `u` для обычного пользователя и `r` для суперпользователя
//! (`root`). Пароль никогда не хранится в открытом виде: сохраняется
//! только соль и хеш.
//!
//! На установочном носителе файла `/etc/users` ещё нет. В этом случае
//! система работает как раньше — от имени root без входа в систему, чтобы
//! установщик оставался доступным.

use core::arch::asm;

use crate::fs::ext4::{AtaDevice, Ext4};
use crate::{keyboard, vga};

/// Путь к файлу учётных записей.
const USERS_PATH: &[u8] = b"/etc/users";
/// Максимальная длина имени пользователя.
pub const USERNAME_MAX: usize = 16;
/// Максимальная длина пароля.
pub const PASSWORD_MAX: usize = 64;
/// Сколько учётных записей помещается в памяти.
const MAX_ACCOUNTS: usize = 8;

/// Учётная запись.
#[derive(Clone, Copy)]
struct Account {
    name: [u8; USERNAME_MAX],
    name_len: usize,
    salt: u64,
    hash: u64,
    super_user: bool,
}

impl Account {
    const EMPTY: Self = Self {
        name: [0; USERNAME_MAX],
        name_len: 0,
        salt: 0,
        hash: 0,
        super_user: false,
    };

    fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }
}

static mut ACCOUNTS: [Account; MAX_ACCOUNTS] = [Account::EMPTY; MAX_ACCOUNTS];
static mut ACCOUNT_COUNT: usize = 0;
/// Есть ли на диске база пользователей (то есть система установлена).
static mut ACCOUNTS_PRESENT: bool = false;

/// Текущий пользователь сеанса.
static mut SESSION_NAME: [u8; USERNAME_MAX] = [0; USERNAME_MAX];
static mut SESSION_NAME_LEN: usize = 0;
/// Домашний каталог текущего пользователя.
static mut SESSION_HOME: [u8; 64] = [0; 64];
static mut SESSION_HOME_LEN: usize = 0;
/// Временное повышение прав командой `elev`.
static mut ELEVATED: bool = false;
/// Вошёл ли в систему сам суперпользователь.
static mut SESSION_SUPER: bool = false;

/// Данные, введённые в установщике.
pub struct NewCredentials {
    pub username: [u8; USERNAME_MAX],
    pub username_len: usize,
    pub password: [u8; PASSWORD_MAX],
    pub password_len: usize,
    pub super_password: [u8; PASSWORD_MAX],
    pub super_password_len: usize,
    /// Сколько строк экрана занял диалог.
    pub rows: usize,
}

/// Читает учётные записи с выбранного ext4-раздела.
///
/// Вызывается после [`crate::disk::auto_select`], поэтому системный
/// раздел уже выбран. При любой ошибке система остаётся в режиме root.
pub fn init() {
    unsafe {
        ACCOUNT_COUNT = 0;
        ACCOUNTS_PRESENT = false;
    }

    // Установочный ISO всегда работает как root, чтобы установщик был
    // доступен независимо от того, что лежит на диске.
    if crate::installer::is_live_media() {
        return;
    }

    if crate::disk::selected_kind() != crate::disk::FsKind::Ext4 {
        return;
    }

    let device = AtaDevice;
    let Ok(filesystem) = Ext4::mount(
        &device,
        crate::disk::selected_lba(),
        crate::disk::selected_sectors(),
    ) else {
        return;
    };

    let Ok(Some(inode)) = filesystem.find_inode_path(USERS_PATH) else {
        return;
    };

    let Ok(info) = filesystem.read_inode(inode) else {
        return;
    };

    if info.is_directory {
        return;
    }

    let mut buffer = [0u8; 1024];
    let mut offset = 0u64;

    while offset < info.size && (offset as usize) < buffer.len() {
        match filesystem.read_file(
            inode,
            offset,
            &mut buffer[offset as usize..],
        ) {
            Ok(0) => break,
            Ok(count) => offset += count as u64,
            Err(_) => return,
        }
    }

    parse_accounts(&buffer[..offset as usize]);

    unsafe {
        ACCOUNTS_PRESENT = ACCOUNT_COUNT > 0;
    }
}

/// Нужен ли вход в систему.
pub fn login_required() -> bool {
    unsafe { ACCOUNTS_PRESENT }
}

/// Домашний каталог текущего пользователя.
pub fn home() -> &'static [u8] {
    unsafe { &SESSION_HOME[..SESSION_HOME_LEN] }
}

/// Работает ли сеанс с правами суперпользователя.
///
/// На установочном носителе без `/etc/users` это всегда `true`.
pub fn is_elevated() -> bool {
    unsafe { ELEVATED || SESSION_SUPER || !ACCOUNTS_PRESENT }
}

/// Включает или выключает временное повышение прав (`elev`).
pub fn set_elevated(value: bool) {
    unsafe {
        ELEVATED = value;
    }
}

/// Проверяет, разрешена ли запись по указанному абсолютному пути.
pub fn can_write(path: &[u8]) -> bool {
    if is_elevated() {
        return true;
    }

    is_user_path(path)
}

/// Относится ли путь к системной части файловой системы.
///
/// Системными считаются все пути вне домашнего каталога пользователя и
/// вне `/tmp`.
pub fn is_system_path(path: &[u8]) -> bool {
    !is_user_path(path)
}

/// Спрашивает подтверждение перед удалением системного файла.
///
/// Возвращает `true`, если пользователь ответил `Y`.
pub fn confirm_system_delete(row: usize) -> bool {
    vga::print_at(row, 0, b"Do you know what you're doing? [Y/N]");

    loop {
        match keyboard::read_key() {
            keyboard::Key::Character(b'y') | keyboard::Key::Character(b'Y') => {
                // Стираем предупреждение, чтобы результат удаления занял
                // его место и не смешался с вопросом.
                vga::print_at(
                    row,
                    0,
                    b"                                        ",
                );
                return true;
            }
            keyboard::Key::Character(b'n') | keyboard::Key::Character(b'N') => {
                return false;
            }
            keyboard::Key::None => unsafe {
                asm!("pause", options(nomem, nostack, preserves_flags));
            },
            _ => {}
        }
    }
}

/// Запрашивает пароль суперпользователя для команды `elev`.
pub fn authorize_super(row: usize) -> bool {
    if !login_required() {
        return true;
    }

    let mut password = [0u8; PASSWORD_MAX];
    let mut prompt_row = row;
    let length = read_line(
        b"password: ",
        true,
        &mut password,
        &mut prompt_row,
    );

    super_password_valid(&password[..length])
}

/// Вход в систему после загрузки установленной системы.
pub fn login() {
    loop {
        vga::clear_screen();
        vga::hide_cursor();

        vga::print_at(0, 0, b"Perfix login");
        vga::print_at(1, 0, b"Enter your user name and password.");

        let mut row = 3usize;
        let mut name = [0u8; USERNAME_MAX];
        let name_len =
            read_line(b"login: ", false, &mut name, &mut row);
        let mut password = [0u8; PASSWORD_MAX];
        let password_len =
            read_line(b"password: ", true, &mut password, &mut row);

        if let Some(account) = find_user(&name[..name_len]) {
            if verify(&account, &password[..password_len]) {
                set_session(&account);
                vga::clear_screen();
                return;
            }
        }

        vga::print_at(row + 1, 0, b"Login incorrect");
        vga::print_at(row + 2, 0, b"Press Enter to try again");

        loop {
            match keyboard::read_key() {
                keyboard::Key::Enter => break,
                keyboard::Key::None => unsafe {
                    asm!("pause", options(nomem, nostack, preserves_flags));
                },
                _ => {}
            }
        }
    }
}

/// Интерактивный ввод имени пользователя и паролей для установщика.
///
/// Возвращает `None`, если ввод недоступен (например, команда запущена
/// внутри окна pterm) или отменён.
pub fn prompt_new_credentials(start_row: usize) -> Option<NewCredentials> {
    if vga::is_capturing() {
        return None;
    }

    let mut row = start_row;
    vga::print_at(row, 0, b"Create the user account for the installed system");
    row += 1;

    let mut username = [0u8; USERNAME_MAX];
    let username_len;

    loop {
        let length = read_line(
            b"Username (1-16 chars): ",
            false,
            &mut username,
            &mut row,
        );

        if valid_username(&username[..length]) {
            username_len = length;
            break;
        }

        vga::print_at(
            row,
            0,
            b"Use 1-16 letters, digits, '_' or '-' (no spaces or '/').",
        );
        row += 1;
    }

    let mut password = [0u8; PASSWORD_MAX];
    let password_len;

    loop {
        let length = read_line(
            b"User password: ",
            true,
            &mut password,
            &mut row,
        );

        if length > 0 {
            password_len = length;
            break;
        }

        vga::print_at(row, 0, b"Password must not be empty.");
        row += 1;
    }

    let mut super_password = [0u8; PASSWORD_MAX];
    let super_password_len;

    loop {
        let length = read_line(
            b"Superuser password: ",
            true,
            &mut super_password,
            &mut row,
        );

        if length > 0 {
            super_password_len = length;
            break;
        }

        vga::print_at(row, 0, b"Password must not be empty.");
        row += 1;
    }

    Some(NewCredentials {
        username,
        username_len,
        password,
        password_len,
        super_password,
        super_password_len,
        rows: row.saturating_sub(start_row),
    })
}

/// Собирает содержимое `/etc/users` для установщика.
///
/// Возвращает длину записанных байт или `None`, если имя или пароль
/// недопустимы.
pub fn build_accounts(
    username: &[u8],
    password: &[u8],
    super_password: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    if !valid_username(username)
        || password.is_empty()
        || super_password.is_empty()
    {
        return None;
    }

    let mut length = 0usize;

    let user_salt = random_salt();
    let user_hash = hash_password(user_salt, password);
    push_account(out, &mut length, b'u', username, user_salt, user_hash)?;

    let super_salt = random_salt() ^ 0xA5A5_5A5A_1234_5678;
    let super_hash = hash_password(super_salt, super_password);
    push_account(out, &mut length, b'r', b"root", super_salt, super_hash)?;

    Some(length)
}

/// Строка `/etc/users` в виде обычной соли и хеша.
fn push_account(
    out: &mut [u8],
    length: &mut usize,
    role: u8,
    name: &[u8],
    salt: u64,
    hash: u64,
) -> Option<()> {
    push_byte(out, length, role)?;
    push_byte(out, length, b':')?;

    for &byte in name {
        push_byte(out, length, byte)?;
    }

    push_byte(out, length, b':')?;
    push_hex(out, length, salt)?;
    push_byte(out, length, b':')?;
    push_hex(out, length, hash)?;
    push_byte(out, length, b'\n')
}

fn push_byte(out: &mut [u8], length: &mut usize, byte: u8) -> Option<()> {
    if *length >= out.len() {
        return None;
    }

    out[*length] = byte;
    *length += 1;
    Some(())
}

fn push_hex(out: &mut [u8], length: &mut usize, value: u64) -> Option<()> {
    let digits = b"0123456789abcdef";

    for index in (0..16).rev() {
        let digit = digits[((value >> (index * 4)) & 0xF) as usize];
        push_byte(out, length, digit)?;
    }

    Some(())
}

/// Разбирает содержимое `/etc/users`.
fn parse_accounts(data: &[u8]) {
    for line in data.split(|&byte| byte == b'\n') {
        if line.is_empty() {
            continue;
        }

        let mut fields = line.split(|&byte| byte == b':');
        let Some(role) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(salt_hex) = fields.next() else {
            continue;
        };
        let Some(hash_hex) = fields.next() else {
            continue;
        };

        if !valid_username(name) {
            continue;
        }

        let (Some(salt), Some(hash)) = (
            parse_hex(salt_hex),
            parse_hex(hash_hex),
        ) else {
            continue;
        };

        let count = unsafe { ACCOUNT_COUNT };

        if count >= MAX_ACCOUNTS {
            break;
        }

        let mut account = Account::EMPTY;
        account.name_len = name.len().min(USERNAME_MAX);
        account.name[..account.name_len]
            .copy_from_slice(&name[..account.name_len]);
        account.salt = salt;
        account.hash = hash;
        account.super_user = role == b"r";

        unsafe {
            ACCOUNTS[count] = account;
            ACCOUNT_COUNT = count + 1;
        }
    }
}

fn find_user(name: &[u8]) -> Option<Account> {
    let count = unsafe { ACCOUNT_COUNT };

    for index in 0..count {
        let account = unsafe { ACCOUNTS[index] };

        if account.name() == name {
            return Some(account);
        }
    }

    None
}

/// Проверяет пароль суперпользователя.
fn super_password_valid(password: &[u8]) -> bool {
    let count = unsafe { ACCOUNT_COUNT };

    for index in 0..count {
        let account = unsafe { ACCOUNTS[index] };

        if account.super_user && verify(&account, password) {
            return true;
        }
    }

    false
}

fn verify(account: &Account, password: &[u8]) -> bool {
    // Пустой пароль не должен совпадать с настоящим.
    if password.is_empty() {
        return false;
    }

    hash_password(account.salt, password) == account.hash
}

fn set_session(account: &Account) {
    let mut home = [0u8; 64];
    let mut home_len = 0usize;

    if account.super_user {
        home[0] = b'/';
        home_len = 1;
    } else {
        for &byte in b"/home/" {
            home[home_len] = byte;
            home_len += 1;
        }

        for &byte in account.name() {
            if home_len >= home.len() {
                break;
            }

            home[home_len] = byte;
            home_len += 1;
        }
    }

    unsafe {
        SESSION_NAME_LEN = account.name_len;
        SESSION_NAME[..account.name_len]
            .copy_from_slice(&account.name[..account.name_len]);
        SESSION_HOME_LEN = home_len;
        SESSION_HOME[..home_len].copy_from_slice(&home[..home_len]);
        SESSION_SUPER = account.super_user;
        ELEVATED = false;
    }
}

/// Записывает строку в экран и читает ввод до Enter.
fn read_line(
    prompt: &[u8],
    mask: bool,
    out: &mut [u8],
    row: &mut usize,
) -> usize {
    let mut length = 0usize;
    let mut column = prompt.len();

    vga::print_at(*row, 0, prompt);

    loop {
        match keyboard::read_key() {
            keyboard::Key::Enter => {
                *row += 1;
                return length;
            }
            keyboard::Key::Backspace => {
                if length > 0 {
                    length -= 1;

                    if column > prompt.len() {
                        column -= 1;
                        vga::print_at(*row, column, b" ");
                    }
                }
            }
            keyboard::Key::Character(ch) => {
                if ch >= 0x20 && length < out.len() && column < 79 {
                    out[length] = ch;
                    length += 1;

                    let shown = if mask { b'*' } else { ch };
                    vga::print_at(*row, column, &[shown]);
                    column += 1;
                }
            }
            keyboard::Key::None => unsafe {
                asm!("pause", options(nomem, nostack, preserves_flags));
            },
            _ => {}
        }
    }
}

/// Допустимое имя пользователя: 1–16 символов, безопасных для пути.
pub fn valid_username(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > USERNAME_MAX {
        return false;
    }

    if name == b"." || name == b".." {
        return false;
    }

    for &byte in name {
        let ok = byte.is_ascii_alphanumeric()
            || byte == b'_'
            || byte == b'-';

        if !ok {
            return false;
        }
    }

    true
}

fn is_user_path(path: &[u8]) -> bool {
    if path == b"/tmp" || path.starts_with(b"/tmp/") {
        return true;
    }

    let home = home();

    if home.is_empty() {
        return false;
    }

    if path == home {
        return true;
    }

    if path.len() <= home.len() {
        return false;
    }

    &path[..home.len()] == home && path[home.len()] == b'/'
}

fn parse_hex(input: &[u8]) -> Option<u64> {
    if input.len() != 16 {
        return None;
    }

    let mut value = 0u64;

    for &byte in input {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };

        value = (value << 4) | digit as u64;
    }

    Some(value)
}

/// Простая соленая хеш-функция для паролей.
///
/// Это не криптостойкий KDF, но открытый пароль на диск не попадает.
fn hash_password(salt: u64, password: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ salt;

    for round in 0..64u64 {
        hash = mix(hash, round as u8);
        hash = mix(hash, salt as u8);

        for &byte in password {
            hash = mix(hash, byte);
        }

        hash ^= salt.rotate_left((round % 63) as u32);
    }

    hash
}

fn mix(mut hash: u64, byte: u8) -> u64 {
    hash ^= byte as u64;
    hash = hash.wrapping_mul(0x100_0000_01b3);
    hash ^ (hash >> 29)
}

/// Соль из счётчика тактов процессора.
fn random_salt() -> u64 {
    let low: u32;
    let high: u32;

    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }

    ((high as u64) << 32) ^ low as u64 ^ 0x9e37_79b9_7f4a_7c15
}
