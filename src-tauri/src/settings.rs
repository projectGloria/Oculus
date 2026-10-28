use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::encode::mf_writer::Codec;
use crate::error::Result;
use crate::util::reserve_output;

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

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum RateControl {
    Default,
    Cbr,
    Vbr,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub version: u32,
    pub launch_at_login: bool,
    pub output_dir: String,
    pub fps: u32,
    pub constant_frame_rate: bool,
    pub output_height: u32,
    pub custom_bitrate_mbps: Option<u32>,
    pub rate_control: RateControl,
    pub quality: Quality,
    pub codec: CodecSetting,
    pub cursor: bool,
    pub system_audio: bool,
    pub mic: bool,
    pub system_device: Option<String>,
    pub mic_device: Option<String>,
    pub system_gain: f32,
    pub mic_gain: f32,
    pub copy_screenshots: bool,
    pub screenshot_sound: bool,
    pub hotkeys: crate::hotkeys::Hotkeys,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            launch_at_login: false,
            output_dir: String::new(),
            fps: 60,
            constant_frame_rate: true,
            output_height: 0,
            custom_bitrate_mbps: None,
            rate_control: RateControl::Default,
            quality: Quality::Medium,
            codec: CodecSetting::H264,
            cursor: true,
            system_audio: true,
            mic: false,
            system_device: None,
            mic_device: None,
            system_gain: 1.0,
            mic_gain: 1.0,
            copy_screenshots: true,
            screenshot_sound: false,
            hotkeys: crate::hotkeys::Hotkeys::default(),
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
        if self.codec == CodecSetting::Hevc {
            base * 0.7
        } else {
            base
        }
    }

    /// Clamp whatever the UI (or a hand-edited JSON) sent to values the pipeline supports.
    fn sanitized(mut self) -> Self {
        // The Windows fragmented MP4 sink truncates sparse-video durations at fragment
        // boundaries. H.264 uses a complete frame grid until a VFR-safe muxer replaces it.
        if self.codec == CodecSetting::H264 {
            self.constant_frame_rate = true;
        }
        if self.version == 0 {
            self.version = 1;
        }
        if ![24, 30, 48, 50, 60, 72, 120].contains(&self.fps) {
            self.fps = if self.fps >= 45 { 60 } else { 30 };
        }
        if ![0, 720, 1080, 1440, 2160].contains(&self.output_height) {
            self.output_height = 0;
        }
        self.custom_bitrate_mbps = self.custom_bitrate_mbps.map(|value| value.clamp(2, 150));
        self.system_gain = if self.system_gain.is_finite() { self.system_gain.clamp(0.0, 2.0) } else { 1.0 };
        self.mic_gain = if self.mic_gain.is_finite() { self.mic_gain.clamp(0.0, 2.0) } else { 1.0 };
        self
    }
}

/// Plain JSON in the app config dir. A plugin would be more code than this.
pub struct SettingsStore {
    path: PathBuf,
    current: Mutex<Settings>,
    warning: Mutex<Option<String>>,
    fallback_dir: PathBuf,
}

