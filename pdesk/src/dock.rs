//! Док рабочего стола в стиле macOS и меню-лончер перфоратора.
//!
//! Док прижат к нижнему краю по центру. Его ширина зависит от количества
//! иконок: крайняя левая — перфоратор (лончер), дальше идут иконки
//! GUI-приложений. Нажатие на перфоратор открывает над доком меню со всеми
//! приложениями и кнопкой выключения компьютера.

use crate::{App, Platform, PROMPT_COLOR, TEXT_COLOR};

/// Максимальное количество GUI-приложений pdesk.
pub const APP_COUNT: usize = App::ALL.len();

/// Сколько пунктов в меню: все видимые приложения, выключение и выход.
pub const MENU_EXTRA: usize = 2;

/// Размер иконки в доке.
pub const ICON_SIZE: i32 = 48;
/// Промежуток между иконками.
pub const ICON_GAP: i32 = 8;
/// Отступ внутри дока.
pub const DOCK_PADDING: i32 = 8;
/// Отступ дока от нижнего края экрана.
pub const DOCK_MARGIN: i32 = 12;
/// Высота дока.
pub const DOCK_HEIGHT: i32 = ICON_SIZE + 2 * DOCK_PADDING;
/// Ширина области часов в правой части дока.
pub const CLOCK_WIDTH: i32 = 80;
/// Отступ между иконками и часами.
const CLOCK_GAP: i32 = 12;

const DOCK_COLOR: u32 = 0x001a222b;
const DOCK_BORDER: u32 = 0x005a6a7a;
const ICON_COLOR: u32 = 0x00161c22;
const ICON_HOVER_COLOR: u32 = 0x002c5a7c;
const MENU_COLOR: u32 = 0x00161c22;
const TERMINAL_SCREEN: u32 = 0x000c0c0c;

/// Иконка дока.
#[derive(Clone, Copy)]
pub enum Icon {
    /// Перфоратор — лончер меню.
    Perforator,
    /// Приложение-терминал.
    Terminal,
    /// Графический браузер Perfbrowse.
    Browser,
    /// Тетрис.
    Tetris,
    /// Змейка.
    Snake,
    /// Текстовый редактор.
    Editor,
    /// Установщик Perfix на ext4.
    Installer,
}

/// Элемент дока или меню под указателем мыши.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hover {
    Dock(usize),
    Menu(usize),
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

/// Док с иконками приложений.
pub struct Dock {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Количество иконок: лончер и видимые приложения.
    pub items: usize,
    /// Количество видимых приложений (без лончера).
    pub apps: usize,
}

impl Dock {
    pub fn new(screen_width: i32, screen_height: i32, apps: usize) -> Self {
        let apps = apps.min(APP_COUNT);
        let items = apps + 1;
        let count = items as i32;
        let icons_width = count * ICON_SIZE + (count - 1) * ICON_GAP;
        let width = 2 * DOCK_PADDING + icons_width + 2 * CLOCK_GAP + 1 + CLOCK_WIDTH;
        let x = ((screen_width - width) / 2).max(0);
        let y = (screen_height - DOCK_HEIGHT - DOCK_MARGIN).max(0);

        Self {
            x,
            y,
            width,
            height: DOCK_HEIGHT,
            items,
            apps,
        }
    }

    /// Иконка по номеру: 0 — перфоратор, дальше приложения.
    pub fn icon(&self, index: usize) -> Icon {
        match index {
            0 => Icon::Perforator,
            1 => Icon::Terminal,
            2 => Icon::Browser,
            3 => Icon::Tetris,
            4 => Icon::Snake,
            5 => Icon::Editor,
            _ => Icon::Installer,
        }
    }

    pub fn item_rect(&self, index: usize) -> Rect {
        Rect {
            x: self.x + DOCK_PADDING + index as i32 * (ICON_SIZE + ICON_GAP),
            y: self.y + DOCK_PADDING,
            width: ICON_SIZE,
            height: ICON_SIZE,
        }
    }

