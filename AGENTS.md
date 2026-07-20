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
  - `eframe`: 0.34.1
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
- **Dev Dependencies:**
  - `tempfile`: 3.10

## 3. Project structure
- `src/api.rs`: External HTTP requests to Binance and fiat currency APIs (shared `reqwest` client with timeouts; Binance results filtered to USDT pairs).
- `src/clipboard.rs`: Clipboard capture via Enigo (Ctrl+C) and Arboard.
- `src/converter.rs`: Core engine for calculating unit and currency conversions (cached unit list with invalidation).
- `src/db.rs`: Thread-safe wrapper for redb; batched rate writes; corrupt-entry detection.
- `src/history.rs`: Append-only conversion log with at-most-daily atomic prune.
- `src/hotkey.rs`: Parsing human-readable hotkeys into system structures.
- `src/main.rs`: Application entry point, tracing init, single-instance lock, eframe handoff.
- `src/models.rs`: Core data structures and local JSON configuration logic.
- `src/parser.rs`: String splitting and value extraction logic.
- `src/ui.rs`: egui/eframe UI state machine, floating window, tray menu, and hotkey wiring.
- `src/workers.rs`: Async Tokio tasks for periodic rate refreshes (`spawn_blocking` for redb I/O, `watch` for config).
- `Cargo.toml`: Project dependencies, metadata, and strict linting rules.

## 4. Architecture and patterns
- **Rendering strategy:** egui/eframe Immediate Mode UI, running as a background daemon with borderless, always-on-top floating windows at cursor coordinates. `vsync` is enabled; repaint is event-driven (not a busy loop).
- **Data fetching patterns:** Background Tokio workers periodically poll APIs (Fawaz Ahmed's API for fiat, Binance for crypto). Rate writes are batched in one redb transaction and run via `tokio::task::spawn_blocking`. Config interval changes use `tokio::sync::watch` so workers wake early.
- **State management:** App state lives in `AppState`, updated in eframe's `ui`/`run_logic`. Shared config is published over a watch channel; a `RatesVersion` atomic invalidates the converter unit cache after successful refreshes.
- **Database:** `redb` embedded key-value store for offline persistence of exchange rates and unit conversion factors.
- **Logging:** `tracing` / `tracing-subscriber` (default filter `info`; override with `RUST_LOG`).

## 5. Available scripts
- `cargo run`: Starts the application in development mode.
- `cargo build --release`: Compiles the optimized production binary.
- `cargo test`: Executes the unit test suite across all modules.
- `cargo clippy`: Runs strict linting based on Cargo.toml configurations.

## 6. Environment variables
- No `.env` file is required. Configuration is managed via a local `config.json` in the OS-specific user config directory.
- Optional: `RUST_LOG` controls tracing filters (e.g. `RUST_LOG=debug`).

## 7. Key configuration
- **Lints:** Extremely strict `clippy` configuration in `Cargo.toml`. `unwrap_used` and `expect_used` are denied globally except in specific, documented startup paths. `pedantic`, `nursery`, `cargo`, and `perf` are set to `deny`.
- **UI Config:** egui Window uses `decorations: false` and `AlwaysOnTop` via `ViewportBuilder`; vsync is on.

## 8. Development conventions
- **Naming conventions:** Standard Rust `snake_case` for functions/variables, `PascalCase` for types.
- **Error Handling:** Pervasive use of `anyhow::Result` and `.context()` for descriptive error bubbling; prefer `tracing::{error,warn}` over silent `let _ =` at call sites.
- **Documentation:** Modules and functions use standard Rust doc comments (`///`).
- **Async:** Do not block the Tokio runtime with redb or other sync I/O — use `spawn_blocking`. Prefer channels/`watch` over shared mutable state when coordinating workers.

## 9. Known constraints and gotchas
- **Database Lock:** `redb` allows only one writer/process. The `single-instance` crate is critical to prevent database initialization failures.
- **Global Hotkeys:** OS-level conflicts may arise if `Shift+Alt+C` is already registered by another application.
- **Clipboard Race Conditions:** Programmatic copy using `enigo` relies on short delays and clipboard restoration, which might be sensitive to OS-level clipboard managers.
- **API Parsing:** Binance pairs are filtered to `*USDT` in `api.rs`, then mapped with `.strip_suffix("USDT")` in workers — pairs without a USDT quote are ignored.
- **History prune:** Append-only on each conversion; `prune_history_if_needed` runs at startup (and at most once per calendar day) using temp-file + rename.
