//! Cross-instance activation: a second launch pings the live process so it
//! surfaces its settings window instead of exiting looking like a crash.
//!
//! Windows uses a session-local named auto-reset event: the first instance
//! creates it and blocks a thread on `WaitForSingleObject`; later launches
//! open the same name and call `SetEvent`. The second instance then exits
//! successfully. Non-Windows targets keep the previous behavior (the second
//! launch reports the conflict and exits with an error).

use anyhow::Result;
use tracing::warn;

/// Session-local named kernel object shared by all instances of the app.
#[cfg(windows)]
const EVENT_NAME: &str = r"Local\com.clippy.clippy-converter.activate";

/// How long a second launch waits for the live process to publish its
/// activation event before giving up. Covers first-instance startup, which
/// acquires the single-instance lock before creating the event.
#[cfg(windows)]
const PING_RETRY_BUDGET_MS: u64 = 2_000;

/// Asks the already-running instance to surface itself (show + focus its
/// settings window). Best-effort by design; callers fall back to the old
/// "already running" error when this fails.
///
/// No-op failure on non-Windows targets (activation is not implemented there).
///
/// # Errors
/// Returns an error when the event cannot be opened within the retry budget
/// or signaling it fails.
pub fn notify_running_instance() -> Result<()> {
    #[cfg(windows)]
    {
        windows_notify_running_instance()
    }
    #[cfg(not(windows))]
    {
        Err(anyhow::anyhow!(
            "activation ping is not supported on this platform"
        ))
    }
}

/// Spawns a background thread that invokes `on_activate` once per ping.
///
/// Used by the UI wiring to forward pings into the app's event channel. The
/// thread lives until the wait fails (typically process exit, which also
/// destroys the named event).
pub fn spawn_activation_thread(on_activate: impl FnMut() + Send + 'static) {
    #[cfg(windows)]
    {
        windows_spawn_activation_thread(on_activate);
    }
    #[cfg(not(windows))]
    {
        let _ = on_activate; // Activation ping is Windows-only.
    }
}

#[cfg(windows)]
fn windows_notify_running_instance() -> Result<()> {
    use anyhow::Context;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    use windows::core::HSTRING;

    let deadline = Instant::now() + Duration::from_millis(PING_RETRY_BUDGET_MS);
    let handle = loop {
        let name = HSTRING::from(EVENT_NAME);
        // SAFETY: `name` outlives the call; no security attributes requested.
        let opened = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &name) };
        match opened {
            Ok(handle) => break handle,
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(err).context("activation event not found");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };

    // SAFETY: `handle` was opened above and is closed right after signaling.
    let signaled = unsafe { SetEvent(handle) };
    // SAFETY: `handle` was opened above and is not used after this close.
    let _ = unsafe { CloseHandle(handle) };
    signaled.context("failed to signal the running instance")
}

#[cfg(windows)]
fn windows_spawn_activation_thread(mut on_activate: impl FnMut() + Send + 'static) {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::HSTRING;

    let name = HSTRING::from(EVENT_NAME);
    // SAFETY: `name` outlives the call; no attributes; auto-reset, initially
    // unsignaled. An ERROR_ALREADY_EXISTS result would still yield a valid
    // handle that works for ping semantics.
    let Ok(event) = (unsafe { CreateEventW(None, false, false, &name) }) else {
        warn!("failed to create activation event; second launches will report an error");
        return;
    };

    // `HANDLE` wraps a raw pointer and is not `Send`, but kernel-object
    // handles are process-wide, not thread-bound, so passing the raw value
    // across the thread boundary is sound; it is reconstructed inside.
    let event_raw = event.0 as usize;

    std::thread::spawn(move || {
        // Constructing the wrapper is safe; only *using* the handle is unsafe.
        let event = windows::Win32::Foundation::HANDLE(event_raw as *mut core::ffi::c_void);
        loop {
            // SAFETY: `event` is a valid event handle owned by this thread.
            let wait = unsafe { WaitForSingleObject(event, INFINITE) };
            if wait != WAIT_OBJECT_0 {
                warn!(?wait, "activation wait failed; stopping listener");
                break;
            }
            on_activate();
        }
        // SAFETY: `event` was created above and is not used after this close.
        let _ = unsafe { CloseHandle(event) };
    });
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn event_name_should_be_session_local_and_stable() {
        // Renaming the event would leave upgraded instances unable to ping
        // instances started by older binaries (and vice versa).
        assert_eq!(
            super::EVENT_NAME,
            r"Local\com.clippy.clippy-converter.activate"
        );
    }
}
