use serde::Serialize;
use windows::core::BOOL;
use windows::Graphics::Capture::GraphicsCaptureSession;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
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
    pub device: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: isize,
    pub title: String,
    pub exe: String,
    pub minimized: bool,
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
        .then_some((mi.monitorInfo.rcMonitor, mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0))
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

pub fn window_at_cursor() -> Option<WindowInfo> {
    unsafe {
        let mut point = POINT::default();
        GetCursorPos(&mut point).ok()?;
        let root = GetAncestor(WindowFromPoint(point), GA_ROOT);
        capturable(root).filter(|window| !window.minimized)
    }
}

pub(crate) fn monitors() -> Vec<MonitorInfo> {
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
                device: monitor_device(h).unwrap_or_default(),
            })
        })
        .collect()
}

pub(crate) fn windows() -> Vec<WindowInfo> {
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
    if !IsWindowVisible(hwnd).as_bool() {
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
    let path = exe_path(pid).unwrap_or_default();
    let exe = path.rsplit('\\').next().unwrap_or(&path).to_string();
    Some(WindowInfo { id: hwnd.0 as isize, title, exe, minimized: IsIconic(hwnd).as_bool() })
}

unsafe fn exe_path(pid: u32) -> Option<String> {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    let ok =
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
    let _ = CloseHandle(h);
    ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

fn monitor_device(handle: HMONITOR) -> Option<String> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(handle, &mut info as *mut _ as *mut MONITORINFO).as_bool() } {
        return None;
    }
    let length = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
    Some(String::from_utf16_lossy(&info.szDevice[..length]))
}

#[derive(Clone)]
pub enum Identity {
    Monitor { device: String, width: i32, height: i32 },
    Window { pid: u32, exe: String, title: String },
}

impl Identity {
    pub fn capture(source: &crate::recorder::Source) -> crate::error::Result<Self> {
        use crate::error::msg;
        use crate::recorder::Source;
        match *source {
            Source::Monitor { id } | Source::Region { monitor: id, .. } => {
                let handle = HMONITOR(id as _);
                let (rect, _) =
                    monitor_info(handle).ok_or_else(|| msg("Display disconnected. Select a display again."))?;
                let device =
                    monitor_device(handle).ok_or_else(|| msg("Display disconnected. Select a display again."))?;
                Ok(Self::Monitor { device, width: rect.right - rect.left, height: rect.bottom - rect.top })
            }
            Source::Window { id } => unsafe {
                let hwnd = HWND(id as _);
                if !IsWindow(Some(hwnd)).as_bool() {
                    return Err(msg("Window closed. Select a window again."));
                }
                if IsIconic(hwnd).as_bool() {
                    return Err(msg("Restore the selected window before capturing it."));
                }
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                let exe = exe_path(pid).ok_or_else(|| msg("Cannot verify the selected window's application."))?;
                let mut title = [0; 512];
                let count = GetWindowTextW(hwnd, &mut title);
                if count == 0 {
                    return Err(msg("Cannot verify the selected window's title."));
                }
                Ok(Self::Window { pid, exe, title: String::from_utf16_lossy(&title[..count as usize]) })
            },
        }
    }

    pub fn resolve(
        &self,
        source: &crate::recorder::Source,
    ) -> crate::error::Result<(crate::recorder::Source, Option<String>)> {
        use crate::error::msg;
        match self {
            Self::Monitor { device, width, height } => resolve_monitor(source, device, *width, *height, &monitors()),
            Self::Window { pid, exe, title } => {
                let current = Self::capture(source)?;
                if let Self::Window { pid: now_pid, exe: now_exe, title: now_title } = current {
                    if now_pid == *pid && now_exe.eq_ignore_ascii_case(exe) && now_title.starts_with(title) {
                        return Ok((source.clone(), None));
                    }
                }
                Err(msg("Selected window changed or closed. Select the window again before recording."))
            }
        }
    }
}

fn resolve_monitor(
    source: &crate::recorder::Source,
    device: &str,
    width: i32,
    height: i32,
    displays: &[MonitorInfo],
) -> crate::error::Result<(crate::recorder::Source, Option<String>)> {
    use crate::error::msg;
    use crate::recorder::Source;
    let found = displays.iter().find(|m| m.device == device);
    match (found, source) {
        (Some(display), Source::Monitor { .. }) => Ok((Source::Monitor { id: display.id }, None)),
        (Some(display), Source::Region { x, y, w, h, .. })
            if display.width == width
                && display.height == height
                && *x >= 0
                && *y >= 0
                && *w >= 2
                && *h >= 2
                && i64::from(*x) + i64::from(*w) <= i64::from(display.width)
                && i64::from(*y) + i64::from(*h) <= i64::from(display.height) =>
        {
            Ok((Source::Region { monitor: display.id, x: *x, y: *y, w: *w, h: *h }, None))
        }
        _ => {
            let primary = displays.iter().find(|m| m.primary).ok_or_else(|| msg("No monitor found"))?;
            Ok((Source::Monitor { id: primary.id }, Some("Selected display or area changed. Using the primary display; select the capture source again if needed.".into())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::Source;

    fn displays() -> Vec<MonitorInfo> {
        vec![MonitorInfo {
            id: 99,
            name: "Primary".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary: true,
            device: "display-a".into(),
        }]
    }

    #[test]
    fn monitor_handle_is_resolved_again_by_device_name() {
        let (source, warning) =
            resolve_monitor(&Source::Monitor { id: 1 }, "display-a", 1920, 1080, &displays()).unwrap();
        assert!(matches!(source, Source::Monitor { id: 99 }));
        assert!(warning.is_none());
    }

    #[test]
    fn missing_display_falls_back_to_primary_with_warning() {
        let (source, warning) =
            resolve_monitor(&Source::Monitor { id: 1 }, "unplugged", 1920, 1080, &displays()).unwrap();
        assert!(matches!(source, Source::Monitor { id: 99 }));
        assert!(warning.is_some());
    }

    #[test]
    fn region_is_rejected_after_resolution_change_or_out_of_bounds() {
        for (width, x) in [(2560, 0), (1920, 1900), (1920, i32::MAX)] {
            let area = Source::Region { monitor: 1, x, y: 0, w: 100, h: 100 };
            let (source, warning) = resolve_monitor(&area, "display-a", width, 1080, &displays()).unwrap();
            assert!(matches!(source, Source::Monitor { id: 99 }));
            assert!(warning.is_some());
        }
    }

    #[test]
    fn closed_window_is_rejected() {
        assert!(Identity::capture(&Source::Window { id: 0 }).is_err());
    }
}
