# Release: Alpha 0.1.0

This alpha release introduces the core functionality of **Clippy Converter**, a lightweight Rust-based utility for instant unit and currency conversions via global hotkeys and a floating UI.

### 🚀 Core Functionalities

*   **Global Hotkey Trigger**: Press `Shift+Alt+C` (configurable) to instantly capture highlighted text or open the conversion window. Selection capture is on by default.
*   **Smart Selection Capture**: Automatically simulates `Ctrl+C` to grab selected text, parses it, and opens the converter at your mouse cursor.
*   **Extensive Unit Support**:
    *   **Fiat Currencies**: 150+ world currencies with daily exchange rate updates.
    *   **Cryptocurrencies**: USDT-quoted Binance pairs with hourly price updates (leveraged / 1000x tokens filtered out).
    *   **Physical Units**: Length, weight, temperature, time, volume (`L`, `gal`), area (`m2`, `acre`), speed (`km/h`, `mph`), and data (`KB`, `MiB`).
*   **Intelligent Parsing**:
    *   Recognizes unit aliases (e.g., `ft.` for `ft`, `kilo` for `kg`, `centigrade` for `C`).
    *   Handles grouped digits (`1,234.56`, `1.234,56`), currency glyphs (`$100`, `100€`), and `to`/`in` targets (`100 USD to PLN`).
    *   Handles currency multipliers (e.g., `5B USD`, `1.2M EUR`).
    *   Supports metric prefixes (e.g., `milligrams`, `kilometers`, `nanometers`).
*   **Offline-First Architecture**: Uses a local `redb` database to cache exchange rates and conversion factors, ensuring fast startups and offline availability.
*   **Interaction & History**:
    *   **Search & Filter**: Quickly find specific target units in the result list.
    *   **Favorites**: Star frequently used units to pin them to the top.
    *   **Recent History**: A dedicated section to re-view and copy previous conversions.
    *   **Click-to-Copy**: Click any result to copy the value directly to your clipboard.
*   **System Tray Integration**: Runs as a background daemon with a tray icon for quick access to settings or exit.
*   **Customizable Settings**:
    *   Rebind global hotkeys.
    *   Adjust API refresh intervals for fiat and crypto.
    *   Configure history retention periods.

### 🛠 Technical Details
*   Built with **Rust 2024** and **egui**.
*   Embedded **redb** for high-performance local storage.
*   Background workers for non-blocking API updates.
*   Strict `clippy` safety (no `unwrap` or `expect` in the codebase).

### ⚠️ Alpha Notice
This is an early alpha release. While the core engine is stable, please report any UI glitches or parsing issues via GitHub Issues. Transparency effects have been temporarily disabled in this version for maximum OS compatibility.
