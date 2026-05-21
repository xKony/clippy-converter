# Clippy Converter

![Version](https://img.shields.io/badge/version-0.1.0-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Rust](https://img.shields.io/badge/rust-2024-orange)

A lightweight, background unit and currency converter with global hotkeys and local caching. It captures selected text via a system-wide hotkey, parses numerical values and units, and displays a floating, transparent UI at the mouse cursor coordinates for instant conversion.

## ✨ Features

- **Global Hotkey Trigger**: Captures highlighted text via simulated clipboard copying using a configurable system-wide shortcut (default: `Shift+Alt+C`).
- **Floating UI Overlay**: Displays a borderless, transparent, always-on-top window at the exact mouse cursor coordinates using `egui`. Includes native Windows transparency effects (Mica/Acrylic) for a modern, frosted-glass appearance.
- **Smart Parsing**: Automatically splits captured strings into numerical values and their accompanying unit or currency symbols.
- **Offline-First Conversions**: Uses a local `redb` database to cache exchange rates and store static unit conversions (length, weight, temperature, time).
- **Background API Workers**: Automatically fetches and updates fiat currency rates from Fawaz Ahmed's API (daily) and crypto prices from Binance (hourly).
- **Favorites & Sorting**: Allows pinning favorite units to the top of the conversion list for quick access.
- **Improved History Interaction**: New "Recent" section in the popup with quick-reopen and copy-to-clipboard functionality, logging to a local database with configurable retention.
- **System Tray Integration**: Runs silently in the background with a tray icon menu to open settings or exit.
- **Single Instance Lock**: Built-in protection to ensure only one instance runs at a time, preventing database locks.

## 🛠 Tech Stack

**Language & Framework**
- **Rust (Edition 2024)**
- **egui / eframe (0.34.1)** - Immediate mode GUI framework
- **tokio (1)** - Asynchronous runtime

**Database & Storage**
- **redb (4.1.0)** - Embedded key-value database
- **bincode (1.3.3)** - Binary serialization
- **directories (6.0)** - OS-specific directory resolution

**System Integration**
- **global-hotkey (0.7.0)** - System-wide shortcut listener
- **arboard (3.4)** - Clipboard access
- **enigo (0.6.1)** - Keystroke simulation (Ctrl+C)
- **tray-icon (0.19)** - System tray integration
- **single-instance (0.3.3)** - Single instance lock
- **window-vibrancy (0.7)** - Native Windows transparency (Mica/Acrylic)
- **open (5.3)** - Opening paths in system explorer

**Networking & Data Processing**
- **reqwest (0.13.2)** - HTTP client with JSON support
- **serde (1.0)** - Serialization/Deserialization
- **serde_json (1.0)** - JSON parsing
- **chrono (0.4)** - Date and time handling

**Testing**
- **tempfile (3.10)** - Temporary files for unit tests

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
│   ├── main.rs          # Application entry point and single instance lock
│   ├── models.rs        # Core data structures and local JSON configuration logic
│   ├── parser.rs        # String splitting and value extraction logic
│   ├── theme.rs         # UI theme and styling definitions
│   ├── ui.rs            # egui UI state machine, floating window, and tray menu
│   └── workers.rs       # Async tokio tasks for periodic background data refreshes
├── Cargo.toml           # Project dependencies, metadata, and strict linting rules
└── LICENSE              # MIT License
```

## 🚀 Getting Started

### Prerequisites

- **Rust**: Edition 2024 (install via `rustup`)
- **OS Compatibility**: Windows (tested natively). Requires OS-level support for global hotkeys, clipboard access, and transparent windows.

### Installation

```powershell
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

1. **Start the application**: It runs in the background and appears in your system tray.
2. **Select text**: Highlight a value and unit anywhere on your computer (e.g., `100 EUR`, `50 kg`, or `1.5 BTC`).
3. **Trigger conversion**: Press the global hotkey (`Shift+Alt+C` by default).
4. **View results**: A floating window appears at your mouse cursor displaying conversion results.
5. **Interact**: Use the search bar to filter units, click the star icon to favorite a unit, or right-click the tray icon for Settings.

## 📄 License

MIT - See [LICENSE](LICENSE) for details.