impl SettingsStore {
    pub fn load(app: &AppHandle) -> Self {
        let dir = app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from("."));
        let path = dir.join("settings.json");
        let (mut s, warning) = read_settings(&path);
        let fallback_dir = app.path().video_dir().unwrap_or_else(|_| PathBuf::from(".")).join("Oculus");
        if s.output_dir.is_empty() {
            s.output_dir = fallback_dir.to_string_lossy().into_owned();
        }
        Self { path, current: Mutex::new(s.sanitized()), warning: Mutex::new(warning), fallback_dir }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().unwrap().clone()
    }

    pub fn warning(&self) -> Option<String> {
        self.warning.lock().unwrap().clone()
    }

    #[cfg(debug_assertions)]
    pub fn configure_selftest(&self, output: PathBuf) {
        let mut current = self.current.lock().unwrap();
        *current = Settings { output_dir: output.to_string_lossy().into_owned(), mic: true, ..Settings::default() };
    }

    pub fn set(&self, s: Settings) -> Result<Settings> {
        if s.version != 1 {
            return Err(crate::error::msg("Unsupported settings version; use a compatible Oculus version"));
        }
        s.hotkeys.parsed()?;
        let s = s.sanitized();
        // Serialize disk writes and in-memory commits together; concurrent IPC requests
        // must not leave the JSON and the current settings describing different versions.
        let mut current = self.current.lock().unwrap();
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let startup_changed = s.launch_at_login != current.launch_at_login;
        let startup_backup = startup_changed.then(crate::autostart::snapshot).transpose()?;
        if startup_changed {
            crate::autostart::configure(s.launch_at_login)?;
        }
        if let Err(error) = atomic_write(&self.path, &serde_json::to_vec_pretty(&s).unwrap()) {
            if let Some(previous) = startup_backup {
                if let Err(rollback) = crate::autostart::restore(&previous) {
                    return Err(crate::error::msg(format!(
                        "Could not save settings: {error}. Could not restore startup setting: {rollback}"
                    )));
                }
            }
            return Err(error.into());
        }
        *current = s.clone();
        Ok(s)
    }

    pub fn output_dir(&self) -> Result<PathBuf> {
        let dir = PathBuf::from(self.get().output_dir);
        match writable_dir(&dir) {
            Ok(()) => Ok(dir),
            Err(e) if dir != self.fallback_dir => {
                writable_dir(&self.fallback_dir)?;
                *self.warning.lock().unwrap() = Some(format!(
                    "Output folder {} is unavailable ({e}). Saving to {} instead.",
                    dir.display(),
                    self.fallback_dir.display()
                ));
                Ok(self.fallback_dir.clone())
            }
            Err(e) => Err(e.into()),
        }
    }
}

