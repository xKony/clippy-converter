# AGENTS.md

## 1. Project overview
**Name:** clippy-converter
**Purpose:** A lightweight, background unit and currency converter with global hotkeys and local caching.
**Description:** Captures selected text via programmatic clipboard manipulation, parses numerical values and units, and displays a floating UI at the mouse cursor coordinates for instant conversion.
**Repository Type:** Single Rust package (binary crate).

## 2. Exact versions
- **Runtime:** Rust Edition 2024
- **Package Manager:** Cargo
- **Framework:** egui 0.34.1 / eframe 0.34.1
- **Language:** Rust 2024
- **Dependencies:**
  - `anyhow`: 1.0
  - `eframe`: 0.34.1 (features: ["glow"])
  - `egui`: 0.34.1
  - `egui_extras`: 0.34.1 (features: ["svg", "image"])
  - `global-hotkey`: 0.7.0
  - `arboard`: 3.4
  - `enigo`: 0.6.1
  - `reqwest`: 0.13.2 (features: ["json"])
  - `tokio`: 1 (features: ["full"])
  - `serde`: 1.0 (features: ["derive"])
  - `serde_json`: 1.0
  - `redb`: 4.1.0
  - `bincode`: 1.3.3
  - `directories`: 6.0
  - `chrono`: 0.4 (features: ["serde"])
  - `tray-icon`: 0.19
  - `open`: 5.3
  - `single-instance`: 0.3.3
  - `tracing`: 0.1
  - `tracing-subscriber`: 0.3 (features: ["env-filter"])
- **Windows-only:** `windows`: 0.58 (features: Win32 Foundation / Gdi / WindowsAndMessaging) — monitor work-area for popup clamp in `placement.rs`.
- **Dev Dependencies:**
  - `tempfile`: 3.10

## 3. Project structure
- `src/api.rs`: External HTTP requests to Binance and fiat currency APIs (shared `reqwest` client with timeouts; Binance results filtered to USDT pairs, with leveraged/1000x/1M noise pairs dropped).
- `src/clipboard.rs`: Clipboard capture via Enigo (Ctrl+C / Cmd+C) and Arboard, with marker-based wait and clipboard restore.
- `src/converter.rs`: Core engine for calculating unit and currency conversions (cached unit list with invalidation; `convert_preferring` pins an explicit `to`/`in` target).
- `src/db.rs`: Thread-safe wrapper for redb; batched rate writes; corrupt-entry detection; static unit seed (`STATIC_UNITS`).
- `src/format.rs`: Display vs copy number formatting (`ThousandSeparator` grouping is display-only).
- `src/history.rs`: Append-only conversion log with at-most-daily atomic prune.
- `src/hotkey.rs`: Parsing human-readable hotkeys into system structures.
- `src/main.rs`: Application entry point, tracing init, single-instance lock, eframe handoff.
- `src/models.rs`: Core data structures (`Config`, `UnitCategory`, `UnitEntry`) and local JSON configuration logic.
- `src/parser.rs`: Value extraction: grouped digits, leading/trailing currency glyphs, and `to`/`in` target clauses.
- `src/placement.rs`: Cursor-relative popup positioning with DPI-aware monitor work-area clamp (Windows `MonitorFromPoint`; 1920×1080 fallback elsewhere).
- `src/theme.rs`: egui dark theme and spacing.
- `src/ui.rs`: egui/eframe UI state machine, floating window, tray menu, and hotkey wiring. Settings checkboxes persist immediately; interval/hotkey changes still have a Save & Apply path.
- `src/workers.rs`: Async Tokio tasks for periodic rate refreshes (`spawn_blocking` for redb I/O, `watch` for config).
- `icons/`: SVG assets for the popup (close, copy, favorite, switch). Tray currently uses the default `tray-icon` image, not a custom `.ico`.
- `Cargo.toml`: Project dependencies, metadata, and strict linting rules.

