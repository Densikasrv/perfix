//! Ядро файловой системы exFAT (только чтение).
//!
//! Модуль не зависит от остального ядра: весь доступ к диску выполняется
//! через трейт [`BlockReader`], поэтому логику можно проверять на хосте
//! подменой устройства чтения.
//!
//! Поддерживается стандартная конфигурация exFAT: сектор 512 байт,
//! один FAT, записи FAT по 4 байта. Запись на диск не реализована.

/// Размер сектора, с которым работает драйвер.
pub const SECTOR_SIZE: usize = 512;

/// Максимальная длина имени файла в exFAT (в символах UTF-16).
pub const MAX_NAME_CHARS: usize = 255;

const ENTRY_SIZE: usize = 32;
const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / ENTRY_SIZE;

/// Атрибут «каталог» в записи файла.
pub const ATTR_DIRECTORY: u16 = 0x10;

const ENTRY_FILE: u8 = 0x85;
const ENTRY_STREAM: u8 = 0xC0;
const ENTRY_NAME: u8 = 0xC1;
const ENTRY_END: u8 = 0x00;

/// Флаги потока: кластеры размещены подряд (без FAT-цепочки).
const STREAM_FLAG_CONTIGUOUS: u8 = 0x02;

/// Значения FAT, начиная с которых цепочка считается законченной
/// (0xFFFFFFF7 — сбойный кластер, 0xFFFFFFF8.. — конец цепочки).
const FAT_END_OF_CHAIN: u32 = 0xFFFF_FFF7;

const MAX_CLUSTER_SHIFT: u8 = 25;

/// Ошибки устройства чтения секторов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    Timeout,
    NoDevice,
    Device,
    InvalidLba,
}

/// Источник секторов для драйвера файловой системы.
pub trait BlockReader {
    fn read_sector(
        &self,
        lba: u32,
        buffer: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), BlockError>;
}

/// Устройство, поддерживающее запись секторов.
pub trait BlockDevice: BlockReader {
    fn write_sector(
        &self,
        lba: u32,
        buffer: &[u8; SECTOR_SIZE],
    ) -> Result<(), BlockError>;

    /// Сбрасывает кэш устройства на носитель.
    fn flush(&self) -> Result<(), BlockError>;
}

/// Ошибки разбора и чтения exFAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Io(BlockError),
    NotExfat,
    UnsupportedSectorSize,
    BadGeometry,
    ClusterOutOfRange,
    FatOutOfRange,
    PathNotFound,
    NotAFile,
    NotADirectory,
    TruncatedChain,
    NoSpace,
    InvalidName,
}

impl From<BlockError> for FsError {
    fn from(error: BlockError) -> FsError {
        FsError::Io(error)
    }
}

impl FsError {
    /// Короткое текстовое описание для вывода в консоль.
    pub fn message(self) -> &'static [u8] {
        match self {
            FsError::Io(BlockError::Timeout) => b"ATA timeout",
            FsError::Io(BlockError::NoDevice) => b"no ATA device",
            FsError::Io(BlockError::Device) => b"ATA device error",
            FsError::Io(BlockError::InvalidLba) => b"invalid LBA",
            FsError::NotExfat => b"not an exFAT volume",
            FsError::UnsupportedSectorSize => b"unsupported sector size",
            FsError::BadGeometry => b"corrupted exFAT geometry",
            FsError::ClusterOutOfRange => b"cluster out of range",
            FsError::FatOutOfRange => b"FAT entry out of range",
            FsError::PathNotFound => b"path not found",
            FsError::NotAFile => b"not a file",
            FsError::NotADirectory => b"not a directory",
            FsError::TruncatedChain => b"broken cluster chain",
            FsError::NoSpace => b"no free space",
            FsError::InvalidName => b"invalid file name",
        }
    }
}

pub fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

pub fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

pub fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

/// Проверяет, что сектор является загрузочным сектором exFAT.
pub fn is_exfat_boot(sector: &[u8; SECTOR_SIZE]) -> bool {
    &sector[3..11] == b"EXFAT   " && read_u16(sector, 510) == 0xAA55
}

/// Переводит символ UTF-16 в код CP866 для вывода в VGA text mode.
///
/// Поддерживаются ASCII, русские буквы А-я, Ё/ё и знак номера.
/// Остальные символы вернут `None` — вызывающий код покажет `?`.
pub fn utf16_to_cp866(code: u16) -> Option<u8> {
    match code {
        0x0020..=0x007E => Some(code as u8),
        0x0410..=0x041F => Some(0x80 + (code - 0x0410) as u8), // А..П
        0x0420..=0x042F => Some(0x90 + (code - 0x0420) as u8), // Р..Я
        0x0430..=0x043F => Some(0xA0 + (code - 0x0430) as u8), // а..п
        0x0440..=0x044F => Some(0xE0 + (code - 0x0440) as u8), // р..я
        0x0401 => Some(0xF0),                                  // Ё
        0x0451 => Some(0xF1),                                  // ё
        0x2116 => Some(0xFC),                                  // №
        _ => None,
    }
}

/// Приводит байт CP866/ASCII к нижнему регистру для сравнения имён.
pub fn fold_case(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte | 0x20,
        0x80..=0x8F => byte + 0x20, // А..П -> а..п
        0x90..=0x9F => byte + 0x50, // Р..Я -> р..я
        0xF0 => 0xF1,               // Ё -> ё
        _ => byte,
    }
}

/// Переводит байт CP866 в код UTF-16 (для имён файлов).
pub fn cp866_to_utf16(code: u8) -> Option<u16> {
    match code {
        0x00..=0x7E => Some(code as u16),
        0x80..=0x8F => Some(0x0410 + (code - 0x80) as u16), // А..П
        0x90..=0x9F => Some(0x0420 + (code - 0x90) as u16), // Р..Я
        0xA0..=0xAF => Some(0x0430 + (code - 0xA0) as u16), // а..п
        0xE0..=0xEF => Some(0x0440 + (code - 0xE0) as u16), // р..я
        0xF0 => Some(0x0401),                               // Ё
        0xF1 => Some(0x0451),                               // ё
        0xFC => Some(0x2116),                               // №
        _ => None,
    }
}

