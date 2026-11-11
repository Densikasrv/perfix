//! pdesk — графическая оболочка Perfix.
//!
//! Оболочка собирается как отдельный crate `pdesk` и ничего не знает о
//! внутренностях ядра: всё взаимодействие идёт через трейт [`Platform`],
//! который ядро реализует в `kernel/src/gui.rs`. Благодаря этому pdesk
//! можно собрать отдельно от ядра.
//!
//! В оболочке есть рабочий стол, док в стиле macOS с часами и несколько
//! окон-приложений: терминал pterm, тетрис и змейка. Окна можно
//! перетаскивать за заголовок, сворачивать, разворачивать на весь экран и
//! закрывать (кнопки справа, как в Windows). Левая иконка дока (перфоратор)
//! открывает меню со всеми программами, кнопкой выключения и выходом в
//! консольную оболочку.

#![no_std]

pub mod cursor;
pub mod dock;
pub mod editor;
pub mod installer;
pub mod snake;
pub mod terminal;
pub mod tetris;
pub mod window;

/// GUI-приложения оболочки.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum App {
    Terminal,
    Tetris,
    Snake,
    Editor,
    Installer,
}

impl App {
    /// Все приложения в порядке иконок дока.
    ///
    /// Установщик всегда последний: на установленной системе он скрывается
    /// простым уменьшением количества видимых приложений на единицу.
    pub const ALL: [App; 5] = [
        App::Terminal,
        App::Tetris,
        App::Snake,
        App::Editor,
        App::Installer,
    ];

    /// Номер приложения (индекс в [`App::ALL`]).
    pub const fn index(self) -> usize {
        match self {
            App::Terminal => 0,
            App::Tetris => 1,
            App::Snake => 2,
            App::Editor => 3,
            App::Installer => 4,
        }
    }

    /// Заголовок окна приложения.
    pub const fn title(self) -> &'static [u8] {
        match self {
            App::Terminal => b"pterm",
            App::Tetris => b"tetris",
            App::Snake => b"snake",
            App::Editor => b"tedit",
            App::Installer => b"install",
        }
    }

    /// Размер окна приложения (ширина, высота) в пикселях.
    pub const fn size(self) -> (i32, i32) {
        let chrome_width = 2 * (window::BORDER + window::PADDING);
        let chrome_height =
            2 * window::BORDER + window::TITLE_HEIGHT + 2 * window::PADDING;

        match self {
            App::Terminal => (
                terminal::COLS as i32 * terminal::CHAR_WIDTH + chrome_width,
                terminal::ROWS as i32 * terminal::CHAR_HEIGHT + chrome_height,
            ),
            App::Tetris => (
                tetris::CONTENT_WIDTH + chrome_width,
                tetris::CONTENT_HEIGHT + chrome_height,
            ),
            App::Snake => (
                snake::CONTENT_WIDTH + chrome_width,
                snake::CONTENT_HEIGHT + chrome_height,
            ),
            App::Editor => (
                editor::CONTENT_WIDTH + chrome_width,
                editor::CONTENT_HEIGHT + chrome_height,
            ),
            App::Installer => (
                installer::CONTENT_WIDTH + chrome_width,
                installer::CONTENT_HEIGHT + chrome_height,
            ),
        }
    }
}

/// Клавиша, которую понимает оболочка.
///
/// Ядро переводит скан-коды PS/2 в эти события и само отвечает за
/// переключение раскладки клавиатуры.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Печатный символ в кодировке CP866.
    Char(u8),
    Enter,
    Backspace,
    Tab,
    Escape,
    Delete,
    Home,
    End,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    PageUp,
    PageDown,
    /// Ctrl+<буква>; буква всегда латинская в нижнем регистре.
    Control(u8),
    /// Сменилась раскладка клавиатуры.
    LayoutChanged,
    /// Запрос на выход из оболочки (Ctrl+Q).
    Quit,
}

/// Состояние мыши в пиксельных координатах экрана.
#[derive(Clone, Copy, Default)]
pub struct Mouse {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
}

/// Левая кнопка мыши.
pub const BUTTON_LEFT: u8 = 0x01;
/// Правая кнопка мыши.
pub const BUTTON_RIGHT: u8 = 0x02;
/// Средняя кнопка мыши.
pub const BUTTON_MIDDLE: u8 = 0x04;

/// Цвета рабочего стола и окон в формате 0x00RRGGBB.
pub const DESKTOP_COLOR: u32 = 0x001b2b38;
pub const WINDOW_COLOR: u32 = 0x000c0c0c;
pub const BORDER_COLOR: u32 = 0x005a6a7a;
pub const TITLE_COLOR: u32 = 0x002c5a7c;
pub const TITLE_TEXT_COLOR: u32 = 0x00ffffff;
pub const TEXT_COLOR: u32 = 0x00d0d0d0;
pub const PROMPT_COLOR: u32 = 0x0060e080;

/// Результат выполнения команды оболочки в окне pterm.
#[derive(Clone, Copy, Default)]
pub struct CommandResult {
    /// Сколько байт вывода записано в буфер (строки через `\n`).
    pub length: usize,
    /// Длина нового приглашения оболочки.
    pub prompt_length: usize,
    /// Команда работала на полном экране: нужна полная перерисовка.
    pub fullscreen: bool,
}


/// Раздел ext4, доступный установщику.
#[derive(Clone, Copy, Default)]
pub struct InstallTarget {
    /// Номер раздела в таблице MBR (с единицы).
    pub number: u32,
    /// Размер раздела в секторах.
    pub sectors: u32,
    /// Раздел уже содержит ext4.
    pub ext4: bool,
}

/// Результат загрузки текстового файла для GUI-редактора.
#[derive(Clone, Copy, Default)]
pub struct LoadedText {
    /// Сколько байт текста записано в буфер.
    pub length: usize,
    /// Файл уже существовал (иначе это новый файл).
    pub existed: bool,
    /// Файл не поместился в буфер и был обрезан.
    pub truncated: bool,
    /// Длина абсолютного пути, записанного во второй буфер.
    pub path_length: usize,
}


