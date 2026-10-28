use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Modifiers, Shortcut};

use crate::error::{msg, Result};
use crate::settings::{Settings, SettingsStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Record,
    Pause,
    Screenshot,
    Area,
    RecordArea,
    RecordWindow,
    MuteMic,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Hotkeys {
    pub enabled: bool,
    pub record: String,
    pub pause: String,
    pub screenshot: String,
    pub area: String,
    pub record_area: String,
    pub record_window: String,
    pub mute_mic: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            enabled: true,
            record: "Ctrl+Shift+R".into(),
            pause: "Ctrl+Shift+P".into(),
            screenshot: "Ctrl+Shift+S".into(),
            area: "Ctrl+Shift+A".into(),
            record_area: String::new(),
            record_window: String::new(),
            mute_mic: String::new(),
        }
    }
}

impl Hotkeys {
    pub fn parsed(&self) -> Result<Vec<(Action, Shortcut)>> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let mut parsed = Vec::new();
        for (action, value) in [
            (Action::Record, &self.record),
            (Action::Pause, &self.pause),
            (Action::Screenshot, &self.screenshot),
            (Action::Area, &self.area),
            (Action::RecordArea, &self.record_area),
            (Action::RecordWindow, &self.record_window),
            (Action::MuteMic, &self.mute_mic),
        ] {
            if value.trim().is_empty() {
                continue;
            }
            let normalized = value
                .split('+')
                .map(|part| {
                    let part = part.trim();
                    if ["win", "windows", "meta"].iter().any(|alias| part.eq_ignore_ascii_case(alias)) {
                        "Super"
                    } else {
                        part
                    }
                })
                .collect::<Vec<_>>()
                .join("+");
            let key = normalized.parse::<Shortcut>().map_err(|e| msg(format!("Invalid shortcut {value}: {e}")))?;
            if !key.mods.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER) {
                return Err(msg(format!(
                    "Shortcut {value} needs Ctrl, Alt or Win to avoid intercepting normal typing"
                )));
            }
            if parsed.iter().any(|(_, previous): &(Action, Shortcut)| previous.id() == key.id()) {
                return Err(msg(format!("Shortcut {value} is assigned to more than one action")));
            }
            parsed.push((action, key));
        }
        Ok(parsed)
    }

    pub fn action(&self, shortcut: &Shortcut) -> Option<Action> {
        self.parsed().ok()?.into_iter().find(|(_, key)| key.id() == shortcut.id()).map(|(action, _)| action)
    }
}

#[derive(Default)]
pub struct Registry(Mutex<Vec<Shortcut>>);

impl Registry {
    pub fn initialize(&self, app: &AppHandle) {
        app.state::<crate::recorder::Recorder>().configure_hotkeys(app, app.state::<SettingsStore>().get().hotkeys);
        let keys = match app.state::<SettingsStore>().get().hotkeys.parsed() {
            Ok(keys) => keys,
            Err(e) => {
                app.state::<crate::recorder::Recorder>().hotkey_failed(app, "Hotkeys", &e.to_string());
                return;
            }
        };
        for (_, key) in keys {
            match app.global_shortcut().register(key) {
                Ok(()) => self.0.lock().unwrap().push(key),
                Err(e) => app.state::<crate::recorder::Recorder>().hotkey_failed(app, &key.to_string(), &e.to_string()),
            }
        }
    }

    pub fn save(&self, app: &AppHandle, settings: Settings, force: bool) -> Result<Settings> {
        // Events read SettingsStore, never this transaction lock: registration dispatches
        // to the main thread, which must remain free to handle shortcut events.
        let mut registered = self.0.lock().unwrap();
        if !force && settings.hotkeys == app.state::<SettingsStore>().get().hotkeys {
            return app.state::<SettingsStore>().set(settings);
        }
        let desired: Vec<_> = settings.hotkeys.parsed()?.into_iter().map(|(_, key)| key).collect();
        transition(
            &mut registered,
            &desired,
            |key| {
                app.global_shortcut()
                    .register(key)
                    .map_err(|e| msg(format!("Shortcut {key} is unavailable, possibly in use by another app: {e}")))
            },
            |key| app.global_shortcut().unregister(key).map_err(|e| msg(e.to_string())),
            || app.state::<SettingsStore>().set(settings),
        )
        .inspect(|saved| app.state::<crate::recorder::Recorder>().configure_hotkeys(app, saved.hotkeys.clone()))
    }
}

