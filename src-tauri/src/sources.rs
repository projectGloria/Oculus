use serde::Serialize;
use windows::core::BOOL;
use windows::Graphics::Capture::GraphicsCaptureSession;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub id: isize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: isize,
    pub title: String,
    pub exe: String,
}

#[derive(Serialize)]
pub struct Sources {
    pub monitors: Vec<MonitorInfo>,
    pub windows: Vec<WindowInfo>,
    pub supported: bool,
}

pub fn list() -> Sources {
    Sources {
        monitors: monitors(),
        windows: windows(),
        supported: GraphicsCaptureSession::IsSupported().unwrap_or(false),
    }
}

pub fn monitor_info(h: HMONITOR) -> Option<(RECT, bool)> {
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    unsafe { GetMonitorInfoW(h, &mut mi as *mut _ as *mut MONITORINFO).as_bool() }
        .then(|| (mi.monitorInfo.rcMonitor, mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0))
}

/// The monitor under the cursor: where the region overlay and "screenshot" hotkey act.
pub fn monitor_at_cursor() -> HMONITOR {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
        MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST)
    }
}

pub fn primary_monitor() -> Option<isize> {
    monitors().into_iter().find(|m| m.primary).map(|m| m.id)
}

fn monitors() -> Vec<MonitorInfo> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = &mut *(data.0 as *mut Vec<HMONITOR>);
        out.push(h);
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut handles as *mut _ as isize));
    }
    // Rects are physical px: Tauri makes the process per-monitor-v2 DPI aware.
    handles
        .into_iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let (r, primary) = monitor_info(h)?;
            Some(MonitorInfo {
                id: h.0 as isize,
                name: format!("Display {}", i + 1),
                x: r.left,
                y: r.top,
                width: r.right - r.left,
                height: r.bottom - r.top,
                primary,
            })
        })
        .collect()
}

fn windows() -> Vec<WindowInfo> {
    unsafe extern "system" fn cb(hwnd: HWND, data: LPARAM) -> BOOL {
        let out = &mut *(data.0 as *mut Vec<WindowInfo>);
        if let Some(w) = capturable(hwnd) {
            out.push(w);
        }
        true.into()
    }
    let mut out = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// Same filter the Alt-Tab list uses, roughly: visible, titled, not a tool window, not
/// cloaked (UWP apps parked on other virtual desktops), not minimized, not us.
unsafe fn capturable(hwnd: HWND) -> Option<WindowInfo> {
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return None;
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 || GetAncestor(hwnd, GA_ROOT) != hwnd {
        return None;
    }
    let mut cloaked = 0u32;
    let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4);
    if cloaked != 0 {
        return None;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == GetCurrentProcessId() {
        return None;
    }
    let mut r = RECT::default();
    let _ = GetWindowRect(hwnd, &mut r);
    if r.right - r.left < 32 || r.bottom - r.top < 32 {
        return None;
    }
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(hwnd, &mut buf);
    if n == 0 {
        return None;
    }
    let title = String::from_utf16_lossy(&buf[..n as usize]);
    Some(WindowInfo { id: hwnd.0 as isize, title, exe: exe_name(pid).unwrap_or_default() })
}

unsafe fn exe_name(pid: u32) -> Option<String> {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
    let _ = CloseHandle(h);
    ok.then(|| {
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit('\\').next().unwrap_or(&full).to_string()
    })
}
