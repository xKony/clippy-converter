# Clippy Converter

![Version](https://img.shields.io/badge/version-0.1.0-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Rust](https://img.shields.io/badge/rust-2024-orange)

A lightweight, background unit and currency converter with global hotkeys and local caching. It captures selected text via a system-wide hotkey, parses numerical values and units, and displays a floating UI at the mouse cursor coordinates for instant conversion.

## ✨ Features

- **Global Hotkey Trigger**: Captures highlighted text via simulated clipboard copying using a configurable system-wide shortcut (default: `Shift+Alt+C`). Selection capture is on by default.
- **Floating UI Overlay**: Displays a borderless, always-on-top window at the exact mouse cursor coordinates using `egui` (Glow / OpenGL).
- **Smart Parsing**: Splits captured strings into a value, source unit, and optional target (`100 USD to PLN`). Understands grouped digits (`1,234.56` / `1.234,56`), leading/trailing currency glyphs (`$100`, `100€`), and scientific notation.
- **Offline-First Conversions**: Uses a local `redb` database to cache exchange rates and store static unit conversions (length, weight, temperature, time, volume, area, speed, data).
- **Background API Workers**: Automatically fetches and updates fiat currency rates from Fawaz Ahmed's API (daily) and crypto prices from Binance (hourly).
- **Favorites & Sorting**: Allows pinning favorite units to the top of the conversion list for quick access.
- **History**: Optional "Recent" section in the popup with quick-reopen and copy-to-clipboard, logged locally with configurable retention.
- **System Tray Integration**: Runs silently in the background with a tray icon menu to open settings or exit.
- **Single Instance Lock**: Built-in protection to ensure only one instance runs at a time, preventing database locks.

## 🛠 Tech Stack

**Language & Framework**
- **Rust (Edition 2024)**
- **egui / eframe (0.34.1)** - Immediate mode GUI via Glow (OpenGL); avoids wgpu's heavy DX12 memory pools on Windows
- **tokio (1)** - Asynchronous runtime
- **tracing / tracing-subscriber** - Logging (`RUST_LOG` overrides the default `info` filter)

**Database & Storage**
- **redb (4.1.0)** - Embedded key-value database
- **bincode (1.3.3)** - Binary serialization
- **directories (6.0)** - OS-specific directory resolution

**System Integration**
- **global-hotkey (0.7.0)** - System-wide shortcut listener
- **arboard (3.4)** - Clipboard access
- **enigo (0.6.1)** - Keystroke simulation (Ctrl+C / Cmd+C)
- **tray-icon (0.19)** - System tray integration
- **single-instance (0.3.3)** - Single instance lock
- **open (5.3)** - Opening paths in system explorer
- **windows (0.58)** - Monitor work-area lookup (Windows only)

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
│   ├── format.rs        # Display vs copy number formatting
│   ├── history.rs       # Local logging and retention of past conversions
│   ├── hotkey.rs        # Parsing human-readable hotkeys into system structures
│   ├── main.rs          # Application entry point, tracing, and single instance lock
│   ├── models.rs        # Core data structures and local JSON configuration logic
│   ├── parser.rs        # Value, unit, and optional target extraction
│   ├── placement.rs     # Cursor-relative popup positioning with DPI-aware work-area clamp
│   ├── theme.rs         # UI theme and styling definitions
│   ├── ui.rs            # egui UI state machine, floating window, and tray menu
│   └── workers.rs       # Async tokio tasks for periodic background data refreshes
├── icons/               # SVG assets used by the popup
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

The application creates a `config.json` file in the user's default configuration directory (`%AppData%\clippy\clippy-converter\config.json` on Windows).
No environment variables (`.env`) are required. Optional: `RUST_LOG` (e.g. `RUST_LOG=debug`).

## 📖 Usage

1. **Start the application**: It runs in the background and appears in your system tray.
2. **Select text**: Highlight a value and unit anywhere on your computer (e.g. `100 EUR`, `50 kg`, `$1,000`, or `100 USD to PLN`).
3. **Trigger conversion**: Press the global hotkey (`Shift+Alt+C` by default). Capture of the current selection is on by default (toggle in Settings).
4. **View results**: A floating window appears at your mouse cursor displaying conversion results.
5. **Interact**: Use the search bar to filter units, click the star icon to favorite a unit, or right-click the tray icon for Settings.

## 📄 License

MIT - See [LICENSE](LICENSE) for details.
