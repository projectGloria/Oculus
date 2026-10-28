//! App-level recorder state. The backend is the single source of truth: every change goes
//! out as a full `recorder://state` snapshot with a monotonic `seq`, and a freshly created
//! webview hydrates with `get_recorder_state` (same shape) after subscribing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::RECT;

use crate::audio::mixer::Gains;
use crate::capture::wgc::Target;
use crate::error::{msg, Result};
use crate::saved_output::SavedOutput;
use crate::session::{self, EndReason, Outcome};
use crate::settings::SettingsStore;
use crate::util::{available_bytes, local_stamp, reserve_output, DISK_STOP_BYTES, DISK_WARN_BYTES};
use crate::{sources, ui};

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    Monitor { id: isize },
    Window { id: isize },
    Region { monitor: isize, x: i32, y: i32, w: i32, h: i32 },
}

impl Source {
    pub fn target(&self) -> Target {
        match *self {
            Source::Monitor { id } => Target::Monitor(id),
            Source::Window { id } => Target::Window(id),
            Source::Region { monitor, .. } => Target::Monitor(monitor),
        }
    }

    pub fn crop(&self) -> Option<RECT> {
        match *self {
            Source::Region { x, y, w, h, .. } => Some(RECT { left: x, top: y, right: x + w, bottom: y + h }),
            _ => None,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub seq: u64,
    /// idle | starting | recording | paused | finalizing
    pub state: &'static str,
    pub elapsed_ms: u64,
    pub fps: u32,
    pub dropped: u64,
    pub source: Option<Source>,
    pub source_label: Option<String>,
    /// Most recent recording *or* screenshot.
    pub last_saved: Option<String>,
    pub saved_output: Option<SavedOutput>,
    pub last_screenshot: Option<SavedOutput>,
    pub error: Option<String>,
    pub audio: crate::audio::wasapi::Health,
    pub system_muted: bool,
    pub mic_muted: bool,
    pub hotkey_errors: Vec<String>,
    pub hotkeys: crate::hotkeys::Hotkeys,
    pub notice: Option<String>,
}

#[derive(Default)]
struct Inner {
    session: Option<session::Handle>,
    starting: bool,
    cancel_start: bool,
    cancelled_start: bool,
    finalizing: bool,
    quitting: bool,
    exit_code: i32,
    system_stopping: bool,
    source: Option<Source>,
    identity: Option<sources::Identity>,
    label: Option<String>,
    last_saved: Option<String>,
    saved_output: Option<SavedOutput>,
    last_screenshot: Option<SavedOutput>,
    error: Option<String>,
    notice: Option<String>,
    hotkey_errors: Vec<String>,
    hotkeys: crate::hotkeys::Hotkeys,
    last_encoder: Option<String>,
    fps: u32,
    frames_at_tick: u64,
    /// Bumped per session so a stale tick thread exits instead of ticking the next one.
    generation: u64,
}

#[derive(Default)]
pub struct Recorder {
    inner: Mutex<Inner>,
    finished_signal: Condvar,
    seq: AtomicU64,
    last_toggle: Mutex<Option<Instant>>,
}

impl Recorder {
    pub fn encoder_name(&self) -> Option<String> {
        let i = self.inner.lock().unwrap();
        i.session.as_ref().and_then(|s| s.stats.encoder_name.lock().unwrap().clone()).or_else(|| i.last_encoder.clone())
    }
    fn snapshot(&self, i: &Inner) -> Snapshot {
        let (state, elapsed, dropped) = match &i.session {
            Some(h) if i.finalizing => ("finalizing", h.clock.now(), h.stats.dropped.load(Ordering::Relaxed)),
            Some(h) => (
                if h.clock.paused() { "paused" } else { "recording" },
                h.clock.now(),
                h.stats.dropped.load(Ordering::Relaxed),
            ),
            None if i.starting => (if i.cancel_start { "finalizing" } else { "starting" }, None, 0),
            None => ("idle", None, 0),
        };
        Snapshot {
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            state,
            elapsed_ms: elapsed.map_or(0, |e| (e / 10_000).max(0) as u64),
            fps: if i.session.is_some() { i.fps } else { 0 },
            dropped,
            source: i.source.clone(),
            source_label: i.label.clone(),
            last_saved: i.last_saved.clone(),
            saved_output: i.saved_output.clone(),
            last_screenshot: i.last_screenshot.clone(),
            error: i.error.clone(),
            audio: i
                .session
                .as_ref()
                .map(|h| h.audio.lock().unwrap().clone())
                .unwrap_or_else(|| crate::audio::wasapi::Health::new(false, false)),
            hotkey_errors: i.hotkey_errors.clone(),
            hotkeys: i.hotkeys.clone(),
            system_muted: i.session.as_ref().is_some_and(|s| s.audio_controls.system_muted.load(Ordering::Relaxed)),
            mic_muted: i.session.as_ref().is_some_and(|s| s.audio_controls.mic_muted.load(Ordering::Relaxed)),
            notice: i.notice.clone(),
        }
    }

    pub fn get(&self) -> Snapshot {
        let i = self.inner.lock().unwrap();
        self.snapshot(&i)
    }

    /// Snapshot + emit under the same lock, so `seq` order == emission order.
    fn emit(&self, app: &AppHandle, i: &Inner) {
        let snapshot = self.snapshot(i);
        crate::tray_status::update(app, snapshot.seq, snapshot.state, snapshot.elapsed_ms);
        let _ = app.emit("recorder://state", snapshot);
    }

    pub fn is_active(&self) -> bool {
        let i = self.inner.lock().unwrap();
        i.starting || i.session.is_some()
    }

    pub fn set_source(&self, app: &AppHandle, source: Source, label: String) -> Result<()> {
        let identity = sources::Identity::capture(&source)?;
        let mut i = self.inner.lock().unwrap();
        if i.starting || i.session.is_some() {
            return Err(msg("Stop recording before changing the source"));
        }
        i.source = Some(source);
        i.identity = Some(identity);
        i.label = Some(label);
        self.emit(app, &i);
        Ok(())
    }

    pub fn source(&self, app: &AppHandle) -> Result<Option<Source>> {
        let (source, identity) = {
            let i = self.inner.lock().unwrap();
            (i.source.clone(), i.identity.clone())
        };
        match (source, identity) {
            (Some(source), Some(identity)) => identity.resolve(&source).map(|(source, warning)| {
                if warning.is_some() {
                    self.set_notice(app, warning);
                }
                Some(source)
            }),
            (source, _) => Ok(source),
        }
    }

    pub fn hotkey_failed(&self, app: &AppHandle, shortcut: &str, error: &str) {
        let mut i = self.inner.lock().unwrap();
        i.hotkey_errors.push(format!("{shortcut} unavailable (possibly in use by another app): {error}"));
        self.emit(app, &i);
    }

    pub fn configure_hotkeys(&self, app: &AppHandle, hotkeys: crate::hotkeys::Hotkeys) {
        let mut i = self.inner.lock().unwrap();
        i.hotkeys = hotkeys;
        i.hotkey_errors.clear();
        self.emit(app, &i);
    }

    pub fn action_failed(&self, app: &AppHandle, message: String) {
        let mut i = self.inner.lock().unwrap();
        i.error = Some(message);
        self.emit(app, &i);
    }

    pub fn set_notice(&self, app: &AppHandle, notice: Option<String>) {
        let mut i = self.inner.lock().unwrap();
        i.notice = notice;
        self.emit(app, &i);
    }

    pub fn set_shot(&self, app: &AppHandle, r: &Result<String>) {
        let mut i = self.inner.lock().unwrap();
        match r {
            Ok(p) => {
                i.last_saved = Some(p.clone());
                let saved = SavedOutput { path: p.clone(), kind: "screenshot", duration_ms: None, complete: true };
                i.last_screenshot = Some(saved.clone());
                i.saved_output = Some(saved);
                i.error = None;
            }
            Err(e) => i.error = Some(e.to_string()),
        }
        self.emit(app, &i);
        drop(i);
        if r.is_ok() {
            if app.state::<SettingsStore>().get().screenshot_sound {
                crate::screenshot::play_sound();
            }
            ui::show_screenshot_feedback(app);
        }
    }

    /// Webviews can operate only on outputs remembered by the recorder, never arbitrary paths.
    pub fn saved_output(&self, path: &str) -> Result<SavedOutput> {
        let i = self.inner.lock().unwrap();
        i.saved_output
            .iter()
            .chain(i.last_screenshot.iter())
            .find(|s| s.path == path)
            .cloned()
            .ok_or_else(|| msg("This file is no longer the current saved output"))
    }

    pub fn update_saved_output(&self, app: &AppHandle, old: &str, path: Option<String>) {
        let mut i = self.inner.lock().unwrap();
        let Inner { saved_output, last_screenshot, .. } = &mut *i;
        for saved in [saved_output, last_screenshot] {
            if saved.as_ref().is_some_and(|s| s.path == old) {
                if let Some(path) = &path {
                    saved.as_mut().unwrap().path = path.clone();
                } else {
                    *saved = None;
                }
            }
        }
        if i.last_saved.as_deref() == Some(old) {
            i.last_saved = path;
        }
        self.emit(app, &i);
    }

    pub fn start(&self, app: &AppHandle) -> Result<()> {
        if crate::overlay::region::is_selecting() {
            return Err(msg("Confirm or cancel area selection before recording."));
        }
        let mut i = self.inner.lock().unwrap();
        if i.quitting {
            return Err(msg("Oculus is quitting"));
        }
        if i.system_stopping {
            return Err(msg("Windows is suspending or ending the session"));
        }
        if i.starting || i.session.is_some() {
            return Err(msg("Already recording"));
        }
        i.starting = true;
        i.cancel_start = false;
        i.generation += 1;
        i.error = None;
        i.notice = None;
        let generation = i.generation;
        let requested_source = i.source.clone();
        let requested_identity = i.identity.clone();
        self.emit(app, &i);
        drop(i);
        ui::set_compact(app, true);

        // The worker may end immediately after setup. Gate its callback until the handle
        // is committed, so an early failure cannot clear a new or still-starting session.
        let (committed_tx, committed_rx) = std::sync::mpsc::channel();
        let mut output_notice = None;
        let mut invalid_source = false;
        let result = (|| -> Result<(session::Handle, Source, sources::Identity)> {
            // No explicit choice yet (first run, hotkey from tray): primary monitor.
            let source = match requested_source {
                Some(s) => match requested_identity.as_ref().map(|identity| identity.resolve(&s)) {
                    Some(Ok((source, warning))) => {
                        output_notice = warning;
                        source
                    }
                    Some(Err(e)) => {
                        invalid_source = true;
                        return Err(e);
                    }
                    None => s,
                },
                None => {
                    let id = sources::primary_monitor().ok_or_else(|| msg("No monitor found"))?;
                    Source::Monitor { id }
                }
            };
            let store = app.state::<SettingsStore>();
            let identity = sources::Identity::capture(&source)?;
            let s = store.get();
            let dir = store.output_dir()?;
            if output_notice.is_none() {
                output_notice = store.warning();
            }
            let free = available_bytes(&dir)?;
            if free < DISK_STOP_BYTES {
                return Err(msg("Less than 200 MB free in the recording folder. Free disk space before recording."));
            }
            if free < DISK_WARN_BYTES {
                output_notice = Some("Low disk space: less than 2 GB free in the recording folder.".into());
            }
            let (path, reservation) = reserve_output(&dir, &format!("Oculus {}", local_stamp()), "mp4")?;
            // MF opens the reserved placeholder by URL. Other saves must leave it in place.
            drop(reservation);
            let output_path = path.clone();
            let cfg = session::Config {
                target: source.target(),
                crop: source.crop(),
                fps: s.fps,
                constant_frame_rate: s.constant_frame_rate,
                output_height: s.output_height,
                custom_bitrate_mbps: s.custom_bitrate_mbps,
                rate_control: s.rate_control,
                bpp: s.bpp(),
                codec: s.codec(),
                cursor: s.cursor,
                system_audio: s.system_audio,
                mic: s.mic,
                system_device: s.system_device.clone(),
                mic_device: s.mic_device.clone(),
                gains: Gains { system: s.system_gain, mic: s.mic_gain },
                path,
            };
            let a = app.clone();
            let handle = session::start(cfg, move |o| {
                if committed_rx.recv().is_ok() {
                    a.state::<Recorder>().finished(&a, generation, o);
                }
            });
            if handle.is_err() {
                let _ = std::fs::remove_file(&output_path);
            }
            handle.map(|h| (h, source, identity))
        })();

        let mut i = self.inner.lock().unwrap();
        i.starting = false;
        let (handle, source, identity) = match result {
            Ok(value) => value,
            Err(e) => {
                i.cancel_start = false;
                i.finalizing = false;
                i.error = Some(e.to_string());
                if invalid_source {
                    i.source = None;
                    i.label = None;
                    i.identity = None;
                }
                self.emit(app, &i);
                self.finished_signal.notify_all();
                let quitting = i.quitting;
                let exit_code = i.exit_code;
                drop(i);
                if quitting {
                    app.exit(exit_code);
                } else {
                    ui::set_compact(app, false);
                }
                return Err(e);
            }
        };
        if i.label.is_none() {
            i.label = Some("Primary display".into());
        }
        i.source = Some(source);
        i.identity = Some(identity);
        if output_notice.as_ref().is_some_and(|s| s.starts_with("Selected display")) {
            i.label = Some("Primary display".into());
        }
        i.notice = output_notice;
        if i.cancel_start || i.quitting || i.system_stopping {
            handle.stop();
            i.finalizing = true;
        }
        i.cancelled_start = i.cancel_start;
        i.cancel_start = false;
        i.session = Some(handle);
        i.error = None;
        i.fps = 0;
        i.frames_at_tick = 0;
        self.emit(app, &i);
        drop(i);
        let _ = committed_tx.send(());
        spawn_ticker(app.clone(), generation);
        Ok(())
    }

    pub fn stop(&self, app: &AppHandle) {
        let mut i = self.inner.lock().unwrap();
        if i.starting {
            i.cancel_start = true;
            self.emit(app, &i);
        }
        if let Some(h) = &i.session {
            h.stop();
            i.finalizing = true;
            self.emit(app, &i);
        }
    }

    /// Exit only from the session completion callback, after the writer has finalized.
    pub fn request_exit(&self, app: &AppHandle) {
        self.request_exit_code(app, 0);
    }

    pub fn request_exit_code(&self, app: &AppHandle, code: i32) {
        let mut i = self.inner.lock().unwrap();
        i.quitting = true;
        i.exit_code = code;
        if i.starting || i.session.is_some() {
            i.cancel_start = i.starting;
            if let Some(h) = &i.session {
                h.stop();
            }
            i.finalizing = true;
            self.emit(app, &i);
            drop(i);
        } else {
            drop(i);
            app.exit(code);
        }
    }

    /// Called by the dedicated Windows lifecycle thread, never by the UI thread.
    pub fn stop_and_wait(&self, app: &AppHandle) {
        let mut i = self.inner.lock().unwrap();
        i.system_stopping = true;
        i.cancel_start = i.starting;
        if let Some(h) = &i.session {
            h.stop();
            i.finalizing = true;
            self.emit(app, &i);
        }
        while i.starting || i.session.is_some() {
            i = self.finished_signal.wait(i).unwrap();
        }
    }

    pub fn system_resumed(&self) {
        self.inner.lock().unwrap().system_stopping = false;
    }

    pub fn toggle(&self, app: &AppHandle) -> Result<()> {
        {
            let mut last = self.last_toggle.lock().unwrap();
            if !accept_toggle(&mut last, Instant::now()) {
                return Ok(());
            }
        }
        if self.is_active() {
            self.stop(app);
            Ok(())
        } else {
            self.start(app)
        }
    }

    pub fn toggle_pause(&self, app: &AppHandle) {
        let i = self.inner.lock().unwrap();
        if let (Some(h), false) = (&i.session, i.finalizing) {
            if h.clock.paused() {
                h.clock.resume()
            } else {
                h.clock.pause()
            }
            self.emit(app, &i);
        }
    }

    pub fn toggle_audio_mute(&self, app: &AppHandle, source: &str) -> Result<()> {
        let i = self.inner.lock().unwrap();
        if i.finalizing {
            return Err(msg("Recording is being saved"));
        }
        let session = i.session.as_ref().ok_or_else(|| msg("No recording is active"))?;
        let control = match source {
            "system" => &session.audio_controls.system_muted,
            "mic" => &session.audio_controls.mic_muted,
            _ => return Err(msg("Unknown audio source")),
        };
        control.fetch_xor(true, Ordering::Relaxed);
        self.emit(app, &i);
        Ok(())
    }

    fn finished(&self, app: &AppHandle, generation: u64, o: Outcome) {
        let mut i = self.inner.lock().unwrap();
        if i.generation != generation {
            return;
        }
        i.last_encoder = o.encoder_name.clone();
        i.session = None;
        i.finalizing = false;
        if i.cancelled_start {
            let _ = std::fs::remove_file(&o.path);
            i.cancelled_start = false;
            i.error = None;
            i.notice = Some("Recording cancelled before startup completed".into());
        } else {
            match o.result {
                Ok(()) => {
                    i.last_saved = Some(o.path.to_string_lossy().into_owned());
                    i.saved_output = Some(SavedOutput {
                        path: o.path.to_string_lossy().into_owned(),
                        kind: "recording",
                        duration_ms: Some(o.duration_ms),
                        complete: true,
                    });
                    i.error = None;
                    if o.reason == EndReason::TargetClosed {
                        i.notice = Some("Window closed — recording saved".into());
                    }
                }
                Err(e) => {
                    i.last_saved = Some(o.path.to_string_lossy().into_owned());
                    i.saved_output = Some(SavedOutput {
                        path: o.path.to_string_lossy().into_owned(),
                        kind: "recording",
                        duration_ms: Some(o.duration_ms),
                        complete: false,
                    });
                    i.error = Some(format!("{e}. Partial output retained at {}", o.path.display()));
                }
            }
        }
        let quitting = i.quitting;
        let exit_code = i.exit_code;
        self.emit(app, &i);
        self.finished_signal.notify_all();
        drop(i);
        if quitting {
            app.exit(exit_code);
            return;
        }
        ui::set_compact(app, false);
    }

    fn tick(&self, app: &AppHandle, generation: u64) -> bool {
        let path = {
            let i = self.inner.lock().unwrap();
            if i.generation != generation {
                return false;
            }
            let Some(h) = &i.session else { return false };
            h.path.parent().map(|p| p.to_path_buf())
        };
        // A removable/network disk can block; never hold recorder state while querying it.
        let free = path.as_deref().and_then(|p| available_bytes(p).ok());
        let mut i = self.inner.lock().unwrap();
        if i.generation != generation {
            return false;
        }
        let Some(h) = &i.session else { return false };
        let frames = h.stats.frames.load(Ordering::Relaxed);
        if free.is_some_and(|free| free < DISK_STOP_BYTES) && !i.finalizing {
            h.stop();
            i.finalizing = true;
            i.notice = Some("Recording stopped to save safely: less than 200 MB disk space remains.".into());
        } else if free.is_some_and(|free| free < DISK_WARN_BYTES) {
            i.notice = Some("Low disk space: less than 2 GB remains in the recording folder.".into());
        }
        i.fps = (frames - i.frames_at_tick) as u32;
        i.frames_at_tick = frames;
        self.emit(app, &i);
        true
    }
}

fn accept_toggle(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|last| now.duration_since(last) < Duration::from_millis(400)) {
        return false;
    }
    *last = Some(now);
    true
}

fn spawn_ticker(app: AppHandle, generation: u64) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        if !app.state::<Recorder>().tick(&app, generation) {
            break;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_serde_uses_stable_tagged_shapes() {
        let source = Source::Region { monitor: 7, x: -20, y: 10, w: 1280, h: 720 };
        let value = serde_json::to_value(&source).unwrap();
        assert_eq!(value["kind"], "region");
        assert_eq!(value["monitor"], 7);
        assert_eq!(value["x"], -20);
        assert!(matches!(serde_json::from_value::<Source>(value).unwrap(), Source::Region { monitor: 7, x: -20, .. }));
        assert!(serde_json::from_str::<Source>(r#"{"kind":"unknown","id":1}"#).is_err());
    }

    #[test]
    fn record_hotkey_debounces_without_extending_the_window() {
        let now = Instant::now();
        let mut last = None;
        assert!(accept_toggle(&mut last, now));
        assert!(!accept_toggle(&mut last, now + Duration::from_millis(100)));
        assert!(!accept_toggle(&mut last, now + Duration::from_millis(399)));
        assert!(accept_toggle(&mut last, now + Duration::from_millis(400)));
    }

    #[test]
    fn starting_and_cancelled_setup_are_visible_and_active() {
        let recorder = Recorder::default();
        recorder.inner.lock().unwrap().starting = true;
        assert!(recorder.is_active());
        assert_eq!(recorder.get().state, "starting");
        recorder.inner.lock().unwrap().cancel_start = true;
        assert_eq!(recorder.get().state, "finalizing");
    }

    #[test]
    fn file_actions_accept_only_remembered_outputs() {
        let recorder = Recorder::default();
        {
            let mut i = recorder.inner.lock().unwrap();
            i.saved_output = Some(SavedOutput {
                path: "C:/out/video.mp4".into(),
                kind: "recording",
                duration_ms: Some(1500),
                complete: true,
            });
            i.last_screenshot = Some(SavedOutput {
                path: "C:/out/shot.png".into(),
                kind: "screenshot",
                duration_ms: None,
                complete: true,
            });
        }
        assert_eq!(recorder.saved_output("C:/out/video.mp4").unwrap().duration_ms, Some(1500));
        assert!(recorder.saved_output("C:/out/shot.png").is_ok());
        assert!(recorder.saved_output("C:/out/other.mp4").is_err());
        assert!(recorder.saved_output("C:/out/../private.exe").is_err());
    }
}
