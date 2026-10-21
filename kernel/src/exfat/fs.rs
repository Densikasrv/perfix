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

    /// Сравнение имени без учёта регистра ASCII (как это делает Windows).
    pub fn name_eq_ignore_ascii_case(&self, other: &[u8]) -> bool {
        if self.name_len != other.len() {
            return false;
        }

        for index in 0..self.name_len {
            if self.name[index].to_ascii_lowercase()
                != other[index].to_ascii_lowercase()
            {
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

        Ok(Some(entry))
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

                            // VGA text mode: печатаем только ASCII.
                            entry.name[entry.name_len] =
                                if (0x20..=0x7E).contains(&code) {
                                    code as u8
                                } else {
                                    b'?'
                                };

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
            if entry.name_eq_ignore_ascii_case(component) {
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