    /// Номер иконки под указателем.
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        for index in 0..self.items {
            if self.item_rect(index).contains(x, y) {
                return Some(index);
            }
        }

        None
    }

    /// Рисует док: иконки, разделитель и часы справа.
    pub fn paint(
        &self,
        platform: &mut dyn Platform,
        menu_open: bool,
        hover: Option<Hover>,
        clock: &[u8],
    ) {
        platform.fill_rect(self.x, self.y, self.width, self.height, DOCK_COLOR);
        platform.draw_rect(self.x, self.y, self.width, self.height, DOCK_BORDER);

        for index in 0..self.items {
            let highlighted = matches!(hover, Some(Hover::Dock(hovered)) if hovered == index)
                || (index == 0 && menu_open);

            paint_icon(platform, self.icon(index), self.item_rect(index), highlighted);
        }

        // Разделитель между иконками и часами.
        let divider_x = self.x + DOCK_PADDING
            + self.items as i32 * ICON_SIZE
            + (self.items as i32 - 1) * ICON_GAP
            + CLOCK_GAP;
        platform.fill_rect(
            divider_x,
            self.y + DOCK_PADDING,
            1,
            ICON_SIZE,
            DOCK_BORDER,
        );

        paint_clock(platform, self.clock_rect(), clock);
    }

    /// Область часов в правой части дока.
    pub fn clock_rect(&self) -> Rect {
        Rect {
            x: self.x + self.width - DOCK_PADDING - CLOCK_WIDTH,
            y: self.y + DOCK_PADDING,
            width: CLOCK_WIDTH,
            height: ICON_SIZE,
        }
    }
}

/// Рисует время и дату: две строки, разделённые `\n`.
fn paint_clock(platform: &mut dyn Platform, rect: Rect, clock: &[u8]) {
    let mut split = clock.len();

    for (index, &byte) in clock.iter().enumerate() {
        if byte == b'\n' {
            split = index;
            break;
        }
    }

    let time = &clock[..split];

    if !time.is_empty() {
        platform.draw_text(rect.x, rect.y + 6, time, TEXT_COLOR, DOCK_COLOR);
    }

    if split + 1 < clock.len() {
        platform.draw_text(
            rect.x,
            rect.y + 26,
            &clock[split + 1..],
            TEXT_COLOR,
            DOCK_COLOR,
        );
    }
}


/// Рисует иконку на плитке.
fn paint_icon(platform: &mut dyn Platform, icon: Icon, rect: Rect, highlighted: bool) {
    let tile = if highlighted { ICON_HOVER_COLOR } else { ICON_COLOR };

    platform.fill_rect(rect.x, rect.y, rect.width, rect.height, tile);
    platform.draw_rect(rect.x, rect.y, rect.width, rect.height, DOCK_BORDER);

    match icon {
        Icon::Perforator => paint_perforator(platform, rect),
        Icon::Terminal => paint_terminal(platform, rect),
        Icon::Browser => paint_browser(platform, rect),
        Icon::Tetris => paint_tetris(platform, rect),
        Icon::Snake => paint_snake(platform, rect),
        Icon::Editor => paint_editor(platform, rect),
        Icon::Installer => paint_installer(platform, rect),
    }
}

/// Иконка терминала: тёмный экран с зелёным приглашением.
fn paint_terminal(platform: &mut dyn Platform, rect: Rect) {
    let inner = Rect {
        x: rect.x + 8,
        y: rect.y + 10,
        width: rect.width - 16,
        height: rect.height - 20,
    };

    platform.fill_rect(inner.x, inner.y, inner.width, inner.height, TERMINAL_SCREEN);
    platform.draw_text(
        inner.x + 2,
        inner.y + (inner.height - 16) / 2,
        b">_",
        PROMPT_COLOR,
        TERMINAL_SCREEN,
    );
}

