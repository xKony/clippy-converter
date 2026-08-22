use crate::clipboard::{CaptureOutcome, ClipboardManager};
use crate::converter::Converter;
use crate::db::Db;
use crate::format::{format_copy_precise, format_display};
use crate::history::{HistoryItem, relative_age};
use crate::hotkey;
use crate::models::{
    self, Config, ConversionResult, HistoryRetention, ThousandSeparator, UnitInfo,
};
use crate::placement;
use crate::workers::{ConfigWatchTx, RatesStatus};
use anyhow::{Context, Result};
use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use tracing::{error, warn};
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

/// Payload sent over the history logging channel to the background runtime.
pub struct HistoryLogEntry {
    pub input_value: f64,
    pub input_unit: String,
    pub output_value: f64,
    pub output_unit: String,
}

#[allow(clippy::struct_excessive_bools)]
pub struct AppState {
    pub config: Config,
    /// Publishes config changes so background workers wake and pick up new intervals.
    pub config_tx: ConfigWatchTx,
    /// Bumped by rate workers; polled to invalidate the unit list cache. Also holds last-success age.
    pub rates_status: RatesStatus,
    last_seen_rates_version: u64,
    pub converter: Converter,
    pub clipboard: ClipboardManager,
    pub hotkey_manager: GlobalHotKeyManager,
    pub hotkey_id: global_hotkey::hotkey::HotKey,
    /// Held alive to keep the OS tray icon visible. Dropping this removes the icon.
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
    /// eframe force-shows the OS window after the first painted frame (its
    /// white-flash workaround), overriding `with_visible(false)`. This flag
    /// tracks the one-time re-hide issued on the first frame.
    startup_hide_done: bool,
    /// One-shot guard so DWM rounded corners are requested only once.
    window_corners_done: bool,

    pub focus_main_input: bool,
    pub main_window_was_focused: bool,

    pub copied_notification: Option<(String, Instant)>,

    pub config_fiat_interval_str: String,
    pub config_crypto_interval_str: String,

    pub history_tx: tokio::sync::mpsc::Sender<HistoryLogEntry>,

    /// Signals the background runtime to wipe the history log file.
    clear_history_tx: tokio::sync::mpsc::Sender<()>,

    /// Signals the background clipboard worker to capture the current selection.
    capture_req_tx: Sender<()>,
    /// Receives capture outcomes from the background worker.
    capture_res_rx: Receiver<anyhow::Result<CaptureOutcome>>,
    /// True while waiting for background clipboard capture (selection-on-hotkey mode).
    pub capture_pending: bool,
    /// Signals the background worker to (re)load recent history from disk.
    recent_req_tx: Sender<()>,
    /// Receives freshly loaded recent history entries from the background worker.
    recent_res_rx: Receiver<Vec<HistoryItem>>,
    pub recent_history: Vec<HistoryItem>,

    /// Query for which [`Self::unit_filter_results`] was computed; `None` forces a recompute.
    unit_filter_query: Option<String>,
    /// Memoized, favorites-first, truncated unit list for the unit picker.
    unit_filter_results: Vec<UnitInfo>,
    /// Highlighted row in the unit picker or results list (arrow keys / Enter).
    list_cursor: usize,
    /// Last inner size we commanded for the converter popup; `None` forces a
    /// re-apply (used after settings morphs the shared OS window).
    applied_inner_size: Option<egui::Vec2>,
}

const CONVERTER_INNER_SIZE: egui::Vec2 = egui::vec2(330.0, 400.0);
/// Compact popup for bare value-input mode (no unit list, no recent
/// history) - the fixed 420 px height used to leave a large dead zone.
const CONVERTER_COMPACT_SIZE: egui::Vec2 = egui::vec2(330.0, 250.0);
const SETTINGS_INNER_SIZE: egui::Vec2 = egui::vec2(440.0, 560.0);
const SETTINGS_MIN_INNER_SIZE: egui::Vec2 = egui::vec2(380.0, 420.0);
/// How long the "Copied!" notification stays visible.
const NOTIFICATION_LIFETIME: Duration = Duration::from_secs(2);
/// How many recent conversions to show in the popup.
const RECENT_HISTORY_LIMIT: usize = 10;

/// Rasterizes the bundled app icon (`icons/clippy-converter_icon.ico`) down to
/// tray size, replacing the old procedurally drawn placeholder.
fn make_tray_icon() -> Result<tray_icon::Icon> {
    const SIZE: u32 = 32;
    let ico = image::load_from_memory_with_format(
        include_bytes!("../icons/clippy-converter_icon.ico"),
        image::ImageFormat::Ico,
    )
    .context("Failed to decode bundled tray icon")?;
    let rgba = ico
        .resize_exact(SIZE, SIZE, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    tray_icon::Icon::from_rgba(rgba.into_raw(), width, height).context("Failed to build tray icon")
}

fn apply_converter_viewport(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
        egui::WindowLevel::AlwaysOnTop,
    ));
}

