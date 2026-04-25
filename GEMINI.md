# GEMINI.md

## 1. Project overview
**Name:** clippy-converter
**Purpose:** A lightweight, background unit and currency converter with global hotkeys and local caching.
**Description:** Captures selected text via programmatic clipboard manipulation, parses numerical values and units, and displays a floating, transparent UI at the mouse cursor coordinates for instant conversion.
**Repository Type:** Single Rust package.

## 2. Exact versions
- **Runtime:** Rust Edition 2024
- **Package Manager:** Cargo
- **Framework:** Iced 0.14.0
- **Language:** Rust 2024
- **Dependencies:**
  - `anyhow`: 1.0
  - `iced`: 0.14.0 (features: ["tokio"])
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
- **Dev Dependencies:**
  - `tempfile`: 3.10

## 3. Project structure
- `src/api.rs`: External HTTP requests to Binance and fiat currency APIs.
- `src/clipboard.rs`: Clipboard capture via Enigo (Ctrl+C) and Arboard.
- `src/converter.rs`: Core engine for calculating unit and currency conversions.
- `src/db.rs`: Thread-safe wrapper for redb embedded database.
- `src/history.rs`: Local logging and retention of past conversions.
- `src/hotkey.rs`: Parsing human-readable hotkeys into system structures.
- `src/main.rs`: Application entry point, single instance lock, and Iced daemon setup.
- `src/models.rs`: Core data structures and local JSON configuration logic.
- `src/parser.rs`: String splitting and value extraction logic.
- `src/ui.rs`: Iced UI state machine, floating window, and tray menu.
- `src/workers.rs`: Async tokio tasks for periodic background data refreshes.
- `Cargo.toml`: Project dependencies, metadata, and strict linting rules.

## 4. Architecture and patterns
- **Rendering strategy:** Iced Elm-inspired architecture (Model-View-Update), running as a background daemon with borderless, transparent, always-on-top floating windows at cursor coordinates.
- **Data fetching patterns:** Background tokio async workers periodically poll APIs (Fawaz Ahmed's API for fiat, Binance for crypto).
- **State management:** Iced application state driven by `Message` enums. Shared configurations and database handles passed down to the UI thread.
- **Database and ORM:** `redb` (embedded key-value store) used for offline persistence of exchange rates and unit conversion factors.

## 5. Available scripts
- `cargo run`: Starts the application in development mode.
- `cargo build --release`: Compiles the optimized production binary.
- `cargo test`: Executes the unit test suite across all modules.
- `cargo clippy`: Runs strict linting based on Cargo.toml configurations.

## 6. Environment variables
No environment variables (`.env`) are required. All configuration is managed via a local `config.json` file in the OS-specific user config directory.

## 7. Key configuration
- **Lints:** Extremely strict `clippy` configuration in `Cargo.toml`. `unwrap_used` and `expect_used` are denied globally except in specific, documented startup paths. `pedantic`, `nursery`, `cargo`, and `perf` are set to `deny`.
- **UI Config:** Iced Window is configured as `transparent: true`, `decorations: false`, and `AlwaysOnTop`.

## 8. Development conventions
- **Naming conventions:** Standard Rust `snake_case` for functions/variables, `PascalCase` for types.
- **Error Handling:** Pervasive use of `anyhow::Result` and `.context()` for descriptive error bubbling, avoiding panics.
- **Documentation:** Modules and functions use standard Rust doc comments (`///`).

## 9. Known constraints and gotchas
- **Database Lock:** `redb` allows only one writer/process. The `single-instance` crate is critical to prevent database initialization failures.
- **Global Hotkeys:** OS-level conflicts may arise if `Shift+Alt+C` is already registered by another application.
- **Clipboard Race Conditions:** Programmatic copy using `enigo` relies on short delays and clipboard restoration, which might be sensitive to OS-level clipboard managers.
- **API Parsing:** Binance pairs are mapped using a naive suffix stripping strategy (`.strip_suffix("USDT")`), meaning pairs without a `USDT` quote are currently ignored.