/// Интерфейс, через который pdesk работает с железом.
///
/// Реализуется ядром Perfix; при необходимости можно подставить заглушку,
/// например для проверки оболочки на хосте.
pub trait Platform {
    /// Размер экрана в пикселях.
    fn screen_size(&mut self) -> (i32, i32);

    /// Заливает прямоугольник.
    fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32);

    /// Рисует рамку толщиной в один пиксель.
    fn draw_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32);

    /// Рисует строку шрифтом 8x16.
    fn draw_text(&mut self, x: i32, y: i32, text: &[u8], foreground: u32, background: u32);

    /// Читает цвет пикселя.
    fn read_pixel(&mut self, x: i32, y: i32) -> u32;

    /// Записывает цвет пикселя.
    fn write_pixel(&mut self, x: i32, y: i32, color: u32);

    /// Сохраняет прямоугольник экрана в буфер (по строкам, без отступа).
    fn read_rect(&mut self, x: i32, y: i32, width: i32, height: i32, out: &mut [u32]);

    /// Возвращает прямоугольник экрана из буфера [`Platform::read_rect`].
    fn write_rect(&mut self, x: i32, y: i32, width: i32, height: i32, data: &[u32]);

    /// Следующее нажатие клавиши, если оно есть.
    fn read_key(&mut self) -> Option<Key>;

    /// Текущее состояние мыши.
    fn mouse(&mut self) -> Mouse;

    /// Записывает в `out` текущее приглашение оболочки (раскладка и каталог)
    /// и возвращает его длину.
    fn prompt(&mut self, out: &mut [u8]) -> usize;

    /// Выполняет команду консольной оболочки Perfix.
    ///
    /// Вывод построчно пишется в `output` через `\n`, новое приглашение —
    /// в `prompt`. Обычные команды выполняются ядром в виртуальной консоли,
    /// полноэкранные (tedit, mouse, диагностика, выключение) — на настоящем
    /// экране; в этом случае нужно перерисовать весь рабочий стол.
    fn run_command(
        &mut self,
        command: &[u8],
        output: &mut [u8],
        prompt: &mut [u8],
    ) -> CommandResult;

    /// Выключает компьютер (кнопка Shutdown в меню дока).
    ///
    /// Возвращает `false`, если выключить питание не удалось.
    fn shutdown(&mut self) -> bool;

    /// Записывает текущие время и дату для часов в доке.
    ///
    /// Формат — две строки через `\n`: `HH:MM` и `DD.MM.YYYY`.
    /// Возвращает длину или 0, если время недоступно.
    fn clock(&mut self, out: &mut [u8]) -> usize;

    /// Монотонный счётчик тактов (TSC) для игровых таймеров.
    fn ticks(&mut self) -> u64;

    /// Сколько тактов [`Platform::ticks`] проходит за секунду.
    ///
    /// Ядро калибрует это значение по RTC при первом обращении.
    fn ticks_per_second(&mut self) -> u64;

    /// Заполняет `out` списком разделов ext4, доступных для установки.
    ///
    /// Возвращает количество записей.
    fn install_targets(&mut self, out: &mut [InstallTarget]) -> usize;

    /// Устанавливает Perfix на раздел с указанным номером.
    ///
    /// При `format` раздел сначала форматируется в ext4 (данные теряются).
    /// `Ok(())` — ядро и GRUB записаны; иначе возвращается текст ошибки.
    fn install_perfix(
        &mut self,
        partition_number: u32,
        format: bool,
        username: &[u8],
        password: &[u8],
        super_password: &[u8],
    ) -> Result<(), &'static [u8]>;

    /// Записывает домашний каталог текущего пользователя.
    ///
    /// Возвращает длину пути или 0, если вход в систему не выполнялся
    /// (например, на установочном носителе).
    fn home_dir(&mut self, out: &mut [u8]) -> usize {
        let _ = out;
        0
    }

    /// Доступен ли установщик Perfix.
    ///
    /// На уже установленной системе он скрыт из дока и меню.
    fn installer_available(&mut self) -> bool {
        true
    }

    /// Загружает текстовый файл для GUI-редактора.
    ///
    /// Текст пишется в `out` (CP866), абсолютный путь — в `path`.
    /// Отсутствующий файл — это не ошибка: возвращается пустой текст
    /// с `existed == false`.
    fn load_text_file(
        &mut self,
        name: &[u8],
        out: &mut [u8],
        path: &mut [u8],
    ) -> Result<LoadedText, &'static [u8]>;

    /// Сохраняет текст GUI-редактора (CP866) в файл по абсолютному пути.
    ///
    /// Возвращает размер записанного UTF-8 текста в байтах.
    fn save_text_file(&mut self, path: &[u8], text: &[u8]) -> Result<usize, &'static [u8]>;

    /// Вызывается, когда событий нет: позволяет ядру не грузить процессор.
    fn idle(&mut self) {}
}

/// Причина выхода из оболочки.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// Пользователь закрыл оболочку: Ctrl+Q, `exit` в pterm или пункт
    /// "Exit to shell" в меню дока.
    Quit,
}

/// Какие области экрана нужно перерисовать.
///
/// Порядок вариантов задаёт приоритет:
/// `None < Cursor < Chrome < Window < Windows < Full`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Repaint {
    None,
    Cursor,
    /// Док, меню и часы (без окон).
    Chrome,
    /// Только изменённое окно приложения (без очистки фона).
    Window,
    /// Все окна без заливки всего рабочего стола.
    Windows,
    /// Весь рабочий стол.
    Full,
}

/// Результат обработки нажатия клавиши рабочим столом.
enum KeyOutcome {
    Ignore,
    Redraw,
    /// Нужно перерисовать все окна (например, открылось новое).
    Windows,
    /// Нужна полная перерисовка (команда работала на всём экране).
    Fullscreen,
    Quit,
}


/// Период падения фигуры в тетрисе.
const TETRIS_PERIOD_MS: u64 = 700;
/// Период шага змейки.
const SNAKE_PERIOD_MS: u64 = 200;

/// Окно приложения и его состояние на рабочем столе.
struct AppWindow {
    window: window::Window,
    open: bool,
    maximized: bool,
    /// Размеры окна до разворачивания: (x, y, ширина, высота).
    saved: (i32, i32, i32, i32),
}

