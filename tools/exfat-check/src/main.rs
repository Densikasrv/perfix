//! Хост-проверка драйвера exFAT из ядра Perfix.
//!
//! Тесты работают с тем же файлом `kernel/src/exfat/fs.rs`, который
//! компилируется в ядро: модуль подключается по относительному пути.

#[path = "../../../kernel/src/exfat/fs.rs"]
mod fs;

#[path = "../../../kernel/src/font_cp866.rs"]
mod font;

#[path = "../../../kernel/src/keyboard.rs"]
mod keyboard;

#[path = "../../../kernel/src/utf8.rs"]
mod utf8;

#[path = "../../../kernel/src/rtc.rs"]
mod rtc;

use fs::{
    fold_case, mount, read_file, resolve, utf16_to_cp866, BlockError, BlockReader,
    DirCursor, FsError, Volume, SECTOR_SIZE,
};

const SEC: usize = SECTOR_SIZE;
const CLUSTER_SHIFT: u8 = 1;
const SECTORS_PER_CLUSTER: u32 = 1 << CLUSTER_SHIFT;
/// Кластер, в котором лежит тестовая битовая карта распределения.
const BITMAP_CLUSTER: u32 = 8;
/// LBA первого сектора битовой карты (heap_offset = 25).
const BITMAP_LBA: usize = 25 + (BITMAP_CLUSTER as usize - 2) * SECTORS_PER_CLUSTER as usize;

fn main() {}

struct MemoryDisk {
    data: std::cell::RefCell<Vec<u8>>,
}

impl MemoryDisk {
    fn new(data: Vec<u8>) -> MemoryDisk {
        MemoryDisk {
            data: std::cell::RefCell::new(data),
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.data.borrow().clone()
    }
}

impl BlockReader for MemoryDisk {
    fn read_sector(&self, lba: u32, buffer: &mut [u8; SEC]) -> Result<(), BlockError> {
        let data = self.data.borrow();
        let start = lba as usize * SEC;
        let end = start + SEC;

        if end > data.len() {
            return Err(BlockError::InvalidLba);
        }

        buffer.copy_from_slice(&data[start..end]);
        Ok(())
    }
}

impl fs::BlockDevice for MemoryDisk {
    fn write_sector(&self, lba: u32, buffer: &[u8; SEC]) -> Result<(), BlockError> {
        let mut data = self.data.borrow_mut();
        let start = lba as usize * SEC;
        let end = start + SEC;

        if end > data.len() {
            return Err(BlockError::InvalidLba);
        }

        data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn flush(&self) -> Result<(), BlockError> {
        Ok(())
    }
}

/// Синтетический том exFAT: 8 кластеров по 2 сектора.
///
/// Раскладка кластеров:
/// 2 — корневой каталог, 3 — подкаталог SUBDIR,
/// 4 -> 5 — файл readme.txt (1200 байт через FAT-цепочку),
/// 6 — файл hello.txt (12 байт, NoFatChain).
struct Image {
    fat_offset: u32,
    fat_length: u32,
    heap_offset: u32,
    cluster_count: u32,
    root_cluster: u32,
    data: Vec<u8>,
}

impl Image {
    fn new() -> Image {
        let fat_offset = 24u32;
        let fat_length = 1u32;
        let heap_offset = 25u32;
        let cluster_count = 12u32;
        let volume_length = heap_offset as u64
            + cluster_count as u64 * SECTORS_PER_CLUSTER as u64;

        let mut image = Image {
            fat_offset,
            fat_length,
            heap_offset,
            cluster_count,
            root_cluster: 2,
            data: vec![0u8; volume_length as usize * SEC],
        };

        image.write_boot_region(volume_length);
        image.write_fat();
        image.write_root();
        image.write_subdir();
        image.write_file_data();
        image.write_checksum_sector();
        image
    }

    fn cluster_offset(&self, cluster: u32) -> usize {
        (self.heap_offset + (cluster - 2) * SECTORS_PER_CLUSTER) as usize * SEC
    }

