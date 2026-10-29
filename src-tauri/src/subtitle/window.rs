use crate::subtitle::settings::DesktopPos;

/// A display's rectangle in physical pixels; x and y can be negative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Gap between the default window and the bottom edge of its monitor.
const DEFAULT_BOTTOM_MARGIN: i64 = 120;
/// A saved window with less than this share (percent) on any monitor counts as lost.
const MIN_VISIBLE_PERCENT: i64 = 30;
const VIRTUAL_MONITOR: Monitor = Monitor { x: 0, y: 0, width: 1920, height: 1080 };

fn overlap_area(p: &DesktopPos, m: &Monitor) -> i64 {
    let left = (p.x as i64).max(m.x as i64);
    let right = (p.x as i64 + p.width as i64).min(m.x as i64 + m.width as i64);
    let top = (p.y as i64).max(m.y as i64);
    let bottom = (p.y as i64 + p.height as i64).min(m.y as i64 + m.height as i64);
    (right - left).max(0) * (bottom - top).max(0)
}

fn default_position_sized(m: &Monitor, size: (u32, u32)) -> DesktopPos {
    let width = size.0.min(m.width);
    let height = size.1.min(m.height);
    DesktopPos {
        x: (m.x as i64 + (m.width as i64 - width as i64) / 2) as i32,
        y: (m.y as i64 + m.height as i64 - DEFAULT_BOTTOM_MARGIN - height as i64).max(m.y as i64) as i32,
        width,
        height,
    }
}

const MIN_FONT: u32 = 14;
const MAX_FONT: u32 = 96;
/// Room for about 32 characters per line of the largest text, in ems.
const WIDTH_EMS: f64 = 32.0;
/// One line of text plus a little padding, in ems. One line only, so the window fits inside a
/// taskbar and the subtitle can sit on it without taking space from the desktop.
const HEIGHT_EMS: f64 = 1.5;

/// The window size (physical pixels) that fits subtitle text of `font_px` CSS pixels on a
/// display whose scale factor is `scale`. The window has to grow with the text, otherwise a
/// larger font setting would just be clipped.
pub fn size_for_font(font_px: u32, scale: f64) -> (u32, u32) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let px = font_px.clamp(MIN_FONT, MAX_FONT) as f64 * scale;
    ((px * WIDTH_EMS).ceil() as u32, (px * HEIGHT_EMS).ceil() as u32)
}

/// The monitors that can hold a window; a virtual 1920x1080 one when there are none.
fn usable(monitors: &[Monitor]) -> Vec<Monitor> {
    let found: Vec<Monitor> = monitors
        .iter()
        .copied()
        .filter(|m| m.width > 0 && m.height > 0)
        .collect();
    if found.is_empty() {
        vec![VIRTUAL_MONITOR]
    } else {
        found
    }
}

fn best_monitor<'a>(p: &DesktopPos, monitors: &'a [Monitor]) -> (&'a Monitor, i64) {
    monitors
        .iter()
        .map(|m| (m, overlap_area(p, m)))
        .max_by_key(|(_, area)| *area)
        .expect("monitors is never empty")
}

fn lost_in(p: &DesktopPos, overlap: i64) -> bool {
    let area = p.width as i64 * p.height as i64;
    area == 0 || overlap * 100 < area * MIN_VISIBLE_PERCENT
}

/// Index in `monitors` of the display `p` overlaps most, or `None` when it overlaps none.
pub fn monitor_index(p: &DesktopPos, monitors: &[Monitor]) -> Option<usize> {
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| (i, overlap_area(p, m)))
        .filter(|(_, area)| *area > 0)
        .max_by_key(|(_, area)| *area)
        .map(|(i, _)| i)
}