impl AppWindow {
    fn new(app: App, screen_width: i32, screen_height: i32, open: bool) -> Self {
        let (width, height) = app.size();
        let window =
            window::Window::centered(screen_width, screen_height, width, height, app.title());

        Self {
            saved: (window.x, window.y, width, height),
            window,
            open,
            maximized: false,
        }
    }

    /// Разворачивает окно на весь экран или возвращает прежние размеры.
    fn toggle_maximize(&mut self, screen_width: i32, screen_height: i32) {
        if self.maximized {
            let (x, y, width, height) = self.saved;
            self.window.x = x;
            self.window.y = y;
            self.window.width = width;
            self.window.height = height;
            self.maximized = false;
        } else {
            self.saved = (
                self.window.x,
                self.window.y,
                self.window.width,
                self.window.height,
            );
            self.window.x = 0;
            self.window.y = 0;
            self.window.width = screen_width;
            self.window.height = screen_height;
            self.maximized = true;
        }
    }
}

/// Состояние рабочего стола оболочки.
struct Desktop {
    screen_width: i32,
    screen_height: i32,
    /// Окна приложений в порядке [`App::ALL`].
    windows: [AppWindow; 5],
    /// Сколько приложений видно в доке и меню (установщик скрывается).
    app_count: usize,
    /// Активное приложение: получает клавиатуру и рисуется поверх остальных.
    focus: App,
    terminal: terminal::Terminal,
    tetris: tetris::Tetris,
    snake: snake::Snake,
    editor: editor::Editor,
    installer: installer::Installer,
    cursor: cursor::Cursor,
    /// Док с иконками приложений.
    dock: dock::Dock,
    /// Открыто ли меню лончера-перфоратора.
    menu_open: bool,
    /// Элемент дока или меню под указателем.
    hover: Option<dock::Hover>,
    /// Кнопка управления окна под указателем.
    control_hover: Option<(App, window::Control)>,
    /// Запрошен выход в консольную оболочку.
    quit_requested: bool,
    /// Время и дата для часов в доке ("HH:MM\nDD.MM.YYYY").
    clock: [u8; 20],
    clock_len: usize,
    /// Счётчик циклов до следующего чтения RTC.
    clock_ticks: u32,
    last_mouse: Mouse,
    /// Перетаскивание окна: приложение и смещение указателя в заголовке.
    drag: Option<(App, i32, i32)>,
    /// Момент следующего шага игр (в тактах TSC).
    next_tetris: u64,
    next_snake: u64,
    /// Окно, которое нужно перерисовать отдельно (шаг игры).
    dirty_app: Option<App>,
    /// Область, которую перед перерисовкой окон нужно залить фоном
    /// рабочего стола (старое место окна или закрытого меню).
    damage: Option<window::Rect>,
    /// Фон, снятый из-под перетаскиваемого окна.
    drag_background_rect: Option<window::Rect>,
    /// Новое положение окна, которое нужно отрисовать в этом кадре.
    pending_drag: Option<(App, i32, i32)>,
}

/// Максимальный размер окна, которое можно перетаскивать.
const DRAG_BACKGROUND_WIDTH: usize = 768;
const DRAG_BACKGROUND_HEIGHT: usize = 768;
const DRAG_BACKGROUND_AREA: usize = DRAG_BACKGROUND_WIDTH * DRAG_BACKGROUND_HEIGHT;

/// Фон под перетаскиваемым окном.
///
/// Хранится отдельно от [`Desktop`], потому что рабочий стол создаётся на
/// стеке, а несколько мегабайт фона туда класть нельзя.
static mut DRAG_BACKGROUND: [u32; DRAG_BACKGROUND_AREA] = [0; DRAG_BACKGROUND_AREA];

fn drag_background() -> &'static mut [u32] {
    let background = core::ptr::addr_of_mut!(DRAG_BACKGROUND);

    unsafe { &mut (&mut *background)[..] }
}

/// Части `a`, не покрытые `b`. Оба прямоугольника одного размера.
fn difference_rects(a: window::Rect, b: window::Rect) -> [Option<window::Rect>; 4] {
    let top_height = (b.y - a.y).clamp(0, a.height);
    let bottom_start = (b.y + b.height - a.y).clamp(0, a.height);
    let middle_top = b.y.max(a.y);
    let middle_bottom = (b.y + b.height).min(a.y + a.height);
    let left_width = (b.x - a.x).clamp(0, a.width);
    let right_start = (b.x + b.width - a.x).clamp(0, a.width);

    [
        (top_height > 0).then_some(window::Rect {
            x: a.x,
            y: a.y,
            width: a.width,
            height: top_height,
        }),
        (bottom_start < a.height).then_some(window::Rect {
            x: a.x,
            y: a.y + bottom_start,
            width: a.width,
            height: a.height - bottom_start,
        }),
        (middle_bottom > middle_top && left_width > 0).then_some(window::Rect {
            x: a.x,
            y: middle_top,
            width: left_width,
            height: middle_bottom - middle_top,
        }),
        (middle_bottom > middle_top && right_start < a.width).then_some(window::Rect {
            x: a.x + right_start,
            y: middle_top,
            width: a.width - right_start,
            height: middle_bottom - middle_top,
        }),
    ]
}

/// Возвращает часть сохранённого фона на экран.
fn write_drag_strip(
    platform: &mut dyn Platform,
    buffer: &[u32],
    origin: window::Rect,
    area: window::Rect,
) {
    let stride = DRAG_BACKGROUND_WIDTH;

    for row in area.y..area.y + area.height {
        let start = (row - origin.y) as usize * stride + (area.x - origin.x) as usize;
        let end = start + area.width as usize;

        platform.write_rect(area.x, row, area.width, 1, &buffer[start..end]);
    }
}

/// Снимает с экрана часть фона в буфер, выровненный по `origin`.
fn read_drag_strip(
    platform: &mut dyn Platform,
    buffer: &mut [u32],
    origin: window::Rect,
    area: window::Rect,
) {
    let stride = DRAG_BACKGROUND_WIDTH;

    for row in area.y..area.y + area.height {
        let start = (row - origin.y) as usize * stride + (area.x - origin.x) as usize;
        let end = start + area.width as usize;

        platform.read_rect(area.x, row, area.width, 1, &mut buffer[start..end]);
    }
}

