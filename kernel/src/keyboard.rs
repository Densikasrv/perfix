
use core::arch::asm;

pub enum Key {
    Character(u8),
    Enter,
    Backspace,
    None,
}

static mut SHIFT_PRESSED: bool = false;

fn inb(port: u16) -> u8 {
    let value: u8;

    unsafe {
        asm!(
            "in al, dx",
            out("al") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }

    value
}

fn keyboard_char(sc: u8, shift: bool) -> Option<u8> {
    let ch = match sc {
        0x02 => if shift { b'!' } else { b'1' },
        0x03 => if shift { b'@' } else { b'2' },
        0x04 => if shift { b'#' } else { b'3' },
        0x05 => if shift { b'$' } else { b'4' },
        0x06 => if shift { b'%' } else { b'5' },
        0x07 => if shift { b'^' } else { b'6' },
        0x08 => if shift { b'&' } else { b'7' },
        0x09 => if shift { b'*' } else { b'8' },
        0x0A => if shift { b'(' } else { b'9' },
        0x0B => if shift { b')' } else { b'0' },
        0x0C => if shift { b'_' } else { b'-' },
        0x0D => if shift { b'+' } else { b'=' },

        0x10 => if shift { b'Q' } else { b'q' },
        0x11 => if shift { b'W' } else { b'w' },
        0x12 => if shift { b'E' } else { b'e' },
        0x13 => if shift { b'R' } else { b'r' },
        0x14 => if shift { b'T' } else { b't' },
        0x15 => if shift { b'Y' } else { b'y' },
        0x16 => if shift { b'U' } else { b'u' },
        0x17 => if shift { b'I' } else { b'i' },
        0x18 => if shift { b'O' } else { b'o' },
        0x19 => if shift { b'P' } else { b'p' },

        0x1E => if shift { b'A' } else { b'a' },
        0x1F => if shift { b'S' } else { b's' },
        0x20 => if shift { b'D' } else { b'd' },
        0x21 => if shift { b'F' } else { b'f' },
        0x22 => if shift { b'G' } else { b'g' },
        0x23 => if shift { b'H' } else { b'h' },
        0x24 => if shift { b'J' } else { b'j' },
        0x25 => if shift { b'K' } else { b'k' },
        0x26 => if shift { b'L' } else { b'l' },

        0x2C => if shift { b'Z' } else { b'z' },
        0x2D => if shift { b'X' } else { b'x' },
        0x2E => if shift { b'C' } else { b'c' },
        0x2F => if shift { b'V' } else { b'v' },
        0x30 => if shift { b'B' } else { b'b' },
        0x31 => if shift { b'N' } else { b'n' },
        0x32 => if shift { b'M' } else { b'm' },

        0x39 => b' ',
        _ => return None,
    };

    Some(ch)
}

pub fn read_key() -> Key {
    if inb(0x64) & 1 == 0 {
        return Key::None;
    }

    let sc = inb(0x60);

    unsafe {
        match sc {
            0x2A | 0x36 => {
                SHIFT_PRESSED = true;
                return Key::None;
            }
            0xAA | 0xB6 => {
                SHIFT_PRESSED = false;
                return Key::None;
            }
            _ => {}
        }

        // Ignore key-release events.
        if sc & 0x80 != 0 {
            return Key::None;
        }

        if sc == 0x1C {
            return Key::Enter;
        }

        if sc == 0x0E {
            return Key::Backspace;
        }

        match keyboard_char(sc, SHIFT_PRESSED) {
            Some(ch) => Key::Character(ch),
            None => Key::None,
        }
    }
}
