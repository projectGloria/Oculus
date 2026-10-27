use tauri::{AppHandle, Manager, State};

use crate::error::{msg, Result};
use crate::overlay::region;
use crate::recorder::{Recorder, Snapshot, Source};
use crate::settings::{Settings, SettingsStore};
use crate::{screenshot, sources, ui};

/// Sync Tauri commands run on the main (UI) thread; anything that can block for more than a
/// few ms goes through here instead.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| msg(e.to_string()))?
}

#[tauri::command]
pub async fn list_sources() -> Result<sources::Sources> {
    blocking(|| Ok(sources::list())).await
}

#[tauri::command]
pub fn get_recorder_state(rec: State<Recorder>) -> Snapshot {
    rec.get()
}

#[tauri::command]
pub fn set_source(app: AppHandle, source: Source, label: String) {
    app.state::<Recorder>().set_source(&app, source, label);
}

#[tauri::command]
pub async fn start_recording(app: AppHandle) -> Result<()> {
    blocking(move || app.state::<Recorder>().start(&app)).await
}

#[tauri::command]
pub fn stop_recording(app: AppHandle) {
    app.state::<Recorder>().stop(&app);
}

#[tauri::command]
pub fn toggle_pause(app: AppHandle) {
    app.state::<Recorder>().toggle_pause(&app);
}

/// Pick a region for recording. Sets it as the current source.
#[tauri::command]
pub async fn select_region(app: AppHandle) -> Result<bool> {
    blocking(move || {
        let Some(pick) = region::select()? else { return Ok(false) };
        let r = pick.rect;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let source = Source::Region { monitor: pick.monitor, x: r.left, y: r.top, w, h };
        app.state::<Recorder>().set_source(&app, source, format!("Region {w} × {h}"));
        Ok(true)
    })
    .await
}

/// Screenshot of the current source (region source reuses its rect, no overlay).
#[tauri::command]
pub async fn take_screenshot(app: AppHandle) -> Result<String> {
    blocking(move || {
        let rec = app.state::<Recorder>();
        let source = rec.source().or_else(|| sources::primary_monitor().map(|id| Source::Monitor { id }));
        let r = source.ok_or_else(|| msg("No source")).and_then(|s| {
            let st = app.state::<SettingsStore>();
            let set = st.get();
            screenshot::capture(s.target(), s.crop(), set.cursor, &st.output_dir()?, set.copy_screenshots)
        });
        let r = r.map(|p| p.to_string_lossy().into_owned());
        rec.set_shot(&app, &r);
        r
    })
    .await
}

/// Snipping-style: overlay, drag, PNG of exactly the frozen pixels.
#[tauri::command]
pub async fn screenshot_region(app: AppHandle) -> Result<Option<String>> {
    blocking(move || region_shot(&app)).await
}

pub fn region_shot(app: &AppHandle) -> Result<Option<String>> {
    let Some(pick) = region::select()? else { return Ok(None) };
    let st = app.state::<SettingsStore>();
    let (gpu, tex) = &pick.frozen.0;
    let r = screenshot::save(gpu, tex, Some(pick.rect), &st.output_dir()?, st.get().copy_screenshots)
        .map(|p| p.to_string_lossy().into_owned());
    app.state::<Recorder>().set_shot(app, &r);
    r.map(Some)
}

#[tauri::command]
pub fn get_settings(store: State<SettingsStore>) -> Settings {
    store.get()
}

#[tauri::command]
pub fn set_settings(store: State<SettingsStore>, settings: Settings) -> Result<Settings> {
    store.set(settings)
}

#[tauri::command]
pub fn hide_to_tray(app: AppHandle) {
    ui::hide_to_tray(&app);
}

#[tauri::command]
pub fn reveal(path: String) -> Result<()> {
    // Only ever called with paths we produced; still refuse anything that isn't a file.
    if !std::path::Path::new(&path).is_file() {
        return Err(msg("File no longer exists"));
    }
    std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn()?;
    Ok(())
}

#[tauri::command]
pub fn open_output_dir(store: State<SettingsStore>) -> Result<()> {
    std::process::Command::new("explorer").arg(store.output_dir()?).spawn()?;
    Ok(())
}
