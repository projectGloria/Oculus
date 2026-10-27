//! Main window lifecycle. The window is created on demand and *destroyed* (not hidden) when
//! sent to the tray: a hidden WebView2 still keeps its browser/GPU/renderer processes
//! (~60–90MB) alive; destroying it drops Oculus to the Rust process alone.

use tauri::{AppHandle, LogicalSize, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};

use crate::recorder::Recorder;

const FULL: (f64, f64) = (400.0, 640.0);
const COMPACT: (f64, f64) = (300.0, 52.0);

// Replaces Tauri's default WebView2 args, so its defaults are repeated here. The rest trims
// background work the UI never needs. --disable-gpu: a 400×640 static panel doesn't need a
// GPU-composited renderer; measured ~60 MB less private memory with the window open.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,Translate,AutofillServerCommunication \
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

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let compact = app.state::<Recorder>().is_active();
    let (w, h) = if compact { COMPACT } else { FULL };
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
        Err(e) => eprintln!("[ui] create window: {e}"),
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
    let (w, h) = if compact { COMPACT } else { FULL };
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
    // Debug escape hatch: lets our own --selftest --shot capture the UI for review.
    if cfg!(debug_assertions) && std::env::var_os("OCULUS_SHOW_IN_CAPTURE").is_some() {
        return;
    }
    if let Ok(h) = win.hwnd() {
        unsafe {
            let _ = SetWindowDisplayAffinity(HWND(h.0 as _), WDA_EXCLUDEFROMCAPTURE);
        }
    }
}
