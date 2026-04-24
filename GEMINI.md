# GEMINI.md

## 1. Project Overview
**Name:** Clippy Converter  
**Purpose:** A lightweight, ultra-fast background utility for unit and currency conversion triggered by global hotkeys.  
**Repository Type:** Single Rust package.  
**Description:** Captures selected text via programmatic clipboard manipulation, parses numerical values and units, and displays a floating, transparent UI at the mouse cursor coordinates for instant conversion.

## 2. Exact Versions
- **Runtime:** Rust Edition 2024
- **Language:** Rust
- **Framework:** Iced v0.14.0 (with `tokio` feature)
- **Database:** redb v4.1.0
- **Primary Dependencies:**
    - `anyhow`: 1.0
    - `global-hotkey`: 0.7.0
    - `arboard`: 3.4
    - `enigo`: 0.6.1
    - `reqwest`: 0.13.2 (with `json` feature)
    - `tokio`: 1 (with `full` feature)
    - `serde`: 1.0 (with `derive` feature)
    - `serde_json`: 1.0
    - `directories`: 6.0
    - `chrono`: 0.4 (with `serde` feature)
    - `tray-icon`: 0.19
    - `single-instance`: 0.3.3

## 3. Project Structure
- `src/main.rs`: Application entry point; handles single-instance locking and boots the Iced daemon.
- `src/ui.rs`: Central UI state machine; manages multiple windows (Main, Settings), tray menus, and event subscriptions.
- `src/converter.rs`: Logic for unit categories (Length, Weight, Temperature, Currency) and conversion factors.
- `src/parser.rs`: Extends regex-like logic to split strings into `f64` values and unit symbols.
- `src/db.rs`: Wrapper for `redb` providing thread-safe access to cached exchange rates.
- `src/models.rs`: Defines core data types (`Config`, `ConversionResult`, `ConvertedValue`) and serialization.
- `src/clipboard.rs`: Manages selection capture via `Enigo` (simulating Ctrl+C) and `Arboard`.
- `src/api.rs`: Handles HTTP requests to currency API endpoints.
- `src/workers.rs`: Async tokio tasks for periodic background data refreshes.
- `src/history.rs`: Implementation of local conversion logging.
- `src/hotkey.rs`: Parsing of human-readable hotkey strings into `global-hotkey` structures.

## 4. Architecture and Patterns
- **Concurrency:** Uses `tokio` for non-blocking background workers and Iced subscriptions.
- **Persistence:** Local `redb` for exchange rates; JSON for user configuration (`config.json`).
- **UI Architecture:** Elm-inspired architecture (Model-View-Update) provided by Iced.
- **Error Handling:** Pervasive use of `anyhow::Result` for application-level errors.
- **Performance:** Programmatic copy restores original clipboard content to minimize side effects.

## 5. Available Scripts
- `cargo run`: Starts the application in development mode.
- `cargo build --release`: Compiles the optimized production binary.
- `cargo test`: Executes the unit test suite (found in `tests` modules within `src/`).
- `cargo clippy`: Runs strict linting (enforces `deny` on `all`, `pedantic`, `nursery`, `cargo`, `perf`).

## 6. Environment Variables
No `.env` file required. All configuration is handled via `config.json` in the user's project-specific config directory.

## 7. Key Configuration
- **Lints:** Extremely strict `clippy` configuration in `Cargo.toml`. `unwrap_used` and `expect_used` are denied globally except in specific, documented startup paths.
- **Iced Window:** Configured as `transparent`, `decorations: false`, and `AlwaysOnTop` for the conversion popup.

## 8. Development Conventions
- **Naming:** Standard Rust snake_case for functions/variables, PascalCase for types.
- **Safety:** Explicit `anyhow` context for all I/O operations.
- **Documentation:** Mandatory audit of dependency documentation before implementation (per `project-whitepaper.md`).

## 9. Known Constraints and Gotchas
- **Database Lock:** `redb` allows only one writer/process. Single-instance protection is critical to avoid database initialization failure.
- **Hotkey Conflicts:** Global hotkeys may be intercepted by other applications if not unique.
- **Clipboard Race:** Programmatic copy relies on a small delay/restore cycle which might be sensitive to OS-level clipboard managers.
