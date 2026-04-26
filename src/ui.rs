use crate::clipboard::ClipboardManager;
use crate::converter::Converter;
use crate::db::Db;
use crate::hotkey;
use crate::models::{Config, ConversionResult, HistoryRetention};
use anyhow::{Context, Result};
use eframe::egui;
use enigo::{Enigo, Mouse, Settings as EnigoSettings};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::mpsc::{self, Receiver};
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    ValueInput,
    SourceUnitSelection,
    Results,
}

pub enum EventMsg {
    HotkeyTriggered,
    OpenSettings,
    Exit,
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

#[allow(clippy::struct_excessive_bools)]
pub struct AppState {
    pub config: Config,
    pub db: Db,
    pub converter: Converter,
    pub clipboard: ClipboardManager,
    pub enigo: Enigo,
    pub hotkey_manager: GlobalHotKeyManager,
    pub hotkey_id: global_hotkey::hotkey::HotKey,
    pub tray_icon: TrayIcon,
    pub event_rx: Receiver<EventMsg>,

    pub current_result: Option<ConversionResult>,
    pub captured_value: f64,
    pub search_query: String,
    pub search_query_lower: String,
    pub manual_input_value: String,
    pub current_mode: WindowMode,

    pub is_recording_hotkey: bool,
    pub recorded_hotkey: Option<String>,

    pub main_window_open: bool,
    pub main_window_pos: egui::Pos2,
    pub settings_window_open: bool,

    pub focus_main_input: bool,
    pub main_window_was_focused: bool,

    pub config_fiat_interval_str: String,
    pub config_crypto_interval_str: String,
}

/// Runs the eframe application.
///
/// # Errors
/// Returns an error if eframe or required services fail to initialize.
///
/// # Panics
/// Panics if the tokio runtime cannot be created.
#[allow(clippy::too_many_lines)]
#[expect(
    clippy::expect_used,
    reason = "Critical infrastructure failure at startup is non-recoverable"
)]
pub fn run(config: Config, db: Db) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_visible(false)
            .with_taskbar(false),
        run_and_return: false,
        ..Default::default()
    };

    let converter = Converter::new(config.clone(), db.clone());
    let clipboard = ClipboardManager::new().context("Failed to initialize clipboard")?;
    let enigo = Enigo::new(&EnigoSettings::default()).context("Failed to initialize enigo")?;
    let hotkey_manager =
        GlobalHotKeyManager::new().context("Failed to initialize hotkey manager")?;
    let hk = hotkey::parse_hotkey(&config.hotkey).context("Failed to parse hotkey")?;
    if let Err(e) = hotkey_manager.register(hk) {
        eprintln!(
            "Warning: Failed to register hotkey {}: {}",
            config.hotkey, e
        );
    }

    let tray_menu = Menu::with_items(&[
        &MenuItem::with_id("settings", "Settings", true, None),
        &MenuItem::with_id("quit", "Quit Clippy Converter", true, None),
    ])
    .context("Failed to create tray menu")?;

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("Clippy Converter")
        .build()
        .context("Failed to create tray icon")?;

    let fiat_str = config.fiat_update_interval_mins.to_string();
    let crypto_str = config.crypto_update_interval_mins.to_string();

    let (tx, rx) = mpsc::channel();

    // Spawn tokio runtime for background workers
    std::thread::spawn({
        let db_fiat = db.clone();
        let config_fiat = config.clone();
        let db_crypto = db.clone();
        let config_crypto = config.clone();

        move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(async {
                tokio::spawn(crate::workers::start_fiat_worker(db_fiat, config_fiat));
                tokio::spawn(crate::workers::start_crypto_worker(
                    db_crypto,
                    config_crypto,
                ));
                std::future::pending::<()>().await;
            });
        }
    });

    eframe::run_native(
        "Clippy Converter",
        options,
        Box::new(move |cc| {
            let tx_hk = tx.clone();
            let ctx_hk = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                let receiver = GlobalHotKeyEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.state == HotKeyState::Pressed {
                        let _ = tx_hk.send(EventMsg::HotkeyTriggered);
                        ctx_hk.request_repaint();
                    }
                }
            });

            let tx_tray = tx.clone();
            let ctx_tray = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                let receiver = MenuEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    match event.id.0.as_str() {
                        "quit" => {
                            let _ = tx_tray.send(EventMsg::Exit);
                            ctx_tray.request_repaint();
                        }
                        "settings" => {
                            let _ = tx_tray.send(EventMsg::OpenSettings);
                            ctx_tray.request_repaint();
                        }
                        _ => {}
                    }
                }
            });

            Ok(Box::new(AppState {
                config,
                db,
                converter,
                clipboard,
                enigo,
                hotkey_manager,
                hotkey_id: hk,
                tray_icon,
                event_rx: rx,

                current_result: None,
                captured_value: 0.0,
                search_query: String::new(),
                search_query_lower: String::new(),
                manual_input_value: String::new(),
                current_mode: WindowMode::SourceUnitSelection,

                is_recording_hotkey: false,
                recorded_hotkey: None,

                main_window_open: false,
                main_window_pos: egui::Pos2::ZERO,
                settings_window_open: false,

                focus_main_input: false,
                main_window_was_focused: false,

                config_fiat_interval_str: fiat_str,
                config_crypto_interval_str: crypto_str,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.run_logic(ctx, frame);
    }

    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Handled via update
    }
}

