//! App-level recorder state. The backend is the single source of truth: every change goes
//! out as a full `recorder://state` snapshot with a monotonic `seq`, and a freshly created
//! webview hydrates with `get_recorder_state` (same shape) after subscribing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::RECT;

use crate::audio::mixer::Gains;
use crate::capture::wgc::Target;
use crate::error::{msg, Result};
use crate::session::{self, EndReason, Outcome};
use crate::settings::SettingsStore;
use crate::util::local_stamp;
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
    /// idle | recording | paused | finalizing
    pub state: &'static str,
    pub elapsed_ms: u64,
    pub fps: u32,
    pub dropped: u64,
    pub source: Option<Source>,
    pub source_label: Option<String>,
    /// Most recent recording *or* screenshot.
    pub last_saved: Option<String>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Inner {
    session: Option<session::Handle>,
    finalizing: bool,
    source: Option<Source>,
    label: Option<String>,
    last_saved: Option<String>,
    error: Option<String>,
    fps: u32,
    frames_at_tick: u64,
    /// Bumped per session so a stale tick thread exits instead of ticking the next one.
    generation: u64,
}

#[derive(Default)]
pub struct Recorder {
    inner: Mutex<Inner>,
    seq: AtomicU64,
}

impl Recorder {
    fn snapshot(&self, i: &Inner) -> Snapshot {
        let (state, elapsed, dropped) = match &i.session {
            Some(h) if i.finalizing => ("finalizing", h.clock.now(), h.stats.dropped.load(Ordering::Relaxed)),
            Some(h) => (if h.clock.paused() { "paused" } else { "recording" }, h.clock.now(), h.stats.dropped.load(Ordering::Relaxed)),
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
            error: i.error.clone(),
        }
    }

    pub fn get(&self) -> Snapshot {
        let i = self.inner.lock().unwrap();
        self.snapshot(&i)
    }

    /// Snapshot + emit under the same lock, so `seq` order == emission order.
    fn emit(&self, app: &AppHandle, i: &Inner) {
        let _ = app.emit("recorder://state", self.snapshot(i));
    }

    pub fn is_active(&self) -> bool {
        self.inner.lock().unwrap().session.is_some()
    }

    pub fn set_source(&self, app: &AppHandle, source: Source, label: String) {
        let mut i = self.inner.lock().unwrap();
        i.source = Some(source);
        i.label = Some(label);
        self.emit(app, &i);
    }

    pub fn source(&self) -> Option<Source> {
        self.inner.lock().unwrap().source.clone()
    }

    pub fn set_shot(&self, app: &AppHandle, r: &Result<String>) {
        let mut i = self.inner.lock().unwrap();
        match r {
            Ok(p) => {
                i.last_saved = Some(p.clone());
                i.error = None;
            }
            Err(e) => i.error = Some(e.to_string()),
        }
        self.emit(app, &i);
    }

    pub fn start(&self, app: &AppHandle) -> Result<()> {
        let mut i = self.inner.lock().unwrap();
        if i.session.is_some() {
            return Err(msg("Already recording"));
        }
        // No explicit choice yet (first run, hotkey from tray): primary monitor.
        let source = match i.source.clone() {
            Some(s) => s,
            None => {
                let id = sources::primary_monitor().ok_or_else(|| msg("No monitor found"))?;
                i.label = Some("Primary display".into());
                Source::Monitor { id }
            }
        };
        let store = app.state::<SettingsStore>();
        let s = store.get();
        let cfg = session::Config {
            target: source.target(),
            crop: source.crop(),
            fps: s.fps,
            bpp: s.bpp(),
            codec: s.codec(),
            cursor: s.cursor,
            system_audio: s.system_audio,
            mic: s.mic,
            gains: Gains { system: s.system_gain, mic: s.mic_gain },
            path: store.output_dir()?.join(format!("Oculus {}.mp4", local_stamp())),
        };
        let a = app.clone();
        let handle = session::start(cfg, move |o| a.state::<Recorder>().finished(&a, o));
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                i.error = Some(e.to_string());
                self.emit(app, &i);
                return Err(e);
            }
        };
        i.source = Some(source);
        i.session = Some(handle);
        i.error = None;
        i.fps = 0;
        i.frames_at_tick = 0;
        i.generation += 1;
        self.emit(app, &i);
        let generation = i.generation;
        drop(i);
        spawn_ticker(app.clone(), generation);
        ui::set_compact(app, true);
        Ok(())
    }

    pub fn stop(&self, app: &AppHandle) {
        let mut i = self.inner.lock().unwrap();
        if let Some(h) = &i.session {
            h.stop();
            i.finalizing = true;
            self.emit(app, &i);
        }
    }

    pub fn toggle(&self, app: &AppHandle) -> Result<()> {
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

    fn finished(&self, app: &AppHandle, o: Outcome) {
        let mut i = self.inner.lock().unwrap();
        i.session = None;
        i.finalizing = false;
        match o.result {
            Ok(()) => {
                i.last_saved = Some(o.path.to_string_lossy().into_owned());
                i.error = (o.reason == EndReason::TargetClosed).then(|| "Window closed — recording saved".to_string());
            }
            Err(e) => i.error = Some(e.to_string()),
        }
        self.emit(app, &i);
        drop(i);
        ui::set_compact(app, false);
    }

    fn tick(&self, app: &AppHandle, generation: u64) -> bool {
        let mut i = self.inner.lock().unwrap();
        if i.generation != generation {
            return false;
        }
        let Some(h) = &i.session else { return false };
        let frames = h.stats.frames.load(Ordering::Relaxed);
        i.fps = (frames - i.frames_at_tick) as u32;
        i.frames_at_tick = frames;
        self.emit(app, &i);
        true
    }
}

fn spawn_ticker(app: AppHandle, generation: u64) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        if !app.state::<Recorder>().tick(&app, generation) {
            break;
        }
    });
}
