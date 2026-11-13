//! Потоковый gzip/deflate для pfpak.
//!
//! Пакеты распространяются как `.tar.gz`, поэтому встроенного zlib нет.
//! Здесь реализован минимальный inflate по RFC 1951 и gzip-обёртка по
//! RFC 1952. Распакованные данные не накапливаются целиком: они сразу
//! передаются в [`Sink`] (извлекатель tar), а в памяти живёт только
//! скользящее окно 32 КиБ и небольшой выходной буфер.

use crate::arena;

/// Ошибка распаковки.
pub type Error = &'static [u8];

/// Приёмник распакованных байтов.
pub trait Sink {
    fn write(&mut self, data: &[u8]) -> Result<(), Error>;
}

const WINDOW_SIZE: usize = 32768;
const WINDOW_MASK: usize = WINDOW_SIZE - 1;
const OUT_SIZE: usize = 4096;
const MAX_BITS: usize = 15;
const MAX_LIT: usize = 288;
const MAX_DIST: usize = 32;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51,
    59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4,
    4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385,
    513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385,
    24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10,
    10, 11, 11, 12, 12, 13, 13,
];
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

struct Bits<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u32,
    count: u8,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffer: 0,
            count: 0,
        }
    }

    fn bits(&mut self, need: u8) -> Result<u32, Error> {
        let mut value = self.buffer;

        while self.count < need {
            if self.position >= self.data.len() {
                return Err(b"truncated deflate stream");
            }

            value |= (self.data[self.position] as u32) << self.count;
            self.position += 1;
            self.count += 8;
        }

        self.buffer = value >> need;
        self.count -= need;

        Ok(value & ((1u32 << need) - 1))
    }

    fn align_byte(&mut self) {
        self.buffer = 0;
        self.count = 0;
    }

    fn read_byte(&mut self) -> Result<u8, Error> {
        if self.position >= self.data.len() {
            return Err(b"truncated deflate stream");
        }

        let byte = self.data[self.position];
        self.position += 1;
        Ok(byte)
    }
}

struct Huffman {
    count: [u16; MAX_BITS + 1],
    symbol: [u16; MAX_LIT],
}

impl Huffman {
    fn build(lengths: &[u8], output: &mut Huffman) -> Result<(), Error> {
        output.count = [0; MAX_BITS + 1];

        for &length in lengths {
            if length as usize > MAX_BITS {
                return Err(b"invalid huffman code length");
            }

            output.count[length as usize] += 1;
        }

        if output.count[0] as usize == lengths.len() {
            return Ok(());
        }

        let mut left = 1i32;

        for length in 1..=MAX_BITS {
            left <<= 1;
            left -= output.count[length] as i32;

            if left < 0 {
                return Err(b"oversubscribed huffman tree");
            }
        }

        let mut offsets = [0u16; MAX_BITS + 2];

        for length in 1..=MAX_BITS {
            offsets[length + 1] = offsets[length] + output.count[length];
        }

        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let index = offsets[length as usize] as usize;
                if index >= output.symbol.len() {
                    return Err(b"too many huffman symbols");
                }

                output.symbol[index] = symbol as u16;
                offsets[length as usize] += 1;
            }
        }

        Ok(())
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, Error> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0usize;

        for length in 1..=MAX_BITS {
            code |= bits.bits(1)? as i32;
            let count = self.count[length] as i32;

            if code - first < count {
                let position = index + (code - first) as usize;
                if position >= MAX_LIT {
                    return Err(b"invalid huffman symbol");
                }

                return Ok(self.symbol[position]);
            }

            index += count as usize;
            first = (first + count) << 1;
            code <<= 1;
        }

        Err(b"invalid huffman code")
    }
}

struct Emitter<'a> {
    sink: &'a mut dyn Sink,
    output: [u8; OUT_SIZE],
    length: usize,
    window: &'a mut [u8],
    window_position: usize,
    total: u64,
    crc: u32,
}

impl<'a> Emitter<'a> {
    fn new(sink: &'a mut dyn Sink, window: &'a mut [u8]) -> Self {
        Self {
            sink,
            output: [0; OUT_SIZE],
            length: 0,
            window,
            window_position: 0,
            total: 0,
            crc: 0xFFFF_FFFF,
        }
    }

    fn flush(&mut self) -> Result<(), Error> {
        if self.length > 0 {
            self.sink.write(&self.output[..self.length])?;
            self.length = 0;
        }

        Ok(())
    }