/// Иконка браузера: синяя веб-страница с надписью www.
fn paint_browser(platform: &mut dyn Platform, rect: Rect) {
    let inner = Rect {
        x: rect.x + 8,
        y: rect.y + 10,
        width: rect.width - 16,
        height: rect.height - 20,
    };
    let page = 0x00183860;

    platform.fill_rect(inner.x, inner.y, inner.width, inner.height, page);
    platform.draw_rect(inner.x, inner.y, inner.width, inner.height, 0x0030a0e0);
    platform.draw_text(
        inner.x + 2,
        inner.y + (inner.height - 16) / 2,
        b"www",
        0x0060c0f0,
        page,
    );
}

/// Иконка тетриса: несколько цветных блоков.
fn paint_tetris(platform: &mut dyn Platform, rect: Rect) {
    let x = rect.x;
    let y = rect.y;

    platform.fill_rect(x + 14, y + 12, 12, 12, 0x00f0d020);
    platform.fill_rect(x + 26, y + 12, 12, 12, 0x0038d8f0);
    platform.fill_rect(x + 14, y + 24, 12, 12, 0x00f05050);
    platform.fill_rect(x + 26, y + 24, 12, 12, 0x0040d040);
}

/// Иконка змейки: изогнутая цепочка пикселей.
fn paint_snake(platform: &mut dyn Platform, rect: Rect) {
    let x = rect.x;
    let y = rect.y;

    platform.fill_rect(x + 12, y + 14, 8, 8, 0x0090d020);
    platform.fill_rect(x + 20, y + 14, 8, 8, 0x0090d020);
    platform.fill_rect(x + 28, y + 14, 8, 8, 0x0090d020);
    platform.fill_rect(x + 28, y + 22, 8, 8, 0x0090d020);
    platform.fill_rect(x + 20, y + 22, 8, 8, 0x0090d020);
    platform.fill_rect(x + 20, y + 30, 8, 8, 0x00f05050);
}

/// Иконка редактора: белый лист в синей рамке со строками текста.
fn paint_editor(platform: &mut dyn Platform, rect: Rect) {
    let x = rect.x;
    let y = rect.y;

    platform.fill_rect(x + 12, y + 8, 24, 32, 0x00ffffff);
    platform.fill_rect(x + 12, y + 8, 24, 5, 0x003060c0);
    platform.fill_rect(x + 16, y + 18, 16, 2, 0x00404040);
    platform.fill_rect(x + 16, y + 23, 16, 2, 0x00404040);
    platform.fill_rect(x + 16, y + 28, 12, 2, 0x00404040);
    platform.fill_rect(x + 16, y + 33, 9, 2, 0x00404040);
}
/// Иконка установщика: диск с направленной вниз стрелкой.
fn paint_installer(platform: &mut dyn Platform, rect: Rect) {
    let x = rect.x;
    let y = rect.y;

    platform.fill_rect(x + 10, y + 14, 28, 20, 0x00b0b8c0);
    platform.fill_rect(x + 10, y + 24, 28, 4, 0x00506070);
    platform.fill_rect(x + 32, y + 17, 3, 3, 0x0060e080);

    // Стрелка вниз.
    platform.fill_rect(x + 23, y + 6, 3, 10, 0x00f0d020);
    platform.fill_rect(x + 20, y + 12, 9, 3, 0x00f0d020);
}

/// Иконка перфоратора: корпус, сверло, рукоятка и шнур.
fn paint_perforator(platform: &mut dyn Platform, rect: Rect) {
    let x = rect.x;
    let y = rect.y;

    // Корпус дрели.
    platform.fill_rect(x + 14, y + 20, 20, 10, 0x00c8c8c8);
    // Патрон и сверло.
    platform.fill_rect(x + 32, y + 18, 5, 14, 0x00808080);
    platform.fill_rect(x + 37, y + 22, 5, 6, 0x00d8d8d8);
    platform.fill_rect(x + 41, y + 24, 2, 2, 0x00ffffff);
    // Рукоятка и курок.
    platform.fill_rect(x + 10, y + 16, 8, 18, 0x00e0a020);
    platform.fill_rect(x + 16, y + 30, 4, 6, 0x00404040);
    // Сетевой шнур.
    platform.fill_rect(x + 6, y + 32, 6, 2, 0x00303030);
}

