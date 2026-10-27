use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Carries a COM pointer across threads. windows-rs marks most Win32 COM interfaces `!Send`
/// because it can't know their threading model; D3D11 (multithread-protected) and Media
/// Foundation objects are free-threaded, so moving them between our worker threads is sound.
pub struct Mta<T>(pub T);
unsafe impl<T> Send for Mta<T> {}
unsafe impl<T> Sync for Mta<T> {}

/// Joins the MTA on the calling thread. Every worker that touches COM calls this first;
/// RPC_E_CHANGED_MODE (thread already STA) is harmless for the free-threaded APIs we use.
pub fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// QPC in 100ns units: the shared time base of WGC `SystemRelativeTime` and WASAPI
/// `u64QPCPosition`, which is what lets audio and video sync without a separate clock.
pub fn qpc_now() -> i64 {
    let (mut c, mut f) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    (c as i128 * 10_000_000 / f as i128) as i64
}

pub fn even(v: i32) -> i32 {
    // 4:2:0 chroma is subsampled 2x2, so every encoded dimension must be even.
    (v.max(2)) & !1
}

/// "2026-10-27 21-05-33" in local time, for file names (no chrono dependency).
pub fn local_stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}
