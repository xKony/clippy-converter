# Clippy Converter - Project Whitepaper

## 1. Objective & Motivation
Clippy Converter is a lightweight, ultra-fast background application designed to convert units and currencies with zero friction. It eliminates the need to manually copy a value, open a browser, and search for a conversion (e.g., "5 USD to PLN"). By leveraging a global hotkey, the app automatically captures highlighted text, parses it, and presents a minimal, floating interface right at the user's cursor for instant conversion.

## 2. Core User Experience (UX)
- **Frictionless Trigger:** The user highlights text (e.g., "5 kg", "$1,000", or "100 USD to PLN") and presses a system-wide global hotkey (default `Shift+Alt+C`). Selection capture is on by default.
- **Intelligent Capture:** The app programmatically injects a `Ctrl+C` (or `Cmd+C`) keystroke to copy the highlighted text, reads the clipboard, and then restores the original clipboard content so the user doesn't lose their previously copied data.
- **Smart Parsing:** The app splits the captured string into a numerical value, an optional source unit, and an optional explicit target (`to` / `in`). Grouped digits, currency glyphs, metric prefixes, and currency multipliers (`5B USD`) are supported.
- **Floating UI Overlay:** A borderless, always-on-top window appears near the mouse cursor, clamped to the monitor work area (DPI-aware on Windows).
- **Quick Selection & Favorites:** Results are filtered by category. Favorites pin to the top; an optional Recent section reopens or copies past conversions.
- **One-Click Actions:** Click a result to copy (plain number, no thousand grouping). Star to favorite. Swap source and a chosen target when needed.

## 3. Architecture & Data Management
- **Single Background Process (System Tray):** The app runs continuously as a hidden background daemon. eframe force-shows the OS window after the first frame (white-flash workaround); a one-shot hide command conceals it again until a hotkey or Settings opens a viewport.
- **Offline Capable & Caching:** Tokio workers refresh fiat rates (Fawaz Ahmed, default daily) and crypto (Binance USDT pairs, default hourly). Writes are batched into one `redb` transaction via `spawn_blocking`. Config interval changes use a `watch` channel so workers wake early.
- **Extensible Conversions:** Dynamic rates (fiat + crypto, EUR base) plus static physical units (length, weight, temperature, time, volume, area, speed, data). Static rows are seeded on first launch; existing symbols are not overwritten.

## 4. Tech Stack & Libraries
- **Language:** Rust (Edition 2024).
- **GUI Framework:** egui / eframe 0.34.1 with the Glow (OpenGL) renderer. Glow is used instead of wgpu because wgpu's DX12 backend on Windows allocated ~200+ MB of GPU memory pools for this small popup.
- **Global Hotkeys:** `global-hotkey`.
- **Clipboard Management:** `arboard` plus `enigo` to simulate the copy keystroke.
- **Networking & Parsing:** `reqwest` (shared client with timeouts), `serde` / `serde_json`.
- **Storage:** `redb` for rates and unit factors (`units_v2` + aliases); `config.json` in the OS user config directory for preferences. History is an append-only log with atomic daily prune.
- **Observability:** `tracing` / `tracing-subscriber` (default `info`; override with `RUST_LOG`).
- **Windows placement:** `windows` crate for monitor work-area clamp.

Stay on this stack. Do not migrate to Iced, replace redb with JSON/SQLite, or switch the renderer to wgpu.
