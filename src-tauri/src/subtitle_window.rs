//! The floating desktop subtitle window: creating it, keeping it on top, click-through, and
//! remembering where the user put it. Placement rules live in `subtitle::window` (tested);
//! this file only talks to Tauri.

use crate::subtitle::settings::{DesktopPos, SubtitleSettings};
use crate::subtitle::window::{fit_position_sized, monitor_index, size_for_font, Monitor};
use crate::subtitle_commands::SubtitleState;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

pub const LABEL: &str = "desktop-subtitle";
/// The taskbar is a top-most window too and can rise above ours when it is clicked.
const RAISE_EVERY: Duration = Duration::from_secs(3);
/// Wait this long after the last move before writing the position down.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

static RAISER_RUNNING: AtomicBool = AtomicBool::new(false);
static MOVE_STAMP: AtomicU64 = AtomicU64::new(0);

/// Every display as a rectangle, with its scale factor (1.0 = 100 %).
fn monitors(app: &AppHandle) -> Vec<(Monitor, f64)> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            (
                Monitor {
                    x: m.position().x,
                    y: m.position().y,
                    width: m.size().width,
                    height: m.size().height,
                },
                m.scale_factor(),
            )
        })
        .collect()
}

/// The size the window should have for this font, at the scale of the display the saved corner
/// is on (the first display when there is no saved position).
fn wanted_size(settings: &SubtitleSettings, displays: &[(Monitor, f64)]) -> (u32, u32) {
    let rects: Vec<Monitor> = displays.iter().map(|(m, _)| *m).collect();
    let scale = settings
        .desktop_pos
        .and_then(|p| monitor_index(&p, &rects))
        .or(if displays.is_empty() { None } else { Some(0) })
        .map(|i| displays[i].1)
        .unwrap_or(1.0);
    size_for_font(settings.font_size, scale)
}

fn want_visible(settings: &SubtitleSettings) -> bool {
    settings.enabled && settings.desktop_enabled
}

/// Brings the window in line with the settings: created on first use, shown or hidden, placed
/// somewhere visible, click-through when locked.
pub fn apply(app: &AppHandle, settings: &SubtitleSettings) {
    let existing = app.get_webview_window(LABEL);
    if !want_visible(settings) {
        if let Some(window) = existing {
            let _ = window.hide();
        }
        return;
    }
    let displays = monitors(app);
    let rects: Vec<Monitor> = displays.iter().map(|(m, _)| *m).collect();
    let size = wanted_size(settings, &displays);
    let placed = fit_position_sized(settings.desktop_pos, size, &rects);
    let window = match existing {
        Some(window) => window,
        None => match create(app, &placed) {
            Some(window) => window,
            None => return,
        },
    };
    // Only touch position/size when they actually differ. Re-applying an identical value on every
    // settings change (locking, colours, ...) is what made the bar jump after the user had put it
    // exactly where they wanted it.
    let (now_pos, now_size) = (window.outer_position().ok(), window.outer_size().ok());
    if now_pos.map(|p| (p.x, p.y)) != Some((placed.x, placed.y)) {
        let _ = window.set_position(PhysicalPosition::new(placed.x, placed.y));
    }
    if now_size.map(|s| (s.width, s.height)) != Some((placed.width, placed.height)) {
        let _ = window.set_size(PhysicalSize::new(placed.width, placed.height));
    }
    let _ = window.set_ignore_cursor_events(settings.desktop_locked);
    let _ = window.set_always_on_top(true);
    let _ = window.show();
    start_raiser(app);
}

fn create(app: &AppHandle, placed: &DesktopPos) -> Option<WebviewWindow> {
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("desktop.html".into()))
        .title("lite-play 桌面字幕")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .focused(false)
        .visible(false)
        .inner_size(placed.width as f64, placed.height as f64)
        .position(placed.x as f64, placed.y as f64)
        .build()
        .ok()?;

    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(_) | WindowEvent::Resized(_) = event {
            remember_later(&handle);
        }
    });
    Some(window)
}

/// Writes the window's position to the settings once it has stopped moving for a moment.
fn remember_later(app: &AppHandle) {
    let stamp = MOVE_STAMP.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SAVE_DEBOUNCE);
        if MOVE_STAMP.load(Ordering::SeqCst) != stamp {
            return; // moved again meanwhile; a later call will save
        }
        let Some(window) = app.get_webview_window(LABEL) else {
            return;
        };
        if !window.is_visible().unwrap_or(false) {
            return;
        }
        let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else {
            return;
        };
        if let Some(state) = app.try_state::<SubtitleState>() {
            state.remember_desktop_position(DesktopPos {
                x: pos.x,
                y: pos.y,
                width: size.width,
                height: size.height,
            });
        }
    });
}

/// While the window is shown, lift it back above the taskbar every few seconds.
///
/// `set_always_on_top(true)` on a window that already has the flag does nothing (tao only acts
/// on a change), and the taskbar is itself a top-most window that rises when clicked. Dropping
/// the flag and setting it again does re-stack the window, but for a moment it is no longer
/// top-most and can flicker. A single `SetWindowPos(HWND_TOPMOST)` re-stacks it without ever
/// losing the flag.
fn start_raiser(app: &AppHandle) {
    if RAISER_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(RAISE_EVERY);
            let Some(window) = app.get_webview_window(LABEL) else {
                break;
            };
            if !window.is_visible().unwrap_or(false) {
                break;
            }
            raise(&window);
        }
        RAISER_RUNNING.store(false, Ordering::Release);
    });
}

#[cfg(windows)]
fn raise(window: &WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    if let Ok(hwnd) = window.hwnd() {
        // SAFETY: `hwnd` is the live window handle Tauri just returned; the call only restacks it.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

#[cfg(not(windows))]
fn raise(window: &WebviewWindow) {
    let _ = window.set_always_on_top(true);
}
