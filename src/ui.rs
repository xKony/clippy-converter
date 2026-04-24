use crate::clipboard::ClipboardManager;
use crate::converter::Converter;
use crate::db::Db;
use crate::hotkey;
use crate::models::{Config, ConversionResult, HistoryRetention};
use enigo::{Enigo, Mouse, Settings as EnigoSettings};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::window;
use iced::{Alignment, Color, Element, Length, Subscription, Task, Theme};
use std::time::Duration;
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

pub struct State {
    pub config: Config,
    pub db: Db,
    pub converter: Converter,
    pub clipboard: ClipboardManager,
    pub enigo: Enigo,
    pub hotkey_manager: GlobalHotKeyManager,
    pub hotkey_id: global_hotkey::hotkey::HotKey,
    pub current_result: Option<ConversionResult>,
    pub captured_value: f64,
    pub captured_unit: Option<String>,
    pub window_id: Option<window::Id>,
    pub settings_window_id: Option<window::Id>,
    pub search_query: String,
    pub tray_icon: TrayIcon,
    pub is_recording_hotkey: bool,
    pub recorded_hotkey: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    HotkeyTriggered,
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    WindowUnfocused(window::Id),
    SettingsWindowOpened(window::Id),
    SearchChanged(String),
    SelectSourceUnit(String),
    ToggleFavorite(String),
    Swap(f64, String),
    CloseWindow,
    OpenSettings,
    ToggleHistory(bool),
    HistoryRetentionChanged(HistoryRetention),
    OpenHistoryFolder,
    FiatIntervalChanged(String),
    CryptoIntervalChanged(String),
    StartHotkeyRecording,
    CancelHotkeyRecording,
    HotkeyRecorded(String),
    KeyPressed(iced::keyboard::Key, iced::keyboard::Modifiers),
    SaveConfig,
    ExitRequested,
    SpawnWorkers,
}

/// Parameters for booting the UI.
pub struct BootParams {
    pub config: Config,
    pub db: Db,
}

/// The title of the application.
#[must_use]
pub fn title() -> String {
    String::from("Clippy Converter")
}