impl AppState {
    fn run_logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.event_rx.try_recv() {
            match msg {
                EventMsg::Exit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                EventMsg::OpenSettings => {
                    self.settings_window_open = true;
                }
                EventMsg::HotkeyTriggered => {
                    self.handle_hotkey(ctx);
                }
            }
        }

        if self.settings_window_open {
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("settings"),
                egui::ViewportBuilder::default()
                    .with_title("Settings")
                    .with_inner_size([400.0, 500.0]),
                |ctx, _class| {
                    if ctx.input(|i| i.viewport().close_requested()) {
                        self.settings_window_open = false;
                    }
                    #[allow(deprecated)]
                    egui::CentralPanel::default().show(ctx, |ui| {
                        self.render_settings(ui, ctx);
                    });
                },
            );
        }

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("main"),
            egui::ViewportBuilder::default()
                .with_title("Clippy Converter")
                .with_decorations(false)
                .with_transparent(true)
                .with_always_on_top()
                .with_taskbar(false)
                .with_visible(false)
                .with_inner_size([350.0, 400.0])
                .with_position(self.main_window_pos),
            |ctx, _class| {
                if !self.main_window_open {
                    return;
                }

                let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
                if !focused && self.main_window_was_focused {
                    self.main_window_open = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
                self.main_window_was_focused = focused;

                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.main_window_open = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }

                let frame = egui::Frame {
                    fill: egui::Color32::from_rgba_unmultiplied(30, 30, 30, 250),
                    stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 60)),
                    corner_radius: egui::CornerRadius::same(12),
                    inner_margin: egui::Margin::same(20),
                    ..Default::default()
                };

                #[allow(deprecated)]
                egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                    self.render_main_window(ui, ctx);
                });
            },
        );
    }

    #[expect(
        clippy::unwrap_used,
        reason = "tokio runtime creation in simple thread is expected to succeed"
    )]
    fn log_conversion_if_enabled(&self) {
        if self.config.history_enabled
            && let Some(result) = &self.current_result
            && let Some(first_output) = result.outputs.first()
        {
            let input_val = result.input_value;
            let input_unit = result.input_unit.clone();
            let out_val = first_output.value;
            let out_unit = first_output.unit.clone();
            let retention = self.config.history_retention;

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    let _ = crate::history::log_conversion(
                        input_val,
                        &input_unit,
                        out_val,
                        &out_unit,
                        retention.to_days(),
                    )
                    .await;
                });
            });
        }
    }

    fn handle_hotkey(&mut self, ctx: &egui::Context) {
        let parsed_opt = self
            .clipboard
            .capture_selection()
            .ok()
            .and_then(|text| crate::parser::parse_input(&text).ok());

        if let Some(parsed) = parsed_opt {
            self.captured_value = parsed.value;
            if let Some(ref unit) = parsed.unit {
                if let Ok(result) = self.converter.convert(parsed.value, unit) {
                    self.current_result = Some(result);
                    self.current_mode = WindowMode::Results;
                    self.log_conversion_if_enabled();
                } else {
                    self.current_result = None;
                    self.current_mode = WindowMode::SourceUnitSelection;
                }
            } else {
                self.current_result = None;
                self.current_mode = WindowMode::SourceUnitSelection;
            }
        } else {
            self.captured_value = 0.0;
            self.current_result = None;
            self.current_mode = WindowMode::ValueInput;
            self.manual_input_value = String::new();
        }

        self.search_query = String::new();
        self.search_query_lower = String::new();

        let (x, y) = self.enigo.location().unwrap_or((100, 100));

        #[expect(
            clippy::cast_precision_loss,
            reason = "Screen coordinates fit in f32 mantissa"
        )]
        {
            self.main_window_pos = egui::pos2(x as f32, y as f32);
        }

        self.main_window_open = true;
        self.main_window_was_focused = false;
        self.focus_main_input = true;

        let main_viewport_id = egui::ViewportId::from_hash_of("main");
        ctx.send_viewport_cmd_to(
            main_viewport_id,
            egui::ViewportCommand::OuterPosition(self.main_window_pos),
        );
        ctx.send_viewport_cmd_to(main_viewport_id, egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd_to(main_viewport_id, egui::ViewportCommand::Focus);
    }

    #[allow(clippy::too_many_lines)]
    fn render_settings(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        if self.is_recording_hotkey {
            ui.ctx().input(|i| {
                for event in &i.events {
                    if let egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } = event
                    {
                        if *key == egui::Key::Escape {
                            self.is_recording_hotkey = false;
                            self.recorded_hotkey = None;
                            let _ = self.hotkey_manager.register(self.hotkey_id);
                        } else if let Some(hk) = format_hotkey(*key, *modifiers) {
                            self.recorded_hotkey = Some(hk);
                            self.is_recording_hotkey = false;
                            let _ = self.hotkey_manager.register(self.hotkey_id);
                        }
                    }
                }
            });
        }

        ui.heading("Settings");
        ui.separator();

        ui.label("Global Hotkey");
        let hotkey_label = if self.is_recording_hotkey {
            "Recording... (Esc to cancel)".to_string()
        } else {
            self.recorded_hotkey
                .as_ref()
                .unwrap_or(&self.config.hotkey)
                .clone()
        };

        if ui.button(hotkey_label).clicked() {
            self.is_recording_hotkey = true;
            self.recorded_hotkey = None;
            let _ = self.hotkey_manager.unregister(self.hotkey_id);
        }

        ui.separator();

        ui.checkbox(&mut self.config.history_enabled, "Enable History Logging");
        if self.config.history_enabled {
            egui::ComboBox::from_label("Retention Period")
                .selected_text(self.config.history_retention.to_string())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.config.history_retention,
                        HistoryRetention::SevenDays,
                        "7 Days",
                    );
                    ui.selectable_value(
                        &mut self.config.history_retention,
                        HistoryRetention::ThirtyDays,
                        "30 Days",
                    );
                    ui.selectable_value(
                        &mut self.config.history_retention,
                        HistoryRetention::OneYear,
                        "1 Year",
                    );
                    ui.selectable_value(
                        &mut self.config.history_retention,
                        HistoryRetention::Never,
                        "Never",
                    );
                });
        }

        if ui.button("Open History Folder").clicked()
            && let Ok(path) = crate::history::get_history_path()
            && let Some(parent) = path.parent()
        {
            let _ = open::that(parent);
        }

        ui.separator();
        ui.label("Update Intervals (minutes)");
        ui.horizontal(|ui| {
            ui.label("Fiat:");
            ui.text_edit_singleline(&mut self.config_fiat_interval_str);
            ui.label("Crypto:");
            ui.text_edit_singleline(&mut self.config_crypto_interval_str);
        });

        ui.separator();
        if ui.button("Save & Apply").clicked() {
            if let Ok(mins) = self.config_fiat_interval_str.parse::<u64>() {
                self.config.fiat_update_interval_mins = mins.max(1);
            }
            if let Ok(mins) = self.config_crypto_interval_str.parse::<u64>() {
                self.config.crypto_update_interval_mins = mins.max(1);
            }
            if let Some(recorded) = self.recorded_hotkey.take() {
                self.config.hotkey = recorded;
            }
            let _ = self.config.save();
            if let Ok(hk) = hotkey::parse_hotkey(&self.config.hotkey)
                && hk != self.hotkey_id
            {
                let _ = self.hotkey_manager.unregister(self.hotkey_id);
                if self.hotkey_manager.register(hk).is_ok() {
                    self.hotkey_id = hk;
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn render_main_window(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let header_response = ui
            .horizontal(|ui| {
                match self.current_mode {
                    WindowMode::ValueInput => {
                        ui.heading("Enter number to convert");
                    }
                    WindowMode::SourceUnitSelection => {
                        ui.heading("Convert ");
                        if ui.button(format!("{:.4}", self.captured_value)).clicked() {
                            self.current_mode = WindowMode::ValueInput;
                            self.manual_input_value = self.captured_value.to_string();
                            self.focus_main_input = true;
                        }
                        ui.heading(" ...");
                    }
                    WindowMode::Results => {
                        if let Some(res) = &self.current_result {
                            if ui.button(format!("{:.2}", res.input_value)).clicked() {
                                self.current_mode = WindowMode::ValueInput;
                                self.manual_input_value = self.captured_value.to_string();
                                self.focus_main_input = true;
                            }
                            ui.heading(" ");
                            if ui.button(&res.input_unit).clicked() {
                                self.current_mode = WindowMode::SourceUnitSelection;
                                self.search_query.clear();
                                self.search_query_lower.clear();
                                self.focus_main_input = true;
                            }
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("×").clicked() {
                        self.main_window_open = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }
                });
            })
            .response;

        let header_interact = ui.interact(
            header_response.rect,
            ui.id().with("header"),
            egui::Sense::drag(),
        );
        if header_interact.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        ui.add_space(10.0);

        if self.current_mode == WindowMode::ValueInput {
            let response = ui.text_edit_singleline(&mut self.manual_input_value);
            if self.focus_main_input {
                response.request_focus();
                self.focus_main_input = false;
            }
            if response.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && let Ok(val) = self.manual_input_value.parse::<f64>()
            {
                self.captured_value = val;
                self.current_mode = WindowMode::SourceUnitSelection;
                self.search_query.clear();
                self.search_query_lower.clear();
                self.focus_main_input = true;
            }
        } else {
            let response = ui.text_edit_singleline(&mut self.search_query);
            if response.changed() {
                self.search_query_lower = self.search_query.to_lowercase();
            }
            if self.focus_main_input {
                response.request_focus();
                self.focus_main_input = false;
            }

            if self.current_mode == WindowMode::SourceUnitSelection {
                if response.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && !self.search_query_lower.is_empty()
                {
                    let all_units = self.converter.get_all_units().unwrap_or_default();
                    let exact = all_units.iter().find(|u| {
                        u.symbol.to_lowercase() == self.search_query_lower
                            || u.aliases
                                .iter()
                                .any(|a| a.to_lowercase() == self.search_query_lower)
                    });
                    let partial = all_units.iter().find(|u| {
                        u.symbol.to_lowercase().contains(&self.search_query_lower)
                            || u.aliases
                                .iter()
                                .any(|a| a.to_lowercase().contains(&self.search_query_lower))
                    });

                    if let Some(unit) = exact.or(partial)
                        && let Ok(result) =
                            self.converter.convert(self.captured_value, &unit.symbol)
                    {
                        self.current_result = Some(result);
                        self.current_mode = WindowMode::Results;
                        self.search_query.clear();
                        self.search_query_lower.clear();
                        self.log_conversion_if_enabled();
                        self.focus_main_input = true;
                    }
                }

                let all_units = self.converter.get_all_units().unwrap_or_default();
                let matching_units: Vec<_> = all_units
                    .into_iter()
                    .filter(|u| {
                        u.symbol.to_lowercase().contains(&self.search_query_lower)
                            || u.aliases
                                .iter()
                                .any(|a| a.to_lowercase().contains(&self.search_query_lower))
                    })
                    .take(self.config.list_size)
                    .collect();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for unit in matching_units {
                        let aliases_str = if unit.aliases.is_empty() {
                            String::new()
                        } else {
                            format!("({})", unit.aliases.join(", "))
                        };

                        if ui
                            .button(format!("{} {}", unit.symbol, aliases_str))
                            .clicked()
                            && let Ok(result) =
                                self.converter.convert(self.captured_value, &unit.symbol)
                        {
                            self.current_result = Some(result);
                            self.current_mode = WindowMode::Results;
                            self.search_query.clear();
                            self.search_query_lower.clear();
                            self.log_conversion_if_enabled();
                            self.focus_main_input = true;
                        }
                    }
                });
            } else if self.current_mode == WindowMode::Results {
                let outputs = if let Some(result) = &self.current_result {
                    result
                        .outputs
                        .iter()
                        .filter(|o| o.unit.to_lowercase().contains(&self.search_query_lower))
                        .take(self.config.list_size)
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };

                if !outputs.is_empty() {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for output in outputs {
                            let is_favorite = self.config.favorites.contains(&output.unit);
                            let favorite_label = if is_favorite { "★" } else { "☆" };

                            ui.horizontal(|ui| {
                                ui.label(format!("{:.4}", output.value));
                                ui.label(&output.unit);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button(favorite_label).clicked() {
                                            if let Some(pos) = self
                                                .config
                                                .favorites
                                                .iter()
                                                .position(|f| f == &output.unit)
                                            {
                                                self.config.favorites.remove(pos);
                                            } else {
                                                self.config.favorites.push(output.unit.clone());
                                            }
                                            let _ = self.config.save();
                                            self.converter = Converter::new(
                                                self.config.clone(),
                                                self.db.clone(),
                                            );
                                        }
                                        if ui.button("⇌").clicked()
                                            && let Ok(new_res) =
                                                self.converter.convert(output.value, &output.unit)
                                        {
                                            self.current_result = Some(new_res);
                                            self.captured_value = output.value;
                                            self.search_query.clear();
                                            self.search_query_lower.clear();
                                            self.log_conversion_if_enabled();
                                            self.focus_main_input = true;
                                        }
                                    },
                                );
                            });
                        }
                    });
                }
            }
        }
    }
}

