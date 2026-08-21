//! Popup placement: cursor-relative positioning clamped to the nearest monitor work area.
//!
//! Work-area sources, tried in order until one succeeds:
//! - Windows: Win32 `MonitorFromPoint` + `GetMonitorInfoW` (per-monitor work area).
//! - macOS: `CoreGraphics` display bounds with an approximated menu-bar strip reserved.
//! - X11: RANDR monitor geometries; also serves `XWayland` sessions.
//! - Anything else (including native Wayland, which exposes no global cursor
//!   position API): the [`FALLBACK_WORK_AREA`] rect.

use eframe::egui;

pub const POPUP_WIDTH: f32 = 350.0;
pub const POPUP_HEIGHT: f32 = 420.0;
const CURSOR_OFFSET: i32 = 12;

/// Fallback work area used when the platform cannot report monitor geometry.
const FALLBACK_WORK_AREA: WorkArea = WorkArea {
    left: 0,
    top: 0,
    right: 1920,
    bottom: 1080,
};

/// Returns the outer position for the converter popup near the cursor, clamped to the monitor work area.
///
/// Both the cursor and the returned position are in physical screen pixels.
/// `pixels_per_point` converts the popup's logical size into physical pixels so
/// the clamp accounts for display scaling.
///
/// The work area comes from, in order: Win32 monitor info (Windows), `CoreGraphics`
/// display bounds (macOS), X11 RANDR monitors (`XWayland` included), or a fixed
/// fallback rect when none of those are available (e.g. native Wayland).
#[must_use]
pub fn popup_position_at_cursor(cursor: (i32, i32), pixels_per_point: f32) -> egui::Pos2 {
    let x = cursor.0.saturating_add(CURSOR_OFFSET);
    let y = cursor.1.saturating_add(CURSOR_OFFSET);
    clamp_position_in_area(
        work_area_at_point(x, y),
        x,
        y,
        POPUP_WIDTH * pixels_per_point,
        POPUP_HEIGHT * pixels_per_point,
    )
}

