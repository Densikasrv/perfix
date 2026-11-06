//! Запуск графической оболочки pdesk.
//!
//! pdesk собирается как отдельный crate и работает только через трейт
//! `pdesk::Platform`. Здесь этот трейт реализуется поверх VGA-фреймбуфера,
//! клавиатуры и PS/2-мыши ядра. Вход в оболочку выполняется командой
//! `guinit`; после выхода (Ctrl+Q или `exit` в pterm) управление
//! возвращается консольной оболочке Perfix.

use core::arch::asm;

use crate::{keyboard, mouse, pterm, shell, vga};

/// Реализация платформы pdesk на железе Perfix.
struct KernelPlatform;

impl pdesk::Platform for KernelPlatform {
    fn screen_size(&mut self) -> (i32, i32) {
        (vga::pixel_width() as i32, vga::pixel_height() as i32)
    }

    fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32) {
        vga::fill_rect(x, y, width, height, color);
    }

    fn draw_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32) {
        vga::draw_rect(x, y, width, height, color);
    }

    fn draw_text(&mut self, x: i32, y: i32, text: &[u8], foreground: u32, background: u32) {
        vga::draw_text(x, y, text, foreground, background);
    }

    fn read_pixel(&mut self, x: i32, y: i32) -> u32 {
        vga::get_pixel(x, y)
    }

    fn write_pixel(&mut self, x: i32, y: i32, color: u32) {
        vga::put_pixel(x, y, color);
    }

    fn read_key(&mut self) -> Option<pdesk::Key> {
        loop {
            match keyboard::read_key() {
                keyboard::Key::None => return None,
                keyboard::Key::Character(ch) => return Some(pdesk::Key::Char(ch)),
                keyboard::Key::Enter => return Some(pdesk::Key::Enter),
                keyboard::Key::Backspace => return Some(pdesk::Key::Backspace),
                keyboard::Key::Tab => return Some(pdesk::Key::Tab),
                keyboard::Key::Escape => return Some(pdesk::Key::Escape),
                keyboard::Key::Delete => return Some(pdesk::Key::Delete),
                keyboard::Key::Home => return Some(pdesk::Key::Home),
                keyboard::Key::End => return Some(pdesk::Key::End),
                keyboard::Key::ArrowUp => return Some(pdesk::Key::ArrowUp),
                keyboard::Key::ArrowDown => return Some(pdesk::Key::ArrowDown),
                keyboard::Key::ArrowLeft => return Some(pdesk::Key::ArrowLeft),
                keyboard::Key::ArrowRight => return Some(pdesk::Key::ArrowRight),
                keyboard::Key::LayoutChanged => return Some(pdesk::Key::LayoutChanged),
                keyboard::Key::Control(b'q') => return Some(pdesk::Key::Quit),
                // Остальные комбинации оболочке пока не нужны.
                keyboard::Key::Control(_) => continue,
                keyboard::Key::PageUp | keyboard::Key::PageDown => continue,
            }
        }
    }

    fn mouse(&mut self) -> pdesk::Mouse {
        mouse::poll();

        let state = mouse::take_state();

        pdesk::Mouse {
            x: state.x,
            y: state.y,
            buttons: state.buttons,
        }
    }

    fn prompt(&mut self, out: &mut [u8]) -> usize {
        let mut buffer = [0u8; 80];
        let length = shell::prompt_text(&mut buffer);
        let copy = length.min(out.len());
        out[..copy].copy_from_slice(&buffer[..copy]);
        copy
    }

    fn run_command(
        &mut self,
        command: &[u8],
        output: &mut [u8],
        prompt: &mut [u8],
    ) -> pdesk::CommandResult {
        pterm::run_command(command, output, prompt)
    }

    fn shutdown(&mut self) -> bool {
        crate::power::shutdown()
    }

    fn ticks(&mut self) -> u64 {
        rdtsc()
    }

    fn ticks_per_second(&mut self) -> u64 {
        ticks_per_second()
    }

    fn clock(&mut self, out: &mut [u8]) -> usize {
        let (year, month, day, hour, minute) = match crate::rtc::now() {
            Some(value) => value,
            None => return 0,
        };

        let mut buffer = [0u8; 16];
        let mut length = 0usize;

        push_two_digits(&mut buffer, &mut length, hour);
        push_byte(&mut buffer, &mut length, b':');
        push_two_digits(&mut buffer, &mut length, minute);
        push_byte(&mut buffer, &mut length, b'\n');
        push_two_digits(&mut buffer, &mut length, day);
        push_byte(&mut buffer, &mut length, b'.');
        push_two_digits(&mut buffer, &mut length, month);
        push_byte(&mut buffer, &mut length, b'.');
        push_two_digits(&mut buffer, &mut length, year / 100);
        push_two_digits(&mut buffer, &mut length, year % 100);

        let copy = length.min(out.len());
        out[..copy].copy_from_slice(&buffer[..copy]);
        copy
    }

    fn idle(&mut self) {
        unsafe {
            asm!("pause", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Дописывает байт в буфер, пока в нём есть место.
fn push_byte(buffer: &mut [u8; 16], length: &mut usize, byte: u8) {
    if *length < buffer.len() {
        buffer[*length] = byte;
        *length += 1;
    }
}

/// Дописывает двузначное число с ведущим нулём.
fn push_two_digits(buffer: &mut [u8; 16], length: &mut usize, value: u32) {
    push_byte(buffer, length, b'0' + ((value / 10) % 10) as u8);
    push_byte(buffer, length, b'0' + (value % 10) as u8);
}

/// Тактов TSC в секунду; стартовое значение уточняется по ходу работы.
static mut TICKS_PER_SECOND: u64 = 1_000_000_000;
/// Секунда RTC, на которой снят предыдущий отсчёт TSC.
static mut LAST_SECOND: u32 = u32::MAX;
static mut LAST_TSC: u64 = 0;

/// Читает счётчик тактов процессора.
fn rdtsc() -> u64 {
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

    ((high as u64) << 32) | low as u64
}

/// Оценивает частоту TSC по секундам RTC.
///
/// Оболочка вызывает это каждый цикл во время игр, поэтому измерение
/// уточняется непрерывно: как только секунда RTC сменилась, разница
/// тактов принимается за новую оценку. Такой подход не зависит от
/// длительности калибровочных циклов и самокорректируется.
fn ticks_per_second() -> u64 {
    let second = crate::rtc::second();
    let now = rdtsc();

    unsafe {
        if LAST_SECOND != u32::MAX && second != LAST_SECOND {
            let delta = now.saturating_sub(LAST_TSC);

            // Отбрасываем слишком короткие и подозрительно длинные интервалы.
            if delta > 1_000_000 && delta < TICKS_PER_SECOND.saturating_mul(4) {
                TICKS_PER_SECOND = delta;
            }

            LAST_TSC = now;
        }

        LAST_SECOND = second;
        TICKS_PER_SECOND
    }
}

/// Запускает pdesk.
///
/// Возвращает текст ошибки, если запуск невозможен (например, GRUB не дал
/// линейный framebuffer и консоль осталась в текстовом режиме).
pub fn run() -> Result<(), &'static [u8]> {
    if !vga::is_graphics() {
        return Err(b"guinit: graphics mode is not available, your gpu is ass");
    }

    // Текстовый курсор консоли в графическом режиме рисуется самой консолью,
    // поэтому просто скрываем его.
    vga::hide_cursor();

    // Указатель мыши должен ходить по всему экрану.
    mouse::set_bounds(vga::pixel_width(), vga::pixel_height());

    let mut platform = KernelPlatform;
    pdesk::run(&mut platform);

    // Возвращаем консоль в согласованное состояние: pdesk рисовал прямо
    // во фреймбуфере, а буфер консоли об этом ничего не знает.
    vga::clear_screen();

    Ok(())
}
