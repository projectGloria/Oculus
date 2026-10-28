//! Tray updates are queued while recorder state is locked, then applied on the UI thread.
//! Calling Tauri's synchronous tray setters under that lock could deadlock the UI.
use std::sync::Mutex;

use tauri::{image::Image, AppHandle, Manager};

#[derive(Default)]
pub struct TrayStatus(Mutex<(u64, String)>);

pub fn update(app: &AppHandle, seq: u64, state: &'static str, elapsed_ms: u64) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id("tray") else { return };
        let status = handle.state::<TrayStatus>();
        let mut previous = status.0.lock().unwrap();
        // Main-thread commands can overtake queued worker updates.
        if seq <= previous.0 {
            return;
        }
        previous.0 = seq;
        if previous.1 != state {
            let icon = if state == "idle" { handle.default_window_icon().cloned() } else { Some(status_icon(state)) };
            match tray.set_icon(icon) {
                Ok(()) => previous.1 = state.into(),
                Err(e) => tracing::warn!(error = %e, "Could not update tray icon"),
            }
        }
        if let Err(e) = tray.set_tooltip(Some(tooltip(state, elapsed_ms))) {
            tracing::warn!(error = %e, "Could not update tray tooltip");
        }
    });
}

fn tooltip(state: &str, elapsed_ms: u64) -> String {
    let label = match state {
        "starting" => "Starting recording…",
        "recording" => "Recording",
        "paused" => "Paused",
        "finalizing" => "Saving recording…",
        _ => return "Oculus".into(),
    };
    let seconds = elapsed_ms / 1000;
    let time = if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    };
    format!("Oculus — {label} · {time}")
}

// A white rim keeps the status legible on light and dark taskbars. Paused uses bars
// as well as amber, so recording and paused remain distinguishable without color.
fn status_icon(state: &str) -> Image<'static> {
    let color = match state {
        "recording" => [239, 68, 68, 255],
        "paused" => [245, 179, 66, 255],
        _ => [138, 143, 156, 255],
    };
    let mut rgba = vec![0; 32 * 32 * 4];
    for y in 0i32..32 {
        for x in 0i32..32 {
            let radius = (x * 2 - 31).pow(2) + (y * 2 - 31).pow(2);
            let pixel = if radius > 29 * 29 {
                [0; 4]
            } else if radius > 25 * 25 {
                [255; 4]
            } else if state == "paused" && (10..22).contains(&y) && ((11..14).contains(&x) || (18..21).contains(&x)) {
                [30, 30, 30, 255]
            } else {
                color
            };
            let offset = (y * 32 + x) as usize * 4;
            rgba[offset..offset + 4].copy_from_slice(&pixel);
        }
    }
    Image::new_owned(rgba, 32, 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_tracks_state_and_long_recordings() {
        assert_eq!(tooltip("idle", 99_000), "Oculus");
        assert_eq!(tooltip("recording", 65_999), "Oculus — Recording · 01:05");
        assert_eq!(tooltip("paused", 3_661_000), "Oculus — Paused · 1:01:01");
        assert_eq!(tooltip("finalizing", 2_000), "Oculus — Saving recording… · 00:02");
    }
}