/// Сдвигает сохранённый фон из положения `old` в положение `new`.
///
/// `copy_within` сводится к `memmove`, который реализован в ядре,
/// поэтому даже полный прямоугольник сдвигается быстро.
fn shift_drag_background(old: window::Rect, new: window::Rect) {
    let stride = DRAG_BACKGROUND_WIDTH;
    let width = old.width as usize;
    let height = old.height as usize;
    let dx = new.x - old.x;
    let dy = new.y - old.y;

    if dx == 0 && dy == 0 {
        return;
    }

    let background = drag_background();

    if dy > 0 && (dy as usize) < height {
        let shift = dy as usize * stride;
        background.copy_within(shift..height * stride, 0);
    } else if dy < 0 && ((-dy) as usize) < height {
        let shift = (-dy) as usize * stride;
        background.copy_within(0..(height - (-dy) as usize) * stride, shift);
    }

    if dx > 0 && (dx as usize) < width {
        let shift = dx as usize;

        for row in 0..height {
            let start = row * stride;
            background.copy_within(start + shift..start + width, start);
        }
    } else if dx < 0 && ((-dx) as usize) < width {
        let shift = (-dx) as usize;

        for row in 0..height {
            let start = row * stride;
            background.copy_within(start..start + width - shift, start + shift);
        }
    }
}

impl Desktop {
    fn new(screen_width: i32, screen_height: i32, app_count: usize) -> Self {
        let app_count = app_count.min(App::ALL.len());
        let mut terminal = terminal::Terminal::new();
        terminal.reset();

        let mut editor = editor::Editor::new();
        editor.reset();

        Self {
            screen_width,
            screen_height,
            windows: [
                AppWindow::new(App::Terminal, screen_width, screen_height, true),
                AppWindow::new(App::Tetris, screen_width, screen_height, false),
                AppWindow::new(App::Snake, screen_width, screen_height, false),
                AppWindow::new(App::Editor, screen_width, screen_height, false),
                AppWindow::new(App::Installer, screen_width, screen_height, false),
            ],
            app_count,
            focus: App::Terminal,
            terminal,
            tetris: tetris::Tetris::new(),
            snake: snake::Snake::new(),
            editor,
            installer: installer::Installer::new(),
            cursor: cursor::Cursor::new(screen_width / 2, screen_height / 2),
            dock: dock::Dock::new(screen_width, screen_height, app_count),
            menu_open: false,
            hover: None,
            control_hover: None,
            quit_requested: false,
            clock: [0; 20],
            clock_len: 0,
            clock_ticks: 0,
            last_mouse: Mouse {
                x: screen_width / 2,
                y: screen_height / 2,
                buttons: 0,
            },
            drag: None,
            next_tetris: 0,
            next_snake: 0,
            dirty_app: None,
            damage: None,
            drag_background_rect: None,
            pending_drag: None,
        }
    }

    /// Порядок отрисовки: активное окно последним (оно сверху).
    fn draw_order(&self) -> [App; 5] {
        let mut order = App::ALL;

        let mut index = 0;
        while index < order.len() {
            if order[index] == self.focus {
                let focus = order[index];
                let mut current = index;

                while current + 1 < order.len() {
                    order[current] = order[current + 1];
                    current += 1;
                }

                order[order.len() - 1] = focus;
                break;
            }

            index += 1;
        }

        order
    }
}


impl Desktop {
    /// Полная перерисовка: рабочий стол, окна, док и меню.
    fn paint_all(&mut self, platform: &mut dyn Platform) {
        platform.fill_rect(0, 0, self.screen_width, self.screen_height, DESKTOP_COLOR);
        self.paint_windows(platform);
        self.paint_chrome(platform);
    }

    /// Перерисовка всех открытых окон в порядке наложения.
    fn paint_windows(&mut self, platform: &mut dyn Platform) {
        for app in self.draw_order() {
            self.paint_app(platform, app);
        }
    }

    /// Перерисовка всех окон, кроме `skip` (перетаскиваемого).
    fn paint_windows_except(&mut self, platform: &mut dyn Platform, skip: App) {
        for app in self.draw_order() {
            if app != skip {
                self.paint_app(platform, app);
            }
        }
    }

    /// Отрисовывает шаг перетаскивания окна.
    ///
    /// Старый фон возвращается из сохранённой копии, поэтому на экране не
    /// бывает кадра с залитым рабочим столом — именно он и давал мигание.
    fn render_drag(&mut self, platform: &mut dyn Platform, app: App, x: i32, y: i32) {
        let index = app.index();

        if !self.windows[index].open || self.windows[index].maximized {
            self.drag = None;
            self.drag_background_rect = None;
            return;
        }

        let width = self.windows[index].window.width;
        let height = self.windows[index].window.height;
        let area = width as usize * height as usize;

        if width > DRAG_BACKGROUND_WIDTH as i32
            || height > DRAG_BACKGROUND_HEIGHT as i32
            || area > DRAG_BACKGROUND_AREA
        {
            // Не должны сюда попадать: перетаскивание развёрнутого окна
            // запрещено, а остальные окна меньше буфера.
            self.windows[index].window.x = x;
            self.windows[index].window.y = y;
            self.paint_windows(platform);
            self.paint_chrome(platform);
            return;
        }

        let old = window_rect(&self.windows[index].window);
        let new = window::Rect {
            x,
            y,
            width,
            height,
        };

        match self.drag_background_rect {
            Some(previous) => {
                // Возвращаем только те части старого окна, которые не будут
                // закрыты новым положением. Пересечение остаётся на экране,
                // поэтому при мелких шагах не видно кадра с пустым фоном.
                let background = drag_background();

                for area in difference_rects(previous, new).iter().flatten() {
                    write_drag_strip(platform, background, previous, *area);
                }

                shift_drag_background(previous, new);

                let background = drag_background();

                for area in difference_rects(new, previous).iter().flatten() {
                    read_drag_strip(platform, background, new, *area);
                }
            }
            None => {
                // Первый шаг: под окном ещё нет снимка фона. Восстанавливаем
                // его один раз, перерисовав всё, что находится ниже окна.
                platform.fill_rect(old.x, old.y, old.width, old.height, DESKTOP_COLOR);
                self.paint_windows_except(platform, app);
                self.paint_chrome(platform);

                let background = drag_background();

                for row in 0..height {
                    let start = row as usize * DRAG_BACKGROUND_WIDTH;
                    let end = start + width as usize;

                    platform.read_rect(
                        x,
                        y + row,
                        width,
                        1,
                        &mut background[start..end],
                    );
                }
            }
        }

        self.drag_background_rect = Some(new);
        self.windows[index].window.x = x;
        self.windows[index].window.y = y;
        self.paint_app(platform, app);
        self.paint_chrome(platform);
    }