/// Верхний регистр по правилам Up-case Table для поддерживаемых символов.
///
/// Для ASCII и русской кириллицы эти отображения совпадают со стандартной
/// Up-case Table Microsoft, поэтому NameHash получается корректным.
fn upcase_for_hash(unit: u16) -> u16 {
    match unit {
        0x0061..=0x007A => unit - 0x20, // a-z -> A-Z
        0x0430..=0x044F => unit - 0x20, // а-я -> А-Я
        0x0451 => 0x0401,               // ё -> Ё
        _ => unit,
    }
}

/// Кодирует текст из CP866 в UTF-8. Возвращает длину результата.
///
/// Файлы пишутся в UTF-8, чтобы их корректно читали и `cat`, и Linux.
pub fn cp866_to_utf8(input: &[u8], out: &mut [u8]) -> usize {
    let mut written = 0usize;

    for &byte in input {
        let Some(code) = cp866_to_utf16(byte) else {
            continue;
        };

        let mut buffer = [0u8; 3];
        let len = if code < 0x80 {
            buffer[0] = code as u8;
            1
        } else if code < 0x800 {
            buffer[0] = 0xc0 | (code >> 6) as u8;
            buffer[1] = 0x80 | (code & 0x3f) as u8;
            2
        } else {
            buffer[0] = 0xe0 | (code >> 12) as u8;
            buffer[1] = 0x80 | ((code >> 6) & 0x3f) as u8;
            buffer[2] = 0x80 | (code & 0x3f) as u8;
            3
        };

        if written + len > out.len() {
            break;
        }

        out[written..written + len].copy_from_slice(&buffer[..len]);
        written += len;
    }

    written
}

/// Вычисляет NameHash набора записей (exFAT, побайтовое вращение).
pub fn name_hash(units: &[u16]) -> u16 {
    let mut hash = 0u16;

    for &unit in units {
        let up = upcase_for_hash(unit);
        hash = ((hash << 15) | (hash >> 1)).wrapping_add((up & 0xFF) as u16);
        hash = ((hash << 15) | (hash >> 1)).wrapping_add((up >> 8) as u16);
    }

    hash
}


/// Смонтированные параметры тома exFAT.
#[derive(Debug, Clone, Copy)]
pub struct Volume {
    /// LBA начала тома (учитывает MBR-разметку).
    pub base_lba: u32,
    /// Размер тома в секторах.
    pub volume_length: u64,
    /// LBA начала таблицы FAT.
    pub fat_lba: u32,
    /// Длина FAT в секторах.
    pub fat_length: u32,
    /// LBA начала области кластеров.
    pub cluster_heap_lba: u32,
    /// Количество кластеров.
    pub cluster_count: u32,
    /// Первый кластер корневого каталога.
    pub root_cluster: u32,
    /// Количество секторов в кластере.
    pub sectors_per_cluster: u32,
    /// Серийный номер тома.
    pub serial: u32,
    /// Версия файловой системы (старший байт — major, младший — minor).
    pub revision: u16,
    /// VolumeFlags из загрузочного сектора.
    pub flags: u16,
    /// Процент занятого места (0..=100 или 0xFF, если неизвестно).
    pub percent_in_use: u8,
    /// Совпала ли контрольная сумма загрузочного региона.
    pub boot_checksum_ok: bool,
}

impl Volume {
    /// Размер кластера в байтах.
    pub fn cluster_bytes(&self) -> u32 {
        self.sectors_per_cluster.saturating_mul(SECTOR_SIZE as u32)
    }

    /// Том помечен как «грязный» (был размонтирован некорректно).
    pub fn is_dirty(&self) -> bool {
        self.flags & 0x0002 != 0
    }

    /// Проверяет, что номер кластера лежит внутри области кластеров.
    pub fn is_valid_cluster(&self, cluster: u32) -> bool {
        cluster >= 2 && (cluster as u64) < self.cluster_count as u64 + 2
    }

    /// Переводит номер кластера в LBA его первого сектора.
    pub fn cluster_lba(&self, cluster: u32) -> Result<u32, FsError> {
        if !self.is_valid_cluster(cluster) {
            return Err(FsError::ClusterOutOfRange);
        }

        let offset = (cluster as u64 - 2) * self.sectors_per_cluster as u64;
        let lba = self.cluster_heap_lba as u64 + offset;

        if lba > 0x0FFF_FFFF {
            return Err(FsError::BadGeometry);
        }

        Ok(lba as u32)
    }

    /// Читает 4-байтовую запись FAT для кластера.
    pub fn fat_entry(
        &self,
        device: &dyn BlockReader,
        cluster: u32,
    ) -> Result<u32, FsError> {
        let byte_offset = cluster as u64 * 4;
        let sector_index = byte_offset / SECTOR_SIZE as u64;

        if sector_index >= self.fat_length as u64 {
            return Err(FsError::FatOutOfRange);
        }

        let lba = self.fat_lba as u64 + sector_index;

        if lba > 0x0FFF_FFFF {
            return Err(FsError::FatOutOfRange);
        }

        let mut sector = [0u8; SECTOR_SIZE];
        device.read_sector(lba as u32, &mut sector)?;

        Ok(read_u32(&sector, (byte_offset % SECTOR_SIZE as u64) as usize))
    }

    /// Возвращает следующий кластер цепочки или `None`, если цепочка кончилась.
    ///
    /// При `contiguous == true` (флаг NoFatChain) кластеры идут подряд,
    /// и таблица FAT не используется.
    pub fn next_cluster(
        &self,
        device: &dyn BlockReader,
        cluster: u32,
        contiguous: bool,
    ) -> Result<Option<u32>, FsError> {
        if contiguous {
            let next = cluster as u64 + 1;

            if next >= self.cluster_count as u64 + 2 {
                return Ok(None);
            }

            return Ok(Some(next as u32));
        }

        let next = self.fat_entry(device, cluster)?;

        if next >= FAT_END_OF_CHAIN {
            return Ok(None);
        }

        if !self.is_valid_cluster(next) {
            return Err(FsError::FatOutOfRange);
        }

        Ok(Some(next))
    }
}