/// Clamps an intended popup origin so the popup fits inside `work_area`.
///
/// Pure geometry, independent of any OS query: the origin is nudged left/up when
/// the popup would overflow the right/bottom edge, and pulled to the area's
/// left/top edge when it starts outside or is larger than the area itself.
#[must_use]
fn clamp_position_in_area(
    work_area: WorkArea,
    x: i32,
    y: i32,
    width: f32,
    height: f32,
) -> egui::Pos2 {
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
    let max_x = work_area.right.saturating_sub(width_i);
    let max_y = work_area.bottom.saturating_sub(height_i);
    let clamped_x = x.clamp(work_area.left, max_x.max(work_area.left));
    let clamped_y = y.clamp(work_area.top, max_y.max(work_area.top));
    #[expect(
        clippy::cast_precision_loss,
        reason = "Screen coordinates fit in f32 mantissa for UI placement"
    )]
    {
        egui::pos2(clamped_x as f32, clamped_y as f32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WorkArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl WorkArea {
    /// Builds a work area from a monitor's top-left corner and size, rejecting
    /// degenerate (non-positive) sizes and extents that overflow `i32`.
    ///
    /// Only compiled where platform code constructs areas from raw geometry;
    /// kept available under test so the math stays covered on every host.
    #[cfg(any(not(windows), test))]
    const fn from_origin_size(left: i32, top: i32, width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        match (left.checked_add(width), top.checked_add(height)) {
            (Some(right), Some(bottom)) => Some(Self {
                left,
                top,
                right,
                bottom,
            }),
            _ => None,
        }
    }

    /// Whether the point lies inside the area, treated as half-open:
    /// `[left, right) x [top, bottom)`.
    #[cfg(any(not(windows), test))]
    #[must_use]
    const fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// A candidate monitor reported by the platform, with its primary status.
#[cfg(any(not(windows), test))]
#[derive(Clone, Copy, Debug)]
struct MonitorCandidate {
    area: WorkArea,
    primary: bool,
}

/// Picks the work area for a point: the containing monitor, else the primary one.
///
/// Returns `None` when no monitor contains the point and no monitor is primary.
#[cfg(any(not(windows), test))]
#[must_use]
fn select_monitor_containing(monitors: &[MonitorCandidate], x: i32, y: i32) -> Option<WorkArea> {
    monitors
        .iter()
        .find(|monitor| monitor.area.contains_point(x, y))
        .or_else(|| monitors.iter().find(|monitor| monitor.primary))
        .map(|monitor| monitor.area)
}

fn work_area_at_point(x: i32, y: i32) -> WorkArea {
    #[cfg(windows)]
    if let Some(area) = windows_work_area_at_point(x, y) {
        return area;
    }
    #[cfg(target_os = "macos")]
    if let Some(area) = macos_work_area_at_point(x, y) {
        return area;
    }
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    if let Some(area) = x11_work_area_at_point(x, y) {
        return area;
    }
    tracing::debug!("no monitor work area found for ({x}, {y}); using fallback");
    FALLBACK_WORK_AREA
}

#[cfg(windows)]
fn windows_work_area_at_point(x: i32, y: i32) -> Option<WorkArea> {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
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

/// Approximate menu-bar height reserved at the top of the display hosting the
/// global origin. Real heights vary by Mac model (~24-25 pt); close enough to
/// keep the popup clear of the menu bar without pulling in `AppKit`.
#[cfg(any(target_os = "macos", test))]
const MACOS_MENU_BAR_HEIGHT: i32 = 24;

#[cfg(target_os = "macos")]
fn macos_work_area_at_point(x: i32, y: i32) -> Option<WorkArea> {
    let mut candidates = macos_work_area_candidates()?;
    reserve_macos_menu_bar(&mut candidates);
    select_monitor_containing(&candidates, x, y)
}

/// Enumerates displays via `CoreGraphics` and converts their bounds into candidates.
///
/// Best-effort: any failure or empty enumeration yields `None` so the caller can
/// fall back. Bounds use the global display space whose primary display starts at
/// the top-left origin with y increasing downward, matching our cursor coords up
/// to Retina scaling.
#[cfg(target_os = "macos")]
fn macos_work_area_candidates() -> Option<Vec<MonitorCandidate>> {
    use core_graphics::display::CGDisplay;

    let main_id = CGDisplay::main().id;
    let display_ids = CGDisplay::active_displays().ok()?;
    let mut candidates = Vec::new();
    for id in display_ids {
        let bounds = CGDisplay::new(id).bounds();
        let Ok(area) = WorkArea::from_origin_size(
            round_cg_coordinate(bounds.origin.x),
            round_cg_coordinate(bounds.origin.y),
            round_cg_coordinate(bounds.size.width),
            round_cg_coordinate(bounds.size.height),
        ) else {
            continue;
        };
        candidates.push(MonitorCandidate {
            area,
            primary: id == main_id,
        });
    }
    (!candidates.is_empty()).then_some(candidates)
}

/// Shrinks the top of the display holding the global origin by the menu-bar height.
///
/// The macOS menu bar lives on the primary (origin-hosting) display; secondary
/// displays keep their full bounds.
#[cfg(any(target_os = "macos", test))]
fn reserve_macos_menu_bar(monitors: &mut [MonitorCandidate]) {
    for candidate in monitors {
        if candidate.area.contains_point(0, 0) {
            let reserved_top = candidate.area.top.saturating_add(MACOS_MENU_BAR_HEIGHT);
            if reserved_top < candidate.area.bottom {
                candidate.area.top = reserved_top;
            }
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[must_use]
const fn round_cg_coordinate(value: f64) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Display coordinates fit comfortably in i32"
    )]
    {
        value.round() as i32
    }
}

/// Picks the work area from an X11 server using RANDR monitor geometry.
///
/// Also serves `XWayland` sessions. Native Wayland compositors expose neither RANDR
/// nor a global cursor position, so such setups fall back to [`FALLBACK_WORK_AREA`].
/// Panel struts are not subtracted; the popup is borderless and always-on-top, so
/// briefly overlapping a panel is tolerated rather than parsing per-output strut
/// properties.
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn x11_work_area_at_point(x: i32, y: i32) -> Option<WorkArea> {
    let candidates = x11_work_area_candidates()?;
    select_monitor_containing(&candidates, x, y)
}

/// Connects to the default X display and collects active RANDR monitors.
///
/// Coordinates are root-window pixels matching our physical-pixel convention.
/// Any connection, protocol, or extension failure yields `None`; degenerate
/// monitor entries are skipped.
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn x11_work_area_candidates() -> Option<Vec<MonitorCandidate>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::randr::ConnectionExt as _;

    let (connection, screen_index) = x11rb::connect(None).ok()?;
    let root = connection.setup().roots.get(screen_index)?.root;
    let reply = connection
        .randr_get_monitors(root, true)
        .ok()?
        .reply()
        .ok()?;
    let candidates: Vec<MonitorCandidate> = reply
        .monitors
        .iter()
        .filter_map(|monitor| {
            Some(MonitorCandidate {
                area: WorkArea::from_origin_size(
                    i32::from(monitor.x),
                    i32::from(monitor.y),
                    i32::from(monitor.width),
                    i32::from(monitor.height),
                )?,
                primary: monitor.primary,
            })
        })
        .collect();
    (!candidates.is_empty()).then_some(candidates)
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

    #[test]
    fn clamp_keeps_position_inside_work_area() {
        let work = WorkArea {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let pos = clamp_position_in_area(work, 112, 212, POPUP_WIDTH, POPUP_HEIGHT);
        assert_eq!(pos.x, 112.0);
        assert_eq!(pos.y, 212.0);
    }

    #[test]
    fn clamp_pulls_popup_back_inside_right_and_bottom_edges() {
        let work = WorkArea {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let pos = clamp_position_in_area(work, 1900, 1000, POPUP_WIDTH, POPUP_HEIGHT);
        assert_eq!(pos.x, 1920.0 - POPUP_WIDTH);
        assert_eq!(pos.y, 1080.0 - POPUP_HEIGHT);
    }

    #[test]
    fn clamp_respects_nonzero_work_area_origin() {
        // A monitor placed right of the primary one.
        let right_monitor = WorkArea {
            left: 2560,
            top: 0,
            right: 4480,
            bottom: 1080,
        };
        let pos = clamp_position_in_area(right_monitor, 2400, 100, POPUP_WIDTH, POPUP_HEIGHT);
        assert_eq!(pos.x, 2560.0);
        assert_eq!(pos.y, 100.0);

        // A monitor placed above the primary one (negative coordinates).
        let upper_monitor = WorkArea {
            left: 0,
            top: -1440,
            right: 2560,
            bottom: 0,
        };
        let pos = clamp_position_in_area(upper_monitor, 100, -1500, POPUP_WIDTH, POPUP_HEIGHT);
        assert_eq!(pos.x, 100.0);
        assert_eq!(pos.y, -1440.0);
    }

    #[test]
    fn clamp_handles_popup_larger_than_work_area() {
        let tiny = WorkArea {
            left: 100,
            top: 50,
            right: 300,
            bottom: 250,
        };
        let pos = clamp_position_in_area(tiny, 150, 150, 1000.0, 1000.0);
        assert_eq!(pos.x, 100.0);
        assert_eq!(pos.y, 50.0);
    }

    #[test]
    fn clamp_scales_popup_size_with_pixels_per_point() {
        let work = WorkArea {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        // At 2x scaling the popup occupies twice the physical pixels.
        let popup_width = POPUP_WIDTH * 2.0;
        let popup_height = POPUP_HEIGHT * 2.0;
        let pos = clamp_position_in_area(work, 1800, 1000, popup_width, popup_height);
        assert_eq!(pos.x, 1920.0 - popup_width);
        assert_eq!(pos.y, 1080.0 - popup_height);
    }

    #[test]
    fn from_origin_size_builds_and_rejects_degenerate_areas() {
        assert_eq!(
            WorkArea::from_origin_size(10, 20, 300, 200),
            Some(WorkArea {
                left: 10,
                top: 20,
                right: 310,
                bottom: 220,
            })
        );
        assert_eq!(WorkArea::from_origin_size(0, 0, 0, 100), None);
        assert_eq!(WorkArea::from_origin_size(0, 0, 100, -1), None);
    }

    #[test]
    fn from_origin_size_rejects_overflowing_extents() {
        assert_eq!(WorkArea::from_origin_size(i32::MAX - 5, 0, 100, 100), None);
        assert_eq!(WorkArea::from_origin_size(0, i32::MAX - 5, 100, 100), None);
    }

    #[test]
    fn contains_point_is_left_top_inclusive_and_right_bottom_exclusive() {
        let work = WorkArea {
            left: 10,
            top: 20,
            right: 110,
            bottom: 120,
        };
        assert!(work.contains_point(10, 20));
        assert!(work.contains_point(109, 119));
        assert!(!work.contains_point(110, 20));
        assert!(!work.contains_point(10, 120));
        assert!(!work.contains_point(9, 20));
        assert!(!work.contains_point(10, 19));
    }

    #[test]
    fn select_monitor_prefers_monitor_containing_point() {
        let monitors = [
            MonitorCandidate {
                area: WorkArea {
                    left: 0,
                    top: 0,
                    right: 2560,
                    bottom: 1080,
                },
                primary: true,
            },
            MonitorCandidate {
                area: WorkArea {
                    left: 2560,
                    top: 0,
                    right: 4480,
                    bottom: 1080,
                },
                primary: false,
            },
        ];
        assert_eq!(
            select_monitor_containing(&monitors, 2600, 500),
            Some(monitors[1].area)
        );
        assert_eq!(
            select_monitor_containing(&monitors, 100, 100),
            Some(monitors[0].area)
        );
    }

    #[test]
    fn select_monitor_outside_all_returns_primary() {
        let monitors = [
            MonitorCandidate {
                area: WorkArea {
                    left: 0,
                    top: 0,
                    right: 2560,
                    bottom: 1080,
                },
                primary: false,
            },
            MonitorCandidate {
                area: WorkArea {
                    left: 2560,
                    top: 0,
                    right: 4480,
                    bottom: 1080,
                },
                primary: true,
            },
        ];
        assert_eq!(
            select_monitor_containing(&monitors, 9999, 9999),
            Some(monitors[1].area)
        );
    }

    #[test]
    fn select_monitor_without_match_or_primary_is_none() {
        let monitors = [MonitorCandidate {
            area: WorkArea {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1080,
            },
            primary: false,
        }];
        assert_eq!(select_monitor_containing(&monitors, 5000, 5000), None);
        assert_eq!(select_monitor_containing(&[], 0, 0), None);
    }

    #[test]
    fn macos_menu_bar_reservation_shrinks_display_holding_origin() {
        let mut monitors = [
            MonitorCandidate {
                area: WorkArea {
                    left: 0,
                    top: 0,
                    right: 2560,
                    bottom: 1080,
                },
                primary: true,
            },
            MonitorCandidate {
                area: WorkArea {
                    left: 2560,
                    top: 0,
                    right: 5120,
                    bottom: 1080,
                },
                primary: false,
            },
        ];
        reserve_macos_menu_bar(&mut monitors);
        assert_eq!(monitors[0].area.top, MACOS_MENU_BAR_HEIGHT);
        assert_eq!(monitors[1].area.top, 0);
    }

    #[test]
    fn macos_menu_bar_reservation_never_inverts_area() {
        let mut monitors = [MonitorCandidate {
            area: WorkArea {
                left: 0,
                top: 0,
                right: 40,
                bottom: 10,
            },
            primary: true,
        }];
        reserve_macos_menu_bar(&mut monitors);
        assert_eq!(monitors[0].area.top, 0);
        assert_eq!(monitors[0].area.bottom, 10);
    }

    #[test]
    fn round_cg_coordinate_rounds_halfway_cases() {
        assert_eq!(round_cg_coordinate(2559.6), 2560);
        assert_eq!(round_cg_coordinate(-1439.5), -1440);
        assert_eq!(round_cg_coordinate(1079.4), 1079);
    }
}