    /// Перерисовка одного окна вместе с содержимым приложения.
    fn paint_app(&mut self, platform: &mut dyn Platform, app: App) {
        let index = app.index();

        if !self.windows[index].open {
            return;
        }

        let maximized = self.windows[index].maximized;
        let hover = match self.control_hover {
            Some((hover_app, control)) if hover_app == app => Some(control),
            _ => None,
        };

        if app == App::Editor {
            let mut title = [0u8; window::TITLE_CAPACITY];
            let length = self.editor.title(&mut title);
            self.windows[index].window.set_title(&title[..length]);
        }

        self.windows[index].window.paint(platform, maximized, hover);

        // Каждое приложение само заливает свою клиентскую область, поэтому
        // здесь нет промежуточной тёмной заливки — она давала заметное
        // мерцание при вводе, игре и перетаскивании окна.
        let area = self.windows[index].window.content();

        match app {
            App::Terminal => self.terminal.paint(platform, area.x, area.y),
            App::Tetris => self.tetris.paint(platform, area.x, area.y),
            App::Snake => self.snake.paint(platform, area.x, area.y),
            App::Editor => self.editor.paint(platform, area),
            App::Installer => self.installer.paint(platform, area),
        }
    }

    /// Перерисовка дока, часов и меню. Рисуется поверх окон.
    fn paint_chrome(&mut self, platform: &mut dyn Platform) {
        self.dock.paint(
            platform,
            self.menu_open,
            self.hover,
            &self.clock[..self.clock_len],
        );

        if self.menu_open {
            let menu = dock::menu_rect(&self.dock, self.screen_width);
            dock::paint_menu(platform, &menu, self.hover, self.dock.apps);
        }
    }

    /// Выполняет запрошенную перерисовку, не забывая про указатель мыши.
    fn apply(&mut self, platform: &mut dyn Platform, repaint: Repaint) {
        if repaint == Repaint::None {
            return;
        }

        self.cursor.erase(platform);

        // Старое место сдвинутого/закрытого окна или меню заливаем фоном,
        // а не перерисовываем весь экран.
        if let Some(rect) = self.damage.take() {
            platform.fill_rect(
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                DESKTOP_COLOR,
            );
        }

        if let Some((app, x, y)) = self.pending_drag.take() {
            // Перетаскивание рисует себя само: возвращает фон из копии и
            // перерисовывает только перемещаемое окно.
            self.dirty_app = None;
            self.render_drag(platform, app, x, y);
        } else {
            match repaint {
                Repaint::None => {}
                Repaint::Cursor => {}
                Repaint::Chrome => self.paint_chrome(platform),
                Repaint::Window => {
                    // Шаг игры или ввод меняет только активное окно.
                    match self.dirty_app.take() {
                        Some(app) => self.paint_app(platform, app),
                        None => self.paint_windows(platform),
                    }

                    // Открытое меню дока рисуется поверх окон.
                    if self.menu_open {
                        self.paint_chrome(platform);
                    }
                }
                Repaint::Windows => {
                    self.dirty_app = None;
                    self.paint_windows(platform);
                    self.paint_chrome(platform);
                }
                Repaint::Full => {
                    self.dirty_app = None;
                    self.paint_all(platform);
                }
            }
        }

        self.cursor.draw(platform);
    }

    /// Показывает окно приложения и делает его активным.
    fn open_app(&mut self, app: App) {
        if app.index() >= self.app_count {
            return;
        }

        self.windows[app.index()].open = true;
        self.focus = app;
        self.menu_open = false;
        self.drag = None;
    }

    /// После закрытия активного окна выбирает другое открытое.
    fn ensure_focus(&mut self) {
        if self.windows[self.focus.index()].open {
            return;
        }

        for app in App::ALL {
            if self.windows[app.index()].open {
                self.focus = app;
                return;
            }
        }
    }

    /// Периодически обновляет часы в доке.
    ///
    /// RTC читается не в каждом цикле: опрос CMOS слишком дорог для
    /// цикла ожидания событий.
    fn refresh_clock(&mut self, platform: &mut dyn Platform) -> Repaint {
        self.clock_ticks += 1;

        if self.clock_ticks < 8192 {
            return Repaint::None;
        }

        self.clock_ticks = 0;

        let mut buffer = [0u8; 20];
        let length = platform.clock(&mut buffer).min(buffer.len());

        if length == self.clock_len && buffer[..length] == self.clock[..length] {
            return Repaint::None;
        }

        self.clock[..length].copy_from_slice(&buffer[..length]);
        self.clock_len = length;
        Repaint::Chrome
    }

    /// Читает время для часов при старте оболочки.
    fn refresh_clock_initial(&mut self, platform: &mut dyn Platform) {
        self.clock_ticks = 8191;
        self.refresh_clock(platform);
    }
}