/// Монтирует первый найденный том exFAT: либо диск целиком, либо первый
/// раздел MBR, содержащий exFAT.
pub fn mount(device: &dyn BlockReader) -> Result<Volume, FsError> {
    let base_lba = find_volume_base(device)?;
    let mut boot = [0u8; SECTOR_SIZE];
    device.read_sector(base_lba, &mut boot)?;
    Volume::parse(base_lba, &boot, device)
}

impl Volume {
    fn parse(
        base_lba: u32,
        boot: &[u8; SECTOR_SIZE],
        device: &dyn BlockReader,
    ) -> Result<Volume, FsError> {
        if !is_exfat_boot(boot) {
            return Err(FsError::NotExfat);
        }

        let sector_shift = boot[108];

        // Драйвер ATA читает по одному 512-байтовому сектору.
        if sector_shift != 9 {
            return Err(FsError::UnsupportedSectorSize);
        }

        let cluster_shift = boot[109];

        if cluster_shift > MAX_CLUSTER_SHIFT {
            return Err(FsError::BadGeometry);
        }

        let sectors_per_cluster = 1u32 << cluster_shift;
        let volume_length = read_u64(boot, 72);
        let fat_offset = read_u32(boot, 80);
        let fat_length = read_u32(boot, 84);
        let heap_offset = read_u32(boot, 88);
        let cluster_count = read_u32(boot, 92);
        let root_cluster = read_u32(boot, 96);
        let number_of_fats = boot[110];

        if number_of_fats == 0 || cluster_count == 0 {
            return Err(FsError::BadGeometry);
        }

        // Загрузочный регион занимает 24 сектора, FAT идёт после него,
        // а область кластеров — после FAT.
        if fat_offset < 24 || fat_length == 0 {
            return Err(FsError::BadGeometry);
        }

        let fat_end = fat_offset as u64 + fat_length as u64 * number_of_fats as u64;

        if (heap_offset as u64) < fat_end {
            return Err(FsError::BadGeometry);
        }

        let heap_end = heap_offset as u64
            + cluster_count as u64 * sectors_per_cluster as u64;

        if heap_end > volume_length {
            return Err(FsError::BadGeometry);
        }

        // Каждому кластеру соответствует 4-байтовая запись FAT.
        let fat_bytes = fat_length as u64 * SECTOR_SIZE as u64;

        if fat_bytes / 4 < cluster_count as u64 + 2 {
            return Err(FsError::BadGeometry);
        }

        if root_cluster < 2 || root_cluster as u64 >= cluster_count as u64 + 2 {
            return Err(FsError::BadGeometry);
        }

        let fat_lba = base_lba.checked_add(fat_offset).ok_or(FsError::BadGeometry)?;
        let cluster_heap_lba = base_lba
            .checked_add(heap_offset)
            .ok_or(FsError::BadGeometry)?;

        Ok(Volume {
            base_lba,
            volume_length,
            fat_lba,
            fat_length,
            cluster_heap_lba,
            cluster_count,
            root_cluster,
            sectors_per_cluster,
            serial: read_u32(boot, 100),
            revision: read_u16(boot, 104),
            flags: read_u16(boot, 106),
            percent_in_use: boot[112],
            boot_checksum_ok: verify_boot_checksum(base_lba, device),
        })
    }
}

/// Ищет начало тома: сначала проверяет сектор 0, затем таблицу разделов MBR.
fn find_volume_base(device: &dyn BlockReader) -> Result<u32, FsError> {
    let mut sector = [0u8; SECTOR_SIZE];
    device.read_sector(0, &mut sector)?;

    if is_exfat_boot(&sector) {
        return Ok(0);
    }

    if read_u16(&sector, 510) != 0xAA55 {
        return Err(FsError::NotExfat);
    }

    for index in 0..4 {
        let offset = 446 + index * 16;
        let status = sector[offset];

        if status != 0x00 && status != 0x80 {
            continue;
        }

        if sector[offset + 4] == 0 {
            continue;
        }

        let start = read_u32(&sector, offset + 8);

        if start == 0 {
            continue;
        }

        let mut probe = [0u8; SECTOR_SIZE];

        if device.read_sector(start, &mut probe).is_err() {
            continue;
        }

        if is_exfat_boot(&probe) {
            return Ok(start);
        }
    }

    Err(FsError::NotExfat)
}

/// Проверяет контрольную сумму загрузочного региона (11 секторов).
///
/// Байты VolumeFlags (106, 107) и PercentInUse (112) в расчёт не входят.
fn verify_boot_checksum(base_lba: u32, device: &dyn BlockReader) -> bool {
    let mut checksum = 0u32;
    let mut sector = [0u8; SECTOR_SIZE];

    for sector_index in 0..11u32 {
        let Some(lba) = base_lba.checked_add(sector_index) else {
            return false;
        };

        if device.read_sector(lba, &mut sector).is_err() {
            return false;
        }

        for (offset, &byte) in sector.iter().enumerate() {
            if sector_index == 0 && (offset == 106 || offset == 107 || offset == 112) {
                continue;
            }

            checksum = if checksum & 1 != 0 {
                (checksum >> 1) | 0x8000_0000
            } else {
                checksum >> 1
            };

            checksum = checksum.wrapping_add(byte as u32);
        }
    }

    let Some(lba) = base_lba.checked_add(11) else {
        return false;
    };

    if device.read_sector(lba, &mut sector).is_err() {
        return false;
    }

    checksum == read_u32(&sector, 0)
}