#[must_use]
pub fn format_hotkey(key: egui::Key, modifiers: egui::Modifiers) -> Option<String> {
    let mut parts = Vec::new();
    if modifiers.mac_cmd || modifiers.command {
        parts.push("Meta");
    }
    if modifiers.ctrl {
        parts.push("Ctrl");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.shift {
        parts.push("Shift");
    }

    let key_str = match key {
        egui::Key::A => "A",
        egui::Key::B => "B",
        egui::Key::C => "C",
        egui::Key::D => "D",
        egui::Key::E => "E",
        egui::Key::F => "F",
        egui::Key::G => "G",
        egui::Key::H => "H",
        egui::Key::I => "I",
        egui::Key::J => "J",
        egui::Key::K => "K",
        egui::Key::L => "L",
        egui::Key::M => "M",
        egui::Key::N => "N",
        egui::Key::O => "O",
        egui::Key::P => "P",
        egui::Key::Q => "Q",
        egui::Key::R => "R",
        egui::Key::S => "S",
        egui::Key::T => "T",
        egui::Key::U => "U",
        egui::Key::V => "V",
        egui::Key::W => "W",
        egui::Key::X => "X",
        egui::Key::Y => "Y",
        egui::Key::Z => "Z",
        egui::Key::Num0 => "0",
        egui::Key::Num1 => "1",
        egui::Key::Num2 => "2",
        egui::Key::Num3 => "3",
        egui::Key::Num4 => "4",
        egui::Key::Num5 => "5",
        egui::Key::Num6 => "6",
        egui::Key::Num7 => "7",
        egui::Key::Num8 => "8",
        egui::Key::Num9 => "9",
        egui::Key::Enter => "Enter",
        egui::Key::Tab => "Tab",
        egui::Key::Space => "Space",
        egui::Key::Escape => "Escape",
        egui::Key::Backspace => "Backspace",
        egui::Key::Delete => "Delete",
        egui::Key::Insert => "Insert",
        egui::Key::Home => "Home",
        egui::Key::End => "End",
        egui::Key::PageUp => "PageUp",
        egui::Key::PageDown => "PageDown",
        egui::Key::ArrowUp => "Up",
        egui::Key::ArrowDown => "Down",
        egui::Key::ArrowLeft => "Left",
        egui::Key::ArrowRight => "Right",
        _ => return None,
    };

    parts.push(key_str);
    Some(parts.join("+"))
}