/// A window of `size` placed from a `saved` position: the saved corner is kept and the window is
/// moved only as far as needed to lie on its monitor. A missing or lost position (judged by the
/// size the user last saw) falls back to bottom-centre of the first monitor.
pub fn fit_position_sized(
    saved: Option<DesktopPos>,
    size: (u32, u32),
    monitors: &[Monitor],
) -> DesktopPos {
    let monitors = usable(monitors);
    let Some(old) = saved else {
        return default_position_sized(&monitors[0], size);
    };
    let (best, overlap) = best_monitor(&old, &monitors);
    if lost_in(&old, overlap) {
        return default_position_sized(&monitors[0], size);
    }
    let width = size.0.min(best.width);
    let height = size.1.min(best.height);
    let max_x = best.x as i64 + best.width as i64 - width as i64;
    let max_y = best.y as i64 + best.height as i64 - height as i64;
    DesktopPos {
        x: (old.x as i64).clamp(best.x as i64, max_x) as i32,
        y: (old.y as i64).clamp(best.y as i64, max_y) as i32,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle::settings::DesktopPos;

    fn mon(x: i32, y: i32, width: u32, height: u32) -> Monitor {
        Monitor { x, y, width, height }
    }

    fn pos(x: i32, y: i32, width: u32, height: u32) -> DesktopPos {
        DesktopPos { x, y, width, height }
    }

    #[test]
    fn size_follows_the_font_size() {
        // width = 32 em, height = 1.5 em (one line plus a little padding), then times the scale
        assert_eq!(size_for_font(28, 1.0), (896, 42));
        assert_eq!(size_for_font(96, 1.0), (3072, 144));
        assert_eq!(size_for_font(28, 1.5), (1344, 63));
    }

    #[test]
    fn size_for_font_clamps_to_the_supported_range() {
        assert_eq!(size_for_font(5, 1.0), size_for_font(14, 1.0));
        assert_eq!(size_for_font(500, 1.0), size_for_font(96, 1.0));
    }

    #[test]
    fn a_bad_scale_factor_counts_as_one() {
        assert_eq!(size_for_font(28, 0.0), size_for_font(28, 1.0));
        assert_eq!(size_for_font(28, -2.0), size_for_font(28, 1.0));
        assert_eq!(size_for_font(28, f64::NAN), size_for_font(28, 1.0));
    }

    #[test]
    fn a_bar_sitting_on_the_taskbar_stays_exactly_where_it_was_put() {
        // The user wants the text right on the taskbar so it takes no desktop space: a one-line
        // bar whose top edge is inside the taskbar strip (work area ends at 1032) must not move.
        let monitors = [mon(0, 0, 1920, 1080)];
        let on_taskbar = pos(600, 1034, 896, 42);
        assert_eq!(fit_position_sized(Some(on_taskbar), (896, 42), &monitors), on_taskbar);
        // and re-applying it (what locking does) is a no-op
        let once = fit_position_sized(Some(on_taskbar), (896, 42), &monitors);
        assert_eq!(fit_position_sized(Some(once), (896, 42), &monitors), once);
    }

    #[test]
    fn a_bar_on_the_far_monitor_taskbar_stays_there() {
        let monitors = [mon(0, 0, 1920, 1080), mon(1920, 0, 1920, 1080)];
        let on_taskbar = pos(2936, 1036, 896, 42);
        assert_eq!(fit_position_sized(Some(on_taskbar), (896, 42), &monitors), on_taskbar);
    }

    #[test]
    fn monitor_index_picks_the_display_with_most_overlap() {
        let monitors = [mon(0, 0, 1920, 1080), mon(1920, 0, 1920, 1080)];
        assert_eq!(monitor_index(&pos(100, 100, 400, 40), &monitors), Some(0));
        assert_eq!(monitor_index(&pos(2000, 100, 400, 40), &monitors), Some(1));
        // x 1800..2200: 120 px on the left display, 280 px on the right one
        assert_eq!(monitor_index(&pos(1800, 100, 400, 40), &monitors), Some(1));
        // x 1700..2100: 220 px left, 180 px right
        assert_eq!(monitor_index(&pos(1700, 100, 400, 40), &monitors), Some(0));
        assert_eq!(monitor_index(&pos(9000, 9000, 400, 40), &monitors), None);
    }

    #[test]
    fn a_bar_with_only_a_sliver_on_screen_counts_as_lost_and_is_reset() {
        let monitors = [mon(0, 0, 1920, 1080)];
        // only 10 % of the bar overlaps the display
        assert_eq!(
            fit_position_sized(Some(pos(1830, 300, 896, 42)), (896, 42), &monitors),
            pos(512, 918, 896, 42)
        );
    }

    #[test]
    fn sized_placement_centres_a_default_window_of_the_given_size() {
        let got = fit_position_sized(None, (1200, 100), &[mon(0, 0, 1920, 1080)]);
        assert_eq!(got, pos(360, 860, 1200, 100));
    }

    #[test]
    fn sized_placement_replaces_the_saved_size_but_keeps_the_saved_corner() {
        let got = fit_position_sized(Some(pos(300, 500, 700, 80)), (1200, 100), &[mon(0, 0, 1920, 1080)]);
        assert_eq!(got, pos(300, 500, 1200, 100));
    }

    #[test]
    fn a_window_that_grows_is_pulled_back_inside_the_monitor_not_reset() {
        // The user placed a 900x90 bar near the bottom right, then raised the font size.
        // It must stay near where they put it, only moved far enough to fit.
        let got = fit_position_sized(Some(pos(1500, 900, 900, 90)), (1800, 276), &[mon(0, 0, 1920, 1080)]);
        assert_eq!(got, pos(120, 804, 1800, 276));
        assert!(got.x + got.width as i32 <= 1920 && got.y + got.height as i32 <= 1080);
    }

    #[test]
    fn a_lost_position_is_still_reset_when_the_size_is_forced() {
        let got = fit_position_sized(Some(pos(5000, 100, 900, 90)), (1200, 100), &[mon(0, 0, 1920, 1080)]);
        assert_eq!(got, pos(360, 860, 1200, 100));
    }

    #[test]
    fn a_forced_size_wider_than_the_monitor_is_shrunk_to_it() {
        let got = fit_position_sized(None, (3072, 144), &[mon(0, 0, 1920, 1080)]);
        assert_eq!((got.width, got.height), (1920, 144));
        assert_eq!(got.x, 0);
    }

    #[test]
    fn a_zero_sized_saved_window_is_treated_as_lost_and_gets_the_new_size() {
        let got = fit_position_sized(Some(pos(100, 100, 0, 0)), (896, 42), &[mon(0, 0, 1920, 1080)]);
        assert_eq!((got.width, got.height), (896, 42));
        assert_eq!(got, pos(512, 918, 896, 42));
    }

    #[test]
    fn no_monitors_falls_back_to_a_virtual_1920x1080() {
        assert_eq!(fit_position_sized(None, (896, 42), &[]), pos(512, 918, 896, 42));
        assert_eq!(fit_position_sized(Some(pos(40, 40, 900, 90)), (896, 42), &[]), pos(40, 40, 896, 42));
    }

    #[test]
    fn a_default_position_follows_a_monitor_that_does_not_start_at_the_origin() {
        let monitors = [mon(-1920, 0, 1920, 1080), mon(0, 0, 2560, 1440)];
        assert_eq!(fit_position_sized(None, (896, 42), &monitors), pos(-1920 + 512, 918, 896, 42));
    }

    #[test]
    fn a_position_on_a_left_monitor_with_negative_coordinates_is_kept() {
        let monitors = [mon(0, 0, 1920, 1080), mon(-1920, 0, 1920, 1080)];
        let saved = pos(-1500, 400, 896, 42);
        assert_eq!(fit_position_sized(Some(saved), (896, 42), &monitors), saved);
    }

    #[test]
    fn a_partly_offscreen_bar_is_pulled_back_in() {
        let got = fit_position_sized(Some(pos(1500, 300, 896, 42)), (896, 42), &[mon(0, 0, 1920, 1080)]);
        assert_eq!(got, pos(1024, 300, 896, 42));
    }
}