/// Запись каталога: файл или подкаталог.
#[derive(Clone, Copy)]
pub struct Entry {
    /// Имя в ASCII: символы вне печатного диапазона заменены на '?'.
    pub name: [u8; MAX_NAME_CHARS + 1],
    pub name_len: usize,
    pub attributes: u16,
    pub first_cluster: u32,
    pub data_length: u64,
    pub valid_data_length: u64,
    /// Поток размещён подряд (NoFatChain), FAT-цепочка не используется.
    pub contiguous: bool,
    /// Количество вторичных записей в наборе (для удаления).
    pub secondary_count: usize,
    /// Индекс основной записи набора в потоке каталога.
    pub entry_index: usize,
}

impl Entry {
    pub const fn empty() -> Entry {
        Entry {
            name: [0u8; MAX_NAME_CHARS + 1],
            name_len: 0,
            attributes: 0,
            first_cluster: 0,
            data_length: 0,
            valid_data_length: 0,
            contiguous: false,
            secondary_count: 0,
            entry_index: 0,
        }
    }

    /// Синтетическая запись для корневого каталога.
    pub fn root(volume: &Volume) -> Entry {
        let mut entry = Entry::empty();
        entry.attributes = ATTR_DIRECTORY;
        entry.first_cluster = volume.root_cluster;
        entry
    }

    pub fn is_directory(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }

    /// Длина данных, которые действительно записаны в файл.
    ///
    /// exFAT допускает preallocation: ValidDataLength может быть меньше
    /// DataLength, а нулевое значение означает «вся длина валидна».
    pub fn content_length(&self) -> u64 {
        if self.valid_data_length == 0 {
            self.data_length
        } else {
            self.valid_data_length.min(self.data_length)
        }
    }

    /// Сравнение имени без учёта регистра ASCII и кириллицы CP866.
    pub fn name_eq_ignore_case(&self, other: &[u8]) -> bool {
        if self.name_len != other.len() {
            return false;
        }

        for index in 0..self.name_len {
            if fold_case(self.name[index]) != fold_case(other[index]) {
                return false;
            }
        }

        true
    }
}

fn checksum_byte(checksum: u16, byte: u8) -> u16 {
    ((checksum << 15) | (checksum >> 1)).wrapping_add(byte as u16)
}

/// Последовательный обход каталога exFAT.
pub struct DirCursor<'a> {
    volume: &'a Volume,
    device: &'a dyn BlockReader,
    cluster: u32,
    contiguous: bool,
    sector_in_cluster: u32,
    sector: [u8; SECTOR_SIZE],
    entry_index: usize,
    sector_loaded: bool,
    cluster_steps: u32,
    finished: bool,
    /// Сколько 32-байтовых записей уже выдано (позиция в каталоге).
    records: usize,
}

impl<'a> DirCursor<'a> {
    /// Создаёт курсор по первому кластеру каталога.
    ///
    /// `contiguous` берётся из записи каталога (флаг NoFatChain);
    /// для корневого каталога всегда используется FAT-цепочка.
    pub fn new(
        volume: &'a Volume,
        device: &'a dyn BlockReader,
        first_cluster: u32,
        contiguous: bool,
    ) -> DirCursor<'a> {
        DirCursor {
            volume,
            device,
            cluster: first_cluster,
            contiguous,
            sector_in_cluster: 0,
            sector: [0u8; SECTOR_SIZE],
            entry_index: 0,
            sector_loaded: false,
            cluster_steps: 0,
            finished: false,
            records: 0,
        }
    }

    fn load_sector(&mut self) -> Result<(), FsError> {
        let base = self.volume.cluster_lba(self.cluster)?;
        let lba = base
            .checked_add(self.sector_in_cluster)
            .ok_or(FsError::BadGeometry)?;

        self.device.read_sector(lba, &mut self.sector)?;
        self.entry_index = 0;
        self.sector_loaded = true;

        Ok(())
    }

    /// Переходит к следующему сектору, при необходимости — к следующему
    /// кластеру цепочки. Возвращает `false`, если каталог закончился.
    fn advance_sector(&mut self) -> Result<bool, FsError> {
        if self.sector_in_cluster + 1 < self.volume.sectors_per_cluster {
            self.sector_in_cluster += 1;
            self.load_sector()?;
            return Ok(true);
        }

        if self.cluster_steps >= self.volume.cluster_count {
            self.finished = true;
            return Ok(false);
        }

        match self
            .volume
            .next_cluster(self.device, self.cluster, self.contiguous)?
        {
            Some(next) => {
                self.cluster = next;
                self.cluster_steps += 1;
                self.sector_in_cluster = 0;
                self.load_sector()?;
                Ok(true)
            }
            None => {
                self.finished = true;
                Ok(false)
            }
        }
    }

    fn next_raw(&mut self) -> Result<Option<[u8; ENTRY_SIZE]>, FsError> {
        if self.finished {
            return Ok(None);
        }

        if !self.sector_loaded {
            self.load_sector()?;
        }

        if self.entry_index >= ENTRIES_PER_SECTOR && !self.advance_sector()? {
            return Ok(None);
        }

        let offset = self.entry_index * ENTRY_SIZE;
        let mut entry = [0u8; ENTRY_SIZE];
        entry.copy_from_slice(&self.sector[offset..offset + ENTRY_SIZE]);
        self.entry_index += 1;
        self.records += 1;

        Ok(Some(entry))
    }

    /// Возвращает следующую «сырую» 32-байтовую запись каталога.
    pub fn next_record(&mut self) -> Result<Option<[u8; ENTRY_SIZE]>, FsError> {
        self.next_raw()
    }

    /// Возвращает следующую запись файла или каталога.
    ///
    /// Набор записей 0x85 + 0xC0 + 0xC1 читается целиком и проверяется
    /// по SetChecksum; повреждённые наборы пропускаются.
    pub fn next_entry(&mut self) -> Result<Option<Entry>, FsError> {
        loop {
            let Some(primary) = self.next_raw()? else {
                return Ok(None);
            };

            let primary_index = self.records - 1;

            let entry_type = primary[0];

            if entry_type == ENTRY_END {
                self.finished = true;
                return Ok(None);
            }

            // Удалённые записи (нет бита InUse) и служебные записи тома
            // (метка, битовая карта, up-case table) пропускаются.
            if entry_type & 0x80 == 0 || entry_type != ENTRY_FILE {
                continue;
            }

            let secondary_count = primary[1] as usize;

            // Максимум: 17 записей имён + stream + сама запись файла.
            if secondary_count == 0 || secondary_count > 18 {
                continue;
            }

            let mut checksum = 0u16;

            for (offset, &byte) in primary.iter().enumerate() {
                if offset == 2 || offset == 3 {
                    continue;
                }

                checksum = checksum_byte(checksum, byte);
            }

            let mut entry = Entry::empty();
            let mut stream_found = false;
            let mut name_target = 0usize;
            let mut complete = true;

            entry.attributes = read_u16(&primary, 4);
            entry.secondary_count = secondary_count;

            for _ in 0..secondary_count {
                let Some(secondary) = self.next_raw()? else {
                    complete = false;
                    break;
                };

                for &byte in secondary.iter() {
                    checksum = checksum_byte(checksum, byte);
                }

                match secondary[0] {
                    ENTRY_STREAM => {
                        stream_found = true;
                        entry.contiguous =
                            secondary[1] & STREAM_FLAG_CONTIGUOUS != 0;
                        name_target = secondary[3] as usize;
                        entry.valid_data_length = read_u64(&secondary, 8);
                        entry.first_cluster = read_u32(&secondary, 20);
                        entry.data_length = read_u64(&secondary, 24);
                    }
                    ENTRY_NAME => {
                        for index in 0..15 {
                            if entry.name_len >= MAX_NAME_CHARS {
                                break;
                            }

                            let code = read_u16(&secondary, 2 + index * 2);

                            entry.name[entry.name_len] =
                                utf16_to_cp866(code).unwrap_or(b'?');

                            entry.name_len += 1;
                        }
                    }
                    _ => {}
                }
            }

            if !complete {
                return Ok(None);
            }

            if checksum != read_u16(&primary, 2) {
                continue;
            }

            if !stream_found || entry.name_len == 0 {
                continue;
            }

            if name_target > 0 && entry.name_len > name_target {
                entry.name_len = name_target;
            }

            if entry.data_length > 0 && !self.volume.is_valid_cluster(entry.first_cluster) {
                continue;
            }

            entry.entry_index = primary_index;

            return Ok(Some(entry));
        }
    }
}

