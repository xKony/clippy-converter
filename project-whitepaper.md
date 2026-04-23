# Clippy Converter - Project Whitepaper

## 1. Objective & Motivation
Clippy Converter is a lightweight, ultra-fast background application designed to convert units and currencies with zero friction. It eliminates the need to manually copy a value, open a browser, and search for a conversion (e.g., "5 USD to PLN"). By leveraging a global hotkey, the app automatically captures highlighted text, parses it, and presents a minimal, floating interface right at the user's cursor for instant conversion.

## 2. Core User Experience (UX)
- **Frictionless Trigger:** The user highlights text (e.g., "5 kg" or "5") and presses a system-wide global hotkey.
- **Intelligent Capture:** The app programmatically injects a `Ctrl+C` (or `Cmd+C`) keystroke to copy the highlighted text, reads the clipboard, and then instantly restores the original clipboard content so the user doesn't lose their previously copied data.
- **Smart Parsing:** The app splits the captured string into a numerical value and its accompanying text (if any). The number is prepped for conversion, while the text (e.g., "kg" or "USD") auto-populates a search bar to filter the source unit.
- **Floating UI Overlay:** A borderless, minimalist UI appears at the exact coordinates of the user's mouse cursor. 
- **Quick Selection & Favorites:** The interface shows a configurable list of 5-10 target outputs. It prioritizes the user's 5 most recent or favorite conversions.
- **One-Click Actions:** Two minimalist buttons are included to "Favorite" a conversion path and to "Swap" the source and target units (e.g., USD -> BTC to BTC -> USD).

## 3. Architecture & Data Management
- **Single Background Process (System Tray):** The app runs continuously as a hidden background daemon to ensure instant UI rendering. It listens for the global hotkey and manages background tasks.
- **Offline Capable & Caching:** A background task runs every 8 hours to fetch the latest currency exchange rates from a free API (e.g., Open Exchange Rates). The data is cached locally, allowing the app to perform conversions instantly without waiting for network requests.
- **Extensible Conversions:** Handles both dynamic rates (currencies/crypto) and static physical unit conversions (distance, weight, temperature).

## 4. Tech Stack & Libraries
- **Language:** Rust (chosen for maximum performance, safety, and a minimal memory footprint).
- **GUI Framework:** Iced (incredibly fast, lightweight, and supports borderless/transparent floating windows).
- **Global Hotkeys:** `global-hotkey` crate for system-wide shortcut listening.
- **Clipboard Management:** `arboard` crate for reading and restoring clipboard state, combined with `enigo` or `rdev` to simulate the `Ctrl+C` keystroke.
- **Networking & Parsing:** `reqwest` for API requests, and `serde` / `serde_json` for parsing JSON payloads.
- **Storage:** A local JSON file. Using JSON ensures the configuration and cache are human-readable and easily modifiable by users. This file will store API caches, favorite conversions, the last 5 used conversions, and general UI preferences.