fn transition<T>(
    registered: &mut Vec<Shortcut>,
    desired: &[Shortcut],
    mut register: impl FnMut(Shortcut) -> Result<()>,
    mut unregister: impl FnMut(Shortcut) -> Result<()>,
    persist: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let previous = registered.clone();
    let result = (|| {
        for key in desired {
            if !registered.contains(key) {
                register(*key)?;
                registered.push(*key);
            }
        }
        for key in &previous {
            if !desired.contains(key) {
                unregister(*key)?;
                registered.retain(|k| k != key);
            }
        }
        persist()
    })();
    if let Err(original) = result {
        let mut failures = Vec::new();
        for key in registered.clone() {
            if !previous.contains(&key) {
                match unregister(key) {
                    Ok(()) => registered.retain(|k| *k != key),
                    Err(e) => failures.push(e.to_string()),
                }
            }
        }
        for key in previous {
            if !registered.contains(&key) {
                match register(key) {
                    Ok(()) => registered.push(key),
                    Err(e) => failures.push(e.to_string()),
                }
            }
        }
        return Err(if failures.is_empty() {
            original
        } else {
            msg(format!("{original}. Could not restore all previous shortcuts: {}", failures.join("; ")))
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn hotkeys_reject_duplicates_and_plain_typing_but_allow_disabled_actions() {
        let mut settings = Hotkeys { pause: "Control+Shift+KeyR".into(), ..Hotkeys::default() };
        assert!(settings.parsed().is_err());
        settings.pause = "a".into();
        assert!(settings.parsed().is_err());
        settings.pause.clear();
        assert_eq!(settings.parsed().unwrap().len(), 3);
        settings.enabled = false;
        assert!(settings.parsed().unwrap().is_empty());
    }

    #[test]
    fn registration_conflict_preserves_previous_shortcuts_and_does_not_persist() {
        let old: Shortcut = "Ctrl+Shift+R".parse().unwrap();
        let new: Shortcut = "Ctrl+Shift+N".parse().unwrap();
        let conflict: Shortcut = "Ctrl+Shift+Q".parse().unwrap();
        let active = RefCell::new(vec![old]);
        let mut tracked = vec![old];
        let result = transition(
            &mut tracked,
            &[new, conflict],
            |key| {
                if key == conflict {
                    return Err(msg("in use"));
                }
                active.borrow_mut().push(key);
                Ok(())
            },
            |key| {
                active.borrow_mut().retain(|k| *k != key);
                Ok(())
            },
            || -> Result<()> { panic!("must not save") },
        );
        assert!(result.is_err());
        assert_eq!(tracked, vec![old]);
        assert_eq!(*active.borrow(), vec![old]);
    }

    #[test]
    fn disk_failure_restores_removed_shortcuts() {
        let old: Shortcut = "Ctrl+Shift+R".parse().unwrap();
        let new: Shortcut = "Ctrl+Shift+N".parse().unwrap();
        let active = RefCell::new(vec![old]);
        let mut tracked = vec![old];
        let result = transition(
            &mut tracked,
            &[new],
            |key| {
                active.borrow_mut().push(key);
                Ok(())
            },
            |key| {
                active.borrow_mut().retain(|k| *k != key);
                Ok(())
            },
            || -> Result<()> { Err(msg("disk full")) },
        );
        assert!(result.is_err());
        assert_eq!(tracked, vec![old]);
        assert_eq!(*active.borrow(), vec![old]);
    }
}
