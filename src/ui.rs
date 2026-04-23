use crate::clipboard::ClipboardManager;
use crate::converter::Converter;
use crate::hotkey;
use crate::models::{Cache, Config, ConversionResult};
use enigo::{Enigo, Mouse, Settings as EnigoSettings};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use iced::window;
use iced::{Alignment, Color, Element, Length, Subscription, Task, Theme};
use iced::widget::{button, column, container, row, scrollable, text, text_input};
use tray_icon::{TrayIcon, TrayIconBuilder, menu::{Menu, MenuItem, MenuEvent}};
use std::time::Duration;

pub struct State {
    pub config: Config,
    pub cache: Cache,
    pub converter: Converter,
    pub clipboard: ClipboardManager,
    pub enigo: Enigo,
    pub hotkey_manager: GlobalHotKeyManager,
    pub hotkey_id: global_hotkey::hotkey::HotKey,
    pub current_result: Option<ConversionResult>,
    pub captured_value: f64,
    pub captured_unit: Option<String>,
    pub window_id: Option<window::Id>,
    pub search_query: String,
    pub tray_icon: TrayIcon,
}

#[derive(Debug, Clone)]
pub enum Message {
    HotkeyTriggered,
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    CurrencyCacheRefreshed(Cache),
    Tick,
    SearchChanged(String),
    SelectSourceUnit(String),
    ToggleFavorite(String),
    Swap(f64, String),
    CloseWindow,
    ExitRequested,
}

/// Initializes the application state.
///
/// # Panics
/// Panics if the clipboard, mouse controller, hotkey manager, or tray icon fails to initialize.
#[expect(clippy::expect_used, reason = "Critical infrastructure failure at startup is non-recoverable")]
pub fn boot() -> (State, Task<Message>) {
    let config = Config::load().unwrap_or_default();
    let cache = Cache::load().unwrap_or_default();
    let converter = Converter::new(config.clone(), cache.clone());
    
    // Infrastructure
    let clipboard = ClipboardManager::new().expect("Failed to initialize clipboard");
    let enigo = Enigo::new(&EnigoSettings::default()).expect("Failed to initialize enigo");
    
    // Hotkeys
    let hotkey_manager = GlobalHotKeyManager::new().expect("Failed to initialize hotkey manager");
    let hk = hotkey::parse_hotkey(&config.hotkey).expect("Failed to parse hotkey");
    hotkey_manager.register(hk).expect("Failed to register hotkey");

    // Tray Icon
    let tray_menu = Menu::with_items(&[
        &MenuItem::with_id("quit", "Quit Clippy Converter", true, None),
    ]).expect("Failed to create tray menu");

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("Clippy Converter")
        .build()
        .expect("Failed to create tray icon");

    (
        State {
            config,
            cache,
            converter,
            clipboard,
            enigo,
            hotkey_manager,
            hotkey_id: hk,
            current_result: None,
            captured_value: 0.0,
            captured_unit: None,
            window_id: None,
            search_query: String::new(),
            tray_icon,
        },
        Task::none(),
    )
}

pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::HotkeyTriggered => handle_hotkey(state),
        Message::WindowOpened(id) => {
            println!("Window opened with ID: {id:?}");
            state.window_id = Some(id);
            Task::none()
        }
        Message::WindowClosed(id) => {
            println!("Window closed with ID: {id:?}");
            if state.window_id == Some(id) {
                state.window_id = None;
            }
            Task::none()
        }
        Message::CurrencyCacheRefreshed(new_cache) => {
            state.cache = new_cache;
            state.converter = Converter::new(state.config.clone(), state.cache.clone());
            Task::none()
        }
        Message::Tick => handle_tick(state),
        Message::SearchChanged(query) => {
            state.search_query = query;
            Task::none()
        }
        Message::SelectSourceUnit(unit) => {
            if let Ok(result) = state.converter.convert(state.captured_value, &unit) {
                state.current_result = Some(result);
                state.captured_unit = Some(unit);
                state.search_query = String::new();
            }
            Task::none()
        }
        Message::ToggleFavorite(unit) => {
            if let Some(pos) = state.config.favorites.iter().position(|f| f == &unit) {
                state.config.favorites.remove(pos);
            } else {
                state.config.favorites.push(unit);
            }
            let _ = state.config.save();
            state.converter = Converter::new(state.config.clone(), state.cache.clone());
            Task::none()
        }
        Message::Swap(value, unit) => {
            if let Ok(result) = state.converter.convert(value, &unit) {
                state.current_result = Some(result);
                state.search_query = String::new();
            }
            Task::none()
        }
        Message::CloseWindow => {
            state.window_id.map_or_else(Task::none, window::close)
        }
        Message::ExitRequested => iced::exit(),
    }
}

fn handle_hotkey(state: &mut State) -> Task<Message> {
    println!("Hotkey triggered!");
    if let Ok(text) = state.clipboard.capture_selection() {
        println!("Captured text: '{text}'");
        if let Ok(parsed) = crate::parser::parse_input(&text) {
            println!("Parsed value: {:.2}, unit: {:?}", parsed.value, parsed.unit);
            
            state.captured_value = parsed.value;
            state.captured_unit = parsed.unit.clone();
            state.search_query = String::new();

            // Try to convert immediately if unit is present
            if let Some(ref unit) = parsed.unit {
                if let Ok(result) = state.converter.convert(parsed.value, unit) {
                    state.current_result = Some(result);
                } else {
                    state.current_result = None;
                }
            } else {
                state.current_result = None;
            }

            let (x, y) = state.enigo.location().unwrap_or((100, 100));
            println!("Opening window at {x}, {y}");
            
            #[expect(clippy::cast_precision_loss, reason = "Screen coordinates fit in f32 mantissa")]
            let (_, open_task) = window::open(window::Settings {
                size: (350.0, 400.0).into(),
                position: window::Position::Specific(iced::Point::new(x as f32, y as f32)),
                decorations: false,
                transparent: true,
                level: window::Level::AlwaysOnTop,
                ..Default::default()
            });
            
            return open_task.map(Message::WindowOpened);
        }
    }
    Task::none()
}