/// Ищет запись по пути вида `DIR/SUBDIR/FILE.TXT`.
///
/// Пустой путь и `/` обозначают корневой каталог. Регистр символов ASCII
/// не учитывается. Разделитель — только `/`.
pub fn resolve(
    device: &dyn BlockReader,
    volume: &Volume,
    path: &[u8],
) -> Result<Entry, FsError> {
    let mut current = Entry::root(volume);

    for component in path.split(|&byte| byte == b'/') {
        if component.is_empty() || (component.len() == 1 && component[0] == b'.') {
            continue;
        }

        if !current.is_directory() {
            return Err(FsError::NotADirectory);
        }

        let mut cursor =
            DirCursor::new(volume, device, current.first_cluster, current.contiguous);
        let mut found = None;

        while let Some(entry) = cursor.next_entry()? {
            if entry.name_eq_ignore_case(component) {
                found = Some(entry);
                break;
            }
        }

        current = found.ok_or(FsError::PathNotFound)?;
    }

    Ok(current)
}

/// Потоково читает содержимое файла и отдаёт его кусками в `sink`.
///
/// Куски кратны секторам (512 байт), кроме последнего. Если `sink`
/// вернул `false`, чтение прекращается — это позволяет оболочке
/// ограничить объём вывода.
pub fn read_file<F>(
    device: &dyn BlockReader,
    volume: &Volume,
    file: &Entry,
    mut sink: F,
) -> Result<(), FsError>
where
    F: FnMut(&[u8]) -> bool,
{
    if file.is_directory() {
        return Err(FsError::NotAFile);
    }

    let mut remaining = file.content_length();

    if remaining == 0 {
        return Ok(());
    }

    if !volume.is_valid_cluster(file.first_cluster) {
        return Err(FsError::ClusterOutOfRange);
    }

    let mut cluster = file.first_cluster;
    let mut cluster_steps = 0u32;
    let mut sector = [0u8; SECTOR_SIZE];

    while remaining > 0 {
        let base = volume.cluster_lba(cluster)?;

        for index in 0..volume.sectors_per_cluster {
            if remaining == 0 {
                break;
            }

            let lba = base.checked_add(index).ok_or(FsError::BadGeometry)?;
            device.read_sector(lba, &mut sector)?;

            let take = if remaining > SECTOR_SIZE as u64 {
                SECTOR_SIZE
            } else {
                remaining as usize
            };

            remaining -= take as u64;

            if !sink(&sector[..take]) {
                return Ok(());
            }
        }

        if remaining == 0 {
            break;
        }

        if cluster_steps >= volume.cluster_count {
            return Err(FsError::TruncatedChain);
        }

        match volume.next_cluster(device, cluster, file.contiguous)? {
            Some(next) => {
                cluster = next;
                cluster_steps += 1;
            }
            None => return Err(FsError::TruncatedChain),
        }
    }

    Ok(())
}

impl Volume {
    /// Устанавливает или снимает флаг VolumeDirty в основном и резервном
    /// загрузочных секторах. В контрольную сумму региона флаг не входит.
    pub fn set_dirty(
        &self,
        device: &dyn BlockDevice,
        dirty: bool,
    ) -> Result<(), FsError> {
        for offset in [0u32, 12u32] {
            let Some(lba) = self.base_lba.checked_add(offset) else {
                return Err(FsError::BadGeometry);
            };

            let mut boot = [0u8; SECTOR_SIZE];
            device.read_sector(lba, &mut boot)?;

            let mut flags = read_u16(&boot, 106);

            if dirty {
                flags |= 0x0002;
            } else {
                flags &= !0x0002;
            }

            boot[106..108].copy_from_slice(&flags.to_le_bytes());
            device.write_sector(lba, &boot)?;
        }

        Ok(())
    }
}

