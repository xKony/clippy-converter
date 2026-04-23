Here is the detailed, step-by-step implementation plan for building the app:

# Clippy Converter - Implementation Plan

## Phase 1: Project Skeleton & Core Dependencies

**Goal:** Set up the Rust project, add required crates, and create the basic application structure.

1. Update `Cargo.toml` with necessary dependencies:
   - `iced` (GUI)
   - `global-hotkey` (System-wide shortcuts)
   - `arboard` (Clipboard management)
   - `enigo` or `rdev` (Keystroke simulation)
   - `reqwest` & `tokio` (Networking & Async runtime)
   - `serde` & `serde_json` (Serialization and local storage)
   - `directories` (OS-specific config paths)
2. Define the basic application entry point (`main.rs`) with Tokio runtime setup.

## Phase 2: Data Models & Storage

**Goal:** Define the structures for units, currencies, configuration, and caching, and implement saving/loading from disk.

1. Create `models.rs` defining:
   - `Config`: User preferences (favorites, hotkey mapping, list size).
   - `Cache`: Stored API responses and timestamps.
   - `ConversionResult`: Struct for the parsed input and available outputs.
2. Implement file I/O operations (read/write JSON) using the OS standard config directory (via `directories` crate).

## Phase 3: Networking & Caching

**Goal:** Implement the background service that fetches currency rates.

1. Create an API client module (`api.rs`) to fetch data from the chosen currency API.
2. Implement a background async task that wakes up every 8 hours to fetch new rates.
3. Update the local JSON cache with new data upon successful fetch.

## Phase 4: Clipboard & Input Parsing

**Goal:** Programmatically copy highlighted text, read it, restore clipboard, and parse the value.

1. Implement the keystroke injection to simulate `Ctrl+C`/`Cmd+C` using `enigo`.
2. Use `arboard` to read the new clipboard content and store the old content to restore it immediately after.
3. Write a robust parser (`parser.rs`) that splits the copied string into a numeric `f64` value and an optional string unit (e.g., "5.5" and "kg").

## Phase 5: Conversion Engine

**Goal:** The core mathematical logic for converting units and currencies.

1. Build a conversion registry (`converter.rs`) that holds dynamic rates (from Phase 3) and static rates (physical units).
2. Implement functions to convert a parsed number + source unit into a list of target unit results.
3. Add support for "favorites" and "recent" sorting logic based on the user's `Config`.

## Phase 6: Global Hotkey Listener

**Goal:** Listen for the system-wide shortcut to trigger the application flow.

1. Setup `global-hotkey` to register the specific shortcut combo.
2. Create an event loop that listens for the hotkey event.
3. Upon trigger, execute Phase 4 (grab text), Phase 5 (convert), and signal the UI to appear.

## Phase 7: Iced UI Overlay

**Goal:** Build the floating, borderless UI.

1. Setup an Iced application that runs in the background (potentially hidden).
2. Configure the window to be borderless, transparent, and floating on top.
3. Implement the UI layout:
   - Search bar (auto-populated with the parsed unit text).
   - List of conversion results (favorites pinned, then recents).
   - Minimalist "Favorite" (star) and "Swap" buttons.
4. Add logic to position the window at the user's current mouse coordinates.

## Phase 8: Integration & Polish

**Goal:** Tie all components together into a seamless experience.

1. Integrate the hotkey listener with the Iced event loop (or run them concurrently using channels).
2. Handle edge cases (network failure during background sync, unparseable text copied, unsupported units).
3. Final performance profiling to ensure the app is "lightning-fast".
4. (Optional) Add a System Tray icon for graceful exit and basic settings management.

I've also saved this plan inside your local temporary plans directory so we can reference it when you're ready to start building! Let me know if you would like to proceed with Phase 1.
