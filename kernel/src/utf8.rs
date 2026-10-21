//! Декодирование UTF-8 в CP866 для вывода в VGA text mode.
//!
//! Файлы на exFAT обычно создаются в UTF-8, а знакогенератор VGA у нас
//! работает с CP866. Декодер принимает поток байт и отдаёт готовые коды
//! CP866, поддерживая последовательности длиной 2 и 3 байта (BMP).
//!
//! Байты, не образующие корректную последовательность UTF-8, считаются
//! готовыми кодами CP866 и передаются без изменений.

const MAX_PENDING: usize = 3;

/// Состояние незавершённой последовательности между вызовами [`feed`].
pub struct Decoder {
    pending: [u8; MAX_PENDING],
    pending_len: usize,
    needed: usize,
}

impl Decoder {
    pub const fn new() -> Decoder {
        Decoder {
            pending: [0; MAX_PENDING],
            pending_len: 0,
            needed: 0,
        }
    }

    /// Принимает один байт и записывает до четырёх кодов CP866 в `out`.
    ///
    /// `map` переводит код Unicode в CP866; `None` означает
    /// неподдерживаемый символ — он выводится как `?`.
    pub fn feed<F>(&mut self, byte: u8, out: &mut [u8; 4], map: F) -> usize
    where
        F: Fn(u16) -> Option<u8>,
    {
        if self.needed == 0 {
            return self.start(byte, out);
        }

        if byte & 0xc0 != 0x80 {
            // Последовательность оборвалась: отдаём накопленное как CP866.
            let mut written = 0;

            for index in 0..self.pending_len {
                out[written] = self.pending[index];
                written += 1;
            }

            self.pending_len = 0;
            self.needed = 0;

            return written + self.start(byte, &mut out[written..]);
        }

        self.pending[self.pending_len] = byte;
        self.pending_len += 1;

        if self.pending_len < self.needed {
            return 0;
        }

        let code = if self.needed == 2 {
            ((self.pending[0] as u16 & 0x1f) << 6) | (self.pending[1] as u16 & 0x3f)
        } else {
            ((self.pending[0] as u16 & 0x0f) << 12)
                | ((self.pending[1] as u16 & 0x3f) << 6)
                | (self.pending[2] as u16 & 0x3f)
        };

        self.pending_len = 0;
        self.needed = 0;

        out[0] = map(code).unwrap_or(b'?');

        1
    }

    fn start(&mut self, byte: u8, out: &mut [u8]) -> usize {
        if byte < 0x80 {
            out[0] = byte;
            return 1;
        }

        let needed = if byte & 0xe0 == 0xc0 {
            2
        } else if byte & 0xf0 == 0xe0 {
            3
        } else {
            0
        };

        if needed == 0 {
            // Не UTF-8: байт уже является кодом CP866.
            out[0] = byte;
            return 1;
        }

        self.pending[0] = byte;
        self.pending_len = 1;
        self.needed = needed;

        0
    }
}
