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
pub async fn get_diagnostics(app: AppHandle) -> Result<crate::platform::Diagnostics> {
    blocking(move || Ok(crate::platform::diagnostics(&app))).await
}

#[tauri::command]
pub async fn list_audio_devices() -> Result<Vec<crate::platform::AudioDevice>> {
    blocking(crate::platform::audio_devices).await
}

#[tauri::command]
pub fn toggle_audio_mute(app: AppHandle, source: String) -> Result<()> {
    app.state::<Recorder>().toggle_audio_mute(&app, &source)
}

#[tauri::command]
pub fn get_recorder_state(rec: State<Recorder>) -> Snapshot {
    rec.get()
}

#[tauri::command]
pub fn set_source(app: AppHandle, source: Source, label: String) -> Result<()> {
    app.state::<Recorder>().set_source(&app, source, label)
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
    if app.state::<Recorder>().is_active() {
        return Err(msg("Stop recording before selecting a new capture area."));
    }
    blocking(move || {
        let Some(pick) = region::select()? else { return Ok(false) };
        let r = pick.rect;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let source = Source::Region { monitor: pick.monitor, x: r.left, y: r.top, w, h };
        app.state::<Recorder>().set_source(&app, source, format!("Region {w} × {h}"))?;
        Ok(true)
    })
    .await
}

#[tauri::command]
pub async fn record_region(app: AppHandle) -> Result<()> {
    if select_region(app.clone()).await? {
        start_recording(app).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn record_window_at_cursor(app: AppHandle) -> Result<()> {
    blocking(move || {
        if region::is_selecting() {
            return Err(msg("Confirm or cancel area selection first."));
        }
        let window =
            sources::window_at_cursor().ok_or_else(|| msg("Point at an application window outside Oculus."))?;
        let recorder = app.state::<Recorder>();
        recorder.set_source(&app, Source::Window { id: window.id }, window.title)?;
        recorder.start(&app)
    })
    .await
}

/// Screenshot of the current source (region source reuses its rect, no overlay).
#[tauri::command]
pub async fn take_screenshot(app: AppHandle) -> Result<String> {
    blocking(move || {
        let rec = app.state::<Recorder>();
        let r = rec
            .source(&app)
            .and_then(|source| {
                source
                    .or_else(|| sources::primary_monitor().map(|id| Source::Monitor { id }))
                    .ok_or_else(|| msg("No source"))
            })
            .and_then(|s| {
                let st = app.state::<SettingsStore>();
                let set = st.get();
                screenshot::capture(s.target(), s.crop(), set.cursor, &st.output_dir()?, set.copy_screenshots)
            });
        let r = r.map(|p| p.to_string_lossy().into_owned());
        rec.set_shot(&app, &r);
        if let Some(warning) = app.state::<SettingsStore>().warning() {
            rec.set_notice(&app, Some(warning));
        }
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
    if let Some(warning) = st.warning() {
        app.state::<Recorder>().set_notice(app, Some(warning));
    }
    r.map(Some)
}

#[tauri::command]
pub fn get_settings(store: State<SettingsStore>) -> Settings {
    store.get()
}

#[tauri::command]
pub async fn set_settings(app: AppHandle, settings: Settings) -> Result<Settings> {
    blocking(move || app.state::<crate::hotkeys::Registry>().save(&app, settings, false)).await
}

#[tauri::command]
pub async fn retry_hotkeys(app: AppHandle) -> Result<()> {
    blocking(move || {
        app.state::<crate::hotkeys::Registry>().save(&app, app.state::<SettingsStore>().get(), true).map(|_| ())
    })
    .await
}

#[tauri::command]
pub fn hide_to_tray(app: AppHandle) {
    ui::hide_to_tray(&app);
}

#[tauri::command]
pub async fn reveal(app: AppHandle, path: String) -> Result<()> {
    blocking(move || {
        let dir = app.state::<SettingsStore>().output_dir()?;
        let path = crate::shell::output_file(&dir, std::path::Path::new(&path))?;
        crate::shell::reveal_file(path)
    })
    .await
}

#[tauri::command]
pub fn open_output_dir(store: State<SettingsStore>) -> Result<()> {
    std::process::Command::new("explorer").arg(store.output_dir()?).spawn()?;
    Ok(())
}

#[tauri::command]
pub fn open_microphone_settings() -> Result<()> {
    std::process::Command::new("explorer.exe").arg("ms-settings:privacy-microphone").spawn()?;
    Ok(())
}

fn saved_path(app: &AppHandle, path: &str) -> Result<std::path::PathBuf> {
    app.state::<Recorder>().saved_output(path)?;
    let dir = app.state::<SettingsStore>().output_dir()?;
    crate::shell::output_file(&dir, std::path::Path::new(path))
}

#[tauri::command]
pub async fn saved_file_details(app: AppHandle, path: String) -> Result<crate::saved_output::Details> {
    blocking(move || crate::saved_output::details(saved_path(&app, &path)?)).await
}

#[tauri::command]
pub async fn open_saved_file(app: AppHandle, path: String) -> Result<()> {
    blocking(move || crate::saved_output::open(saved_path(&app, &path)?)).await
}

#[tauri::command]
pub async fn copy_saved_file(app: AppHandle, path: String) -> Result<()> {
    blocking(move || crate::saved_output::copy(&saved_path(&app, &path)?)).await
}

#[tauri::command]
pub async fn rename_saved_file(app: AppHandle, path: String, name: String) -> Result<()> {
    blocking(move || {
        let resolved = saved_path(&app, &path)?;
        let renamed = crate::saved_output::rename(&resolved, &name)?;
        // Keep normal paths in snapshots; canonical paths carry the extended Win32 prefix.
        let renamed = std::path::Path::new(&path).with_file_name(renamed.file_name().unwrap());
        app.state::<Recorder>().update_saved_output(&app, &path, Some(renamed.to_string_lossy().into_owned()));
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn delete_saved_file(app: AppHandle, path: String) -> Result<()> {
    blocking(move || {
        std::fs::remove_file(saved_path(&app, &path)?)?;
        app.state::<Recorder>().update_saved_output(&app, &path, None);
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn dismiss_screenshot_feedback(app: AppHandle) {
    ui::dismiss_screenshot_feedback(&app);
}

#[tauri::command]
pub async fn drag_saved_file(app: AppHandle, path: String) -> Result<()> {
    blocking(move || {
        let path = saved_path(&app, &path)?;
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = app.clone();
        app.run_on_main_thread(move || {
            let generation = ui::hold_screenshot_feedback(&handle);
            let result = crate::shell::drag_file(&path);
            ui::resume_screenshot_feedback(&handle, generation);
            let _ = tx.send(result);
        })
        .map_err(|e| msg(e.to_string()))?;
        rx.recv().map_err(|_| msg("File drag did not complete"))?
    })
    .await
}
