// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod capture;
mod clock;
mod commands;
mod encode;
mod error;
mod overlay;
mod recorder;
mod screenshot;
#[cfg(debug_assertions)]
mod selftest;
mod session;
mod settings;
mod sources;
mod ui;
mod util;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use recorder::Recorder;
use settings::SettingsStore;

const MODS: Modifiers = Modifiers::CONTROL.union(Modifiers::SHIFT);

fn on_hotkey(app: &AppHandle, sc: &Shortcut) {
    let app = app.clone();
    let rec = || app.state::<Recorder>();
    if sc.matches(MODS, Code::KeyR) {
        // start() blocks ~100ms on device/encoder setup; keep the hotkey thread free.
        std::thread::spawn(move || {
            if let Err(e) = app.state::<Recorder>().toggle(&app) {
                eprintln!("[hotkey] {e}");
            }
        });
    } else if sc.matches(MODS, Code::KeyP) {
        rec().toggle_pause(&app);
    } else if sc.matches(MODS, Code::KeyA) {
        std::thread::spawn(move || {
            let _ = commands::region_shot(&app);
        });
    } else if sc.matches(MODS, Code::KeyS) {
        tauri::async_runtime::spawn(async move {
            let _ = commands::take_screenshot(app).await;
        });
    }
}

fn tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Oculus", true, None::<&str>)?;
    let record = MenuItem::with_id(app, "record", "Start / stop recording\tCtrl+Shift+R", true, None::<&str>)?;
    let shot = MenuItem::with_id(app, "region", "Screenshot area\tCtrl+Shift+A", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &record, &shot, &PredefinedMenuItem::separator(app)?, &quit])?;
    TrayIconBuilder::with_id("tray")
        .icon(app.default_window_icon().cloned().expect("bundle icon"))
        .tooltip("Oculus")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => ui::show_main(app),
            "record" => on_hotkey(app, &Shortcut::new(Some(MODS), Code::KeyR)),
            "region" => on_hotkey(app, &Shortcut::new(Some(MODS), Code::KeyA)),
            "quit" => {
                // Finalize a running recording instead of leaving a truncated MP4.
                app.state::<Recorder>().stop(app);
                let app = app.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        if !app.state::<Recorder>().is_active() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    app.exit(0);
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                ui::show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    #[cfg(debug_assertions)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("--selftest") {
            std::process::exit(selftest::run(&args[1..]));
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, sc, ev| {
                    if ev.state() == ShortcutState::Pressed {
                        on_hotkey(app, sc);
                    }
                })
                .build(),
        )
        .manage(Recorder::default())
        .setup(|app| {
            app.manage(SettingsStore::load(app.handle()));
            tray(app)?;
            for code in [Code::KeyR, Code::KeyP, Code::KeyS, Code::KeyA] {
                // Another app may own the combo; Oculus still works from the UI/tray.
                if let Err(e) = app.global_shortcut().register(Shortcut::new(Some(MODS), code)) {
                    eprintln!("[hotkey] {code:?}: {e}");
                }
            }
            ui::show_main(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_sources,
            commands::get_recorder_state,
            commands::set_source,
            commands::start_recording,
            commands::stop_recording,
            commands::toggle_pause,
            commands::select_region,
            commands::take_screenshot,
            commands::screenshot_region,
            commands::get_settings,
            commands::set_settings,
            commands::hide_to_tray,
            commands::reveal,
            commands::open_output_dir,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Oculus")
        .run(|_, ev| {
            // Last window closed (sent to tray) → keep running. `code: Some` = explicit quit.
            if let RunEvent::ExitRequested { api, code: None, .. } = ev {
                api.prevent_exit();
            }
        });
}