/// Initializes the application state.
///
/// # Panics
/// Panics if the clipboard, mouse controller, hotkey manager, or tray icon fails to initialize.   
#[expect(
    clippy::expect_used,
    reason = "Critical infrastructure failure at startup is non-recoverable"
)]
pub fn boot(params: BootParams) -> (State, Task<Message>) {
    let BootParams { config, db } = params;
    let converter = Converter::new(config.clone(), db.clone());

    // Infrastructure
    let clipboard = ClipboardManager::new().expect("Failed to initialize clipboard");
    let enigo = Enigo::new(&EnigoSettings::default()).expect("Failed to initialize enigo");        

    // Hotkeys
    let hotkey_manager = GlobalHotKeyManager::new().expect("Failed to initialize hotkey manager"); 
    let hk = hotkey::parse_hotkey(&config.hotkey).expect("Failed to parse hotkey");
    if let Err(e) = hotkey_manager.register(hk) {
        eprintln!("Warning: Failed to register hotkey {}: {}", config.hotkey, e);
    }

    // Tray Icon
    let tray_menu = Menu::with_items(&[
        &MenuItem::with_id("settings", "Settings", true, None),
        &MenuItem::with_id("quit", "Quit Clippy Converter", true, None),
    ])
    .expect("Failed to create tray menu");

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("Clippy Converter")
        .build()
        .expect("Failed to create tray icon");

    (
        State {
            config,
            db,
            converter,
            clipboard,
            enigo,
            hotkey_manager,
            hotkey_id: hk,
            current_result: None,
            captured_value: 0.0,
            captured_unit: None,
            window_id: None,
            settings_window_id: None,
            search_query: String::new(),
            tray_icon,
            is_recording_hotkey: false,
            recorded_hotkey: None,
        },
        Task::done(Message::SpawnWorkers),
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "Centralized update logic for complex UI state"
)]
pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::SpawnWorkers => {
            let db = state.db.clone();
            let config = state.config.clone();
            tokio::spawn(crate::workers::start_fiat_worker(db.clone(), config.clone()));
            tokio::spawn(crate::workers::start_crypto_worker(db, config));
            Task::none()
        }
        Message::HotkeyTriggered => handle_hotkey(state),
        Message::WindowOpened(id) => {
            state.window_id = Some(id);
            Task::none()
        }
        Message::WindowClosed(id) => {
            if state.window_id == Some(id) {
                state.window_id = None;
            }
            if state.settings_window_id == Some(id) {
                state.settings_window_id = None;
            }
            Task::none()
        }
        Message::WindowUnfocused(id) => {
            if state.window_id == Some(id) {
                return window::close(id);
            }
            Task::none()
        }
        Message::SettingsWindowOpened(id) => {
            state.settings_window_id = Some(id);
            Task::none()
        }
        Message::SearchChanged(query) => {
            state.search_query = query;
            Task::none()
        }
        Message::SelectSourceUnit(unit) => {
            if let Ok(result) = state.converter.convert(state.captured_value, &unit) {
                state.current_result = Some(result);
                state.captured_unit = Some(unit);
                state.search_query = String::new();
                log_conversion_if_enabled(state);
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
            state.converter = Converter::new(state.config.clone(), state.db.clone());
            Task::none()
        }
        Message::Swap(value, unit) => {
            if let Ok(result) = state.converter.convert(value, &unit) {
                state.current_result = Some(result);
                state.search_query = String::new();
                log_conversion_if_enabled(state);
            }
            Task::none()
        }
        Message::CloseWindow => state.window_id.map_or_else(Task::none, window::close),
        Message::OpenSettings => {
            if state.settings_window_id.is_some() {
                return Task::none();
            }
            let (_, open_task) = window::open(window::Settings {
                size: (400.0, 500.0).into(),
                decorations: true,
                ..Default::default()
            });
            open_task.map(Message::SettingsWindowOpened)
        }
        Message::ToggleHistory(enabled) => {
            state.config.history_enabled = enabled;
            Task::none()
        }
        Message::HistoryRetentionChanged(retention) => {
            state.config.history_retention = retention;
            Task::none()
        }
        Message::OpenHistoryFolder => {
            if let Ok(path) = crate::history::get_history_path()
                && let Some(parent) = path.parent()
            {
                let _ = open::that(parent);
            }
            Task::none()
        }
        Message::FiatIntervalChanged(val) => {
            if let Ok(mins) = val.parse::<u64>() {
                state.config.fiat_update_interval_mins = mins.max(1);
            }
            Task::none()
        }
        Message::CryptoIntervalChanged(val) => {
            if let Ok(mins) = val.parse::<u64>() {
                state.config.crypto_update_interval_mins = mins.max(1);
            }
            Task::none()
        }
        Message::StartHotkeyRecording => {
            state.is_recording_hotkey = true;
            state.recorded_hotkey = None;
            let _ = state.hotkey_manager.unregister(state.hotkey_id);
            Task::none()
        }
        Message::CancelHotkeyRecording => {
            state.is_recording_hotkey = false;
            state.recorded_hotkey = None;
            let _ = state.hotkey_manager.register(state.hotkey_id);
            Task::none()
        }
        Message::HotkeyRecorded(val) => {
            state.recorded_hotkey = Some(val);
            state.is_recording_hotkey = false;
            let _ = state.hotkey_manager.register(state.hotkey_id);
            Task::none()
        }
        Message::KeyPressed(key, modifiers) => {
            if state.is_recording_hotkey {
                if let Some(hotkey_str) = format_hotkey(&key, modifiers) {
                    state.recorded_hotkey = Some(hotkey_str);
                    state.is_recording_hotkey = false;
                    let _ = state.hotkey_manager.register(state.hotkey_id);
                } else if matches!(
                    key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                ) {
                    state.is_recording_hotkey = false;
                    state.recorded_hotkey = None;
                    let _ = state.hotkey_manager.register(state.hotkey_id);
                }
            } else if matches!(
                key,
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
            ) {
                return state.window_id.map_or_else(Task::none, window::close);
            }
            Task::none()
        }
        Message::SaveConfig => {
            if let Some(recorded) = state.recorded_hotkey.take() {
                state.config.hotkey = recorded;
            }
            let _ = state.config.save();
            // Re-register hotkey if it changed
            if let Ok(hk) = hotkey::parse_hotkey(&state.config.hotkey)
                && hk != state.hotkey_id
            {
                let _ = state.hotkey_manager.unregister(state.hotkey_id);
                if state.hotkey_manager.register(hk).is_ok() {
                    state.hotkey_id = hk;
                }
            }
            Task::none()
        }
        Message::ExitRequested => iced::exit(),
    }
}