impl Desktop {
    /// Обрабатывает клавишу для активного приложения.
    fn handle_key(&mut self, platform: &mut dyn Platform, key: Key) -> KeyOutcome {
        if key == Key::Quit {
            return KeyOutcome::Quit;
        }

        if !self.windows[self.focus.index()].open {
            return KeyOutcome::Ignore;
        }

        match self.focus {
            App::Terminal => self.handle_terminal_key(platform, key),
            App::Tetris => {
                if self.tetris.handle_key(key) {
                    KeyOutcome::Redraw
                } else {
                    KeyOutcome::Ignore
                }
            }
            App::Snake => {
                if self.snake.handle_key(key) {
                    KeyOutcome::Redraw
                } else {
                    KeyOutcome::Ignore
                }
            }
            App::Editor => match self.editor.handle_key(platform, key) {
                editor::EditorOutcome::Ignored => KeyOutcome::Ignore,
                editor::EditorOutcome::Redraw => KeyOutcome::Redraw,
                editor::EditorOutcome::Close => {
                    let index = App::Editor.index();
                    self.windows[index].open = false;
                    self.editor.reset();
                    self.ensure_focus();
                    // Закрытое окно нужно затереть фоном рабочего стола.
                    KeyOutcome::Fullscreen
                }
            },
            App::Installer => {
                if self.installer.handle_key(platform, key) {
                    KeyOutcome::Redraw
                } else {
                    KeyOutcome::Ignore
                }
            }
        }
    }

    /// Ввод в терминале: строка ввода и выполнение команд оболочки.
    fn handle_terminal_key(&mut self, platform: &mut dyn Platform, key: Key) -> KeyOutcome {
        if key != Key::Enter {
            return if self.terminal.handle_key(key) {
                KeyOutcome::Redraw
            } else {
                KeyOutcome::Ignore
            };
        }

        // Enter: строку ввода выполняет настоящая оболочка Perfix.
        let mut command = [0u8; terminal::COLS];
        let command_len = self.terminal.take_input(&mut command);

        self.terminal.echo(&command[..command_len]);

        if command_len == 0 {
            return KeyOutcome::Redraw;
        }

        let command = &command[..command_len];
        self.terminal.history_add(command);

        // Локальные команды окна: очистка и выход из pdesk.
        if command == b"exit" {
            return KeyOutcome::Quit;
        }

        if command == b"clear" {
            self.terminal.clear();
            return KeyOutcome::Redraw;
        }

        // tedit в pterm открывает графический редактор, а не консольный.
        if let Some(argument) = command_argument(command, b"tedit") {
            return self.open_editor(platform, argument);
        }

        let mut output = [0u8; terminal::OUTPUT_CAPACITY];
        let mut prompt = [0u8; terminal::PROMPT_CAPACITY];
        let result = platform.run_command(command, &mut output, &mut prompt);

        self.terminal.append_text(&output[..result.length]);

        if result.prompt_length > 0 {
            self.terminal
                .set_prompt(&prompt[..result.prompt_length.min(prompt.len())]);
        }

        if result.fullscreen {
            KeyOutcome::Fullscreen
        } else {
            KeyOutcome::Redraw
        }
    }

    /// Открывает GUI-редактор с файлом из команды `tedit`.
    ///
    /// Без аргумента открывается пустой документ без имени (как кнопка
    /// «Создать» в «Блокноте»); сохранить его можно после `tedit <файл>`.
    fn open_editor(
        &mut self,
        platform: &mut dyn Platform,
        argument: &[u8],
    ) -> KeyOutcome {
        if self.editor.is_dirty() {
            self.terminal
                .push_line(b"tedit: unsaved changes in the editor");
            return KeyOutcome::Redraw;
        }

        if argument.is_empty() {
            // Новый документ сразу получает путь в домашнем каталоге
            // пользователя, поэтому Ctrl+O сохраняет его туда.
            let mut home = [0u8; editor::PATH_CAPACITY];
            let home_len = platform.home_dir(&mut home).min(home.len());

            if home_len > 0 {
                let mut path = [0u8; editor::PATH_CAPACITY];
                let mut length = home_len;

                path[..length].copy_from_slice(&home[..length]);

                if path[length - 1] != b'/' && length < path.len() {
                    path[length] = b'/';
                    length += 1;
                }

                length = push_text(&mut path, length, b"untitled");
                self.editor.open(&path[..length], b"", false, false);
            } else {
                self.editor.reset();
            }

            self.open_app(App::Editor);
            return KeyOutcome::Windows;
        }

        let mut data = [0u8; editor::MAX_TEXT];
        let mut path = [0u8; editor::PATH_CAPACITY];

        match platform.load_text_file(argument, &mut data, &mut path) {
            Ok(info) => {
                let length = info.path_length.min(path.len());
                self.editor.open(
                    &path[..length],
                    &data[..info.length],
                    info.existed,
                    info.truncated,
                );
                self.open_app(App::Editor);
                KeyOutcome::Windows
            }
            Err(message) => {
                let mut line = [0u8; 96];
                let mut length = push_text(&mut line, 0, b"tedit: ");
                length = push_text(&mut line, length, message);
                self.terminal.push_line(&line[..length]);
                KeyOutcome::Redraw
            }
        }
    }

    /// Элемент дока или меню под указателем.
    fn hit_chrome(&self, x: i32, y: i32) -> Option<dock::Hover> {
        if self.menu_open {
            let menu = dock::menu_rect(&self.dock, self.screen_width);

            if let Some(index) = dock::menu_hit(&menu, x, y, self.dock.apps) {
                return Some(dock::Hover::Menu(index));
            }

            // Внутри панели меню, но мимо пунктов.
            if menu.contains(x, y) {
                return None;
            }
        }

        self.dock.hit(x, y).map(dock::Hover::Dock)
    }

    /// Самое верхнее окно под указателем.
    fn window_at(&self, x: i32, y: i32) -> Option<App> {
        let order = self.draw_order();

        for app in order.iter().rev() {
            let index = app.index();

            if self.windows[index].open && self.windows[index].window.contains(x, y) {
                return Some(*app);
            }
        }

        None
    }

    /// Кнопка управления окна под указателем.
    fn control_at(&self, x: i32, y: i32) -> Option<(App, window::Control)> {
        let order = self.draw_order();

        for app in order.iter().rev() {
            let index = app.index();

            if !self.windows[index].open {
                continue;
            }

            if let Some(control) = self.windows[index].window.control_at(x, y) {
                return Some((*app, control));
            }
        }

        None
    }