    fn emit(&mut self, byte: u8) -> Result<(), Error> {
        self.crc = crc32_update(self.crc, &[byte]);
        self.total += 1;

        self.window[self.window_position & WINDOW_MASK] = byte;
        self.window_position = (self.window_position + 1) & WINDOW_MASK;
        self.output[self.length] = byte;
        self.length += 1;

        if self.length == OUT_SIZE {
            self.flush()?;
        }

        Ok(())
    }

    fn distance(&mut self, distance: usize, length: usize) -> Result<(), Error> {
        if distance == 0 || distance > WINDOW_SIZE || distance as u64 > self.total {
            return Err(b"invalid deflate distance");
        }

        for _ in 0..length {
            let index =
                (self.window_position + WINDOW_SIZE - distance) & WINDOW_MASK;
            // Читаем ровно один байт: emit ниже сам обновляет окно.
            let byte = self.window[index];
            self.emit(byte)?;
        }

        Ok(())
    }
}

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= byte as u32;

        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }

    crc
}

fn fixed_trees(
    literal: &mut Huffman,
    distance: &mut Huffman,
) -> Result<(), Error> {
    let mut lengths = [0u8; MAX_LIT];

    for (symbol, length) in lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }

    Huffman::build(&lengths, literal)?;

    let mut distances = [0u8; MAX_DIST];

    for length in distances.iter_mut() {
        *length = 5;
    }

    Huffman::build(&distances, distance)
}

fn dynamic_trees(
    bits: &mut Bits<'_>,
    literal: &mut Huffman,
    distance: &mut Huffman,
) -> Result<(), Error> {
    let literal_count = bits.bits(5)? as usize + 257;
    let distance_count = bits.bits(5)? as usize + 1;
    let code_count = bits.bits(4)? as usize + 4;

    if literal_count > 286 || distance_count > 30 {
        return Err(b"invalid deflate tree size");
    }

    let mut code_lengths = [0u8; 19];

    for index in 0..code_count {
        code_lengths[CODE_LENGTH_ORDER[index]] = bits.bits(3)? as u8;
    }

    let mut code_tree = Huffman {
        count: [0; MAX_BITS + 1],
        symbol: [0; MAX_LIT],
    };
    Huffman::build(&code_lengths, &mut code_tree)?;

    let total = literal_count + distance_count;
    let mut lengths = [0u8; MAX_LIT + MAX_DIST];
    let mut position = 0usize;

    while position < total {
        let symbol = code_tree.decode(bits)?;

        match symbol {
            0..=15 => {
                lengths[position] = symbol as u8;
                position += 1;
            }
            16 => {
                if position == 0 {
                    return Err(b"invalid deflate repeat");
                }

                let repeat = bits.bits(2)? as usize + 3;
                let value = lengths[position - 1];

                for _ in 0..repeat {
                    if position >= total {
                        return Err(b"deflate repeat overflow");
                    }

                    lengths[position] = value;
                    position += 1;
                }
            }
            17 => {
                let repeat = bits.bits(3)? as usize + 3;

                for _ in 0..repeat {
                    if position >= total {
                        return Err(b"deflate repeat overflow");
                    }

                    lengths[position] = 0;
                    position += 1;
                }
            }
            18 => {
                let repeat = bits.bits(7)? as usize + 11;

                for _ in 0..repeat {
                    if position >= total {
                        return Err(b"deflate repeat overflow");
                    }

                    lengths[position] = 0;
                    position += 1;
                }
            }
            _ => return Err(b"invalid deflate code length"),
        }
    }

    if lengths[256] == 0 {
        return Err(b"deflate tree has no end marker");
    }

    Huffman::build(&lengths[..literal_count], literal)?;
    Huffman::build(&lengths[literal_count..total], distance)
}

fn compressed_block(
    bits: &mut Bits<'_>,
    emitter: &mut Emitter<'_>,
    literal: &Huffman,
    distance: &Huffman,
) -> Result<(), Error> {
    loop {
        let symbol = literal.decode(bits)? as usize;

        match symbol {
            0..=255 => emitter.emit(symbol as u8)?,
            256 => return Ok(()),
            257..=285 => {
                let index = symbol - 257;
                let length = LENGTH_BASE[index] as usize
                    + bits.bits(LENGTH_EXTRA[index])? as usize;
                let distance_symbol = distance.decode(bits)? as usize;

                if distance_symbol >= 30 {
                    return Err(b"invalid deflate distance code");
                }

                let distance_value = DIST_BASE[distance_symbol] as usize
                    + bits.bits(DIST_EXTRA[distance_symbol])? as usize;
                emitter.distance(distance_value, length)?;
            }
            _ => return Err(b"invalid deflate length code"),
        }
    }
}