fn handle_tick(state: &State) -> Task<Message> {
    if state.cache.is_expired() {
        return Task::perform(crate::api::fetch_latest_rates(), |res| {
            res.map_or(Message::Tick, |rates| {
                let mut cache = Cache::load().unwrap_or_default();
                cache.rates = rates;
                cache.last_updated = chrono::Utc::now();
                let _ = cache.save();
                Message::CurrencyCacheRefreshed(cache)
            })
        });
    }
    Task::none()
}

#[must_use]
pub fn view(state: &State, _window_id: window::Id) -> Element<'_, Message> {
    let content = if let Some(result) = &state.current_result {
        let search_query_lower = state.search_query.to_lowercase();
        
        column![
            row![
                text(format!("{:.2} {}", result.input_value, result.input_unit))
                    .size(24)
                    .color(Color::WHITE)
                    .width(Length::Fill),
                button(text("✕").color(Color::WHITE))
                    .padding(5)
                    .on_press(Message::CloseWindow)
                    .style(button::secondary)
            ]
            .align_y(Alignment::Center),
            
            text_input("Search units...", &state.search_query)
                .on_input(Message::SearchChanged)
                .padding(10)
                .size(16),

            scrollable(
                column(
                    result.outputs.iter()
                        .filter(|o| o.unit.to_lowercase().contains(&search_query_lower))
                        .map(|output| {
                            let is_favorite = state.config.favorites.contains(&output.unit);
                            let favorite_label = if is_favorite { "★" } else { "☆" };

                            container(
                                row![
                                    column![
                                        text(format!("{:.4}", output.value))
                                            .size(18)
                                            .color(Color::WHITE),
                                        text(&output.unit)
                                            .size(14)
                                            .color(Color::from_rgb8(150, 150, 150))
                                    ]
                                    .width(Length::Fill),
                                    
                                    row![
                                        button(text("⇄"))
                                            .on_press(Message::Swap(output.value, output.unit.clone()))
                                            .padding(5),
                                        button(text(favorite_label))
                                            .on_press(Message::ToggleFavorite(output.unit.clone()))
                                            .padding(5)
                                    ]
                                    .spacing(5)
                                ]
                                .align_y(Alignment::Center)
                            )
                            .padding(10)
                            .style(|_theme: &Theme| {
                                container::Style {
                                    border: iced::Border {
                                        color: Color::from_rgba8(255, 255, 255, 0.1),
                                        width: 1.0,
                                        radius: 4.0.into(),
                                    },
                                    ..Default::default()
                                }
                            })
                            .into()
                        })
                )
                .spacing(10)
            )
        ]
        .spacing(15)
        .align_x(Alignment::Start)
    } else {
        let search_query_lower = state.search_query.to_lowercase();
        let all_units = state.converter.get_all_units();
        
        column![
            row![
                text(format!("Convert {:.4} ...", state.captured_value))
                    .size(24)
                    .color(Color::WHITE)
                    .width(Length::Fill),
                button(text("✕").color(Color::WHITE))
                    .padding(5)
                    .on_press(Message::CloseWindow)
                    .style(button::secondary)
            ]
            .align_y(Alignment::Center),

            text_input("Search source unit...", &state.search_query)
                .on_input(Message::SearchChanged)
                .padding(10)
                .size(16),

            scrollable(
                column(
                    all_units.into_iter()
                        .filter(|u| u.to_lowercase().contains(&search_query_lower))
                        .map(|unit| {
                            button(text(unit.clone()).color(Color::WHITE))
                                .on_press(Message::SelectSourceUnit(unit))
                                .width(Length::Fill)
                                .padding(10)
                                .style(button::secondary)
                                .into()
                        })
                )
                .spacing(5)
            )
        ]
        .spacing(15)
        .align_x(Alignment::Start)
    };

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20)
        .style(|_theme: &Theme| {
            container::Style {
                background: Some(Color::from_rgba8(30, 30, 30, 0.98).into()),
                border: iced::Border {
                    color: Color::from_rgb8(60, 60, 60),
                    width: 1.0,
                    radius: 12.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}


pub fn subscription(_state: &State) -> Subscription<Message> {
    let hotkey_sub = Subscription::run(|| {
        iced::stream::channel(100, |mut output| async move {
            let receiver = GlobalHotKeyEvent::receiver();
            loop {
                if let Ok(_event) = receiver.try_recv() {
                    use iced::futures::SinkExt;
                    let _ = output.send(Message::HotkeyTriggered).await;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    });

    let tick_sub = iced::time::every(Duration::from_secs(3600)).map(|_| Message::Tick);

    let keyboard_sub = iced::keyboard::on_key_press(|key, _modifiers| {
        match key {
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape) => {
                Some(Message::CloseWindow)
            }
            _ => None,
        }
    });

    let tray_sub = Subscription::run(|| {
        iced::stream::channel(100, |mut output| async move {
            let receiver = MenuEvent::receiver();
            loop {
                if let Ok(event) = receiver.try_recv() && event.id == "quit" {
                    use iced::futures::SinkExt;
                    let _ = output.send(Message::ExitRequested).await;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    });

    Subscription::batch(vec![hotkey_sub, tick_sub, keyboard_sub, tray_sub])
}
