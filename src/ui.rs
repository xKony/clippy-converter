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
use std::time::{Duration, Instant};
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

    pub copied_notification: Option<(String, Instant)>,

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
            .with_taskbar(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_inner_size([350.0, 420.0]),
        run_and_return: false,
        vsync: true,
        hardware_acceleration: eframe::HardwareAcceleration::Required,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    let converter = Converter::new(config.clone(), db.clone());
    let clipboard = ClipboardManager::new().context("Failed to initialize clipboard")?;
    let enigo = Enigo::new(&EnigoSettings::default()).context("Failed to initialize enigo")?;
    let hotkey_manager =
        GlobalHotKeyManager::new().context("Failed to initialize hotkey manager")?;
    let hk = hotkey::parse_hotkey(&config.hotkey).context("Failed to parse hotkey")?;
    let _ = hotkey_manager.register(hk);

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
        "Clippy Converter Daemon",
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

            egui_extras::install_image_loaders(&cc.egui_ctx);

            crate::theme::apply_theme(&cc.egui_ctx);

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

                copied_notification: None,

                config_fiat_interval_str: fiat_str,
                config_crypto_interval_str: crypto_str,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

impl eframe::App for AppState {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.run_logic(&ctx, frame, ui);
    }
}

impl AppState {
    fn run_logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame, ui: &mut egui::Ui) {
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

        // Settings as a child viewport (has its own title bar and decorations)
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

        // The main converter popup is the ROOT viewport itself.
        // When not open, hide it. When open, render the converter UI.
        if !self.main_window_open {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));

        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(false));
        if !focused && self.main_window_was_focused {
            self.main_window_open = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }
        self.main_window_was_focused = focused;

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.main_window_open = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }

        // Fill the entire viewport with the popup background color.
        // We set it on the panel directly since wgpu per-pixel transparency
        // is unreliable on Windows — this avoids black areas and corners.
        let bg_color = egui::Color32::from_rgb(30, 30, 30);
        ui.painter()
            .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, bg_color);

        // Add padding via a frame with no extra fill
        let content_frame = egui::Frame {
            fill: egui::Color32::TRANSPARENT,
            inner_margin: egui::Margin::same(16),
            ..Default::default()
        };

        content_frame.show(ui, |ui| {
            self.render_main_window(ui, ctx);
        });


        ctx.request_repaint();
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

        // The converter popup is the root viewport — send commands to ROOT
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(self.main_window_pos));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
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
        // Allocate space for the header first. This allows us to add a drag interaction
        // to the background before we add the actual interactive widgets.
        let header_height = 32.0;
        let (header_rect, _) = ui.allocate_at_least(
            egui::vec2(ui.available_width(), header_height),
            egui::Sense::hover(),
        );

        // Add the drag interaction to the background. Since it's added first,
        // any widgets added on top of this area later will take priority for interactions.
        let drag_response = ui.interact(
            header_rect,
            ui.id().with("header_drag"),
            egui::Sense::drag(),
        );
        if drag_response.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        // Render the header content on top of the background drag area.
        ui.scope_builder(egui::UiBuilder::new().max_rect(header_rect), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                match self.current_mode {
                    WindowMode::ValueInput => {
                        ui.label(egui::RichText::new("Enter value").strong());
                    }
                    WindowMode::SourceUnitSelection => {
                        if ui
                            .button(
                                egui::RichText::new(format!("{:.4}", self.captured_value)).strong(),
                            )
                            .clicked()
                        {
                            self.current_mode = WindowMode::ValueInput;
                            self.manual_input_value = self.captured_value.to_string();
                            self.focus_main_input = true;
                        }
                        ui.label(
                            egui::RichText::new("select unit")
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                    WindowMode::Results => {
                        if let Some(res) = &self.current_result {
                            if ui
                                .button(
                                    egui::RichText::new(format!("{:.2}", res.input_value)).strong(),
                                )
                                .clicked()
                            {
                                self.current_mode = WindowMode::ValueInput;
                                self.manual_input_value = self.captured_value.to_string();
                                self.focus_main_input = true;
                            }
                            if ui
                                .button(egui::RichText::new(&res.input_unit).strong())
                                .clicked()
                            {
                                self.current_mode = WindowMode::SourceUnitSelection;
                                self.search_query.clear();
                                self.search_query_lower.clear();
                                self.focus_main_input = true;
                            }
                        }
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::image(
                            egui::Image::new(egui::include_image!("../icons/close.svg"))
                                .fit_to_exact_size(egui::vec2(16.0, 16.0))
                                .tint(ui.visuals().text_color()),
                        ))
                        .clicked()
                    {
                        self.main_window_open = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }
                });
            });
        });

        ui.add_space(5.0);

        if self.current_mode == WindowMode::ValueInput {
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.manual_input_value)
                        .hint_text("0.00")
                        .font(egui::TextStyle::Heading)
                        .desired_width(f32::INFINITY),
                );
                if self.focus_main_input {
                    response.request_focus();
                    self.focus_main_input = false;
                }
                if ((response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    || ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    && let Ok(val) = self.manual_input_value.parse::<f64>()
                {
                    self.captured_value = val;
                    self.current_mode = WindowMode::SourceUnitSelection;
                    self.search_query.clear();
                    self.search_query_lower.clear();
                    self.focus_main_input = true;
                }
            });
        } else {
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("Search units...")
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    self.search_query_lower = self.search_query.to_lowercase();
                }
                if self.focus_main_input {
                    response.request_focus();
                    self.focus_main_input = false;
                }

                if self.current_mode == WindowMode::SourceUnitSelection
                    && response.lost_focus()
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
            });

            ui.add_space(5.0);

            if self.current_mode == WindowMode::SourceUnitSelection {
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

                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            for unit in matching_units {
                                let aliases_str = if unit.aliases.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({})", unit.aliases.join(", "))
                                };

                                let button_text =
                                    egui::RichText::new(format!("{} {}", unit.symbol, aliases_str));
                                if ui
                                    .add(
                                        egui::Button::new(button_text)
                                            .fill(egui::Color32::TRANSPARENT),
                                    )
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
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                for output in outputs {
                                    let is_favorite = self.config.favorites.contains(&output.unit);
                                    let favorite_icon = if is_favorite {
                                        egui::include_image!("../icons/favorite_on.svg")
                                    } else {
                                        egui::include_image!("../icons/favorite.svg")
                                    };

                                    let tint = if is_favorite {
                                        egui::Color32::from_rgb(255, 215, 0)
                                    } else {
                                        ui.visuals().text_color()
                                    };

                                    ui.add_space(2.0);
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!("{:.4}", output.value))
                                                    .strong()
                                                    .size(18.0),
                                            );
                                            ui.label(
                                                egui::RichText::new(&output.unit)
                                                    .size(14.0)
                                                    .color(ui.visuals().weak_text_color()),
                                            );
                                        });

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add(egui::Button::image(
                                                        egui::Image::new(favorite_icon).tint(tint),
                                                    ))
                                                    .clicked()
                                                {
                                                    if let Some(pos) = self
                                                        .config
                                                        .favorites
                                                        .iter()
                                                        .position(|f| f == &output.unit)
                                                    {
                                                        self.config.favorites.remove(pos);
                                                    } else {
                                                        self.config
                                                            .favorites
                                                            .push(output.unit.clone());
                                                    }
                                                    let _ = self.config.save();
                                                    self.converter = Converter::new(
                                                        self.config.clone(),
                                                        self.db.clone(),
                                                    );
                                                }

                                                if ui
                                                    .add(egui::Button::image(
                                                        egui::Image::new(egui::include_image!(
                                                            "../icons/switch.svg"
                                                        ))
                                                        .tint(ui.visuals().text_color()),
                                                    ))
                                                    .clicked()
                                                    && let Ok(new_res) = self
                                                        .converter
                                                        .convert(output.value, &output.unit)
                                                {
                                                    self.current_result = Some(new_res);
                                                    self.captured_value = output.value;
                                                    self.search_query.clear();
                                                    self.search_query_lower.clear();
                                                    self.log_conversion_if_enabled();
                                                    self.focus_main_input = true;
                                                }

                                                if ui
                                                    .add(egui::Button::image(
                                                        egui::Image::new(egui::include_image!(
                                                            "../icons/copy.svg"
                                                        ))
                                                        .tint(ui.visuals().text_color()),
                                                    ))
                                                    .clicked()
                                                {
                                                    if output.value.is_nan()
                                                        || output.value.is_infinite()
                                                    {
                                                        self.copied_notification = Some((
                                                            "Invalid value".to_string(),
                                                            Instant::now(),
                                                        ));
                                                    } else {
                                                        let val_str = output.value.to_string();
                                                        if self.clipboard.set_text(val_str).is_ok()
                                                        {
                                                            self.copied_notification = Some((
                                                                "Copied!".to_string(),
                                                                Instant::now(),
                                                            ));
                                                        }
                                                    }
                                                }
                                            },
                                        );
                                    });
                                    ui.separator();
                                }
                            });
                        });
                }
            }
        }

        if let Some((msg, time)) = &self.copied_notification {
            if Instant::now().duration_since(*time) > Duration::from_secs(2) {
                self.copied_notification = None;
            } else {
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(msg)
                            .color(if msg == "Copied!" {
                                egui::Color32::GREEN
                            } else {
                                egui::Color32::RED
                            })
                            .strong(),
                    );
                });
                ctx.request_repaint();
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