/// Расположение файла битовой карты распределения кластеров (запись 0x81).
#[derive(Clone, Copy)]
struct AllocationBitmap {
    first_cluster: u32,
    data_length: u64,
}

fn find_allocation_bitmap(
    device: &dyn BlockReader,
    volume: &Volume,
) -> Result<AllocationBitmap, FsError> {
    let mut cursor = DirCursor::new(volume, device, volume.root_cluster, false);

    while let Some(record) = cursor.next_record()? {
        if record[0] == ENTRY_END {
            break;
        }

        if record[0] == 0x81 {
            let first_cluster = read_u32(&record, 20);
            let data_length = read_u64(&record, 24);

            if !volume.is_valid_cluster(first_cluster) || data_length == 0 {
                return Err(FsError::BadGeometry);
            }

            return Ok(AllocationBitmap {
                first_cluster,
                data_length,
            });
        }
    }

    Err(FsError::BadGeometry)
}

/// Доступ к битовой карте распределения с кэшем одного сектора.
struct BitmapWindow<'a> {
    device: &'a dyn BlockDevice,
    volume: &'a Volume,
    bitmap: AllocationBitmap,
    cluster: u32,
    cluster_index: u32,
    sector: [u8; SECTOR_SIZE],
    lba: u32,
    valid: bool,
}

impl<'a> BitmapWindow<'a> {
    fn new(
        device: &'a dyn BlockDevice,
        volume: &'a Volume,
        bitmap: AllocationBitmap,
    ) -> BitmapWindow<'a> {
        BitmapWindow {
            device,
            volume,
            bitmap,
            cluster: bitmap.first_cluster,
            cluster_index: 0,
            sector: [0u8; SECTOR_SIZE],
            lba: 0,
            valid: false,
        }
    }

    fn load_sector(&mut self, sector_index: u64) -> Result<(), FsError> {
        let sectors_per_cluster = self.volume.sectors_per_cluster as u64;
        let target_cluster = (sector_index / sectors_per_cluster) as u32;
        let sector_in_cluster = (sector_index % sectors_per_cluster) as u32;

        if target_cluster < self.cluster_index {
            self.cluster = self.bitmap.first_cluster;
            self.cluster_index = 0;
        }

        while self.cluster_index < target_cluster {
            let Some(next) = self
                .volume
                .next_cluster(self.device, self.cluster, false)?
            else {
                return Err(FsError::TruncatedChain);
            };

            self.cluster = next;
            self.cluster_index += 1;
        }

        let lba = self
            .volume
            .cluster_lba(self.cluster)?
            .checked_add(sector_in_cluster)
            .ok_or(FsError::BadGeometry)?;

        if !self.valid || self.lba != lba {
            self.device.read_sector(lba, &mut self.sector)?;
            self.lba = lba;
            self.valid = true;
        }

        Ok(())
    }

    fn bit(&mut self, index: u32) -> Result<bool, FsError> {
        let byte_index = index as u64 / 8;

        if byte_index >= self.bitmap.data_length {
            return Err(FsError::BadGeometry);
        }

        self.load_sector(byte_index / SECTOR_SIZE as u64)?;

        let offset = (byte_index % SECTOR_SIZE as u64) as usize;

        Ok(self.sector[offset] & (1 << (index % 8)) != 0)
    }

    fn set_bit(&mut self, index: u32, used: bool) -> Result<(), FsError> {
        let byte_index = index as u64 / 8;

        if byte_index >= self.bitmap.data_length {
            return Err(FsError::BadGeometry);
        }

        self.load_sector(byte_index / SECTOR_SIZE as u64)?;

        let offset = (byte_index % SECTOR_SIZE as u64) as usize;
        let mask = 1u8 << (index % 8);

        if used {
            self.sector[offset] |= mask;
        } else {
            self.sector[offset] &= !mask;
        }

        self.device.write_sector(self.lba, &self.sector)?;

        Ok(())
    }
}

/// Ищет непрерывный свободный участок из `count` кластеров и помечает его.
fn allocate_contiguous(window: &mut BitmapWindow, count: u32) -> Result<u32, FsError> {
    let total = window.volume.cluster_count;
    let mut run_start = 0u32;
    let mut run_len = 0u32;

    for index in 0..total {
        if window.bit(index)? {
            run_len = 0;
            continue;
        }

        if run_len == 0 {
            run_start = index;
        }

        run_len += 1;

        if run_len == count {
            for bit in run_start..run_start + count {
                window.set_bit(bit, true)?;
            }

            return Ok(run_start + 2);
        }
    }

    Err(FsError::NoSpace)
}

/// Освобождает кластеры файла в битовой карте.
fn free_clusters(
    device: &dyn BlockDevice,
    volume: &Volume,
    window: &mut BitmapWindow,
    first_cluster: u32,
    data_length: u64,
    contiguous: bool,
) -> Result<(), FsError> {
    if data_length == 0 {
        return Ok(());
    }

    let cluster_bytes = volume.cluster_bytes() as u64;
    let count = (data_length + cluster_bytes - 1) / cluster_bytes;
    let mut cluster = first_cluster;

    for step in 0..count {
        if !volume.is_valid_cluster(cluster) {
            return Err(FsError::ClusterOutOfRange);
        }

        window.set_bit(cluster - 2, false)?;

        if step + 1 < count {
            cluster = if contiguous {
                cluster + 1
            } else {
                volume
                    .next_cluster(device, cluster, false)?
                    .ok_or(FsError::TruncatedChain)?
            };
        }
    }

    Ok(())
}

/// Преобразует имя в UTF-16 и проверяет допустимые символы.
fn encode_name(name: &[u8]) -> Result<([u16; MAX_NAME_CHARS], usize), FsError> {
    const FORBIDDEN: &[u8] = b"/\\:*?\"<>|";

    if name.is_empty() || name.len() > MAX_NAME_CHARS {
        return Err(FsError::InvalidName);
    }

    let mut units = [0u16; MAX_NAME_CHARS];

    for (index, &byte) in name.iter().enumerate() {
        if byte < 0x20 || byte == 0x7f || FORBIDDEN.contains(&byte) {
            return Err(FsError::InvalidName);
        }

        units[index] = cp866_to_utf16(byte).ok_or(FsError::InvalidName)?;
    }

    Ok((units, name.len()))
}

