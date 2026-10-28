//! Main window lifecycle. The window is created on demand and *destroyed* (not hidden) when
//! sent to the tray: a hidden WebView2 still keeps its browser/GPU/renderer processes
//! (~60–90MB) alive; destroying it drops Oculus to the Rust process alone.

use tauri::{AppHandle, LogicalSize, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};

use crate::recorder::Recorder;

const FULL: (f64, f64) = (400.0, 640.0);
const COMPACT: (f64, f64) = (370.0, 52.0);

#[derive(Default)]
pub struct ScreenshotFeedback(std::sync::atomic::AtomicU64);

pub fn hold_screenshot_feedback(app: &AppHandle) -> Option<u64> {
    app.get_webview_window("screenshot-feedback")?;
    Some(app.state::<ScreenshotFeedback>().0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1)
}

pub fn resume_screenshot_feedback(app: &AppHandle, generation: Option<u64>) {
    if generation.is_some_and(|g| app.state::<ScreenshotFeedback>().0.load(std::sync::atomic::Ordering::Relaxed) == g)
        && app.get_webview_window("screenshot-feedback").is_some()
    {
        show_screenshot_feedback(app);
    }
}

pub fn dismiss_screenshot_feedback(app: &AppHandle) {
    app.state::<ScreenshotFeedback>().0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("screenshot-feedback") {
        let _ = win.destroy();
    }
}

pub fn show_screenshot_feedback(app: &AppHandle) {
    use std::sync::atomic::Ordering;
    let generation = app.state::<ScreenshotFeedback>().0.fetch_add(1, Ordering::Relaxed) + 1;
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if handle.state::<ScreenshotFeedback>().0.load(Ordering::Relaxed) != generation {
            return;
        }
        if handle.get_webview_window("screenshot-feedback").is_none() {
            let built = WebviewWindowBuilder::new(
                &handle,
                "screenshot-feedback",
                WebviewUrl::App("index.html#screenshot-feedback".into()),
            )
            .title("Screenshot saved")
            .inner_size(340.0, 210.0)
            .decorations(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false)
            .background_color(tauri::window::Color(14, 15, 18, 255))
            .additional_browser_args(&browser_args())
            .build();
            match built {
                Ok(win) => {
                    exclude_from_capture(&win);
                    if let (Ok(Some(m)), Ok(size)) = (win.primary_monitor(), win.outer_size()) {
                        let margin = (48.0 * m.scale_factor()) as i32;
                        let _ = win.set_position(tauri::PhysicalPosition::new(
                            m.position().x + m.size().width as i32 - size.width as i32 - margin,
                            m.position().y + m.size().height as i32 - size.height as i32 - margin,
                        ));
                    }
                    let _ = win.show();
                }
                Err(e) => tracing::warn!(error = %e, "Could not show screenshot feedback"),
            }
        }
        // Start the lifetime after creation, and never let an old timer close a newer save.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let app = handle.clone();
            let _ = handle.run_on_main_thread(move || {
                if app.state::<ScreenshotFeedback>().0.load(Ordering::Relaxed) == generation {
                    if let Some(win) = app.get_webview_window("screenshot-feedback") {
                        let _ = win.destroy();
                    }
                }
            });
        });
    });
}

// Replaces Tauri's default WebView2 args, so its defaults are repeated here. The rest trims
// background work the UI never needs. --disable-gpu: a 400×640 static panel doesn't need a
// GPU-composited renderer; measured ~60 MB less private memory with the window open.
const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,Translate,AutofillServerCommunication \
--disable-background-networking --disable-component-update --disable-sync --no-first-run \
--disable-gpu --disable-gpu-compositing";

fn browser_args() -> String {
    // Debug-only override for measuring WebView2 flag variants.
    if cfg!(debug_assertions) {
        if let Ok(a) = std::env::var("OCULUS_WEBVIEW_ARGS") {
            return a;
        }
    }
    BROWSER_ARGS.to_string()
}

fn panel_size(app: &AppHandle) -> (f64, f64) {
    let monitor = app
        .get_webview_window("main")
        .and_then(|win| win.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    monitor
        .map(|monitor| {
            let area = monitor.work_area();
            (
                FULL.0.min(area.size.width as f64 / monitor.scale_factor()),
                FULL.1.min((area.size.height as f64 / monitor.scale_factor() - 24.0).max(240.0)),
            )
        })
        .unwrap_or(FULL)
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let compact = app.state::<Recorder>().is_active();
    let (w, h) = if compact { COMPACT } else { panel_size(app) };
    let built = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Oculus")
        .inner_size(w, h)
        .resizable(false)
        .maximizable(false)
        .decorations(false)
        .always_on_top(compact)
        .background_color(tauri::window::Color(14, 15, 18, 255)) // no white flash before React paints
        .additional_browser_args(&browser_args())
        .visible(false)
        .build();
    match built {
        Ok(win) => {
            exclude_from_capture(&win);
            if compact {
                dock_top(&win);
            } else {
                let _ = win.center();
            }
            let _ = win.show();
            let _ = win.set_focus();
        }
        Err(e) => tracing::error!(error = %e, "Could not create Oculus window"),
    }
}

pub fn hide_to_tray(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.destroy();
    }
}

/// Recording collapses the panel into a small always-on-top bar (still excluded from capture).
pub fn set_compact(app: &AppHandle, compact: bool) {
    let Some(win) = app.get_webview_window("main") else { return };
    let (w, h) = if compact { COMPACT } else { panel_size(app) };
    let _ = win.set_always_on_top(compact);
    let _ = win.set_size(LogicalSize::new(w, h));
    if compact {
        dock_top(&win);
    } else {
        let _ = win.center();
    }
}

fn dock_top(win: &WebviewWindow) {
    if let (Ok(Some(m)), Ok(size)) = (win.current_monitor(), win.outer_size()) {
        let x = m.position().x + (m.size().width as i32 - size.width as i32) / 2;
        let _ = win.set_position(tauri::PhysicalPosition::new(x, m.position().y + 12));
    }
}

/// WDA_EXCLUDEFROMCAPTURE: the window renders on screen but is blank-skipped by WGC, so the
/// panel / floating bar never shows up in recordings or screenshots.
fn exclude_from_capture(win: &WebviewWindow) {
    if let Ok(h) = win.hwnd() {
        exclude_native_window(HWND(h.0 as _));
    }
}

pub(crate) fn exclude_native_window(hwnd: HWND) {
    // Debug escape hatch: lets our own --selftest --shot capture the UI for review.
    let debug_override = false;
    #[cfg(debug_assertions)]
    let debug_override = debug_override || std::env::var_os("OCULUS_SHOW_IN_CAPTURE").is_some();
    if !capture_exclusion_enabled(debug_override) {
        return;
    }
    unsafe {
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    }
}

fn capture_exclusion_enabled(debug_override: bool) -> bool {
    !cfg!(debug_assertions) || !debug_override
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_exclusion_override_is_available_only_in_debug_builds() {
        assert!(capture_exclusion_enabled(false));
        assert_eq!(capture_exclusion_enabled(true), !cfg!(debug_assertions));
    }
}
