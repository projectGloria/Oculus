// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod autostart;
mod capture;
mod clock;
mod commands;
mod encode;
mod error;
mod hotkeys;
mod lifecycle;
mod logging;
mod overlay;
mod platform;
mod queue;
mod recorder;
mod saved_output;
mod screenshot;
#[cfg(debug_assertions)]
mod selftest;
mod session;
mod settings;
mod shell;
mod sources;
mod tray_status;
mod ui;
mod util;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_global_shortcut::{Shortcut, ShortcutState};

use recorder::Recorder;
use settings::SettingsStore;

fn on_hotkey(app: &AppHandle, sc: &Shortcut) {
    if let Some(action) = app.state::<SettingsStore>().get().hotkeys.action(sc) {
        perform_action(app, action);
    }
}

fn perform_action(app: &AppHandle, action: hotkeys::Action) {
    let app = app.clone();
    let rec = || app.state::<Recorder>();
    if action == hotkeys::Action::Record {
        // start() blocks ~100ms on device/encoder setup; keep the hotkey thread free.
        std::thread::spawn(move || {
            if let Err(e) = app.state::<Recorder>().toggle(&app) {
                tracing::error!(error = %e, "Recording hotkey failed");
                app.state::<Recorder>().action_failed(&app, format!("Could not start recording: {e}"));
            }
        });
    } else if action == hotkeys::Action::Pause {
        rec().toggle_pause(&app);
    } else if action == hotkeys::Action::Area {
        std::thread::spawn(move || {
            if let Err(e) = commands::region_shot(&app) {
                app.state::<Recorder>().action_failed(&app, format!("Could not take area screenshot: {e}"));
            }
        });
    } else if action == hotkeys::Action::Screenshot {
        tauri::async_runtime::spawn(async move {
            let _ = commands::take_screenshot(app).await;
        });
    } else if action == hotkeys::Action::MuteMic {
        if rec().is_active() {
            if let Err(e) = rec().toggle_audio_mute(&app, "mic") {
                rec().action_failed(&app, e.to_string());
            }
        }
    } else if action == hotkeys::Action::RecordArea || action == hotkeys::Action::RecordWindow {
        tauri::async_runtime::spawn(async move {
            let result = if action == hotkeys::Action::RecordArea {
                commands::record_region(app.clone()).await
            } else {
                commands::record_window_at_cursor(app.clone()).await
            };
            if let Err(e) = result {
                app.state::<Recorder>().action_failed(&app, e.to_string());
            }
        });
    }
}

fn tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Oculus", true, None::<&str>)?;
    let record = MenuItem::with_id(app, "record", "Start / stop recording", true, None::<&str>)?;
    let shot = MenuItem::with_id(app, "region", "Screenshot area", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &record, &shot, &PredefinedMenuItem::separator(app)?, &quit])?;
    TrayIconBuilder::with_id("tray")
        .icon(app.default_window_icon().cloned().expect("bundle icon"))
        .tooltip("Oculus")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => ui::show_main(app),
            "record" => perform_action(app, hotkeys::Action::Record),
            "region" => perform_action(app, hotkeys::Action::Area),
            "quit" => {
                app.state::<Recorder>().request_exit(app);
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
    let logging_error = logging::init().err().map(|e| e.to_string());
    #[cfg(debug_assertions)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("--selftest") {
            std::process::exit(selftest::run(&args[1..]));
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().any(|arg| arg == "--tray") {
                ui::show_main(app);
            }
        }))
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
        .manage(tray_status::TrayStatus::default())
        .manage(ui::ScreenshotFeedback::default())
        .manage(hotkeys::Registry::default())
        .setup(move |app| {
            app.manage(SettingsStore::load(app.handle()));
            if app.state::<SettingsStore>().get().launch_at_login {
                if let Err(e) = autostart::configure(true) {
                    app.state::<Recorder>()
                        .action_failed(app.handle(), format!("Could not update launch-at-login setting: {e}"));
                }
            }
            let warning = app.state::<SettingsStore>().warning().or(logging_error);
            app.state::<Recorder>().set_notice(app.handle(), warning);
            tray(app)?;
            lifecycle::start(app.handle().clone())?;
            app.state::<hotkeys::Registry>().initialize(app.handle());
            if !std::env::args().any(|arg| arg == "--tray") {
                ui::show_main(app.handle());
            }
            #[cfg(debug_assertions)]
            selftest::install_app_driver(app.handle());
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
            commands::record_region,
            commands::record_window_at_cursor,
            commands::take_screenshot,
            commands::screenshot_region,
            commands::get_settings,
            commands::set_settings,
            commands::hide_to_tray,
            commands::reveal,
            commands::open_output_dir,
            commands::open_microphone_settings,
            commands::saved_file_details,
            commands::open_saved_file,
            commands::copy_saved_file,
            commands::rename_saved_file,
            commands::delete_saved_file,
            commands::dismiss_screenshot_feedback,
            commands::drag_saved_file,
            commands::retry_hotkeys,
            commands::get_diagnostics,
            commands::list_audio_devices,
            commands::toggle_audio_mute,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Oculus")
        .run(|app, ev| {
            // Last window closed (sent to tray) → keep running. `code: Some` = explicit quit.
            if let RunEvent::ExitRequested { api, code, .. } = ev {
                if code.is_none() {
                    api.prevent_exit();
                } else if app.state::<Recorder>().is_active() {
                    api.prevent_exit();
                    app.state::<Recorder>().request_exit_code(app, code.unwrap_or(0));
                }
            }
        });
}
