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
}

impl App {
    /// Все приложения в порядке иконок дока.
    pub const ALL: [App; 3] = [App::Terminal, App::Tetris, App::Snake];

    /// Номер приложения (индекс в [`App::ALL`]).
    pub const fn index(self) -> usize {
        match self {
            App::Terminal => 0,
            App::Tetris => 1,
            App::Snake => 2,
        }
    }

    /// Заголовок окна приложения.
    pub const fn title(self) -> &'static [u8] {
        match self {
            App::Terminal => b"pterm",
            App::Tetris => b"tetris",
            App::Snake => b"snake",
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
/// `None < Cursor < Chrome < Window < Full`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Repaint {
    None,
    Cursor,
    /// Док, меню и часы (без окон).
    Chrome,
    /// Окна приложений и док.
    Window,
    /// Весь рабочий стол.
    Full,
}

/// Результат обработки нажатия клавиши рабочим столом.
enum KeyOutcome {
    Ignore,
    Redraw,
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
    windows: [AppWindow; 3],
    /// Активное приложение: получает клавиатуру и рисуется поверх остальных.
    focus: App,
    terminal: terminal::Terminal,
    tetris: tetris::Tetris,
    snake: snake::Snake,
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
}

impl Desktop {
    fn new(screen_width: i32, screen_height: i32) -> Self {
        let mut terminal = terminal::Terminal::new();
        terminal.reset();

        Self {
            screen_width,
            screen_height,
            windows: [
                AppWindow::new(App::Terminal, screen_width, screen_height, true),
                AppWindow::new(App::Tetris, screen_width, screen_height, false),
                AppWindow::new(App::Snake, screen_width, screen_height, false),
            ],
            focus: App::Terminal,
            terminal,
            tetris: tetris::Tetris::new(),
            snake: snake::Snake::new(),
            cursor: cursor::Cursor::new(screen_width / 2, screen_height / 2),
            dock: dock::Dock::new(screen_width, screen_height),
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
        }
    }

    /// Порядок отрисовки: активное окно последним (оно сверху).
    fn draw_order(&self) -> [App; 3] {
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

        self.windows[index].window.paint(platform, maximized, hover);

        let area = self.windows[index].window.content();
        platform.fill_rect(area.x, area.y, area.width, area.height, WINDOW_COLOR);

        match app {
            App::Terminal => self.terminal.paint(platform, area.x, area.y),
            App::Tetris => self.tetris.paint(platform, area.x, area.y),
            App::Snake => self.snake.paint(platform, area.x, area.y),
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
            dock::paint_menu(platform, &menu, self.hover);
        }
    }

    /// Выполняет запрошенную перерисовку, не забывая про указатель мыши.
    fn apply(&mut self, platform: &mut dyn Platform, repaint: Repaint) {
        if repaint == Repaint::None {
            return;
        }

        self.cursor.erase(platform);

        match repaint {
            Repaint::None => {}
            Repaint::Cursor => {}
            Repaint::Chrome => self.paint_chrome(platform),
            Repaint::Window => {
                // Шаг игры меняет только одно окно — остальные не трогаем.
                match self.dirty_app.take() {
                    Some(app) => self.paint_app(platform, app),
                    None => self.paint_windows(platform),
                }

                self.paint_chrome(platform);
            }
            Repaint::Full => self.paint_all(platform),
        }

        self.cursor.draw(platform);
    }

    /// Показывает окно приложения и делает его активным.
    fn open_app(&mut self, app: App) {
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

        // Локальные команды окна: очистка и выход из pdesk.
        if command == b"exit" {
            return KeyOutcome::Quit;
        }

        if command == b"clear" {
            self.terminal.clear();
            return KeyOutcome::Redraw;
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

    /// Элемент дока или меню под указателем.
    fn hit_chrome(&self, x: i32, y: i32) -> Option<dock::Hover> {
        if self.menu_open {
            let menu = dock::menu_rect(&self.dock, self.screen_width);

            if let Some(index) = dock::menu_hit(&menu, x, y) {
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

                if let Some(index) = dock::menu_hit(&menu, mouse.x, mouse.y) {
                    self.menu_open = false;
                    self.drag = None;

                    if index < dock::APP_COUNT {
                        self.open_app(App::ALL[index]);
                    } else if index == dock::MENU_SHUTDOWN {
                        if !platform.shutdown() {
                            self.terminal.push_line(b"shutdown: failed");
                        }
                    } else {
                        // Exit to shell: выйти из pdesk в консольную оболочку.
                        self.quit_requested = true;
                    }

                    return repaint.max(Repaint::Full);
                }

                if menu.contains(mouse.x, mouse.y) {
                    self.drag = None;
                    return repaint;
                }

                // Клик мимо меню закрывает его.
                self.menu_open = false;
                closed_menu = true;
                repaint = repaint.max(Repaint::Full);
            }

            // 2. Иконки дока.
            if let Some(index) = self.dock.hit(mouse.x, mouse.y) {
                self.drag = None;

                if index == 0 {
                    // Перфоратор: открыть или закрыть меню.
                    self.menu_open = !closed_menu;
                } else {
                    // Приложение: показать окно и вывести его на передний план.
                    self.open_app(App::ALL[index - 1]);
                }

                return repaint.max(Repaint::Full);
            }

            // 3. Кнопки управления окна в заголовке.
            if let Some((app, control)) = self.control_at(mouse.x, mouse.y) {
                self.drag = None;
                self.focus = app;
                let index = app.index();

                match control {
                    window::Control::Close => {
                        self.windows[index].open = false;

                        // Закрытие терминала начинает новую сессию.
                        if app == App::Terminal {
                            self.terminal.reset();
                        }

                        self.ensure_focus();
                    }
                    window::Control::Minimize => {
                        self.windows[index].open = false;
                        self.ensure_focus();
                    }
                    window::Control::Maximize => {
                        self.windows[index].toggle_maximize(self.screen_width, self.screen_height)
                    }
                }

                return repaint.max(Repaint::Full);
            }

            // 4. Окно под указателем: фокус и перетаскивание за заголовок.
            if let Some(app) = self.window_at(mouse.x, mouse.y) {
                self.focus = app;
                self.menu_open = false;
                let index = app.index();

                if !self.windows[index].maximized
                    && self.windows[index].window.contains_title(mouse.x, mouse.y)
                {
                    let window = &self.windows[index].window;
                    self.drag = Some((app, mouse.x - window.x, mouse.y - window.y));
                }

                return repaint.max(Repaint::Full);
            }
        } else if !left_is && left_was {
            self.drag = None;
        }

        if let Some((app, offset_x, offset_y)) = self.drag {
            if !left_is {
                self.drag = None;
                return repaint;
            }

            let index = app.index();

            if !self.windows[index].open || self.windows[index].maximized {
                self.drag = None;
                return repaint;
            }

            let (x, y) = self.windows[index].window.clamped_position(
                mouse.x - offset_x,
                mouse.y - offset_y,
                self.screen_width,
                self.screen_height,
            );

            if x != self.windows[index].window.x || y != self.windows[index].window.y {
                self.windows[index].window.x = x;
                self.windows[index].window.y = y;
                repaint = Repaint::Full;
            }
        }

        repaint
    }
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

    let mut desktop = Desktop::new(screen_width, screen_height);

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
                KeyOutcome::Redraw => repaint = repaint.max(Repaint::Window),
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