const MENU_WIDTH: i32 = 180;
const MENU_ITEM_HEIGHT: i32 = 26;
const MENU_PADDING: i32 = 4;
const MENU_SEPARATOR: i32 = 8;

/// Сколько пунктов в меню при заданном числе приложений.
pub fn menu_items(apps: usize) -> usize {
    apps.min(APP_COUNT) + MENU_EXTRA
}

/// Прямоугольник меню — над иконкой перфоратора.
pub fn menu_rect(dock: &Dock, screen_width: i32) -> Rect {
    let apps = dock.apps as i32;
    let system = MENU_EXTRA as i32;
    let height = 2 * MENU_PADDING
        + apps * MENU_ITEM_HEIGHT
        + MENU_SEPARATOR
        + system * MENU_ITEM_HEIGHT;
    let max_x = (screen_width - MENU_WIDTH - 8).max(8);

    Rect {
        x: dock.x.clamp(8, max_x),
        y: dock.y - height - 8,
        width: MENU_WIDTH,
        height,
    }
}

/// Прямоугольник пункта меню: сначала приложения, затем системные пункты.
pub fn menu_item_rect(menu: &Rect, index: usize, apps: usize) -> Rect {
    let apps = apps.min(APP_COUNT) as i32;
    let index = index as i32;

    let offset = if index < apps {
        index * MENU_ITEM_HEIGHT
    } else {
        apps * MENU_ITEM_HEIGHT + MENU_SEPARATOR + (index - apps) * MENU_ITEM_HEIGHT
    };

    Rect {
        x: menu.x + MENU_PADDING,
        y: menu.y + MENU_PADDING + offset,
        width: menu.width - 2 * MENU_PADDING,
        height: MENU_ITEM_HEIGHT,
    }
}

/// Номер пункта меню под указателем.
pub fn menu_hit(menu: &Rect, x: i32, y: i32, apps: usize) -> Option<usize> {
    for index in 0..menu_items(apps) {
        if menu_item_rect(menu, index, apps).contains(x, y) {
            return Some(index);
        }
    }

    None
}

/// Название пункта меню.
pub fn menu_label(index: usize, apps: usize) -> &'static [u8] {
    let apps = apps.min(APP_COUNT);

    if index < apps {
        App::ALL[index].title()
    } else if index == apps {
        b"Shutdown".as_slice()
    } else {
        b"Exit to shell".as_slice()
    }
}

/// Рисует меню лончера.
pub fn paint_menu(
    platform: &mut dyn Platform,
    menu: &Rect,
    hover: Option<Hover>,
    apps: usize,
) {
    platform.fill_rect(menu.x, menu.y, menu.width, menu.height, DOCK_BORDER);
    platform.fill_rect(
        menu.x + 2,
        menu.y + 2,
        menu.width - 4,
        menu.height - 4,
        MENU_COLOR,
    );

    let apps = apps.min(APP_COUNT);

    for index in 0..menu_items(apps) {
        let rect = menu_item_rect(menu, index, apps);
        let highlighted = matches!(hover, Some(Hover::Menu(hovered)) if hovered == index);
        let background = if highlighted { ICON_HOVER_COLOR } else { MENU_COLOR };

        if highlighted {
            platform.fill_rect(rect.x, rect.y, rect.width, rect.height, background);
        }

        platform.draw_text(
            rect.x + 8,
            rect.y + (rect.height - 16) / 2,
            menu_label(index, apps),
            TEXT_COLOR,
            background,
        );
    }

    // Разделитель между приложениями и системными пунктами.
    let separator_y =
        menu.y + MENU_PADDING + apps as i32 * MENU_ITEM_HEIGHT + MENU_SEPARATOR / 2;
    platform.fill_rect(menu.x + 6, separator_y, menu.width - 12, 1, DOCK_BORDER);
}