    /// Обновляет игровые таймеры активной игры.
    fn tick(&mut self, platform: &mut dyn Platform) -> Repaint {
        let tetris_active =
            self.windows[App::Tetris.index()].open && self.focus == App::Tetris;
        let snake_active = self.windows[App::Snake.index()].open && self.focus == App::Snake;

        if !tetris_active && !snake_active {
            return Repaint::None;
        }

        let ticks = platform.ticks();
        let per_second = platform.ticks_per_second().max(1);
        let mut repaint = Repaint::None;

        if tetris_active && ticks >= self.next_tetris {
            self.next_tetris = ticks + per_second * TETRIS_PERIOD_MS / 1000;

            if self.tetris.step() {
                self.dirty_app = Some(App::Tetris);
                repaint = repaint.max(Repaint::Window);
            }
        }

        if snake_active && ticks >= self.next_snake {
            self.next_snake = ticks + per_second * SNAKE_PERIOD_MS / 1000;

            if self.snake.step() {
                self.dirty_app = Some(App::Snake);
                repaint = repaint.max(Repaint::Window);
            }
        }

        repaint
    }
}



impl Desktop {
    /// Обрабатывает положение и кнопки мыши.
    fn handle_mouse(&mut self, platform: &mut dyn Platform, mouse: Mouse) -> Repaint {
        let moved = mouse.x != self.last_mouse.x || mouse.y != self.last_mouse.y;
        let left_was = self.last_mouse.buttons & BUTTON_LEFT != 0;
        let left_is = mouse.buttons & BUTTON_LEFT != 0;
        self.last_mouse = mouse;

        let mut repaint = Repaint::None;

        if moved {
            self.cursor.x = mouse.x.clamp(0, self.screen_width - 1);
            self.cursor.y = mouse.y.clamp(0, self.screen_height - 1);
            repaint = Repaint::Cursor;
        }

        let hover = self.hit_chrome(mouse.x, mouse.y);

        if hover != self.hover {
            self.hover = hover;
            repaint = repaint.max(Repaint::Chrome);
        }

        let control_hover = self.control_at(mouse.x, mouse.y);

        if control_hover != self.control_hover {
            self.control_hover = control_hover;
            repaint = repaint.max(Repaint::Window);
        }

        let mut closed_menu = false;

        if left_is && !left_was {
            // 1. Пункты открытого меню лончера.
            if self.menu_open {
                let menu = dock::menu_rect(&self.dock, self.screen_width);

                if let Some(index) = dock::menu_hit(&menu, mouse.x, mouse.y, self.dock.apps) {
                    self.menu_open = false;
                    self.drag = None;
                    self.damage = Some(dock_rect(menu));

                    if index < self.dock.apps {
                        self.open_app(App::ALL[index]);
                    } else if index == self.dock.apps {
                        if !platform.shutdown() {
                            self.terminal.push_line(b"shutdown: failed");
                        }
                    } else {
                        // Exit to shell: выйти из pdesk в консольную оболочку.
                        self.quit_requested = true;
                    }

                    return repaint.max(Repaint::Windows);
                }

                if menu.contains(mouse.x, mouse.y) {
                    self.drag = None;
                    return repaint;
                }

                // Клик мимо меню закрывает его.
                self.menu_open = false;
                closed_menu = true;
                self.damage = Some(dock_rect(menu));
                repaint = repaint.max(Repaint::Windows);
            }

            // 2. Иконки дока.
            if let Some(index) = self.dock.hit(mouse.x, mouse.y) {
                self.drag = None;

                if index == 0 {
                    // Перфоратор: открыть или закрыть меню.
                    let was_open = self.menu_open;
                    self.menu_open = !closed_menu;

                    if was_open || !self.menu_open {
                        return repaint.max(Repaint::Windows);
                    }

                    return repaint.max(Repaint::Chrome);
                } else {
                    // Приложение: показать окно и вывести его на передний план.
                    self.open_app(App::ALL[index - 1]);
                }

                return repaint.max(Repaint::Windows);
            }

            // 3. Кнопки управления окна в заголовке.
            if let Some((app, control)) = self.control_at(mouse.x, mouse.y) {
                self.drag = None;
                self.focus = app;
                let index = app.index();
                let old = window_rect(&self.windows[index].window);

                match control {
                    window::Control::Close => {
                        // Несохранённый документ сначала спрашивает Y/N.
                        if app == App::Editor && !self.editor.close_requested() {
                            self.focus = app;
                            self.dirty_app = Some(App::Editor);
                            return repaint.max(Repaint::Window);
                        }

                        self.windows[index].open = false;

                        // Закрытие терминала начинает новую сессию.
                        if app == App::Terminal {
                            self.terminal.reset();
                        }

                        // Закрытие редактора сбрасывает документ.
                        if app == App::Editor {
                            self.editor.reset();
                        }

                        self.damage = Some(old);
                        self.ensure_focus();
                    }
                    window::Control::Minimize => {
                        self.windows[index].open = false;
                        self.damage = Some(old);
                        self.ensure_focus();
                    }
                    window::Control::Maximize => {
                        self.damage = Some(old);
                        self.windows[index].toggle_maximize(self.screen_width, self.screen_height);
                    }
                }

                return repaint.max(Repaint::Windows);
            }

            // 4. Окно под указателем: фокус и перетаскивание за заголовок.
            if let Some(app) = self.window_at(mouse.x, mouse.y) {
                let focus_changed = self.focus != app;
                self.focus = app;
                self.menu_open = false;
                let index = app.index();

                if app == App::Installer {
                    let content = self.windows[index].window.content();

                    if content.contains(mouse.x, mouse.y) {
                        self.installer
                            .handle_click(platform, content, mouse.x, mouse.y);

                        if focus_changed {
                            return repaint.max(Repaint::Windows);
                        }

                        self.dirty_app = Some(App::Installer);
                        return repaint.max(Repaint::Window);
                    }
                }

                if app == App::Editor {
                    let content = self.windows[index].window.content();

                    // Клик в заголовке оставляем перетаскиванию окна.
                    if content.contains(mouse.x, mouse.y) {
                        let old = window_rect(&self.windows[index].window);

                        return match self.editor.handle_click(platform, content, mouse.x, mouse.y) {
                            editor::ClickOutcome::Ignored => {
                                if focus_changed {
                                    repaint.max(Repaint::Windows)
                                } else {
                                    repaint
                                }
                            }
                            editor::ClickOutcome::Redraw => {
                                if focus_changed {
                                    repaint.max(Repaint::Windows)
                                } else {
                                    self.dirty_app = Some(App::Editor);
                                    repaint.max(Repaint::Window)
                                }
                            }
                            editor::ClickOutcome::Close => {
                                self.windows[index].open = false;
                                self.editor.reset();
                                self.damage = Some(old);
                                self.ensure_focus();
                                repaint.max(Repaint::Windows)
                            }
                        };
                    }
                }

                if !self.windows[index].maximized
                    && self.windows[index].window.contains_title(mouse.x, mouse.y)
                {
                    let window = &self.windows[index].window;
                    self.drag = Some((app, mouse.x - window.x, mouse.y - window.y));
                    self.drag_background_rect = None;
                }

                return repaint.max(Repaint::Windows);
            }
        } else if !left_is && left_was {
            self.drag = None;
        }

        if let Some((app, offset_x, offset_y)) = self.drag {
            if !left_is {
                self.drag = None;
                self.drag_background_rect = None;
                return repaint;
            }

            let index = app.index();

            if !self.windows[index].open || self.windows[index].maximized {
                self.drag = None;
                self.drag_background_rect = None;
                return repaint;
            }

            let (x, y) = self.windows[index].window.clamped_position(
                mouse.x - offset_x,
                mouse.y - offset_y,
                self.screen_width,
                self.screen_height,
            );

            if x != self.windows[index].window.x || y != self.windows[index].window.y {
                // Отрисовку откладываем до apply: сначала нужно убрать
                // указатель, чтобы он не попал в сохранённый фон.
                self.pending_drag = Some((app, x, y));
                repaint = repaint.max(Repaint::Cursor);
            }
        }

        repaint
    }
}

