use eframe::egui;

pub const POPUP_WIDTH: f32 = 350.0;
pub const POPUP_HEIGHT: f32 = 420.0;
const CURSOR_OFFSET: i32 = 12;

/// Returns the outer position for the converter popup near the cursor, clamped to the monitor work area.
///
/// Both the cursor and the returned position are in physical screen pixels.
/// `pixels_per_point` converts the popup's logical size into physical pixels so
/// the clamp accounts for display scaling.
#[must_use]
pub fn popup_position_at_cursor(cursor: (i32, i32), pixels_per_point: f32) -> egui::Pos2 {
    let x = cursor.0.saturating_add(CURSOR_OFFSET);
    let y = cursor.1.saturating_add(CURSOR_OFFSET);
    clamp_to_work_area(
        x,
        y,
        POPUP_WIDTH * pixels_per_point,
        POPUP_HEIGHT * pixels_per_point,
    )
}

#[must_use]
fn clamp_to_work_area(x: i32, y: i32, width: f32, height: f32) -> egui::Pos2 {
    let work = work_area_at_point(x, y);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Popup dimensions are small fixed pixel values"
    )]
    let width_i = width.round() as i32;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Popup dimensions are small fixed pixel values"
    )]
    let height_i = height.round() as i32;
    let max_x = work.right.saturating_sub(width_i);
    let max_y = work.bottom.saturating_sub(height_i);
    let clamped_x = x.clamp(work.left, max_x.max(work.left));
    let clamped_y = y.clamp(work.top, max_y.max(work.top));
    #[expect(
        clippy::cast_precision_loss,
        reason = "Screen coordinates fit in f32 mantissa for UI placement"
    )]
    {
        egui::pos2(clamped_x as f32, clamped_y as f32)
    }
}

struct WorkArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn work_area_at_point(x: i32, y: i32) -> WorkArea {
    #[cfg(windows)]
    {
        if let Some(area) = windows_work_area_at_point(x, y) {
            return area;
        }
    }
    WorkArea {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    }
}

#[cfg(windows)]
fn windows_work_area_at_point(x: i32, y: i32) -> Option<WorkArea> {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    unsafe {
        let point = POINT { x, y };
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        if monitor.is_invalid() {
            return None;
        }

        let mut info = MONITORINFO {
            cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).ok()?,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, std::ptr::from_mut(&mut info)).as_bool() {
            return None;
        }

        let work: RECT = info.rcWork;
        Some(WorkArea {
            left: work.left,
            top: work.top,
            right: work.right,
            bottom: work.bottom,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
    use super::*;

    #[test]
    fn popup_position_applies_cursor_offset() {
        let pos = popup_position_at_cursor((100, 200), 1.0);
        assert!(pos.x >= 112.0);
        assert!(pos.y >= 212.0);
    }
}
