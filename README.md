# Clippy Converter

![Version](https://img.shields.io/badge/version-0.1.0-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Rust](https://img.shields.io/badge/rust-2024-orange)

A lightweight, background unit and currency converter with global hotkeys and local caching. It captures selected text via a system-wide hotkey, parses numerical values and units, and displays a floating, transparent UI at the mouse cursor coordinates for instant conversion.

## ✨ Features

- **Global Hotkey Trigger**: Captures highlighted text via simulated clipboard copying using a configurable system-wide shortcut (default: `Shift+Alt+C`).
- **Floating UI Overlay**: Displays a borderless, transparent, always-on-top window at the exact mouse cursor coordinates.
- **Smart Parsing**: Automatically splits captured strings into numerical values and their accompanying unit or currency symbols.
- **Offline-First Conversions**: Uses a local `redb` database to cache exchange rates and store static unit conversions (length, weight, temperature).
- **Background API Workers**: Automatically fetches and updates fiat currency rates from Fawaz Ahmed's API (daily) and crypto prices from Binance (hourly).
- **Favorites & Sorting**: Allows pinning favorite units to the top of the conversion list for quick access.
- **Conversion History**: Logs past conversions to a local file with configurable retention periods (e.g., 7 days, 30 days, 1 year).
- **System Tray Integration**: Runs silently in the background with a tray icon menu to open settings or exit.
- **Single Instance Lock**: Built-in protection to ensure only one instance runs at a time, preventing database locks.

## 🛠 Tech Stack

**Language & Framework**
- Rust (Edition 2024)
- `iced` (0.14.0) - GUI framework
- `tokio` (1) - Asynchronous runtime

**Database & Storage**
- `redb` (4.1.0) - Embedded key-value database
- `bincode` (1.3.3) - Binary serialization
- `directories` (6.0) - OS-specific directory resolution

**System Integration**
- `global-hotkey` (0.7.0) - System-wide shortcut listener
- `arboard` (3.4) - Clipboard access
- `enigo` (0.6.1) - Keystroke simulation (Ctrl+C)
- `tray-icon` (0.19) - System tray integration
- `single-instance` (0.3.3) - Single instance lock
- `open` (5.3) - Opening paths in system explorer

**Networking & Data Processing**
- `reqwest` (0.13.2) - HTTP client
- `serde` (1.0) - Serialization/Deserialization
- `serde_json` (1.0) - JSON parsing
- `chrono` (0.4) - Date and time handling

**Testing**
- `tempfile` (3.10) - Temporary files for unit tests

## 📁 Project Structure

```text
.
├── src/
│   ├── api.rs           # External HTTP requests to Binance and fiat currency APIs
│   ├── clipboard.rs     # Clipboard capture via Enigo (Ctrl+C) and Arboard
│   ├── converter.rs     # Core engine for calculating unit and currency conversions
│   ├── db.rs            # Thread-safe wrapper for redb embedded database
│   ├── history.rs       # Local logging and retention of past conversions
│   ├── hotkey.rs        # Parsing human-readable hotkeys into system structures
│   ├── main.rs          # Application entry point, single instance lock, and Iced daemon setup
│   ├── models.rs        # Core data structures and local JSON configuration logic
│   ├── parser.rs        # String splitting and value extraction logic
│   ├── ui.rs            # Iced UI state machine, floating window, and tray menu
│   └── workers.rs       # Async tokio tasks for periodic background data refreshes
├── Cargo.toml           # Project dependencies, metadata, and strict linting rules
├── README.md            # Project documentation
└── project-whitepaper.md # Original architectural and UX vision
```

## 🚀 Getting Started

### Prerequisites

- **Rust**: Edition 2024 (install via `rustup`)
- **OS Compatibility**: Windows (tested natively). Requires OS-level support for global hotkeys, clipboard access, and transparent windows.

### Installation

```bash
# Clone the repository
git clone https://github.com/xKony/clippy-converter.git
cd clippy-converter

# Build and run the application in development mode
cargo run

# Or build the optimized production binary
cargo build --release
```

### Configuration

The application creates a `config.json` file in the user's default configuration directory (e.g., `AppData/Roaming/com/clippy/clippy-converter/config.json` on Windows).
No environment variables (`.env`) are required.

## 📖 Usage

1. Start the application. It will run in the background and appear in your system tray.
2. Highlight a value and unit anywhere on your computer (e.g., `100 EUR`, `50 kg`, or `1.5 BTC`).
3. Press the global hotkey (`Shift+Alt+C` by default).
4. A floating window will appear at your mouse cursor displaying the conversion results.
5. Use the search bar to filter target units, click the star icon to favorite a unit, or click the swap icon to reverse the conversion.
6. Right-click the system tray icon to access Settings or Quit the application.

## 📄 License

MIT
