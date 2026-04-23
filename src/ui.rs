use crate::clipboard::ClipboardManager;
use crate::converter::Converter;
use crate::hotkey;
use crate::models::{Cache, Config, ConversionResult};
use enigo::{Enigo, Mouse, Settings as EnigoSettings};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use iced::window;
use iced::{Alignment, Color, Element, Length, Subscription, Task, Theme};
use iced::widget::{column, container, row, text, scrollable};
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
    pub window_id: Option<window::Id>,
}

#[derive(Debug, Clone)]
pub enum Message {
    HotkeyTriggered,
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    CurrencyCacheRefreshed(Cache),
    Tick,
}

pub fn boot() -> (State, Task<Message>) {
    let config = Config::load().unwrap_or_default();
    let cache = Cache::load().unwrap_or_default();
    let converter = Converter::new(config.clone(), cache.clone());
    let clipboard = ClipboardManager::new().expect("Failed to initialize clipboard");
    let enigo = Enigo::new(&EnigoSettings::default()).expect("Failed to initialize enigo");
    
    let hotkey_manager = GlobalHotKeyManager::new().expect("Failed to initialize hotkey manager");
    let hk = hotkey::parse_hotkey(&config.hotkey).expect("Failed to parse hotkey");
    hotkey_manager.register(hk).expect("Failed to register hotkey");

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
            window_id: None,
        },
        Task::none(),
    )
}

pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::HotkeyTriggered => {
            println!("Hotkey triggered!");
            match state.clipboard.capture_selection() {
                Ok(text) => {
                    println!("Captured text: '{}'", text);
                    match crate::parser::parse_input(&text) {
                        Ok(parsed) => {
                            println!("Parsed value: {:.2}, unit: {:?}", parsed.value, parsed.unit);
                            match state.converter.convert(parsed.value, parsed.unit.as_deref().unwrap_or("")) {
                                Ok(result) => {
                                    state.current_result = Some(result);
                                    
                                    // Get mouse position for window placement
                                    let (x, y) = state.enigo.location().unwrap_or((100, 100));
                                    println!("Opening window at {}, {}", x, y);
                                    
                                    let (_, open_task) = window::open(window::Settings {
                                        size: (300.0, 350.0).into(),
                                        position: window::Position::Specific(iced::Point::new(x as f32, y as f32)),
                                        decorations: false,
                                        transparent: true,
                                        level: window::Level::AlwaysOnTop,
                                        ..Default::default()
                                    });
                                    
                                    return open_task.map(Message::WindowOpened);
                                }
                                Err(e) => eprintln!("Conversion error: {}", e),
                            }
                        }
                        Err(e) => eprintln!("Parsing error: {}", e),
                    }
                }
                Err(e) => eprintln!("Clipboard capture error: {}", e),
            }
            Task::none()
        }
        Message::WindowOpened(id) => {
            println!("Window opened with ID: {:?}", id);
            state.window_id = Some(id);
            Task::none()
        }
        Message::WindowClosed(id) => {
            println!("Window closed with ID: {:?}", id);
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
        Message::Tick => {
            // Background refresh check
            let cache_expired = state.cache.is_expired();
            if cache_expired {
                return Task::perform(crate::api::fetch_latest_rates(), |res| {
                    if let Ok(rates) = res {
                        let mut cache = Cache::load().unwrap_or_default();
                        cache.rates = rates;
                        cache.last_updated = chrono::Utc::now();
                        let _ = cache.save();
                        Message::CurrencyCacheRefreshed(cache)
                    } else {
                        // Silently fail background update
                        Message::Tick // Should ideally be a no-op but we need a Message
                    }
                });
            }
            Task::none()
        }
    }
}

pub fn view(state: &State, _window_id: window::Id) -> Element<'_, Message> {
    let content = if let Some(result) = &state.current_result {
        column![
            text(format!("{:.2} {}", result.input_value, result.input_unit))
                .size(24)
                .color(Color::WHITE),
            scrollable(
                column(
                    result.outputs.iter().map(|output| {
                        row![
                            text(format!("{:.2}", output.value))
                                .width(Length::FillPortion(2))
                                .color(Color::WHITE),
                            text(&output.unit)
                                .width(Length::FillPortion(1))
                                .color(Color::from_rgb8(150, 150, 150))
                        ]
                        .spacing(10)
                        .padding(5)
                        .into()
                    }).collect::<Vec<_>>()
                )
            )
        ]
        .spacing(20)
        .align_x(Alignment::Start)
    } else {
        column![text("No conversion data").color(Color::WHITE)]
    };

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20)
        .style(|_theme: &Theme| {
            container::Style {
                background: Some(Color::from_rgba8(30, 30, 30, 0.95).into()),
                border: iced::Border {
                    color: Color::from_rgb8(60, 60, 60),
                    width: 1.0,
                    radius: 8.0.into(),
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

    Subscription::batch(vec![hotkey_sub, tick_sub])
}