/// Максимальное количество 32-байтовых записей в наборе для одного файла.
const MAX_ENTRY_SET: usize = 20;

/// Собирает набор записей 0x85 + 0xC0 + 0xC1 с корректным SetChecksum.
fn build_entry_set(
    units: &[u16],
    first_cluster: u32,
    data_length: u64,
    timestamp: u32,
) -> Result<([[u8; ENTRY_SIZE]; MAX_ENTRY_SET], usize), FsError> {
    let name_entries = (units.len() + 14) / 15;
    let count = 2 + name_entries;

    if units.is_empty() || count > MAX_ENTRY_SET {
        return Err(FsError::InvalidName);
    }

    let mut set = [[0u8; ENTRY_SIZE]; MAX_ENTRY_SET];

    // Основная запись файла.
    set[0][0] = ENTRY_FILE;
    set[0][1] = (1 + name_entries) as u8;
    set[0][4..6].copy_from_slice(&0x0020u16.to_le_bytes()); // archive

    for offset in [8usize, 12, 16] {
        set[0][offset..offset + 4].copy_from_slice(&timestamp.to_le_bytes());
    }

    // Stream extension.
    set[1][0] = ENTRY_STREAM;
    set[1][1] = if data_length > 0 { 0x03 } else { 0x00 };
    set[1][3] = units.len() as u8;

    let hash = name_hash(units);
    set[1][4..6].copy_from_slice(&hash.to_le_bytes());
    set[1][8..16].copy_from_slice(&data_length.to_le_bytes());
    set[1][20..24].copy_from_slice(&first_cluster.to_le_bytes());
    set[1][24..32].copy_from_slice(&data_length.to_le_bytes());

    // File name entries.
    for (chunk_index, chunk) in units.chunks(15).enumerate() {
        let entry = &mut set[2 + chunk_index];
        entry[0] = ENTRY_NAME;

        for (index, unit) in chunk.iter().enumerate() {
            entry[2 + index * 2..4 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
    }

    // SetChecksum: байты 2-3 основной записи в расчёт не входят.
    let mut checksum = 0u16;

    for entry_index in 0..count {
        for (offset, &byte) in set[entry_index].iter().enumerate() {
            if entry_index == 0 && (offset == 2 || offset == 3) {
                continue;
            }

            checksum = checksum_byte(checksum, byte);
        }
    }

    set[0][2..4].copy_from_slice(&checksum.to_le_bytes());

    Ok((set, count))
}

/// Ищет свободные слоты в корневом каталоге.
///
/// Возвращает индекс первой 32-байтовой записи; набор может пересекать
/// границы секторов, поэтому поиск идёт по всему потоку каталога.
fn find_free_slots(
    device: &dyn BlockReader,
    volume: &Volume,
    count: usize,
) -> Result<usize, FsError> {
    let mut index = 0usize;
    let mut run = 0usize;
    let mut start = 0usize;
    let mut cluster = volume.root_cluster;
    let mut steps = 0u32;

    loop {
        let base = volume.cluster_lba(cluster)?;

        for sector_offset in 0..volume.sectors_per_cluster {
            let lba = base
                .checked_add(sector_offset)
                .ok_or(FsError::BadGeometry)?;
            let mut sector = [0u8; SECTOR_SIZE];
            device.read_sector(lba, &mut sector)?;

            for slot in 0..ENTRIES_PER_SECTOR {
                let entry_type = sector[slot * ENTRY_SIZE];
                let free = entry_type == ENTRY_END || entry_type & 0x80 == 0;

                if free {
                    if run == 0 {
                        start = index;
                    }

                    run += 1;

                    if run == count {
                        return Ok(start);
                    }
                } else {
                    run = 0;
                }

                index += 1;
            }
        }

        if steps >= volume.cluster_count {
            break;
        }

        match volume.next_cluster(device, cluster, false)? {
            Some(next) => {
                cluster = next;
                steps += 1;
            }
            None => break,
        }
    }

    Err(FsError::NoSpace)
}

/// Записывает набор записей, начиная с указанного индекса каталога.
fn write_entry_set(
    device: &dyn BlockDevice,
    volume: &Volume,
    start_index: usize,
    entries: &[[u8; ENTRY_SIZE]],
) -> Result<(), FsError> {
    let entries_per_cluster = volume.sectors_per_cluster as usize * ENTRIES_PER_SECTOR;
    let mut cluster = volume.root_cluster;
    let mut cluster_index = 0usize;
    let mut sector = [0u8; SECTOR_SIZE];
    let mut loaded_lba: Option<u32> = None;

    for (offset_in_set, entry) in entries.iter().enumerate() {
        let index = start_index + offset_in_set;
        let target_cluster = index / entries_per_cluster;
        let within = index % entries_per_cluster;
        let sector_offset = within / ENTRIES_PER_SECTOR;
        let slot = within % ENTRIES_PER_SECTOR;

        while cluster_index < target_cluster {
            let Some(next) = volume.next_cluster(device, cluster, false)? else {
                return Err(FsError::BadGeometry);
            };

            cluster = next;
            cluster_index += 1;
        }

        let lba = volume.cluster_lba(cluster)?
            .checked_add(sector_offset as u32)
            .ok_or(FsError::BadGeometry)?;

        if loaded_lba != Some(lba) {
            if let Some(previous) = loaded_lba {
                device.write_sector(previous, &sector)?;
            }

            device.read_sector(lba, &mut sector)?;
            loaded_lba = Some(lba);
        }

        let start = slot * ENTRY_SIZE;
        sector[start..start + ENTRY_SIZE].copy_from_slice(entry);
    }

    if let Some(lba) = loaded_lba {
        device.write_sector(lba, &sector)?;
    }

    Ok(())
}

/// Записывает данные в непрерывно выделенные кластеры.
fn write_data(
    device: &dyn BlockDevice,
    volume: &Volume,
    first_cluster: u32,
    data: &[u8],
) -> Result<(), FsError> {
    let mut cluster = first_cluster;
    let mut written = 0usize;

    loop {
        let base = volume.cluster_lba(cluster)?;

        for sector_offset in 0..volume.sectors_per_cluster {
            let mut sector = [0u8; SECTOR_SIZE];

            if written < data.len() {
                let end = (written + SECTOR_SIZE).min(data.len());
                sector[..end - written].copy_from_slice(&data[written..end]);
                written = end;
            }

            let lba = base
                .checked_add(sector_offset)
                .ok_or(FsError::BadGeometry)?;
            device.write_sector(lba, &sector)?;
        }

        if written >= data.len() {
            break;
        }

        cluster = cluster.checked_add(1).ok_or(FsError::BadGeometry)?;
    }

    Ok(())
}

/// Ищет запись файла в корневом каталоге.
fn find_entry(
    device: &dyn BlockReader,
    volume: &Volume,
    name: &[u8],
) -> Result<Option<(Entry, usize)>, FsError> {
    let mut cursor = DirCursor::new(volume, device, volume.root_cluster, false);

    loop {
        let Some(entry) = cursor.next_entry()? else {
            return Ok(None);
        };

        if entry.name_eq_ignore_case(name) {
            return Ok(Some((entry, entry.entry_index)));
        }
    }
}

/// Помечает набор записей как удалённый (сбрасывает бит InUse).
fn delete_entry_set(
    device: &dyn BlockDevice,
    volume: &Volume,
    start: usize,
    count: usize,
) -> Result<(), FsError> {
    let end = start + count;
    let mut index = 0usize;
    let mut cluster = volume.root_cluster;
    let mut steps = 0u32;

    loop {
        let base = volume.cluster_lba(cluster)?;

        for sector_offset in 0..volume.sectors_per_cluster {
            let lba = base
                .checked_add(sector_offset)
                .ok_or(FsError::BadGeometry)?;
            let mut sector = [0u8; SECTOR_SIZE];
            device.read_sector(lba, &mut sector)?;

            let mut changed = false;

            for slot in 0..ENTRIES_PER_SECTOR {
                let offset = slot * ENTRY_SIZE;

                if index >= start && index < end && sector[offset] & 0x80 != 0 {
                    sector[offset] &= 0x7f;
                    changed = true;
                }

                index += 1;
            }

            if changed {
                device.write_sector(lba, &sector)?;
            }
        }

        if steps >= volume.cluster_count {
            break;
        }

        match volume.next_cluster(device, cluster, false)? {
            Some(next) => {
                cluster = next;
                steps += 1;
            }
            None => break,
        }
    }

    Ok(())
}

/// Создаёт файл в корневом каталоге или перезаписывает существующий.
///
/// Кластеры выделяются непрерывно (NoFatChain), битовая карта
/// распределения обновляется. `timestamp` — упакованные дата и время
/// exFAT (см. `rtc::timestamp`).
pub fn write_file(
    device: &dyn BlockDevice,
    volume: &Volume,
    name: &[u8],
    data: &[u8],
    timestamp: u32,
) -> Result<(), FsError> {
    let reader: &dyn BlockReader = device;
    let (units, units_len) = encode_name(name)?;
    let bitmap = find_allocation_bitmap(reader, volume)?;

    volume.set_dirty(device, true)?;

    let result = write_file_inner(
        device,
        volume,
        bitmap,
        name,
        &units[..units_len],
        data,
        timestamp,
    );

    if result.is_err() {
        // Том изменён не до конца — оставляем VolumeDirty установленным.
        let _ = device.flush();
        return result;
    }

    volume.set_dirty(device, false)?;
    device.flush()?;

    Ok(())
}

fn write_file_inner(
    device: &dyn BlockDevice,
    volume: &Volume,
    bitmap: AllocationBitmap,
    name: &[u8],
    units: &[u16],
    data: &[u8],
    timestamp: u32,
) -> Result<(), FsError> {
    let reader: &dyn BlockReader = device;
    let mut window = BitmapWindow::new(device, volume, bitmap);

    if let Some((entry, index)) = find_entry(reader, volume, name)? {
        free_clusters(
            device,
            volume,
            &mut window,
            entry.first_cluster,
            entry.data_length,
            entry.contiguous,
        )?;

        delete_entry_set(device, volume, index, 1 + entry.secondary_count)?;
    }

    let first_cluster = if data.is_empty() {
        0
    } else {
        let cluster_bytes = volume.cluster_bytes() as u64;
        let count = ((data.len() as u64 + cluster_bytes - 1) / cluster_bytes) as u32;
        let first = allocate_contiguous(&mut window, count)?;
        write_data(device, volume, first, data)?;
        first
    };

    let (set, count) = build_entry_set(units, first_cluster, data.len() as u64, timestamp)?;
    let start = find_free_slots(reader, volume, count)?;

    write_entry_set(device, volume, start, &set[..count])?;

    Ok(())
}

/// Удаляет файл из корневого каталога и освобождает его кластеры.
pub fn remove_file(
    device: &dyn BlockDevice,
    volume: &Volume,
    name: &[u8],
) -> Result<(), FsError> {
    let reader: &dyn BlockReader = device;

    let Some((entry, index)) = find_entry(reader, volume, name)? else {
        return Err(FsError::PathNotFound);
    };

    let bitmap = find_allocation_bitmap(reader, volume)?;

    volume.set_dirty(device, true)?;

    let mut window = BitmapWindow::new(device, volume, bitmap);

    let result = free_clusters(
        device,
        volume,
        &mut window,
        entry.first_cluster,
        entry.data_length,
        entry.contiguous,
    )
    .and_then(|()| delete_entry_set(device, volume, index, 1 + entry.secondary_count));

    if result.is_err() {
        let _ = device.flush();
        return result;
    }

    volume.set_dirty(device, false)?;
    device.flush()?;

    Ok(())
}


