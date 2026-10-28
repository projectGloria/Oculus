//! A hidden top-level window receives shutdown and power broadcasts even in tray mode.
//! It has its own message thread so waiting for finalization never blocks the Tauri UI.

use std::cell::RefCell;

use tauri::{AppHandle, Manager};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::error::{msg, Result};
use crate::recorder::Recorder;

thread_local! {
    static APP: RefCell<Option<AppHandle>> = const { RefCell::new(None) };
}

pub fn start(app: AppHandle) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new().name("oculus-lifecycle".into()).spawn(move || unsafe {
        APP.with(|slot| *slot.borrow_mut() = Some(app));
        let setup = (|| -> windows::core::Result<()> {
            let instance = GetModuleHandleW(None)?;
            let class = w!("OculusLifecycle");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
            // A message-only window would not receive WM_QUERYENDSESSION broadcasts.
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                w!("Oculus"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            Ok(())
        })();
        let ok = setup.is_ok();
        let _ = tx.send(setup.map_err(|e| msg(format!("Windows lifecycle monitor: {e}"))));
        if ok {
            let mut event = MSG::default();
            while GetMessageW(&mut event, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&event);
                DispatchMessageW(&event);
            }
        }
        APP.with(|slot| slot.borrow_mut().take());
    })?;
    rx.recv().map_err(|_| msg("Windows lifecycle thread stopped during setup"))?
}

unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_QUERYENDSESSION => {
            APP.with(|slot| {
                if let Some(app) = slot.borrow().as_ref() {
                    let _ = ShutdownBlockReasonCreate(hwnd, w!("Saving Oculus recording…"));
                    app.state::<Recorder>().stop_and_wait(app);
                    let _ = ShutdownBlockReasonDestroy(hwnd);
                }
            });
            LRESULT(1)
        }
        // PBT_APMSUSPEND = 4. Windows gives applications limited time here; finalize
        // immediately rather than allowing WGC/audio to continue with stale devices.
        WM_POWERBROADCAST if wparam.0 == PBT_APMSUSPEND as usize => {
            APP.with(|slot| {
                if let Some(app) = slot.borrow().as_ref() {
                    app.state::<Recorder>().stop_and_wait(app);
                }
            });
            LRESULT(1)
        }
        WM_ENDSESSION if wparam.0 != 0 => {
            APP.with(|slot| {
                if let Some(app) = slot.borrow().as_ref() {
                    app.state::<Recorder>().stop_and_wait(app);
                }
            });
            LRESULT(0)
        }
        WM_ENDSESSION | WM_POWERBROADCAST
            if message == WM_ENDSESSION
                || wparam.0 == PBT_APMRESUMEAUTOMATIC as usize
                || wparam.0 == PBT_APMRESUMESUSPEND as usize =>
        {
            APP.with(|slot| {
                if let Some(app) = slot.borrow().as_ref() {
                    app.state::<Recorder>().system_resumed();
                }
            });
            LRESULT(1)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}
