# Clippy Converter - Development Guide

## Project Overview

Clippy Converter is a lightweight, ultra-fast background application designed for seamless unit and currency conversion. It allows users to highlight a value, press a global hotkey, and instantly see conversion results in a floating minimalist UI.

### Core Features

- **Global Hotkey Trigger:** Intercepts text selection via programmatic copy.
- **Floating UI Overlay:** Minimalist window at the mouse cursor.
- **Smart Parsing:** Auto-detects numbers and units.
- **Local Caching:** 8-hour background refresh of currency rates for offline availability.
- **Extensible:** Supports both physical units and dynamic currencies.

### Tech Stack

- **Language:** Rust
- **GUI:** [Iced](https://iced.rs/)
- **Hotkeys:** `global-hotkey`
- **Clipboard:** `arboard`
- **Networking:** `reqwest`, `tokio`
- **Serialization:** `serde`, `serde_json`

## Building and Running

As a standard Rust project, use the following commands:

- **Build:** `cargo build`
- **Run:** `cargo run`
- **Test:** `cargo test`
- **Linting:** `cargo clippy` (Note: The project enforces very strict lints)

## Development Conventions

### Documentation & Dependencies

- **MANDATORY Documentation Audit:** Before implementing any feature, agents MUST read the documentation of relevant dependencies (e.g., via Context7 or web search).
- **Stale Data Prevention:** Since LLMs are trained on older data, never rely on internal training knowledge for API signatures or behaviors.
- **Local Docs Fallback:** If online documentation is unavailable or ambiguous, generate local documentation using `cargo doc --open` (or by reading the generated HTML in `target/doc`) to verify the exact version's API before writing code.

### Error Handling

- Use `anyhow::Result` for application-level error handling.

### Linting & Code Quality

The project enforces strict `clippy` lints defined in `Cargo.toml`. The following levels are set to `deny`:

- `all`, `pedantic`, `nursery`, `cargo`, `perf`
- `unwrap_used`, `expect_used` (Use proper error handling instead of panicking)

### Project Structure (Planned)

- `src/main.rs`: Entry point and background loop.
- `src/models.rs`: Data structures for config and caching.
- `src/api.rs`: Currency API integration.
- `src/parser.rs`: Input string parsing logic.
- `src/converter.rs`: Unit conversion engine.
- `src/ui.rs`: Iced GUI implementation.

## Key Files

- `Cargo.toml`: Project dependencies and lint configuration.
- `project-whitepaper.md`: High-level vision and UX goals.
- `plan.md`: Detailed step-by-step implementation plan.
- `GEMINI.md`: This file, providing context for development.