    fn write_cluster(&mut self, cluster: u32, bytes: &[u8]) {
        let offset = self.cluster_offset(cluster);
        self.data[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn write_boot_region(&mut self, volume_length: u64) {
        let mut boot = [0u8; SEC];
        boot[0] = 0xEB;
        boot[1] = 0x76;
        boot[2] = 0x90;
        boot[3..11].copy_from_slice(b"EXFAT   ");
        boot[72..80].copy_from_slice(&volume_length.to_le_bytes());
        boot[80..84].copy_from_slice(&self.fat_offset.to_le_bytes());
        boot[84..88].copy_from_slice(&self.fat_length.to_le_bytes());
        boot[88..92].copy_from_slice(&self.heap_offset.to_le_bytes());
        boot[92..96].copy_from_slice(&self.cluster_count.to_le_bytes());
        boot[96..100].copy_from_slice(&self.root_cluster.to_le_bytes());
        boot[100..104].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        boot[104..106].copy_from_slice(&0x0100u16.to_le_bytes());
        boot[108] = 9;
        boot[109] = CLUSTER_SHIFT;
        boot[110] = 1;
        boot[111] = 0x80;
        boot[112] = 25;
        boot[510] = 0x55;
        boot[511] = 0xAA;

        self.data[..SEC].copy_from_slice(&boot);
    }

    fn write_fat(&mut self) {
        let entries: [u32; 7] = [
            0xFFFF_FFF8,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            5,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
        ];

        let offset = self.fat_offset as usize * SEC;

        for (index, value) in entries.iter().enumerate() {
            let at = offset + index * 4;
            self.data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    fn write_root(&mut self) {
        let mut buffer = vec![0u8; SECTORS_PER_CLUSTER as usize * SEC];
        let mut offset = 0usize;

        // Метка тома: служебная запись, драйвер обязан её пропускать.
        let mut label = [0u8; 32];
        label[0] = 0x83;
        label[1] = 4;

        for (index, unit) in "TEST".encode_utf16().enumerate() {
            label[2 + index * 2..4 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }

        buffer[offset..offset + 32].copy_from_slice(&label);
        offset += 32;

        // Битовая карта распределения кластеров (запись 0x81).
        let mut bitmap = [0u8; 32];
        bitmap[0] = 0x81;
        bitmap[20..24].copy_from_slice(&BITMAP_CLUSTER.to_le_bytes());
        bitmap[24..32].copy_from_slice(&2u64.to_le_bytes());
        buffer[offset..offset + 32].copy_from_slice(&bitmap);
        offset += 32;

        // Файл из двух кластеров, связанных через FAT.
        offset = write_set(&mut buffer, offset, 0x20, b"readme.txt", 4, 1200, false);

        // Набор записей с испорченной контрольной суммой — игнорируется.
        let mut broken = build_set(0x20, b"broken.txt", 2, 0, false);
        broken[2][4] ^= 0x20;

        for entry in &broken {
            buffer[offset..offset + 32].copy_from_slice(entry);
            offset += 32;
        }

        // Подкаталог с непрерывным размещением кластеров.
        offset = write_set(&mut buffer, offset, 0x10, b"SUBDIR", 3, 0, true);

        // Файл с кириллическим именем: в каталоге имя хранится в UTF-16.
        let cyrillic: Vec<u16> = "привет.txt".encode_utf16().collect();
        write_set_wide(&mut buffer, offset, 0x20, &cyrillic, 7, 3, true);

        self.write_cluster(self.root_cluster, &buffer);
    }

    fn write_subdir(&mut self) {
        let mut buffer = vec![0u8; SECTORS_PER_CLUSTER as usize * SEC];
        let mut offset = 0usize;

        // 15 неактивных записей: набор записей hello.txt начнётся
        // в последней записи сектора и продолжится в следующем секторе.
        for _ in 0..15 {
            let mut filler = [0u8; 32];
            filler[0] = 0x05;
            buffer[offset..offset + 32].copy_from_slice(&filler);
            offset += 32;
        }

        write_set(&mut buffer, offset, 0x20, b"hello.txt", 6, 12, true);

        self.write_cluster(3, &buffer);
    }

    fn write_file_data(&mut self) {
        let pattern: Vec<u8> = (0..1200u32)
            .map(|index| b'a' + (index % 26) as u8)
            .collect();

        self.write_cluster(4, &pattern[..1024]);
        self.write_cluster(5, &pattern[1024..]);
        self.write_cluster(6, b"hello world\n");
        // Содержимое привет.txt в CP866: "да\n".
        self.write_cluster(7, &[0xA4, 0xA0, b'\n']);
        // Битовая карта: заняты кластеры 2..8 (root, subdir, data, bitmap).
        self.write_cluster(BITMAP_CLUSTER, &[0x7F, 0x00]);
    }

    fn write_checksum_sector(&mut self) {
        let mut checksum = 0u32;

        for sector_index in 0..11usize {
            for offset in 0..SEC {
                if sector_index == 0 && (offset == 106 || offset == 107 || offset == 112) {
                    continue;
                }

                let byte = self.data[sector_index * SEC + offset];
                checksum = if checksum & 1 != 0 {
                    (checksum >> 1) | 0x8000_0000
                } else {
                    checksum >> 1
                };

                checksum = checksum.wrapping_add(byte as u32);
            }
        }

        let at = 11 * SEC;
        self.data[at..at + 4].copy_from_slice(&checksum.to_le_bytes());
    }
}

fn write_set(
    buffer: &mut [u8],
    mut offset: usize,
    attributes: u16,
    name: &[u8],
    first_cluster: u32,
    data_length: u64,
    contiguous: bool,
) -> usize {
    for entry in build_set(attributes, name, first_cluster, data_length, contiguous) {
        buffer[offset..offset + 32].copy_from_slice(&entry);
        offset += 32;
    }

    offset
}

fn write_set_wide(
    buffer: &mut [u8],
    mut offset: usize,
    attributes: u16,
    units: &[u16],
    first_cluster: u32,
    data_length: u64,
    contiguous: bool,
) -> usize {
    for entry in build_set_wide(attributes, units, first_cluster, data_length, contiguous) {
        buffer[offset..offset + 32].copy_from_slice(&entry);
        offset += 32;
    }

    offset
}

/// Собирает набор записей 0x85 + 0xC0 + 0xC1 с корректным SetChecksum.
fn build_set(
    attributes: u16,
    name: &[u8],
    first_cluster: u32,
    data_length: u64,
    contiguous: bool,
) -> Vec<[u8; 32]> {
    let units: Vec<u16> = name.iter().map(|&byte| byte as u16).collect();
    build_set_wide(attributes, &units, first_cluster, data_length, contiguous)
}

/// То же, но имя задано символами UTF-16 (для кириллицы).
fn build_set_wide(
    attributes: u16,
    units: &[u16],
    first_cluster: u32,
    data_length: u64,
    contiguous: bool,
) -> Vec<[u8; 32]> {
    let name_entries = units.len().div_ceil(15);
    let mut entries: Vec<[u8; 32]> = Vec::new();

    let mut primary = [0u8; 32];
    primary[0] = 0x85;
    primary[1] = (1 + name_entries) as u8;
    primary[4..6].copy_from_slice(&attributes.to_le_bytes());
    entries.push(primary);

    let mut stream = [0u8; 32];
    stream[0] = 0xC0;
    stream[1] = if contiguous { 0x03 } else { 0x01 };
    stream[3] = units.len() as u8;
    stream[8..16].copy_from_slice(&data_length.to_le_bytes());
    stream[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    stream[24..32].copy_from_slice(&data_length.to_le_bytes());
    entries.push(stream);

    for chunk in units.chunks(15) {
        let mut entry = [0u8; 32];
        entry[0] = 0xC1;
        entry[1] = 0x01;

        for (index, unit) in chunk.iter().enumerate() {
            entry[2 + index * 2..4 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }

        entries.push(entry);
    }

    let mut checksum = 0u16;

    for (entry_index, entry) in entries.iter().enumerate() {
        for (offset, &byte) in entry.iter().enumerate() {
            if entry_index == 0 && (offset == 2 || offset == 3) {
                continue;
            }

            checksum = ((checksum << 15) | (checksum >> 1)).wrapping_add(byte as u16);
        }
    }

    entries[0][2..4].copy_from_slice(&checksum.to_le_bytes());
    entries
}

fn mounted() -> (MemoryDisk, Volume) {
    let image = Image::new();
    let disk = MemoryDisk::new(image.data);
    let volume = mount(&disk).expect("mount failed");
    (disk, volume)
}

fn collect_file(disk: &MemoryDisk, volume: &Volume, path: &[u8]) -> Vec<u8> {
    let entry = resolve(disk, volume, path).expect("file not found");
    let mut data = Vec::new();

    read_file(disk, volume, &entry, |chunk| {
        data.extend_from_slice(chunk);
        true
    })
    .expect("read failed");

    data
}

#[test]
fn mounts_and_validates_boot_region() {
    let (_, volume) = mounted();

    assert_eq!(volume.base_lba, 0);
    assert_eq!(volume.sectors_per_cluster, 2);
    assert_eq!(volume.cluster_count, 12);
    assert_eq!(volume.root_cluster, 2);
    assert_eq!(volume.cluster_bytes(), 1024);
    assert_eq!(volume.serial, 0x1234_5678);
    assert_eq!(volume.revision, 0x0100);
    assert_eq!(volume.percent_in_use, 25);
    assert!(volume.boot_checksum_ok);
    assert!(!volume.is_dirty());
}

#[test]
fn rejects_non_exfat_disk() {
    let disk = MemoryDisk::new(vec![0u8; 64 * SEC]);
    assert!(matches!(mount(&disk), Err(FsError::NotExfat)));
}

#[test]
fn rejects_bad_geometry() {
    // ������� ��������� ��������� �� FAT.
    let mut image = Image::new();
    image.data[88..92].copy_from_slice(&24u32.to_le_bytes());

    let disk = MemoryDisk::new(image.data);
    assert!(matches!(mount(&disk), Err(FsError::BadGeometry)));

    // �������� ������� ��� ������� ���������.
    let mut image = Image::new();
    image.data[96..100].copy_from_slice(&100u32.to_le_bytes());

    let disk = MemoryDisk::new(image.data);
    assert!(matches!(mount(&disk), Err(FsError::BadGeometry)));
}

#[test]
fn resolves_paths_case_insensitively() {
    let (disk, volume) = mounted();

    let root = resolve(&disk, &volume, b"/").expect("root");
    assert!(root.is_directory());
    assert_eq!(root.first_cluster, 2);

    let entry = resolve(&disk, &volume, b"/README.TXT").expect("readme");
    assert_eq!(entry.name(), b"readme.txt");
    assert_eq!(entry.data_length, 1200);
    assert_eq!(entry.first_cluster, 4);
    assert!(!entry.contiguous);
    assert!(!entry.is_directory());

    let subdir = resolve(&disk, &volume, b"subdir").expect("subdir");
    assert!(subdir.is_directory());
    assert_eq!(subdir.first_cluster, 3);
    assert!(subdir.contiguous);

    assert!(matches!(
        resolve(&disk, &volume, b"/missing.txt"),
        Err(FsError::PathNotFound)
    ));
    assert!(matches!(
        resolve(&disk, &volume, b"/readme.txt/child"),
        Err(FsError::NotADirectory)
    ));
}

#[test]
fn lists_root_and_skips_broken_entry_sets() {
    let (disk, volume) = mounted();
    let root = resolve(&disk, &volume, b"/").expect("root");

    let mut cursor = DirCursor::new(&volume, &disk, root.first_cluster, root.contiguous);
    let mut names: Vec<(Vec<u8>, bool)> = Vec::new();

    while let Some(entry) = cursor.next_entry().expect("entry") {
        names.push((entry.name().to_vec(), entry.is_directory()));
    }

    assert_eq!(names.len(), 3, "broken.txt должен быть отброшен: {names:?}");
    assert!(names.contains(&(b"readme.txt".to_vec(), false)));
    assert!(names.contains(&(b"SUBDIR".to_vec(), true)));
    assert!(names.contains(&(b"\xAF\xE0\xA8\xA2\xA5\xE2.txt".to_vec(), false)));
    assert!(!names.iter().any(|(name, _)| name == b"broken.txt"));
}

#[test]
fn resolves_cyrillic_names_in_cp866() {
    let (disk, volume) = mounted();

    // "привет.txt" в CP866.
    let path = b"/\xAF\xE0\xA8\xA2\xA5\xE2.txt";
    let entry = resolve(&disk, &volume, path).expect("cp866 path");

    assert_eq!(entry.name(), b"\xAF\xE0\xA8\xA2\xA5\xE2.txt");
    assert_eq!(entry.data_length, 3);
    assert_eq!(collect_file(&disk, &volume, path), b"\xA4\xA0\n");

    // Регистр кириллицы не важен: "ПРИВЕТ.txt" в CP866.
    let upper = resolve(&disk, &volume, b"/\x8F\x90\x88\x82\x85\x92.txt")
        .expect("uppercase cyrillic path");

    assert_eq!(upper.name(), b"\xAF\xE0\xA8\xA2\xA5\xE2.txt");
}

#[test]
fn maps_utf16_to_cp866_for_russian() {
    assert_eq!(utf16_to_cp866(0x0410), Some(0x80)); // А
    assert_eq!(utf16_to_cp866(0x044F), Some(0xEF)); // я
    assert_eq!(utf16_to_cp866(0x0401), Some(0xF0)); // Ё
    assert_eq!(utf16_to_cp866(0x0451), Some(0xF1)); // ё
    assert_eq!(utf16_to_cp866(0x2116), Some(0xFC)); // №
    assert_eq!(utf16_to_cp866(b'A' as u16), Some(b'A'));
    assert_eq!(utf16_to_cp866(0x20AC), None);
}

#[test]
fn folds_case_for_ascii_and_cyrillic() {
    assert_eq!(fold_case(b'Q'), b'q');
    assert_eq!(fold_case(0x80), 0xA0); // А -> а
    assert_eq!(fold_case(0x9F), 0xEF); // Я -> я
    assert_eq!(fold_case(0xF0), 0xF1); // Ё -> ё
    assert_eq!(fold_case(b'.'), b'.');
}

#[test]
fn russian_layout_matches_jcuken() {
    // Скан-код -> строчная буква CP866.
    let letters: [(u8, u8); 33] = [
        (0x10, 0xA9), // q -> й
        (0x11, 0xE6), // w -> ц
        (0x12, 0xE3), // e -> у
        (0x13, 0xAA), // r -> к
        (0x14, 0xA5), // t -> е
        (0x15, 0xAD), // y -> н
        (0x16, 0xA3), // u -> г
        (0x17, 0xE8), // i -> ш
        (0x18, 0xE9), // o -> щ
        (0x19, 0xA7), // p -> з
        (0x1A, 0xE5), // [ -> х
        (0x1B, 0xEA), // ] -> ъ
        (0x1E, 0xE4), // a -> ф
        (0x1F, 0xEB), // s -> ы
        (0x20, 0xA2), // d -> в
        (0x21, 0xA0), // f -> а
        (0x22, 0xAF), // g -> п
        (0x23, 0xE0), // h -> р
        (0x24, 0xAE), // j -> о
        (0x25, 0xAB), // k -> л
        (0x26, 0xA4), // l -> д
        (0x27, 0xA6), // ; -> ж
        (0x28, 0xED), // ' -> э
        (0x29, 0xF1), // ` -> ё
        (0x2C, 0xEF), // z -> я
        (0x2D, 0xE7), // x -> ч
        (0x2E, 0xE1), // c -> с
        (0x2F, 0xAC), // v -> м
        (0x30, 0xA8), // b -> и
        (0x31, 0xE2), // n -> т
        (0x32, 0xEC), // m -> ь
        (0x33, 0xA1), // , -> б
        (0x34, 0xEE), // . -> ю
    ];

    for &(sc, code) in &letters {
        assert_eq!(
            keyboard::russian_char(sc, false),
            Some(code),
            "строчная буква для скан-кода {sc:#04X}"
        );
    }

    // Shift даёт заглавные: Й, А, Я, Ё.
    assert_eq!(keyboard::russian_char(0x10, true), Some(0x89));
    assert_eq!(keyboard::russian_char(0x21, true), Some(0x80));
    assert_eq!(keyboard::russian_char(0x2C, true), Some(0x9F));
    assert_eq!(keyboard::russian_char(0x29, true), Some(0xF0));

    // Знаки препинания и цифры.
    assert_eq!(keyboard::russian_char(0x02, false), Some(b'1'));
    assert_eq!(keyboard::russian_char(0x04, true), Some(0xFC)); // №
    assert_eq!(keyboard::russian_char(0x07, true), Some(b':'));
    assert_eq!(keyboard::russian_char(0x08, true), Some(b'?'));
    assert_eq!(keyboard::russian_char(0x35, false), Some(b'.'));
    assert_eq!(keyboard::russian_char(0x35, true), Some(b','));
    assert_eq!(keyboard::russian_char(0x2B, false), Some(b'\\'));
    assert_eq!(keyboard::russian_char(0x39, false), Some(b' '));
    assert_eq!(keyboard::russian_char(0x1C, false), None); // Enter обрабатывается отдельно
    assert_eq!(keyboard::russian_char(0x3A, false), None); // Caps Lock не поддерживается
}

#[test]
fn english_layout_is_unchanged() {
    assert_eq!(keyboard::keyboard_char(0x21, false), Some(b'f'));
    assert_eq!(keyboard::keyboard_char(0x21, true), Some(b'F'));
    assert_eq!(keyboard::keyboard_char(0x34, false), Some(b'.'));
    assert_eq!(keyboard::keyboard_char(0x34, true), Some(b'>'));
    assert_eq!(keyboard::keyboard_char(0x35, false), Some(b'/'));
    assert_eq!(keyboard::keyboard_char(0x28, true), Some(b'"'));
    assert_eq!(keyboard::keyboard_char(0x0E, false), None);
}

#[test]
fn decodes_utf8_text_to_cp866() {
    let mut decoder = utf8::Decoder::new();
    let mut output = Vec::new();

    for &byte in "привет, Ёжик №5\n".as_bytes() {
        let mut decoded = [0u8; 4];
        let len = decoder.feed(byte, &mut decoded, utf16_to_cp866);
        output.extend_from_slice(&decoded[..len]);
    }

    assert_eq!(
        output,
        b"\xAF\xE0\xA8\xA2\xA5\xE2, \xF0\xA6\xA8\xAA \xFC5\n".to_vec()
    );
}

#[test]
fn decodes_utf8_split_across_chunks() {
    let mut decoder = utf8::Decoder::new();
    let mut output = Vec::new();

    // "я" = D1 8F; байты приходят по одному, как при чтении секторами.
    for &byte in "я".as_bytes() {
        let mut decoded = [0u8; 4];
        let len = decoder.feed(byte, &mut decoded, utf16_to_cp866);
        output.extend_from_slice(&decoded[..len]);
    }

    assert_eq!(output, b"\xEF".to_vec());
}

#[test]
fn keeps_ascii_and_bare_cp866_bytes() {
    let mut decoder = utf8::Decoder::new();
    let mut output = Vec::new();

    // 0xC2 — начало UTF-8, но следующий байт не продолжение,
    // значит оба байта выводятся как есть (CP866/ASCII).
    for &byte in &[b'O', b'K', 0xC2, b'X', 0xFF] {
        let mut decoded = [0u8; 4];
        let len = decoder.feed(byte, &mut decoded, utf16_to_cp866);
        output.extend_from_slice(&decoded[..len]);
    }

    assert_eq!(output, vec![b'O', b'K', 0xC2, b'X', 0xFF]);
}

#[test]
fn holds_incomplete_utf8_sequence() {
    let mut decoder = utf8::Decoder::new();
    let mut decoded = [0u8; 4];

    assert_eq!(decoder.feed(0xD0, &mut decoded, utf16_to_cp866), 0);
    assert_eq!(decoder.feed(0xBF, &mut decoded, utf16_to_cp866), 1);
    assert_eq!(decoded[0], 0xAF);
}

#[test]
fn font_table_covers_cp866_cyrillic() {
    let glyphs = &font::GLYPHS;

    assert_eq!(font::GLYPH_HEIGHT, 16);
    assert_eq!(glyphs.len(), 67);

    for code in 0x80..=0xAFu8 {
        assert!(
            glyphs.iter().any(|&(glyph_code, _)| glyph_code == code),
            "нет глифа для CP866 {code:#04X}"
        );
    }

    for code in 0xE0..=0xF1u8 {
        assert!(
            glyphs.iter().any(|&(glyph_code, _)| glyph_code == code),
            "нет глифа для CP866 {code:#04X}"
        );
    }

    assert!(glyphs.iter().any(|&(glyph_code, _)| glyph_code == 0xFC));

    for &(code, bitmap) in glyphs {
        assert!(
            bitmap.iter().any(|&row| row != 0),
            "пустой глиф для CP866 {code:#04X}"
        );
    }
}

#[test]
fn reads_file_through_fat_chain() {
    let (disk, volume) = mounted();
    let data = collect_file(&disk, &volume, b"/readme.txt");

    let expected: Vec<u8> = (0..1200u32)
        .map(|index| b'a' + (index % 26) as u8)
        .collect();

    assert_eq!(data.len(), 1200);
    assert_eq!(data, expected);
}

#[test]
fn reads_contiguous_file_whose_entry_set_crosses_sector_boundary() {
    let (disk, volume) = mounted();
    let entry = resolve(&disk, &volume, b"/SubDir/HELLO.TXT").expect("hello");

    assert!(entry.contiguous);
    assert_eq!(entry.first_cluster, 6);
    assert_eq!(
        collect_file(&disk, &volume, b"/SubDir/hello.txt"),
        b"hello world\n"
    );
}

#[test]
fn reads_file_with_valid_data_length() {
    let (disk, volume) = mounted();
    let mut entry = resolve(&disk, &volume, b"/readme.txt").expect("readme");

    // Preallocation: записано только 600 байт из 1200 выделенных.
    entry.valid_data_length = 600;

    let mut data = Vec::new();
    read_file(&disk, &volume, &entry, |chunk| {
        data.extend_from_slice(chunk);
        true
    })
    .expect("read");

    assert_eq!(data.len(), 600);
}

#[test]
fn finds_volume_inside_mbr_partition() {
    let image = Image::new();
    let start_lba = 2048usize;

    let mut data = vec![0u8; start_lba * SEC];
    data.extend_from_slice(&image.data);

    data[446 + 4] = 0x07;
    data[446 + 8..446 + 12].copy_from_slice(&(start_lba as u32).to_le_bytes());
    data[510] = 0x55;
    data[511] = 0xAA;

    let disk = MemoryDisk::new(data);
    let volume = mount(&disk).expect("mount");

    assert_eq!(volume.base_lba as usize, start_lba);
    assert_eq!(volume.fat_lba as usize, start_lba + 24);
    assert!(volume.boot_checksum_ok);
    assert_eq!(collect_file(&disk, &volume, b"/readme.txt").len(), 1200);
}

#[test]
fn reports_corrupted_boot_checksum() {
    let mut image = Image::new();

    // PartitionOffset входит в контрольную сумму загрузочного региона.
    image.data[64] ^= 0xFF;

    let disk = MemoryDisk::new(image.data);
    let volume = mount(&disk).expect("mount");

    assert!(!volume.boot_checksum_ok);
}

#[test]
fn detects_broken_cluster_chain() {
    let mut image = Image::new();
    let fat_offset = image.fat_offset as usize * SEC;
    let entry_at = fat_offset + 4 * 4;
    image.data[entry_at..entry_at + 4].copy_from_slice(&0u32.to_le_bytes());

    let disk = MemoryDisk::new(image.data);
    let volume = mount(&disk).expect("mount");
    let entry = resolve(&disk, &volume, b"/readme.txt").expect("readme");

    let mut data = Vec::new();
    let result = read_file(&disk, &volume, &entry, |chunk| {
        data.extend_from_slice(chunk);
        true
    });

    assert_eq!(result, Err(FsError::FatOutOfRange));
    assert_eq!(data.len(), 1024, "первый кластер должен быть прочитан");
}

#[test]
fn stops_reading_when_sink_returns_false() {
    let (disk, volume) = mounted();
    let entry = resolve(&disk, &volume, b"/readme.txt").expect("readme");

    let mut calls = 0usize;
    read_file(&disk, &volume, &entry, |_| {
        calls += 1;
        false
    })
    .expect("read");

    assert_eq!(calls, 1);
}

fn bitmap_byte(disk: &MemoryDisk, offset: usize) -> u8 {
    disk.snapshot()[BITMAP_LBA * SEC + offset]
}

#[test]
fn creates_file_and_reads_it_back() {
    let (disk, volume) = mounted();

    fs::write_file(&disk, &volume, b"new.txt", b"hello exfat\n", 0).expect("write");

    let reloaded = MemoryDisk::new(disk.snapshot());
    let volume2 = mount(&reloaded).expect("mount");

    assert!(volume2.boot_checksum_ok);
    assert!(!volume2.is_dirty());
    assert_eq!(
        collect_file(&reloaded, &volume2, b"/new.txt"),
        b"hello exfat\n"
    );

    let root = resolve(&reloaded, &volume2, b"/").expect("root");
    let mut cursor = DirCursor::new(&volume2, &reloaded, root.first_cluster, root.contiguous);
    let mut names = Vec::new();

    while let Some(entry) = cursor.next_entry().expect("entry") {
        names.push(entry.name().to_vec());
    }

    assert!(names.contains(&b"new.txt".to_vec()));
    assert_eq!(names.len(), 4, "лишние записи: {names:?}");
}

#[test]
fn overwrites_existing_file() {
    let (disk, volume) = mounted();
    let pattern: Vec<u8> = (0..1500u32)
        .map(|index| b'a' + (index % 26) as u8)
        .collect();

    fs::write_file(&disk, &volume, b"doc.txt", b"short", 0).expect("create");
    fs::write_file(&disk, &volume, b"doc.txt", &pattern, 0).expect("overwrite");

    let reloaded = MemoryDisk::new(disk.snapshot());
    let volume2 = mount(&reloaded).expect("mount");

    assert_eq!(collect_file(&reloaded, &volume2, b"/doc.txt"), pattern);

    let root = resolve(&reloaded, &volume2, b"/").expect("root");
    let mut cursor = DirCursor::new(&volume2, &reloaded, root.first_cluster, root.contiguous);
    let mut count = 0usize;

    while cursor.next_entry().expect("entry").is_some() {
        count += 1;
    }

    assert_eq!(count, 4, "после перезаписи должна остаться одна запись файла");
}

#[test]
fn removes_file_and_frees_clusters() {
    let (disk, volume) = mounted();

    assert_eq!(bitmap_byte(&disk, 0), 0x7F);

    fs::remove_file(&disk, &volume, b"readme.txt").expect("remove");

    // Кластеры 4 и 5 (биты 2 и 3) освобождены.
    assert_eq!(bitmap_byte(&disk, 0), 0x73);
    assert!(matches!(
        resolve(&disk, &volume, b"/readme.txt"),
        Err(FsError::PathNotFound)
    ));

    fs::write_file(&disk, &volume, b"again.txt", &[b'x'; 1200], 0).expect("write");
    assert_eq!(bitmap_byte(&disk, 0), 0x7F);

    let reloaded = MemoryDisk::new(disk.snapshot());
    let volume2 = mount(&reloaded).expect("mount");
    assert_eq!(collect_file(&reloaded, &volume2, b"/again.txt").len(), 1200);
}

#[test]
fn writes_cyrillic_file_name() {
    let (disk, volume) = mounted();

    // "тест.txt" в CP866.
    fs::write_file(&disk, &volume, b"\xE2\xA5\xE1\xE2.txt", b"data\n", 0).expect("write");

    let reloaded = MemoryDisk::new(disk.snapshot());
    let volume2 = mount(&reloaded).expect("mount");

    assert_eq!(
        collect_file(&reloaded, &volume2, b"/\xE2\xA5\xE1\xE2.txt"),
        b"data\n"
    );

    // Регистр кириллицы не важен: "ТЕСТ.TXT" в CP866.
    assert_eq!(
        collect_file(&reloaded, &volume2, b"/\x92\x85\x91\x92.TXT"),
        b"data\n"
    );
}

#[test]
fn creates_empty_file() {
    let (disk, volume) = mounted();

    fs::write_file(&disk, &volume, b"empty.txt", b"", 0).expect("write");

    let reloaded = MemoryDisk::new(disk.snapshot());
    let volume2 = mount(&reloaded).expect("mount");

    assert_eq!(
        collect_file(&reloaded, &volume2, b"/empty.txt"),
        Vec::<u8>::new()
    );
}

#[test]
fn rejects_invalid_names_and_reports_no_space() {
    let (disk, volume) = mounted();

    assert!(matches!(
        fs::write_file(&disk, &volume, b"a/b", b"x", 0),
        Err(FsError::InvalidName)
    ));
    assert!(matches!(
        fs::write_file(&disk, &volume, b"", b"x", 0),
        Err(FsError::InvalidName)
    ));

    // Свободно 5 кластеров по 1024 байта.
    let huge = vec![b'z'; 6 * 1024];

    assert!(matches!(
        fs::write_file(&disk, &volume, b"huge.bin", &huge, 0),
        Err(FsError::NoSpace)
    ));
}

#[test]
fn encodes_cp866_text_to_utf8() {
    let mut out = [0u8; 64];
    let len = fs::cp866_to_utf8(b"\xAF\xE0\xA8\xA2\xA5\xE2", &mut out);

    assert_eq!(&out[..len], "привет".as_bytes());
}

#[test]
fn roundtrips_cyrillic_text_through_disk() {
    let (disk, volume) = mounted();

    // Пользователь вводит текст в CP866, на диск он пишется в UTF-8.
    let text_cp866 = b"\xAF\xE0\xA8\xA2\xA5\xE2\n";
    let mut utf8 = [0u8; 64];
    let utf8_len = fs::cp866_to_utf8(text_cp866, &mut utf8);

    fs::write_file(&disk, &volume, b"ru.txt", &utf8[..utf8_len], 0).expect("write");

    let reloaded = MemoryDisk::new(disk.snapshot());
    let volume2 = mount(&reloaded).expect("mount");
    let stored = collect_file(&reloaded, &volume2, b"/ru.txt");

    assert_eq!(stored, "привет\n".as_bytes());

    // Обратное декодирование тем же путём, что делает cat.
    let mut decoder = utf8::Decoder::new();
    let mut decoded = Vec::new();

    for &byte in &stored {
        let mut out = [0u8; 4];
        let count = decoder.feed(byte, &mut out, utf16_to_cp866);
        decoded.extend_from_slice(&out[..count]);
    }

    assert_eq!(decoded, text_cp866);
}

#[test]
fn packs_exfat_timestamp() {
    assert_eq!(rtc::pack(2024, 1, 1, 0, 0, 0), 0x5821_0000);
    assert_eq!(rtc::pack(2024, 1, 1, 0, 0, 59), 0x5821_0000 | 29);
}