fn writable_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let (probe, file) = reserve_output(dir, ".oculus-write-check", "tmp")?;
    drop(file);
    std::fs::remove_file(probe)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_name().unwrap().to_string_lossy();
    let (temporary, mut file) = reserve_output(dir, &stem, "tmp")?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // Same-directory rename replaces the destination atomically on NTFS.
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn read_settings(path: &Path) -> (Settings, Option<String>) {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(settings) => (settings, None),
            Err(error) => {
                let backup = (|| -> std::io::Result<PathBuf> {
                    let (backup, mut file) = reserve_output(path.parent().unwrap(), "settings.json", "bad")?;
                    file.write_all(&bytes)?;
                    file.sync_all()?;
                    Ok(backup)
                })();
                let detail = match backup {
                    Ok(backup) => format!("The damaged file was backed up to {}", backup.display()),
                    Err(e) => format!("Could not back up {}: {e}", path.display()),
                };
                (Settings::default(), Some(format!("Settings could not be read ({error}). Using defaults. {detail}")))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Settings::default(), None),
        Err(e) => (Settings::default(), Some(format!("Cannot read {}: {e}. Using defaults.", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::tests::TestDir;

    #[test]
    fn legacy_settings_gain_safe_new_defaults_and_schema_version() {
        let settings: Settings = serde_json::from_str(r#"{"mic":true,"fps":30}"#).unwrap();
        assert_eq!(settings.version, 1);
        assert!(settings.mic);
        assert!(!settings.screenshot_sound);
        assert!(settings.constant_frame_rate);
        assert_eq!(settings.hotkeys.record, "Ctrl+Shift+R");
        let migrated = Settings { version: 0, ..settings }.sanitized();
        assert_eq!(migrated.version, 1);
    }

    #[test]
    fn sanitized_settings_bound_gains_and_replace_nonfinite_values() {
        let invalid =
            Settings { fps: 0, system_gain: f32::NAN, mic_gain: f32::INFINITY, ..Settings::default() }.sanitized();
        assert_eq!(invalid.fps, 30);
        assert_eq!(invalid.system_gain, 1.0);
        assert_eq!(invalid.mic_gain, 1.0);
        let clamped = Settings { fps: 900, system_gain: -1.0, mic_gain: 10.0, ..Settings::default() }.sanitized();
        assert_eq!(clamped.fps, 60);
        assert_eq!(clamped.system_gain, 0.0);
        assert_eq!(clamped.mic_gain, 2.0);
    }

    #[test]
    fn encoder_preferences_preserve_supported_rates_and_bound_custom_values() {
        assert!(Settings { constant_frame_rate: false, ..Settings::default() }.sanitized().constant_frame_rate);
        assert!(
            !Settings { codec: CodecSetting::Hevc, constant_frame_rate: false, ..Settings::default() }
                .sanitized()
                .constant_frame_rate
        );
        for fps in [24, 30, 48, 50, 60, 72, 120] {
            assert_eq!(Settings { fps, ..Settings::default() }.sanitized().fps, fps);
        }
        let settings =
            Settings { output_height: 99999, custom_bitrate_mbps: Some(u32::MAX), ..Settings::default() }.sanitized();
        assert_eq!(settings.output_height, 0);
        assert_eq!(settings.custom_bitrate_mbps, Some(150));
        assert_eq!(
            Settings { custom_bitrate_mbps: Some(0), ..Settings::default() }.sanitized().custom_bitrate_mbps,
            Some(2)
        );
    }

    #[test]
    fn unsupported_schema_cannot_overwrite_existing_settings() {
        let dir = TestDir::new();
        let path = dir.0.join("settings.json");
        std::fs::write(&path, b"preserve newer settings").unwrap();
        let store = SettingsStore {
            path: path.clone(),
            current: Mutex::new(Settings::default()),
            warning: Mutex::new(None),
            fallback_dir: dir.0.clone(),
        };
        assert!(store.set(Settings { version: 2, ..Settings::default() }).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"preserve newer settings");
    }

    #[test]
    fn atomic_write_replaces_existing_settings() {
        let dir = TestDir::new();
        let path = dir.0.join("settings.json");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new settings").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new settings");
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_settings_are_backed_up_without_overwriting_older_backups() {
        let dir = TestDir::new();
        let path = dir.0.join("settings.json");
        std::fs::write(&path, b"{broken").unwrap();
        std::fs::write(dir.0.join("settings.json.bad"), b"older backup").unwrap();
        let (_, warning) = read_settings(&path);
        assert!(warning.unwrap().contains("Using defaults"));
        assert_eq!(std::fs::read(dir.0.join("settings.json (2).bad")).unwrap(), b"{broken");
        assert_eq!(std::fs::read(dir.0.join("settings.json.bad")).unwrap(), b"older backup");
        assert_eq!(std::fs::read(path).unwrap(), b"{broken");
    }

    #[test]
    fn failed_save_does_not_commit_settings_or_leave_temporary_file() {
        let dir = TestDir::new();
        let path = dir.0.join("settings.json");
        std::fs::create_dir(&path).unwrap();
        let store = SettingsStore {
            path,
            current: Mutex::new(Settings::default()),
            warning: Mutex::new(None),
            fallback_dir: dir.0.clone(),
        };
        let changed = Settings { mic: true, ..Settings::default() };
        assert!(store.set(changed).is_err());
        assert!(!store.get().mic);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn unavailable_output_falls_back_with_warning_without_changing_preference() {
        let dir = TestDir::new();
        let unavailable = dir.0.join("not-a-directory");
        std::fs::write(&unavailable, b"keep").unwrap();
        let fallback = dir.0.join("fallback");
        let store = SettingsStore {
            path: dir.0.join("settings.json"),
            current: Mutex::new(Settings {
                output_dir: unavailable.to_string_lossy().into_owned(),
                ..Settings::default()
            }),
            warning: Mutex::new(None),
            fallback_dir: fallback.clone(),
        };
        assert_eq!(store.output_dir().unwrap(), fallback);
        assert!(store.warning().unwrap().contains("Saving to"));
        assert_eq!(PathBuf::from(store.get().output_dir), unavailable);
        assert_eq!(std::fs::read(unavailable).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(fallback).unwrap().count(), 0);
    }
}