fn deflate(data: &[u8], emitter: &mut Emitter<'_>) -> Result<(), Error> {
    let mut bits = Bits::new(data);

    loop {
        let last = bits.bits(1)?;
        let block_type = bits.bits(2)?;

        match block_type {
            0 => {
                bits.align_byte();
                let length = bits.read_byte()? as usize
                    | ((bits.read_byte()? as usize) << 8);
                let inverse = bits.read_byte()? as usize
                    | ((bits.read_byte()? as usize) << 8);

                if (length ^ 0xFFFF) != inverse {
                    return Err(b"invalid stored deflate block");
                }

                for _ in 0..length {
                    let byte = bits.read_byte()?;
                    emitter.emit(byte)?;
                }
            }
            1 => {
                let mut literal = Huffman {
                    count: [0; MAX_BITS + 1],
                    symbol: [0; MAX_LIT],
                };
                let mut distance = Huffman {
                    count: [0; MAX_BITS + 1],
                    symbol: [0; MAX_LIT],
                };
                fixed_trees(&mut literal, &mut distance)?;
                compressed_block(&mut bits, emitter, &literal, &distance)?;
            }
            2 => {
                let mut literal = Huffman {
                    count: [0; MAX_BITS + 1],
                    symbol: [0; MAX_LIT],
                };
                let mut distance = Huffman {
                    count: [0; MAX_BITS + 1],
                    symbol: [0; MAX_LIT],
                };
                dynamic_trees(&mut bits, &mut literal, &mut distance)?;
                compressed_block(&mut bits, emitter, &literal, &distance)?;
            }
            _ => return Err(b"invalid deflate block type"),
        }

        if last == 1 {
            return Ok(());
        }
    }
}

/// Распаковывает один gzip-поток и передаёт данные в `sink`.
///
/// Проверяются метод сжатия, CRC32 и ISIZE. Многопоточные gzip-архивы
/// (несколько членов подряд) не поддерживаются.
pub fn gunzip(data: &[u8], sink: &mut dyn Sink) -> Result<(), Error> {
    if data.len() < 18 || data[0] != 0x1F || data[1] != 0x8B || data[2] != 8 {
        return Err(b"not a gzip file");
    }

    let flags = data[3];
    let mut position = 10usize;

    if flags & 0x04 != 0 {
        if position + 2 > data.len() {
            return Err(b"truncated gzip header");
        }

        let extra = data[position] as usize
            | ((data[position + 1] as usize) << 8);
        position += 2;

        if position + extra > data.len() {
            return Err(b"truncated gzip header");
        }

        position += extra;
    }

    if flags & 0x08 != 0 {
        while position < data.len() && data[position] != 0 {
            position += 1;
        }

        position += 1;
    }

    if flags & 0x10 != 0 {
        while position < data.len() && data[position] != 0 {
            position += 1;
        }

        position += 1;
    }

    if flags & 0x02 != 0 {
        position += 2;
    }

    if position + 8 > data.len() {
        return Err(b"truncated gzip file");
    }

    let tail = data.len() - 8;
    if position > tail {
        return Err(b"truncated gzip stream");
    }

    // Окно deflate выделяется из арены только на время распаковки:
    // 32 КиБ в .bss заметно мешают загрузке на машине с 6 МиБ RAM.
    let mark = arena::mark();
    let Some(window) = arena::allocate(WINDOW_SIZE, 16) else {
        return Err(b"not enough memory for deflate window");
    };

    let mut emitter = Emitter::new(sink, window);
    let result = match deflate(&data[position..tail], &mut emitter) {
        Ok(()) => emitter.flush(),
        Err(error) => Err(error),
    };

    let crc = emitter.crc ^ 0xFFFF_FFFF;
    let total = emitter.total;
    drop(emitter);
    arena::release(mark);

    result?;

    let expected_crc = u32::from_le_bytes([
        data[tail],
        data[tail + 1],
        data[tail + 2],
        data[tail + 3],
    ]);
    let expected_size = u32::from_le_bytes([
        data[tail + 4],
        data[tail + 5],
        data[tail + 6],
        data[tail + 7],
    ]);

    if crc != expected_crc {
        return Err(b"gzip CRC mismatch");
    }

    if total as u32 != expected_size {
        return Err(b"gzip size mismatch");
    }

    Ok(())
}