/// Переводит прямоугольник дока в общий прямоугольник экрана.
fn dock_rect(rect: dock::Rect) -> window::Rect {
    window::Rect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

/// Прямоугольник окна на экране.
fn window_rect(window: &window::Window) -> window::Rect {
    window::Rect {
        x: window.x,
        y: window.y,
        width: window.width,
        height: window.height,
    }
}

/// Возвращает аргумент команды `name` (текст после имени).
///
/// Повторяет разбор консольной оболочки: имя должно совпадать целиком,
/// аргумент отделяется пробелом или табуляцией.
fn command_argument<'a>(input: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let rest = input.strip_prefix(name)?;

    if rest.is_empty() {
        return Some(rest);
    }

    if rest[0] != b' ' && rest[0] != b'\t' {
        return None;
    }

    let mut start = 0usize;
    let mut end = rest.len();

    while start < end && (rest[start] == b' ' || rest[start] == b'\t') {
        start += 1;
    }

    while end > start && (rest[end - 1] == b' ' || rest[end - 1] == b'\t') {
        end -= 1;
    }

    Some(&rest[start..end])
}

/// Дописывает текст в буфер и возвращает новую длину.
pub fn push_text(buffer: &mut [u8], length: usize, text: &[u8]) -> usize {
    let mut length = length;

    for &byte in text {
        if length >= buffer.len() {
            break;
        }

        buffer[length] = byte;
        length += 1;
    }

    length
}

/// Дописывает число в десятичном виде и возвращает новую длину.
pub fn push_decimal(buffer: &mut [u8], length: usize, value: u32) -> usize {
    if value == 0 {
        return push_text(buffer, length, b"0");
    }

    let mut digits = [0u8; 10];
    let mut count = 0usize;
    let mut value = value;

    while value > 0 && count < digits.len() {
        digits[count] = b'0' + (value % 10) as u8;
        value /= 10;
        count += 1;
    }

    let mut length = length;

    while count > 0 {
        count -= 1;
        length = push_text(buffer, length, &[digits[count]]);
    }

    length
}



/// Запускает оболочку и работает до её закрытия.
///
/// Вызывается ядром по команде `guinit`; после возврата управление
/// переходит обратно в консольную оболочку Perfix.
pub fn run(platform: &mut dyn Platform) -> ExitReason {
    let (screen_width, screen_height) = platform.screen_size();
    let screen_width = screen_width.max(1);
    let screen_height = screen_height.max(1);

    let app_count = if platform.installer_available() {
        App::ALL.len()
    } else {
        App::ALL.len() - 1
    };

    let mut desktop = Desktop::new(screen_width, screen_height, app_count);

    let mut prompt = [0u8; terminal::PROMPT_CAPACITY];
    let prompt_len = platform.prompt(&mut prompt);
    desktop.terminal.set_prompt(&prompt[..prompt_len]);

    desktop.refresh_clock_initial(platform);

    desktop.paint_all(platform);
    desktop.cursor.draw(platform);

    loop {
        let mut repaint = Repaint::None;

        while let Some(key) = platform.read_key() {
            match desktop.handle_key(platform, key) {
                KeyOutcome::Ignore => {}
                KeyOutcome::Redraw => {
                    // Ввод и шаги игр меняют только активное окно.
                    desktop.dirty_app = Some(desktop.focus);
                    repaint = repaint.max(Repaint::Window);
                }
                KeyOutcome::Windows => repaint = repaint.max(Repaint::Windows),
                KeyOutcome::Fullscreen => repaint = Repaint::Full,
                KeyOutcome::Quit => {
                    desktop.cursor.erase(platform);
                    return ExitReason::Quit;
                }
            }
        }

        let mouse = platform.mouse();
        repaint = repaint.max(desktop.handle_mouse(platform, mouse));
        repaint = repaint.max(desktop.tick(platform));
        repaint = repaint.max(desktop.refresh_clock(platform));

        desktop.apply(platform, repaint);

        // Пункт "Exit to shell" в меню лончера.
        if desktop.quit_requested {
            desktop.cursor.erase(platform);
            return ExitReason::Quit;
        }

        if repaint == Repaint::None {
            platform.idle();
        }
    }
}