## 4. Architecture and patterns
- **Rendering strategy:** egui/eframe Immediate Mode UI via `eframe::Renderer::Glow` (OpenGL), running as a background daemon with borderless, always-on-top floating windows at cursor coordinates. Glow is used instead of wgpu because wgpu's DX12 backend on Windows allocated ~200+ MB of GPU memory pools for this small popup (~300 MB RAM); Glow stays near the ~70 MB baseline. `vsync` is enabled; repaint is event-driven (not a busy loop). Stay on egui — do not migrate to Iced or wgpu.
- **Data fetching patterns:** Background Tokio workers periodically poll APIs (Fawaz Ahmed's API for fiat, Binance for crypto). Rate writes are batched in one redb transaction and run via `tokio::task::spawn_blocking`. Config interval changes use `tokio::sync::watch` so workers wake early. Crypto pairs are mapped with `.strip_suffix("USDT")` in workers; conversion through EUR uses the cached USDT factor, or `0.92` if USDT is missing.
- **State management:** App state lives in `AppState`, updated in eframe's `ui`/`run_logic`. Shared config is published over a watch channel; a `RatesVersion` atomic invalidates the converter unit cache after successful refreshes. Clipboard capture and history listing run off the UI thread over `mpsc` channels.
- **Database:** `redb` embedded key-value store for offline persistence of exchange rates and unit conversion factors (`units_v2` + `aliases` tables).
- **Parsing / convert:** `parse_input` returns `ParsedInput { value, unit, target }`. The popup calls `Converter::convert_preferring` so `100 USD to PLN` can pin PLN first. Metric prefixes and currency multipliers (`5B USD`) are handled in the converter, not the parser.
- **Config defaults:** Fresh installs enable selection capture (`read_selection_on_hotkey: true`). Default hotkey is `Shift+Alt+C`. `config.json` is a non-atomic `fs::write`.
- **Unit categories:** Currency (live rates) plus static length, weight, temperature, time, volume, area, speed, and data. Seeded in `db.rs` `STATIC_UNITS`.
- **Logging:** `tracing` / `tracing-subscriber` (default filter `info`; override with `RUST_LOG`).

## 5. Available scripts
- `cargo run`: Starts the application in development mode.
- `cargo build --release`: Compiles the optimized production binary.
- `cargo test`: Executes the unit test suite across all modules.
- `cargo clippy`: Runs strict linting based on Cargo.toml configurations.

## 6. Environment variables
- No `.env` file is required. Configuration is managed via a local `config.json` in the OS-specific user config directory (`directories` crate: `ProjectDirs::from("com", "clippy", "clippy-converter")` → `%AppData%\clippy\clippy-converter\config.json` on Windows).
- Optional: `RUST_LOG` controls tracing filters (e.g. `RUST_LOG=debug`).

## 7. Key configuration
- **Lints:** Extremely strict `clippy` configuration in `Cargo.toml`. `unwrap_used` and `expect_used` are denied globally except in specific, documented startup paths. `pedantic`, `nursery`, `cargo`, and `perf` are set to `deny`.
- **UI Config:** egui Window uses `decorations: false` and `AlwaysOnTop` via `ViewportBuilder`; vsync is on.

## 8. Development conventions
- **Naming conventions:** Standard Rust `snake_case` for functions/variables, `PascalCase` for types.
- **Error Handling:** Pervasive use of `anyhow::Result` and `.context()` for descriptive error bubbling; prefer `tracing::{error,warn}` over silent `let _ =` at call sites.
- **Documentation:** Modules and functions use standard Rust doc comments (`///`).
- **Async:** Do not block the Tokio runtime with redb or other sync I/O — use `spawn_blocking`. Prefer channels/`watch` over shared mutable state when coordinating workers.
- **UI thread:** Clipboard/Enigo and redb must not run on the eframe frame path; existing capture/history channels are the pattern to follow.

## 9. Known constraints and gotchas
- **Database Lock:** `redb` allows only one writer/process. The `single-instance` crate is critical to prevent database initialization failures. A second launch currently errors and exits rather than focusing the existing instance.
- **Static unit seed:** `add_unit_static` skips symbols that already exist. New categories or factor/alias fixes in `STATIC_UNITS` never reach databases that already ran init. Existing installs need a seed/schema version (or a one-shot rewrite) to pick up volume/area/speed/data after first launch.
- **Global Hotkeys:** OS-level conflicts may arise if `Shift+Alt+C` is already registered by another application.
- **Clipboard Race Conditions:** Programmatic copy using `enigo` relies on a unique clipboard marker, short delays, and clipboard restoration, which might be sensitive to OS-level clipboard managers.
- **API Parsing:** Binance pairs are filtered to `*USDT` in `api.rs` (leveraged UP/DOWN/BULL/BEAR and `1000`/`1M` prefixes dropped), then mapped with `.strip_suffix("USDT")` in workers — pairs without a USDT quote are ignored.
- **Fiat API:** Single jsDelivr URL for Fawaz Ahmed's EUR-base feed; no `error_for_status` and no Cloudflare Pages fallback yet. Fiat API notes live in `currency-exchange-rates-api.md`.
- **History prune:** Append-only on each conversion; `prune_history_if_needed` runs at startup (and at most once per calendar day) using temp-file + rename.
- **Startup hide:** eframe force-shows the OS window after the first painted frame (white-flash workaround), overriding `with_visible(false)`. `AppState::startup_hide_done` and a one-shot `ViewportCommand::Visible(false)` in `run_logic` re-hide after that force-show so a black always-on-top box does not linger at startup.
- **Popup DPI:** OS cursor position is in physical pixels, but `ViewportCommand::OuterPosition` expects logical points (egui-winit multiplies by `pixels_per_point`). `show_converter_window` divides by `ctx.pixels_per_point()`, and `placement::popup_position_at_cursor` takes `pixels_per_point` so the work-area clamp uses the popup's physical size — otherwise the popup is offset on Windows display scaling ≠ 100%.
- **Work-area fallback:** Non-Windows builds clamp to 1920×1080 when Win32 monitor info is unavailable, so Linux/macOS popups can open off-screen on multi-monitor or scaled displays.
- **Docs:** `AGENTS.md` is the agent source of truth. `project-whitepaper.md` is a product/architecture overview of the current egui + redb stack, not a backlog. Do not recreate a second agent file (`GEMINI.md`).