fn apply_settings_viewport(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
        egui::WindowLevel::Normal,
    ));
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(SETTINGS_INNER_SIZE));
    ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(SETTINGS_MIN_INNER_SIZE));
    ctx.send_viewport_cmd(egui::ViewportCommand::Title(
        "Clippy Converter - Settings".into(),
    ));
    ctx.send_viewport_cmd(egui::ViewportCommand::EnableButtons {
        close: true,
        minimized: true,
        maximize: true,
    });
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
            .with_always_on_top()
            .with_resizable(false)
            .with_inner_size([330.0, 400.0]),
        run_and_return: false,
        vsync: true,
        hardware_acceleration: eframe::HardwareAcceleration::Required,
        // Glow (OpenGL) instead of Wgpu: the DX12 backend wgpu selects on
        // Windows allocates ~200+ MB of GPU memory pools for this tiny popup.
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    let converter = Converter::new(config.clone(), db.clone());
    let clipboard = ClipboardManager::new().context("Failed to initialize clipboard")?;
    let hotkey_manager =
        GlobalHotKeyManager::new().context("Failed to initialize hotkey manager")?;
    let hk = hotkey::parse_hotkey_or_default(&config.hotkey);
    if let Err(err) = hotkey_manager.register(hk) {
        warn!(error = %err, "failed to register global hotkey");
    }

    let tray_menu = Menu::with_items(&[
        &MenuItem::with_id("settings", "Settings", true, None),
        &MenuItem::with_id("quit", "Quit Clippy Converter", true, None),
    ])
    .context("Failed to create tray menu")?;

    if let Err(err) = crate::autostart::set_enabled(config.start_with_windows) {
        warn!(error = %err, "failed to sync Start with Windows");
    }

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("Clippy Converter")
        .with_icon(make_tray_icon()?)
        .build()
        .context("Failed to create tray icon")?;

    let fiat_str = config.fiat_update_interval_mins.to_string();
    let crypto_str = config.crypto_update_interval_mins.to_string();

    let (tx, rx) = mpsc::channel();

    let (capture_req_tx, capture_req_rx) = mpsc::channel::<()>();
    let (capture_res_tx, capture_res_rx) = mpsc::channel::<anyhow::Result<CaptureOutcome>>();

    let (recent_req_tx, recent_req_rx) = mpsc::channel::<()>();
    let (recent_res_tx, recent_res_rx) = mpsc::channel::<Vec<HistoryItem>>();

    let (config_tx, config_rx) = tokio::sync::watch::channel(config.clone());
    let rates_status = RatesStatus::new();
    if let Ok((fiat, crypto)) = db.latest_currency_timestamps() {
        rates_status.seed(fiat, crypto);
    }

    // Channel for sending history log entries to the background runtime
    let (history_tx, mut history_rx) = tokio::sync::mpsc::channel::<HistoryLogEntry>(64);
    let (clear_history_tx, mut clear_history_rx) = tokio::sync::mpsc::channel::<()>(1);

    let retention_days_startup = config.history_retention.to_days();

    // Spawn tokio runtime for background workers
    std::thread::spawn({
        let db_fiat = db.clone();
        let db_crypto = db;
        let config_fiat = config_rx.clone();
        let config_crypto = config_rx;
        let rates_fiat = rates_status.clone();
        let rates_crypto = rates_status.clone();

        move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(async {
                if let Err(err) =
                    crate::history::prune_history_if_needed(retention_days_startup).await
                {
                    warn!(error = %err, "startup history prune failed");
                }

                tokio::spawn(crate::workers::start_fiat_worker(
                    db_fiat,
                    config_fiat,
                    rates_fiat,
                ));
                tokio::spawn(crate::workers::start_crypto_worker(
                    db_crypto,
                    config_crypto,
                    rates_crypto,
                ));

                // History log receiver — processes entries from the UI thread
                tokio::spawn(async move {
                    while let Some(entry) = history_rx.recv().await {
                        if let Err(err) = crate::history::log_conversion(
                            entry.input_value,
                            &entry.input_unit,
                            entry.output_value,
                            &entry.output_unit,
                        )
                        .await
                        {
                            error!(error = %err, "failed to append history entry");
                        }
                    }
                });

                // Clear-all receiver — wipes the history log off the UI thread
                tokio::spawn(async move {
                    while clear_history_rx.recv().await.is_some() {
                        if let Err(err) = crate::history::clear_history().await {
                            error!(error = %err, "failed to clear history log");
                        }
                    }
                });

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

            // Second-launch activation: surface this instance (open settings)
            // when another process pings the shared activation event.
            {
                let tx_activation = tx.clone();
                let ctx_activation = cc.egui_ctx.clone();
                crate::activation::spawn_activation_thread(move || {
                    let _ = tx_activation.send(EventMsg::OpenSettings);
                    ctx_activation.request_repaint();
                });
            }

            // Clipboard capture worker: owns its own clipboard handle and wakes the
            // UI as soon as a capture completes, so the popup doesn't wait for the
            // next incidental OS event to process the result.
            let ctx_capture = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                let Ok(mut worker_clipboard) = ClipboardManager::new() else {
                    return;
                };
                while capture_req_rx.recv().is_ok() {
                    let result = worker_clipboard.capture_selection();
                    let _ = capture_res_tx.send(result);
                    ctx_capture.request_repaint();
                }
            });

            // History loader: reads the (potentially large) history file off the
            // UI thread so opening the popup never blocks on disk I/O.
            let ctx_history = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                while recent_req_rx.recv().is_ok() {
                    let items =
                        crate::history::list_recent(RECENT_HISTORY_LIMIT).unwrap_or_default();
                    let _ = recent_res_tx.send(items);
                    ctx_history.request_repaint();
                }
            });

            egui_extras::install_image_loaders(&cc.egui_ctx);

            crate::theme::apply_theme(&cc.egui_ctx);

            Ok(Box::new(AppState {
                config,
                config_tx,
                rates_status: rates_status.clone(),
                last_seen_rates_version: 0,
                converter,
                clipboard,
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
                startup_hide_done: false,
                window_corners_done: false,

                focus_main_input: false,
                main_window_was_focused: false,

                copied_notification: None,

                config_fiat_interval_str: fiat_str,
                config_crypto_interval_str: crypto_str,
                history_tx,
                clear_history_tx,

                capture_req_tx,
                capture_res_rx,
                capture_pending: false,
                recent_req_tx,
                recent_res_rx,
                recent_history: Vec::new(),
                unit_filter_query: None,
                unit_filter_results: Vec::new(),
                list_cursor: 0,
                applied_inner_size: None,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

impl eframe::App for AppState {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // One-shot: ask DWM for rounded corners while the native HWND exists.
        // Cheap no-op on Windows 10/other platforms.
        if !self.window_corners_done {
            self.window_corners_done = true;
            placement::round_window_corners(frame);
        }
        let ctx = ui.ctx().clone();
        self.run_logic(&ctx, ui);
    }
}

impl AppState {
    fn fmt_num(&self, value: f64, precision: usize) -> String {
        let precision = self.config.display_decimals.map_or(precision, usize::from);
        format_display(value, precision, self.config.thousand_separator)
    }

    /// Drains pending tray/hotkey/activation events and applies the
    /// corresponding mode transitions on the single OS window.
    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.event_rx.try_recv() {
            match msg {
                EventMsg::Exit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                EventMsg::OpenSettings => {
                    self.settings_window_open = true;
                    // The settings viewport commands its own size; forget the
                    // converter size so it is re-applied on the way back.
                    self.applied_inner_size = None;
                    apply_settings_viewport(ctx);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                EventMsg::HotkeyTriggered => {
                    // There is only one OS window; if it is currently shaped
                    // as Settings, hand it back to the converter first so the
                    // popup appears at the cursor instead of inside the
                    // settings-shaped window. Settings widget state (interval
                    // strings, favorites edits, ...) lives on `AppState`
                    // fields and survives; the user can reopen Settings from
                    // the tray after the popup dismisses.
                    if self.settings_window_open {
                        self.dismiss_settings_for_popup(ctx);
                    }
                    self.reset_converter_popup_state();
                    // Show the popup immediately - selection capture runs in
                    // the background and fills the result in via a spinner in
                    // the header, so hotkey-to-window latency does not include
                    // the target app's copy-response time.
                    self.request_recent_history();
                    self.show_converter_window(ctx);
                    if self.config.read_selection_on_hotkey {
                        self.capture_pending = true;
                        let _ = self.capture_req_tx.send(());
                    }
                }
            }
        }
    }

    fn run_logic(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // eframe calls `window.set_visible(true)` right after the first painted
        // frame regardless of `with_visible(false)`, which used to leave a black
        // always-on-top box on screen at startup. Viewport commands are applied
        // after that force-show, so re-hiding here on the first frame wins.
        if !self.startup_hide_done {
            self.startup_hide_done = true;
            if !self.main_window_open && !self.settings_window_open {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }

        let rates_v = self.rates_status.version();
        if rates_v != self.last_seen_rates_version {
            self.converter.invalidate_units_cache();
            self.unit_filter_query = None;
            self.last_seen_rates_version = rates_v;
        }

        self.handle_events(ctx);

        if let Ok(capture_result) = self.capture_res_rx.try_recv() {
            // The popup is already visible (shown at hotkey time); this just
            // fills the captured result in. If the user dismissed it before
            // the capture landed, only the hidden state updates.
            self.apply_capture_result(capture_result);
            ctx.request_repaint();
        }

        if let Ok(items) = self.recent_res_rx.try_recv() {
            self.recent_history = items;
        }

        // Settings rendered inside the root viewport rather than a child OS
        // window. Viewport visibility is commanded on transitions
        // (EventMsg::OpenSettings), not per frame.
        if self.settings_window_open {
            let bg_color = egui::Color32::from_rgb(24, 24, 24);
            ui.painter()
                .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, bg_color);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.max_rect().width());
                    let content_frame = egui::Frame {
                        fill: egui::Color32::TRANSPARENT,
                        inner_margin: egui::Margin::same(16),
                        ..Default::default()
                    };
                    content_frame.show(ui, |ui| {
                        self.render_settings(ui, ctx);
                    });
                });

            if ctx.input(|i| i.viewport().close_requested()) {
                self.settings_window_open = false;
                self.applied_inner_size = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                apply_converter_viewport(ctx);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.main_window_open));
            }

            return;
        }

        // The main converter popup is the ROOT viewport itself. Visibility and
        // viewport style are commanded on transitions (show/close), not per frame,
        // to avoid issuing OS window calls on every repaint.
        if !self.main_window_open {
            return;
        }

        // Compact sizing: the popup shrinks when the mode has little content
        // (bare value input) and grows for lists / recent history. Only sent
        // on change to avoid per-frame OS window calls.
        let desired = match self.current_mode {
            WindowMode::ValueInput if self.recent_history.is_empty() => CONVERTER_COMPACT_SIZE,
            _ => CONVERTER_INNER_SIZE,
        };
        if self.applied_inner_size != Some(desired) {
            self.applied_inner_size = Some(desired);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(desired));
        }

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

        let bg_color = egui::Color32::from_rgb(30, 30, 30);
        ui.painter()
            .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, bg_color);

        // Add padding via a frame with no extra fill
        let content_frame = egui::Frame {
            fill: egui::Color32::TRANSPARENT,
            inner_margin: egui::Margin::same(12),
            ..Default::default()
        };

        content_frame.show(ui, |ui| {
            self.render_main_window(ui, ctx);
        });
    }

    fn persist_config(&mut self) {
        self.converter.set_config(self.config.clone());
        self.unit_filter_query = None;
        if let Err(err) = self.config.save() {
            error!(error = %err, "failed to save config");
        }
        if self.config_tx.send(self.config.clone()).is_err() {
            warn!("config watch has no receivers");
        }
        if let Err(err) = crate::autostart::set_enabled(self.config.start_with_windows) {
            error!(error = %err, "failed to update Start with Windows");
        }
    }

    fn render_rate_freshness(&self, ui: &mut egui::Ui) {
        let snap = self.rates_status.snapshot();
        let now = chrono::Utc::now().timestamp();
        let color = if snap.is_stale(now) {
            egui::Color32::from_rgb(232, 176, 80)
        } else {
            ui.visuals().weak_text_color()
        };
        ui.label(
            egui::RichText::new(snap.summary_line(now))
                .small()
                .color(color),
        );
    }

    fn apply_recorded_hotkey(&mut self) {
        let Some(recorded) = self.recorded_hotkey.take() else {
            return;
        };
        self.config.hotkey = recorded;
        let hk = hotkey::parse_hotkey_or_default(&self.config.hotkey);
        if hk != self.hotkey_id {
            if let Err(err) = self.hotkey_manager.unregister(self.hotkey_id) {
                warn!(error = %err, "failed to unregister previous hotkey");
            }
            if self.hotkey_manager.register(hk).is_ok() {
                self.hotkey_id = hk;
            } else {
                warn!("failed to register new hotkey");
            }
        }
        self.persist_config();
    }

    fn apply_interval_fields(&mut self) {
        if let Ok(mins) = self.config_fiat_interval_str.parse::<u64>() {
            self.config.fiat_update_interval_mins = models::sanitize_interval_mins(
                "fiat_update_interval_mins",
                mins,
                models::DEFAULT_FIAT_INTERVAL_MINS,
            );
        }
        if let Ok(mins) = self.config_crypto_interval_str.parse::<u64>() {
            self.config.crypto_update_interval_mins = models::sanitize_interval_mins(
                "crypto_update_interval_mins",
                mins,
                models::DEFAULT_CRYPTO_INTERVAL_MINS,
            );
        }
    }

    fn log_conversion_if_enabled(&self) {
        if self.config.history_enabled
            && let Some(result) = &self.current_result
            && let Some(first_output) = result.outputs.first()
            && let Err(err) = self.history_tx.try_send(HistoryLogEntry {
                input_value: result.input_value,
                input_unit: result.input_unit.clone(),
                output_value: first_output.value,
                output_unit: first_output.unit.clone(),
            })
        {
            warn!(error = %err, "history log channel full or closed; dropping entry");
        }
    }

    fn reset_converter_popup_state(&mut self) {
        self.capture_pending = false;
        self.current_result = None;
        self.current_mode = WindowMode::ValueInput;
        self.manual_input_value = String::new();
        self.captured_value = 0.0;
        self.search_query.clear();
        self.search_query_lower.clear();
        self.list_cursor = 0;
    }

    /// Asks the background worker to reload recent history; results arrive via
    /// `recent_res_rx` and are applied in [`Self::run_logic`]. Keeps disk I/O
    /// off the UI thread so the popup opens without waiting on the file read.
    fn request_recent_history(&self) {
        let _ = self.recent_req_tx.send(());
    }

    /// Recomputes the memoized unit picker list for the current search query.
    fn recompute_unit_filter(&mut self) {
        let mut matching: Vec<UnitInfo> = self
            .converter
            .all_units()
            .map(|all_units| {
                all_units
                    .iter()
                    .filter(|u| u.matches(&self.search_query_lower))
                    .map(|u| u.info.clone())
                    .collect()
            })
            .unwrap_or_default();
        Self::sort_units_favorites_first(&mut matching, &self.config.favorites);
        matching.truncate(self.config.list_size);
        self.unit_filter_results = matching;
        self.unit_filter_query = Some(self.search_query_lower.clone());
        self.list_cursor = 0;
    }

    fn step_list_cursor(&mut self, ui: &egui::Ui, len: usize) -> bool {
        if len == 0 {
            self.list_cursor = 0;
            return false;
        }
        if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            self.list_cursor = self
                .list_cursor
                .saturating_add(1)
                .min(len.saturating_sub(1));
        }
        if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            self.list_cursor = self.list_cursor.saturating_sub(1);
        }
        if self.list_cursor >= len {
            self.list_cursor = len.saturating_sub(1);
        }
        ui.input(|i| i.key_pressed(egui::Key::Enter))
    }

    fn apply_source_symbol(&mut self, symbol: &str) {
        if let Ok(result) = self.converter.convert(self.captured_value, symbol) {
            self.current_result = Some(result);
            self.current_mode = WindowMode::Results;
            self.search_query.clear();
            self.search_query_lower.clear();
            self.list_cursor = 0;
            self.log_conversion_if_enabled();
            self.focus_main_input = true;
        }
    }

    fn copy_value_to_clipboard(&mut self, ctx: &egui::Context, value: f64, dismiss: bool) {
        if value.is_nan() || value.is_infinite() {
            self.copied_notification = Some(("Invalid value".to_string(), Instant::now()));
            return;
        }
        let text = format_copy_precise(value, self.config.copy_decimals.map(usize::from));
        if self.clipboard.set_text(text).is_ok() {
            self.copied_notification = Some(("Copied!".to_string(), Instant::now()));
            if dismiss {
                self.main_window_open = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }
    }

    /// Switches the single OS window out of settings mode so a converter
    /// popup can take over. Only the mode flag and viewport styling change;
    /// settings widget state is intentionally preserved. The window is hidden
    /// until [`Self::show_converter_window`] re-shows it at the cursor on the
    /// next hotkey trigger.
    fn dismiss_settings_for_popup(&mut self, ctx: &egui::Context) {
        self.settings_window_open = false;
        apply_converter_viewport(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// Shows the converter popup at the cursor (clipboard capture is optional and separate).
    fn show_converter_window(&mut self, ctx: &egui::Context) {
        self.capture_pending = false;

        // The cursor position from the OS is in physical pixels, but
        // `ViewportCommand::OuterPosition` expects logical points (egui-winit
        // multiplies by pixels_per_point). Without this division the popup
        // lands offset from the cursor on any display scale other than 100%.
        let cursor = self.clipboard.cursor_position();
        let ppp = ctx.pixels_per_point();
        let physical = placement::popup_position_at_cursor(cursor, ppp);
        self.main_window_pos = egui::pos2(physical.x / ppp, physical.y / ppp);

        self.main_window_open = true;
        self.main_window_was_focused = false;
        self.focus_main_input = true;
        self.applied_inner_size = None;

        apply_converter_viewport(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(self.main_window_pos));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    /// Applies a background capture outcome: parse-and-convert on success,
    /// or surface a short hint explaining why nothing was captured.
    fn apply_capture_result(&mut self, capture_result: anyhow::Result<CaptureOutcome>) {
        self.capture_pending = false;

        let parsed = match capture_result {
            Err(err) => Err(err),
            Ok(CaptureOutcome::Text(text)) => crate::parser::parse_input(&text),
            // The copy answered with an empty string: nothing was selected.
            Ok(CaptureOutcome::EmptySelection) => {
                self.captured_value = 0.0;
                self.current_result = None;
                self.current_mode = WindowMode::ValueInput;
                self.manual_input_value = String::new();
                self.focus_main_input = true;
                self.copied_notification = Some(("Nothing selected".to_string(), Instant::now()));
                return;
            }
            // The target app never answered the simulated Ctrl+C (elevated
            // window, RDP, clipboard-manager interference).
            Ok(CaptureOutcome::NoResponse) => {
                self.captured_value = 0.0;
                self.current_result = None;
                self.current_mode = WindowMode::ValueInput;
                self.manual_input_value = String::new();
                self.focus_main_input = true;
                self.copied_notification =
                    Some(("Could not read selection".to_string(), Instant::now()));
                return;
            }
        };

        if let Ok(parsed) = parsed {
            self.apply_parsed_input(parsed);
        } else {
            self.captured_value = 0.0;
            self.current_result = None;
            self.current_mode = WindowMode::ValueInput;
            self.manual_input_value = String::new();
        }
        self.focus_main_input = true;
    }

    fn apply_parsed_input(&mut self, parsed: crate::parser::ParsedInput) {
        self.captured_value = parsed.value;
        let Some(unit) = parsed.unit else {
            self.current_result = None;
            self.current_mode = WindowMode::SourceUnitSelection;
            return;
        };

        if let Ok(result) =
            self.converter
                .convert_preferring(parsed.value, &unit, parsed.target.as_deref())
        {
            self.current_result = Some(result);
            self.current_mode = WindowMode::Results;
            self.log_conversion_if_enabled();
        } else {
            self.search_query = unit;
            self.search_query_lower = self.search_query.to_lowercase();
            self.current_result = None;
            self.current_mode = WindowMode::SourceUnitSelection;
        }
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
                            self.apply_recorded_hotkey();
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

        if ui
            .checkbox(
                &mut self.config.read_selection_on_hotkey,
                "Read selected text on hotkey",
            )
            .changed()
        {
            self.persist_config();
        }
        ui.label(
            egui::RichText::new("On by default. Unchecked: hotkey opens an empty popup.")
                .small()
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(2.0);

        #[cfg(windows)]
        {
            if ui
                .checkbox(&mut self.config.start_with_windows, "Start with Windows")
                .changed()
            {
                self.persist_config();
            }
        }

        ui.separator();

        ui.label("Thousand separators (display only)");
        ui.horizontal(|ui| {
            let off = ui.selectable_value(
                &mut self.config.thousand_separator,
                ThousandSeparator::None,
                "Off",
            );
            let spaces = ui.selectable_value(
                &mut self.config.thousand_separator,
                ThousandSeparator::Space,
                "Spaces",
            );
            let commas = ui.selectable_value(
                &mut self.config.thousand_separator,
                ThousandSeparator::Comma,
                "Commas",
            );
            if off.changed() || spaces.changed() || commas.changed() {
                self.persist_config();
            }
        });

        ui.add_space(4.0);

        // Fixed decimal places for displayed and copied values. Toggling Auto
        // writes back immediately; the drag fields only exist when pinned.
        ui.label("Decimal places (Auto adapts per context)");
        ui.horizontal(|ui| {
            let mut changed = false;

            ui.label("Display:");
            let mut display_auto = self.config.display_decimals.is_none();
            if ui.checkbox(&mut display_auto, "Auto").changed() {
                self.config.display_decimals = if display_auto { None } else { Some(2) };
                changed = true;
            }
            if let Some(decimals) = self.config.display_decimals.as_mut() {
                let widget = egui::DragValue::new(decimals)
                    .range(0..=crate::models::MAX_DECIMALS)
                    .suffix(" digits");
                changed |= ui.add(widget).changed();
            }

            ui.add_space(8.0);
            ui.label("Copy:");
            let mut copy_auto = self.config.copy_decimals.is_none();
            if ui.checkbox(&mut copy_auto, "Auto").changed() {
                self.config.copy_decimals = if copy_auto { None } else { Some(2) };
                changed = true;
            }
            if let Some(decimals) = self.config.copy_decimals.as_mut() {
                let widget = egui::DragValue::new(decimals)
                    .range(0..=crate::models::MAX_DECIMALS)
                    .suffix(" digits");
                changed |= ui.add(widget).changed();
            }

            if changed {
                self.persist_config();
            }
        });

        ui.separator();

        if ui
            .checkbox(&mut self.config.history_enabled, "Enable History Logging")
            .changed()
        {
            self.persist_config();
        }
        if self.config.history_enabled {
            ui.horizontal(|ui| {
                ui.label("Retention:");
                let d7 = ui.selectable_value(
                    &mut self.config.history_retention,
                    HistoryRetention::SevenDays,
                    "7d",
                );
                let d30 = ui.selectable_value(
                    &mut self.config.history_retention,
                    HistoryRetention::ThirtyDays,
                    "30d",
                );
                let y1 = ui.selectable_value(
                    &mut self.config.history_retention,
                    HistoryRetention::OneYear,
                    "1y",
                );
                let never = ui.selectable_value(
                    &mut self.config.history_retention,
                    HistoryRetention::Never,
                    "Never",
                );
                if d7.changed() || d30.changed() || y1.changed() || never.changed() {
                    self.persist_config();
                }
            });
            if ui.button("Clear All History").clicked() {
                if self.clear_history_tx.try_send(()).is_err() {
                    error!("failed to queue history clear");
                }
                self.recent_history.clear();
            }
        }

        if ui.button("Open History Folder").clicked()
            && let Ok(path) = crate::history::get_history_path()
            && let Some(parent) = path.parent()
        {
            let _ = open::that(parent);
        }

        ui.separator();

        ui.label("Unit packs");
        ui.label(
            egui::RichText::new("Length, weight, temperature, time, and currency are always on.")
                .small()
                .color(ui.visuals().weak_text_color()),
        );
        let volume = ui.checkbox(&mut self.config.unit_packs.volume, "Volume (L, gal, fl oz)");
        let area = ui.checkbox(&mut self.config.unit_packs.area, "Area (m², acre, ha)");
        let speed = ui.checkbox(&mut self.config.unit_packs.speed, "Speed (km/h, mph, kn)");
        let data = ui.checkbox(&mut self.config.unit_packs.data, "Data (KB, MiB, GB)");
        let scientific = ui.checkbox(
            &mut self.config.unit_packs.scientific,
            "Scientific (Pa, J, W, N, deg, Hz)",
        );
        if volume.changed()
            || area.changed()
            || speed.changed()
            || data.changed()
            || scientific.changed()
        {
            self.persist_config();
        }

        ui.separator();
        ui.label("Update Intervals (minutes)");
        ui.horizontal(|ui| {
            ui.label("Fiat:");
            let fiat = ui.add(
                egui::TextEdit::singleline(&mut self.config_fiat_interval_str).desired_width(72.0),
            );
            if fiat.lost_focus() {
                self.apply_interval_fields();
                self.persist_config();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Crypto:");
            let crypto = ui.add(
                egui::TextEdit::singleline(&mut self.config_crypto_interval_str)
                    .desired_width(72.0),
            );
            if crypto.lost_focus() {
                self.apply_interval_fields();
                self.persist_config();
            }
        });

        ui.separator();
        if ui.button("Save & Apply").clicked() {
            self.apply_interval_fields();
            if self.recorded_hotkey.is_some() {
                self.apply_recorded_hotkey();
            } else {
                self.persist_config();
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
                        if self.capture_pending {
                            ui.spinner();
                        }
                    }
                    WindowMode::SourceUnitSelection => {
                        if ui
                            .button(
                                egui::RichText::new(self.fmt_num(self.captured_value, 4)).strong(),
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
                                    egui::RichText::new(self.fmt_num(res.input_value, 2)).strong(),
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

        self.render_rate_freshness(ui);

        if self.current_mode == WindowMode::ValueInput {
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.manual_input_value)
                        .hint_text("100 USD to PLN")
                        .font(egui::TextStyle::Heading)
                        .desired_width(f32::INFINITY),
                );
                if self.focus_main_input {
                    response.request_focus();
                    self.focus_main_input = false;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && let Ok(parsed) = crate::parser::parse_input(&self.manual_input_value)
                {
                    self.apply_parsed_input(parsed);
                    self.focus_main_input = true;
                }
            });

            if self.current_result.is_none() {
                self.render_recent_history(ui, ctx);
            }
        } else {
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("Search units...")
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    self.search_query_lower = self.search_query.to_lowercase();
                    self.list_cursor = 0;
                }
                if self.focus_main_input {
                    response.request_focus();
                    self.focus_main_input = false;
                }
            });

            ui.add_space(5.0);

            if self.current_mode == WindowMode::SourceUnitSelection {
                // The filtered list is memoized and only recomputed when the query
                // changes (or the memo is invalidated by a rate refresh or a
                // favorites change), instead of re-filtering every frame.
                if self.unit_filter_query.as_deref() != Some(self.search_query_lower.as_str()) {
                    self.recompute_unit_filter();
                }

                let enter = self.step_list_cursor(ui, self.unit_filter_results.len());
                if enter && let Some(unit) = self.unit_filter_results.get(self.list_cursor) {
                    let symbol = unit.symbol.clone();
                    self.apply_source_symbol(&symbol);
                }

                let mut clicked_symbol: Option<String> = None;
                // Gentle neutral washes instead of the selection blue; see
                // design-guidelines.md ("Row states").
                let cursor = self.list_cursor;
                let mut hovered_row: Option<usize> = None;
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            for (idx, unit) in self.unit_filter_results.iter().enumerate() {
                                let aliases_str = if unit.aliases.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({})", unit.aliases.join(", "))
                                };

                                let button_text =
                                    egui::RichText::new(format!("{} {}", unit.symbol, aliases_str));
                                let fill = if idx == cursor {
                                    crate::theme::row_cursor_wash()
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                let row = ui.add(
                                    egui::Button::new(button_text)
                                        .fill(fill)
                                        .corner_radius(crate::theme::ROW_CORNER_RADIUS),
                                );
                                // Hovering steers the single gentle highlight:
                                // the mouse simply moves the keyboard cursor,
                                // so there is never a second competing color.
                                if idx != cursor && row.hovered() {
                                    hovered_row = Some(idx);
                                }
                                if row.clicked() {
                                    clicked_symbol = Some(unit.symbol.clone());
                                }
                            }
                        });
                    });

                if let Some(idx) = hovered_row {
                    self.list_cursor = idx;
                }

                if let Some(symbol) = clicked_symbol {
                    self.apply_source_symbol(&symbol);
                }
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

                let enter = self.step_list_cursor(ui, outputs.len());
                if enter
                    && let Some(output) = outputs.get(self.list_cursor)
                    && output.value.is_finite()
                {
                    self.copy_value_to_clipboard(ctx, output.value, true);
                }

                if !outputs.is_empty() {
                    // Gentle neutral washes instead of the selection blue; see
                    // design-guidelines.md ("Row states"). Hover steers the
                    // single highlight, exactly like the unit picker.
                    let cursor = self.list_cursor;
                    let mut hovered_row: Option<usize> = None;
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                for (idx, output) in outputs.iter().enumerate() {
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
                                    let fill = if idx == cursor {
                                        crate::theme::row_cursor_wash()
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    };
                                    // The margin is applied to every row (not just
                                    // the highlighted one) so rows do not shift
                                    // when the keyboard cursor moves.
                                    let row = egui::Frame::new()
                                        .fill(fill)
                                        .corner_radius(crate::theme::ROW_CORNER_RADIUS)
                                        .inner_margin(egui::Margin::symmetric(6, 4))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.vertical(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            self.fmt_num(output.value, 4),
                                                        )
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
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        if ui
                                                            .add(egui::Button::image(
                                                                egui::Image::new(favorite_icon)
                                                                    .tint(tint),
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
                                                            // Update sorting config without
                                                            // discarding the units cache.
                                                            self.converter
                                                                .set_config(self.config.clone());
                                                            self.unit_filter_query = None;
                                                        }

                                                        if ui
                                                            .add(egui::Button::image(
                                                                egui::Image::new(
                                                                    egui::include_image!(
                                                                        "../icons/switch.svg"
                                                                    ),
                                                                )
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

                                                        if output.value.is_finite() {
                                                            if ui
                                                                .add(
                                                                    egui::Button::image(
                                                                        egui::Image::new(
                                                                            egui::include_image!(
                                                                                "../icons/copy.svg"
                                                                            ),
                                                                        )
                                                                        .tint(
                                                                            ui.visuals()
                                                                                .text_color(),
                                                                        ),
                                                                    ),
                                                                )
                                                                .clicked()
                                                            {
                                                                self.copy_value_to_clipboard(
                                                                    ctx,
                                                                    output.value,
                                                                    false,
                                                                );
                                                            }
                                                        } else {
                                                            // Non-copyable value (inf/NaN):
                                                            // render a disabled, dimmed
                                                            // control instead of a live
                                                            // button, so accidental
                                                            // clicks while scrolling the
                                                            // list cannot trigger the
                                                            // red "Invalid value" toast.
                                                            ui.add_enabled(
                                                                false,
                                                                egui::Button::image(
                                                                    egui::Image::new(
                                                                        egui::include_image!(
                                                                            "../icons/copy.svg"
                                                                        ),
                                                                    ),
                                                                ),
                                                            );
                                                        }
                                                    },
                                                );
                                            });
                                        });
                                    if idx != cursor && row.response.hovered() {
                                        hovered_row = Some(idx);
                                    }
                                    ui.separator();
                                }
                            });
                        });
                    if let Some(idx) = hovered_row {
                        self.list_cursor = idx;
                    }
                }
            }
        }

        if let Some((msg, time)) = &self.copied_notification {
            let elapsed = Instant::now().duration_since(*time);
            if elapsed > NOTIFICATION_LIFETIME {
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
                // Wake up once the notification is due to expire, instead of
                // repainting continuously while it is visible.
                ctx.request_repaint_after(NOTIFICATION_LIFETIME.saturating_sub(elapsed));
            }
        }
    }

    fn render_recent_history(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.recent_history.is_empty() {
            return;
        }

        ui.label(
            egui::RichText::new("Recent")
                .small()
                .color(ui.visuals().weak_text_color()),
        );

        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let max_list_height = row_height * 5.5;

        let history_action = egui::ScrollArea::vertical()
            .id_salt("recent_conversions_list")
            .max_height(max_list_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.set_width(ui.max_rect().width());
                let mut clicked_idx: Option<usize> = None;
                let mut copy_idx: Option<usize> = None;
                for (idx, item) in self.recent_history.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let row = self.history_row(ui, item);
                        if row.clicked() {
                            clicked_idx = Some(idx);
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Copy").clicked() {
                                copy_idx = Some(idx);
                            }
                            if let Some(timestamp) = item.timestamp {
                                ui.label(
                                    egui::RichText::new(relative_age(timestamp))
                                        .small()
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                        });
                    });
                }
                (clicked_idx, copy_idx)
            });

        if let Some(idx) = history_action.inner.0
            && let Some(item) = self.recent_history.get(idx).cloned()
        {
            self.reopen_history_item(&item);
            ctx.request_repaint();
        }
        if let Some(idx) = history_action.inner.1
            && let Some(item) = self.recent_history.get(idx)
        {
            let text = self.history_output_text(item);
            if self.clipboard.set_text(text).is_ok() {
                self.copied_notification = Some(("Copied!".to_string(), Instant::now()));
            }
        }

        ui.add_space(6.0);
    }

    /// Renders one recent row with a painted arrow (default font lacks Unicode →).
    fn history_row(&self, ui: &mut egui::Ui, item: &HistoryItem) -> egui::Response {
        let weak = ui.visuals().weak_text_color();
        let row_height = ui.spacing().interact_size.y;
        let width = ui.available_width();
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::CLICK);
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} {}",
                    self.fmt_num(item.input_value, 1),
                    item.input_unit
                ));
                let (arrow_rect, _) =
                    ui.allocate_exact_size(egui::vec2(22.0, 14.0), egui::Sense::empty());
                Self::paint_right_arrow(ui, arrow_rect, weak);
                ui.label(format!(
                    "{} {}",
                    self.fmt_num(item.output_value, 1),
                    item.output_unit
                ));
            });
        });
        response
    }

    fn paint_right_arrow(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32) {
        let stroke = egui::Stroke::new(1.5_f32, color);
        let y = rect.center().y;
        let left = rect.left() + 2.0;
        let right = rect.right() - 2.0;
        let tip = rect.right() - 6.0;
        let p = ui.painter();
        p.line_segment([egui::pos2(left, y), egui::pos2(right, y)], stroke);
        p.line_segment([egui::pos2(right, y), egui::pos2(tip, y - 4.0)], stroke);
        p.line_segment([egui::pos2(right, y), egui::pos2(tip, y + 4.0)], stroke);
    }

    fn history_output_text(&self, item: &HistoryItem) -> String {
        let value = format_copy_precise(
            item.output_value,
            self.config.copy_decimals.map(usize::from),
        );
        format!("{value} {}", item.output_unit)
    }

    fn reopen_history_item(&mut self, item: &HistoryItem) {
        if let Ok(result) = self.converter.convert(item.input_value, &item.input_unit) {
            self.captured_value = item.input_value;
            self.current_result = Some(result);
            self.current_mode = WindowMode::Results;
            self.search_query.clear();
            self.search_query_lower.clear();
            self.focus_main_input = true;
        }
    }

    fn sort_units_favorites_first(units: &mut [UnitInfo], favorites: &[String]) {
        let ranks = crate::models::favorite_ranks(favorites);
        units.sort_by(|a, b| crate::models::cmp_favorite_rank(&a.symbol, &b.symbol, &ranks));
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
