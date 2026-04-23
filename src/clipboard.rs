use anyhow::{Context, Result};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::thread;
use std::time::{Duration, Instant};

/// Manages clipboard operations and programmatic text copying.
pub struct ClipboardManager {
    clipboard: Clipboard,
    enigo: Enigo,
}

impl ClipboardManager {
    /// Creates a new `ClipboardManager`.
    ///
    /// # Errors
    /// Returns an error if the system clipboard cannot be initialized.
    pub fn new() -> Result<Self> {
        Ok(Self {
            clipboard: Clipboard::new().context("Failed to initialize clipboard")?,
            enigo: Enigo::new(&Settings::default()).context("Failed to initialize enigo")?,
        })
    }

    /// Captures the current selection by simulating a copy command (Ctrl+C).
    ///
    /// This method preserves the original clipboard content, triggers a copy,
    /// reads the new content, and then restores the original content.
    ///
    /// # Errors
    /// Returns an error if any clipboard operation or keystroke simulation fails.
    pub fn capture_selection(&mut self) -> Result<String> {
        // 1. Store original clipboard content
        let original_content = self.clipboard.get_text().ok();

        // 2. Set a unique marker to the clipboard to detect when the copy completes
        let marker = format!(
            "clippy_converter_marker_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let _ = self.clipboard.set_text(marker.clone());

        // Give the OS a tiny moment to register the new clipboard state
        thread::sleep(Duration::from_millis(10));

        // 3. Trigger Ctrl+C
        #[cfg(target_os = "macos")]
        let modifier = Key::Meta;
        #[cfg(not(target_os = "macos"))]
        let modifier = Key::Control;

        self.enigo
            .key(modifier, Direction::Press)
            .context("Failed to press modifier key")?;
        self.enigo
            .key(Key::Unicode('c'), Direction::Click)
            .context("Failed to click 'c' key")?;
        self.enigo
            .key(modifier, Direction::Release)
            .context("Failed to release modifier key")?;

        // 4. Poll clipboard until the content changes from the marker
        let start_time = Instant::now();
        let mut captured_text = String::new();

        while start_time.elapsed() < Duration::from_millis(500) {
            if let Ok(text) = self.clipboard.get_text()
                && text != marker
            {
                captured_text = text;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        // 5. Restore original content if it existed
        if let Some(original) = original_content {
            let _ = self.clipboard.set_text(original);
        } else {
            let _ = self.clipboard.clear();
        }

        Ok(captured_text)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;
    use std::sync::{LazyLock, Mutex};

    // Use a global mutex to prevent tests from clashing on the shared system clipboard
    static CLIPBOARD_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    #[ignore = "This test interacts with the system clipboard and requires a selection to work correctly."]
    fn test_capture_selection() {
        let _lock = CLIPBOARD_MUTEX.lock().unwrap();
        let mut manager = ClipboardManager::new().unwrap();

        // This is hard to test automatically without a real selection,
        // but we can verify that the original clipboard is preserved.
        let original = "original content";
        manager.clipboard.set_text(original.to_string()).unwrap();

        // Trigger capture (this will likely fail or capture nothing in CI)
        let _ = manager.capture_selection();

        assert_eq!(manager.clipboard.get_text().unwrap(), original);
    }
}
