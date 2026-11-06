//! Окно оболочки: рамка, заголовок с кнопками управления и клиентская область.
//!
//! Кнопки расположены справа, как в Windows: свернуть, развернуть, закрыть.
//! Значки рисуются примитивами — без растровых картинок.

use crate::{Platform, BORDER_COLOR, TITLE_COLOR, TITLE_TEXT_COLOR, WINDOW_COLOR};

/// Толщина рамки окна в пикселях.
pub const BORDER: i32 = 2;
/// Высота заголовка окна в пикселях.
pub const TITLE_HEIGHT: i32 = 28;
/// Отступ между рамкой/заголовком и содержимым.
pub const PADDING: i32 = 6;

/// Ширина кнопки управления.
const BUTTON_WIDTH: i32 = 40;
/// Подсветка кнопок «свернуть» и «развернуть».
const BUTTON_HOVER: u32 = 0x003a4a5a;
/// Подсветка кнопки закрытия.
const BUTTON_CLOSE_HOVER: u32 = 0x00e81123;
const GLYPH_COLOR: u32 = 0x00e0e0e0;
const GLYPH_HOVER_COLOR: u32 = 0x00ffffff;

/// Кнопка управления окном в заголовке.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Close,
    Minimize,
    Maximize,
}

/// Прямоугольная область экрана.
#[derive(Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && x < self.x + self.width
            && y >= self.y
            && y < self.y + self.height
    }
}

/// Прямоугольное окно с заголовком.
pub struct Window {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub title: &'static [u8],
}

impl Window {
    pub const fn new(x: i32, y: i32, width: i32, height: i32, title: &'static [u8]) -> Self {
        Self {
            x,
            y,
            width,
            height,
            title,
        }
    }

    /// Создаёт окно по центру экрана.
    pub fn centered(
        screen_width: i32,
        screen_height: i32,
        width: i32,
        height: i32,
        title: &'static [u8],
    ) -> Self {
        Self::new(
            ((screen_width - width) / 2).max(0),
            ((screen_height - height) / 2).max(0),
            width,
            height,
            title,
        )
    }

    /// Попадает ли точка в окно целиком.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && x < self.x + self.width
            && y >= self.y
            && y < self.y + self.height
    }

    /// Область заголовка, за которую окно можно перетаскивать.
    pub fn contains_title(&self, x: i32, y: i32) -> bool {
        x >= self.x + BORDER
            && x < self.x + self.width - BORDER - 3 * BUTTON_WIDTH
            && y >= self.y + BORDER
            && y < self.y + BORDER + TITLE_HEIGHT
    }

    /// Прямоугольник кнопки управления.
    fn control_rect(&self, control: Control) -> Rect {
        let offset = match control {
            Control::Close => 0,
            Control::Maximize => 1,
            Control::Minimize => 2,
        };

        Rect {
            x: self.x + self.width - BORDER - (offset + 1) * BUTTON_WIDTH,
            y: self.y + BORDER,
            width: BUTTON_WIDTH,
            height: TITLE_HEIGHT,
        }
    }

    /// Кнопка управления под указателем.
    pub fn control_at(&self, x: i32, y: i32) -> Option<Control> {
        for control in [Control::Minimize, Control::Maximize, Control::Close] {
            if self.control_rect(control).contains(x, y) {
                return Some(control);
            }
        }

        None
    }

    /// Клиентская область — прямоугольник под заголовком.
    pub fn content(&self) -> Rect {
        Rect {
            x: self.x + BORDER + PADDING,
            y: self.y + BORDER + TITLE_HEIGHT + PADDING,
            width: self.width - 2 * (BORDER + PADDING),
            height: self.height - 2 * BORDER - TITLE_HEIGHT - 2 * PADDING,
        }
    }

    /// Ограничивает положение окна так, чтобы оно целиком оставалось на экране.
    pub fn clamped_position(
        &self,
        x: i32,
        y: i32,
        screen_width: i32,
        screen_height: i32,
    ) -> (i32, i32) {
        let max_x = (screen_width - self.width).max(0);
        let max_y = (screen_height - self.height).max(0);

        (x.clamp(0, max_x), y.clamp(0, max_y))
    }
}


impl Window {
    /// Рисует рамку, заголовок, кнопки управления и фон окна.
    pub fn paint(&self, platform: &mut dyn Platform, maximized: bool, hover: Option<Control>) {
        platform.fill_rect(self.x, self.y, self.width, self.height, BORDER_COLOR);

        platform.fill_rect(
            self.x + BORDER,
            self.y + BORDER,
            self.width - 2 * BORDER,
            self.height - 2 * BORDER,
            WINDOW_COLOR,
        );

        platform.fill_rect(
            self.x + BORDER,
            self.y + BORDER,
            self.width - 2 * BORDER,
            TITLE_HEIGHT,
            TITLE_COLOR,
        );

        // Кнопки справа: свернуть, развернуть, закрыть.
        for control in [Control::Minimize, Control::Maximize, Control::Close] {
            let rect = self.control_rect(control);
            let highlighted = hover == Some(control);

            let background = if highlighted {
                match control {
                    Control::Close => BUTTON_CLOSE_HOVER,
                    _ => BUTTON_HOVER,
                }
            } else {
                TITLE_COLOR
            };

            platform.fill_rect(rect.x, rect.y, rect.width, rect.height, background);
            draw_glyph(platform, control, rect, maximized, highlighted);
        }

        // Название окна — слева, чтобы не залезать на кнопки.
        let text_x = self.x + BORDER + 10;
        let available = self.width - 2 * BORDER - 3 * BUTTON_WIDTH - 16;
        let max_chars = ((available.max(0) / crate::terminal::CHAR_WIDTH) as usize)
            .min(self.title.len());

        if max_chars > 0 {
            platform.draw_text(
                text_x,
                self.y + BORDER + (TITLE_HEIGHT - crate::terminal::CHAR_HEIGHT) / 2,
                &self.title[..max_chars],
                TITLE_TEXT_COLOR,
                TITLE_COLOR,
            );
        }
    }
}

/// Рисует значок кнопки управления.
fn draw_glyph(
    platform: &mut dyn Platform,
    control: Control,
    rect: Rect,
    maximized: bool,
    highlighted: bool,
) {
    let color = if highlighted {
        GLYPH_HOVER_COLOR
    } else {
        GLYPH_COLOR
    };

    let center_x = rect.x + rect.width / 2;
    let center_y = rect.y + rect.height / 2;

    match control {
        Control::Minimize => {
            platform.fill_rect(center_x - 5, center_y + 3, 10, 2, color);
        }
        Control::Maximize => {
            if maximized {
                // Кнопка "восстановить": два наложенных окна.
                platform.draw_rect(center_x - 6, center_y - 6, 9, 9, color);
                platform.draw_rect(center_x - 3, center_y - 3, 9, 9, color);
            } else {
                platform.draw_rect(center_x - 5, center_y - 5, 10, 10, color);
            }
        }
        Control::Close => {
            for offset in -5..=5 {
                platform.fill_rect(center_x + offset, center_y + offset, 1, 1, color);
                platform.fill_rect(center_x + offset, center_y - offset, 1, 1, color);
            }
        }
    }
}
