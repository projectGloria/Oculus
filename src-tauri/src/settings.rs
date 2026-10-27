use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::encode::mf_writer::Codec;
use crate::error::Result;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum CodecSetting {
    H264,
    Hevc,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub output_dir: String,
    pub fps: u32,
    pub quality: Quality,
    pub codec: CodecSetting,
    pub cursor: bool,
    pub system_audio: bool,
    pub mic: bool,
    pub system_gain: f32,
    pub mic_gain: f32,
    pub copy_screenshots: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            output_dir: String::new(),
            fps: 60,
            quality: Quality::Medium,
            codec: CodecSetting::H264,
            cursor: true,
            system_audio: true,
            mic: false,
            system_gain: 1.0,
            mic_gain: 1.0,
            copy_screenshots: true,
        }
    }
}

impl Settings {
    pub fn codec(&self) -> Codec {
        match self.codec {
            CodecSetting::H264 => Codec::H264,
            CodecSetting::Hevc => Codec::Hevc,
        }
    }

    /// Bits per pixel per frame. 1440p60 at Medium ≈ 18 Mbps; HEVC gets the same
    /// quality for ~30% less.
    pub fn bpp(&self) -> f32 {
        let base = match self.quality {
            Quality::Low => 0.05,
            Quality::Medium => 0.08,
            Quality::High => 0.14,
        };
        if self.codec == CodecSetting::Hevc { base * 0.7 } else { base }
    }

    /// Clamp whatever the UI (or a hand-edited JSON) sent to values the pipeline supports.
    fn sanitized(mut self) -> Self {
        self.fps = if self.fps >= 45 { 60 } else { 30 };
        self.system_gain = self.system_gain.clamp(0.0, 2.0);
        self.mic_gain = self.mic_gain.clamp(0.0, 2.0);
        self
    }
}

/// Plain JSON in the app config dir. A plugin would be more code than this.
pub struct SettingsStore {
    path: PathBuf,
    current: Mutex<Settings>,
}

impl SettingsStore {
    pub fn load(app: &AppHandle) -> Self {
        let dir = app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from("."));
        let path = dir.join("settings.json");
        let mut s: Settings = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if s.output_dir.is_empty() {
            let videos = app.path().video_dir().unwrap_or_else(|_| PathBuf::from("."));
            s.output_dir = videos.join("Oculus").to_string_lossy().into_owned();
        }
        Self { path, current: Mutex::new(s.sanitized()) }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().unwrap().clone()
    }

    pub fn set(&self, s: Settings) -> Result<Settings> {
        let s = s.sanitized();
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, serde_json::to_vec_pretty(&s).unwrap())?;
        *self.current.lock().unwrap() = s.clone();
        Ok(s)
    }

    pub fn output_dir(&self) -> Result<PathBuf> {
        let dir = PathBuf::from(self.get().output_dir);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}