fn log_conversion_if_enabled(state: &State) {
    if state.config.history_enabled
        && let Some(result) = &state.current_result
    {
        // Log first output as representative
        if let Some(first_output) = result.outputs.first() {
            let input_val = result.input_value;
            let input_unit = result.input_unit.clone();
            let out_val = first_output.value;
            let out_unit = first_output.unit.clone();
            let retention = state.config.history_retention;

            tokio::spawn(async move {
                let _ = crate::history::log_conversion(
                    input_val,
                    &input_unit,
                    out_val,
                    &out_unit,
                    retention.to_days(),
                )
                .await;
            });
        }
    }
}

fn handle_hotkey(state: &mut State) -> Task<Message> {
    println!("Hotkey triggered!");
    if let Ok(text) = state.clipboard.capture_selection() {
        println!("Captured text: '{text}'");
        if let Ok(parsed) = crate::parser::parse_input(&text) {
            println!("Parsed value: {:.2}, unit: {:?}", parsed.value, parsed.unit);

            state.captured_value = parsed.value;
            state.captured_unit.clone_from(&parsed.unit);
            state.search_query = String::new();

            // Try to convert immediately if unit is present
            if let Some(ref unit) = parsed.unit {
                if let Ok(result) = state.converter.convert(parsed.value, unit) {
                    state.current_result = Some(result);
                    log_conversion_if_enabled(state);
                } else {
                    state.current_result = None;
                }
            } else {
                state.current_result = None;
            }

            let (x, y) = state.enigo.location().unwrap_or((100, 100));
            println!("Opening window at {x}, {y}");

            #[expect(
                clippy::cast_precision_loss,
                reason = "Screen coordinates fit in f32 mantissa"
            )]
            let settings = window::Settings {
                size: (350.0, 400.0).into(),
                position: window::Position::Specific(iced::Point::new(x as f32, y as f32)),        
                decorations: false,
                transparent: true,
                level: window::Level::AlwaysOnTop,
                platform_specific: iced::window::settings::PlatformSpecific {
                    skip_taskbar: true,
                    ..Default::default()
                },
                ..Default::default()
            };

            // If a window is already open, close it first
            if let Some(id) = state.window_id {
                return window::close::<Message>(id).then(move |_| {
                    window::open(settings.clone()).1.map(Message::WindowOpened)
                });
            }
            return window::open(settings).1.map(Message::WindowOpened);
        }
    }
    Task::none()
}

