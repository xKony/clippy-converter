//! Platform-specific window visual effects (transparency / vibrancy).

/// Applies a frosted-glass effect to the popup on Windows (WGPU alpha alone is unreliable).
///
/// # Errors
/// Returns an error if the native window handle is unavailable or the OS rejects the effect.
#[cfg(windows)]
pub fn try_apply_popup_effect(
    frame: &eframe::Frame,
    applied: &mut bool,
) -> Result<(), window_vibrancy::Error> {
    use raw_window_handle::HasWindowHandle;
    use window_vibrancy::{apply_acrylic, apply_blur, apply_mica};

    if *applied {
        return Ok(());
    }

    let handle = frame
        .window_handle()
        .map_err(window_vibrancy::Error::NoWindowHandle)?;

    // Mica on Win11 when available; acrylic/blur as fallback.
    if apply_mica(handle, None).is_ok() {
        *applied = true;
        return Ok(());
    }
    if apply_acrylic(handle, Some((24, 24, 28, 140))).is_ok() {
        *applied = true;
        return Ok(());
    }
    apply_blur(handle, Some((24, 24, 28, 140)))?;
    *applied = true;
    Ok(())
}

#[cfg(not(windows))]
pub fn try_apply_popup_effect(_frame: &eframe::Frame, _applied: &mut bool) -> Result<(), ()> {
    Ok(())
}
