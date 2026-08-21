use anyhow::{Context, Result};
use arboard::Clipboard;
use enigo::{Direction, Enigo, Key, Keyboard, Mouse, Settings};
use std::thread;
use std::time::{Duration, Instant};
use tracing::warn;

/// Total poll budget for the target app to answer the simulated Ctrl+C.
///
/// The old fixed 300 ms budget failed regularly on RDP sessions,
/// UAC-elevated windows, and busy machines where the clipboard update lands
/// several hundred milliseconds late. Success still exits the loop
/// immediately, so this only extends the worst-case wait before
/// [`CaptureOutcome::NoResponse`] is reported — never the common-case latency.
const CAPTURE_TOTAL_BUDGET: Duration = Duration::from_millis(700);

/// Deadline for the tight fast-path phase of polling. Most copies complete
/// within a few milliseconds, so the loop spins aggressively (zero to 2 ms
/// sleeps) until this deadline to keep normal hotkey-to-popup latency at
/// effectively zero extra delay; past it, the tail relaxes (see below).
const CAPTURE_FAST_PHASE: Duration = Duration::from_millis(50);

/// Poll interval during the slow tail phase (after [`CAPTURE_FAST_PHASE`]).
/// Relaxed 10 ms polling keeps the CPU idle while still catching late copies
/// from slow/RDP targets well past the fast phase.
const CAPTURE_TAIL_INTERVAL: Duration = Duration::from_millis(10);

/// Outcome of a selection-capture attempt.
///
/// Mapping of real-world situations:
/// - The target app received the simulated Ctrl+C and wrote non-empty text →
///   [`CaptureOutcome::Text`].
/// - The clipboard changed from our marker to an empty string →
///   [`CaptureOutcome::EmptySelection`] (the keystrokes were delivered and
///   there was simply nothing selected).
/// - The marker was never replaced within the poll budget →
///   [`CaptureOutcome::NoResponse`] (the target window never answered the
///   copy: UAC-elevated windows that cannot receive simulated input,
///   clipboard-manager interference, RDP or very slow machines).
///
/// Hard failures such as keystroke-simulation errors remain `Err`
/// ([`anyhow::Error`]); this enum only replaces the old silent
/// empty-string return that conflated these situations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// Selection was copied and captured.
    Text(String),
    /// The copy responded with an empty string: nothing was selected.
    EmptySelection,
    /// The marker was never replaced within the budget: no response.
    NoResponse,
}

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
    /// A successful copy maps to [`CaptureOutcome::Text`]; an empty response
    /// to [`CaptureOutcome::EmptySelection`]; no replacement of the detection
    /// marker within [`CAPTURE_TOTAL_BUDGET`] to [`CaptureOutcome::NoResponse`].
    ///
    /// # Errors
    /// Returns an error if any clipboard operation or keystroke simulation fails.
    pub fn capture_selection(&mut self) -> Result<CaptureOutcome> {
        // 1. Store original clipboard content. Reading can legitimately fail
        // when the clipboard holds no *text* (empty or an image), so only
        // unexpected errors are worth surfacing here.
        let original_content = match self.clipboard.get_text() {
            Ok(text) => Some(text),
            Err(error) => {
                warn!(%error, "failed to save original clipboard content before capture");
                None
            }
        };

        // 2. Set a unique marker to the clipboard to detect when the copy completes
        let marker = format!(
            "clippy_converter_marker_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        if let Err(error) = self.clipboard.set_text(marker.clone()) {
            // Without the marker we cannot tell an arriving copy from stale
            // content, but the capture attempt is still worth making.
            warn!(%error, "failed to set clipboard capture marker");
        }

        // Brief yield so the OS registers the marker before Ctrl+C
        thread::sleep(Duration::from_millis(5));

        // 3. Trigger Ctrl+C
        #[cfg(target_os = "macos")]
        let modifier = Key::Meta;
        #[cfg(not(target_os = "macos"))]
        let modifier = Key::Control;

        // Release physical modifiers that might be held down by the user
        // to avoid combinations like Ctrl+Shift+Alt+C. Best-effort: failure
        // only risks a wrong key combination, not data loss.
        for key in [Key::Shift, Key::Alt, Key::Control, Key::Meta] {
            if let Err(error) = self.enigo.key(key, Direction::Release) {
                warn!(%error, "failed to release held modifier key before capture");
            }
        }

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
        let mut new_text: Option<String> = None;
        let mut polls: u32 = 0;

        while start_time.elapsed() < CAPTURE_TOTAL_BUDGET {
            if let Ok(text) = self.clipboard.get_text()
                && text != marker
            {
                new_text = Some(text);
                break;
            }
            polls = polls.saturating_add(1);
            let delay = if start_time.elapsed() < CAPTURE_FAST_PHASE {
                // Tight spin for the first few polls; most copies complete quickly
                match polls {
                    0..=8 => Duration::ZERO,
                    9..=20 => Duration::from_millis(1),
                    _ => Duration::from_millis(2),
                }
            } else {
                // Slow tail: cheap polling for stragglers (RDP/elevated/slow
                // machines) instead of giving up at 300 ms
                CAPTURE_TAIL_INTERVAL
            };
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }

        // 5. Restore original content if it existed
        if let Some(original) = original_content {
            if let Err(error) = self.clipboard.set_text(original) {
                warn!(%error, "failed to restore original clipboard content");
            }
        } else if let Err(error) = self.clipboard.clear() {
            warn!(%error, "failed to clear clipboard after capture");
        }

        Ok(match new_text {
            Some(text) if text.is_empty() => CaptureOutcome::EmptySelection,
            Some(text) => CaptureOutcome::Text(text),
            None => CaptureOutcome::NoResponse,
        })
    }

    /// Sets the system clipboard content.
    ///
    /// # Errors
    /// Returns an error if the clipboard content cannot be set.
    pub fn set_text(&mut self, text: String) -> Result<()> {
        self.clipboard
            .set_text(text)
            .context("Failed to set clipboard text")
    }

    /// Returns the current mouse cursor position as `(x, y)`.
    ///
    /// Falls back to `(100, 100)` if the position cannot be determined.
    #[must_use]
    pub fn cursor_position(&self) -> (i32, i32) {
        self.enigo.location().unwrap_or((100, 100))
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