#[allow(
    clippy::too_many_lines,
    reason = "View logic for multi-window application"
)]
#[allow(
    clippy::option_if_let_else,
    reason = "if let Some is more readable for complex UI branching"
)]
#[must_use]
pub fn view(state: &State, window_id: window::Id) -> Element<'_, Message> {
    if state.settings_window_id == Some(window_id) {
        return view_settings(state);
    }

    let search_query_lower = state.search_query.to_lowercase();
    let content = if let Some(result) = &state.current_result {
        column![
            row![
                text(format!("{:.2} {}", result.input_value, result.input_unit))
                    .size(24)
                    .color(Color::WHITE)
                    .width(Length::Fill),
                button(text("×").color(Color::WHITE))
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
                    result
                        .outputs
                        .iter()
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
                                        button(text("⇌"))
                                            .on_press(Message::Swap(
                                                output.value,
                                                output.unit.clone()
                                            ))
                                            .padding(5),
                                        button(text(favorite_label))
                                            .on_press(Message::ToggleFavorite(output.unit.clone()))
                                            .padding(5)
                                    ]
                                    .spacing(5)
                                ]
                                .align_y(Alignment::Center),
                            )
                            .padding(10)
                            .style(|_theme: &Theme| container::Style {
                                border: iced::Border {
                                    color: Color::from_rgba8(255, 255, 255, 0.1),
                                    width: 1.0,
                                    radius: 4.0.into(),
                                },
                                ..Default::default()
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
        let all_units = state.converter.get_all_units().unwrap_or_default();

        column![
            row![
                text(format!("Convert {:.4} ...", state.captured_value))
                    .size(24)
                    .color(Color::WHITE)
                    .width(Length::Fill),
                button(text("×").color(Color::WHITE))
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
                    all_units
                        .into_iter()
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
        .style(|_theme: &Theme| container::Style {
            background: Some(Color::from_rgba8(30, 30, 30, 0.98).into()),
            border: iced::Border {
                color: Color::from_rgb8(60, 60, 60),
                width: 1.0,
                radius: 12.0.into(),
            },
            ..Default::default()
        })
        .into()
}

pub fn subscription(_state: &State) -> Subscription<Message> {
    let hotkey_sub = Subscription::run(|| {
        iced::stream::channel(100, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
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

    let keyboard_sub =
        iced::keyboard::listen().filter_map(|event| {
            if let iced::keyboard::Event::KeyPressed { key, modifiers, .. } = event {
                return Some(Message::KeyPressed(key, modifiers));
            }
            None
        });  

    let tray_sub = Subscription::run(|| {
        iced::stream::channel(100, |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            let receiver = MenuEvent::receiver();
            loop {
                if let Ok(event) = receiver.try_recv() {
                    use iced::futures::SinkExt;
                    match event.id.0.as_str() {
                        "quit" => {
                            let _ = output.send(Message::ExitRequested).await;
                        }
                        "settings" => {
                            let _ = output.send(Message::OpenSettings).await;
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    });

    let blur_sub = iced::event::listen_with(|event, _status, id| match event {
        iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::WindowUnfocused(id)), 
        iced::Event::Window(iced::window::Event::Closed) => Some(Message::WindowClosed(id)),       
        _ => None,
    });

    Subscription::batch(vec![hotkey_sub, keyboard_sub, tray_sub, blur_sub])
}

fn format_hotkey(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<String> {
    let mut parts = Vec::new();
    // On Windows/Linux, iced::Modifiers::command() refers to Control.
    // We use logo() for the OS key (Meta/Win/Super) and explicit control() for Ctrl.
    if modifiers.logo() {
        parts.push("Meta");
    }
    if modifiers.control() {
        parts.push("Ctrl");
    }
    if modifiers.alt() {
        parts.push("Alt");
    }
    if modifiers.shift() {
        parts.push("Shift");
    }

    let key_str = match key {
        iced::keyboard::Key::Character(c) => c.to_uppercase(),
        iced::keyboard::Key::Named(named) => match named {
            iced::keyboard::key::Named::Enter => "Enter".to_string(),
            iced::keyboard::key::Named::Tab => "Tab".to_string(),
            iced::keyboard::key::Named::Space => "Space".to_string(),
            iced::keyboard::key::Named::Escape => "Escape".to_string(),
            iced::keyboard::key::Named::Backspace => "Backspace".to_string(),
            iced::keyboard::key::Named::Delete => "Delete".to_string(),
            iced::keyboard::key::Named::Insert => "Insert".to_string(),
            iced::keyboard::key::Named::Home => "Home".to_string(),
            iced::keyboard::key::Named::End => "End".to_string(),
            iced::keyboard::key::Named::PageUp => "PageUp".to_string(),
            iced::keyboard::key::Named::PageDown => "PageDown".to_string(),
            iced::keyboard::key::Named::ArrowUp => "Up".to_string(),
            iced::keyboard::key::Named::ArrowDown => "Down".to_string(),
            iced::keyboard::key::Named::ArrowLeft => "Left".to_string(),
            iced::keyboard::key::Named::ArrowRight => "Right".to_string(),
            _ => return None,
        },
        iced::keyboard::Key::Unidentified => return None,
    };

    if key_str.is_empty() {
        return None;
    }

    // Check if key_str is a modifier itself (to avoid "Ctrl+Ctrl")
    // Note: Iced's Named variant for modifiers are different,
    // but Character might catch some if something weird happens.
    // More importantly, we don't want to return just "Ctrl" as a hotkey usually,
    // though global-hotkey might allow it. But our parser expects at least one non-modifier.      
    if matches!(
        key_str.as_str(),
        "Ctrl" | "Alt" | "Shift" | "Meta" | "Control" | "Command" | "Win"
    ) {
        return None;
    }

    parts.push(&key_str);
    Some(parts.join("+"))
}

impl std::fmt::Display for HistoryRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SevenDays => write!(f, "7 Days"),
            Self::ThirtyDays => write!(f, "30 Days"),
            Self::OneYear => write!(f, "1 Year"),
            Self::Never => write!(f, "Never"),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn view_settings(state: &State) -> Element<'_, Message> {
    let hotkey_label = if state.is_recording_hotkey {
        "Recording... (Esc to cancel)".to_string()
    } else {
        state
            .recorded_hotkey
            .as_ref()
            .unwrap_or(&state.config.hotkey)
            .clone()
    };

    let content = column![
        text("Settings").size(24).color(Color::WHITE),
        column![
            text("Global Hotkey")
                .size(16)
                .color(Color::from_rgb8(200, 200, 200)),
            button(text(hotkey_label).color(if state.is_recording_hotkey {
                Color::from_rgb8(255, 100, 100)
            } else {
                Color::WHITE
            }))
            .on_press(Message::StartHotkeyRecording)
            .padding(10)
            .width(Length::Fill)
            .style(button::secondary),
        ]
        .spacing(5),
        column![
            checkbox(state.config.history_enabled)
                .label("Enable History Logging")
                .on_toggle(Message::ToggleHistory)
                .size(20),
            if state.config.history_enabled {
                column![
                    text("Retention Period")
                        .size(14)
                        .color(Color::from_rgb8(150, 150, 150)),
                    pick_list(
                        &[
                            HistoryRetention::SevenDays,
                            HistoryRetention::ThirtyDays,
                            HistoryRetention::OneYear,
                            HistoryRetention::Never
                        ][..],
                        Some(state.config.history_retention),
                        Message::HistoryRetentionChanged,
                    )
                    .width(Length::Fill),
                ]
                .spacing(5)
                .into()
            } else {
                Element::from(column![])
            },
            button(text("Open History Folder").color(Color::WHITE))
                .on_press(Message::OpenHistoryFolder)
                .padding(10)
                .style(button::secondary),
        ]
        .spacing(10),
        column![
            text("Update Intervals (minutes)")
                .size(16)
                .color(Color::from_rgb8(200, 200, 200)),
            row![
                column![
                    text("Fiat").size(12).color(Color::from_rgb8(150, 150, 150)),
                    text_input("1440", &state.config.fiat_update_interval_mins.to_string())        
                        .on_input(Message::FiatIntervalChanged)
                        .padding(10),
                ]
                .width(Length::Fill),
                column![
                    text("Crypto")
                        .size(12)
                        .color(Color::from_rgb8(150, 150, 150)),
                    text_input("1", &state.config.crypto_update_interval_mins.to_string())
                        .on_input(Message::CryptoIntervalChanged)
                        .padding(10),
                ]
                .width(Length::Fill),
            ]
            .spacing(20),
            text("Note: Crypto defaults to 1 min, Fiat to 24h (1440 min).")
                .size(12)
                .color(Color::from_rgb8(100, 100, 100)),
        ]
        .spacing(5),
        button(text("Save & Apply").color(Color::WHITE))
            .on_press(Message::SaveConfig)
            .padding(12)
            .width(Length::Fill)
            .style(button::primary),
    ]
    .spacing(25)
    .padding(20);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(Color::from_rgb8(30, 30, 30).into()),
            ..Default::default()
        })
        .into()
}
