use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Carries a COM pointer across threads. windows-rs marks most Win32 COM interfaces `!Send`
/// because it can't know their threading model; D3D11 (multithread-protected) and Media
/// Foundation objects are free-threaded, so moving them between our worker threads is sound.
pub struct Mta<T>(pub T);
// MF samples move through the writer queue or a mutex-protected tracked-sample pool.
unsafe impl Send for Mta<windows::Win32::Media::MediaFoundation::IMFSample> {}
// This WinRT projection refers to our multithread-protected D3D11 device. WGC's
// free-threaded callback uses it only to recreate that device's frame pool.
unsafe impl Send for Mta<windows::Graphics::DirectX::Direct3D11::IDirect3DDevice> {}
// D3D11 resource handles are free-threaded; callers serialize immediate-context work.
unsafe impl Send for Mta<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D> {}
// Region selection transfers its frozen texture together with the owning protected device.
unsafe impl Send for Mta<(crate::capture::gpu::Gpu, windows::Win32::Graphics::Direct3D11::ID3D11Texture2D)> {}

pub struct ComScope(bool);
impl ComScope {
    pub fn enter() -> crate::error::Result<Self> {
        let status = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if status.is_ok() {
            return Ok(Self(true));
        }
        if status == windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Ok(Self(false));
        }
        Err(windows::core::Error::from_hresult(status).into())
    }
}
impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

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
    format!("{:04}-{:02}-{:02} {:02}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Reserve a new output atomically, including across concurrent saves or app instances.
/// Keep the placeholder until the caller writes it; checking `exists()` would race.
pub fn reserve_output(dir: &Path, stem: &str, extension: &str) -> io::Result<(PathBuf, File)> {
    for number in 1u64.. {
        let name = if number == 1 { format!("{stem}.{extension}") } else { format!("{stem} ({number}).{extension}") };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("Output filename suffix exhausted"))
}

pub fn available_bytes(path: &Path) -> windows::core::Result<u64> {
    let path = windows::core::HSTRING::from(path.as_os_str());
    let mut available = 0;
    unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(&path, Some(&mut available), None, None)?;
    }
    Ok(available)
}

pub const DISK_WARN_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DISK_STOP_BYTES: u64 = 200 * 1024 * 1024;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct TestDir(pub PathBuf);

    impl TestDir {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "oculus-output-test-{}-{}-{}",
                std::process::id(),
                qpc_now(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn same_second_saves_preserve_existing_outputs() {
        let dir = TestDir::new();
        for extension in ["png", "mp4"] {
            let stem = "Oculus 2026-10-27 21-05-33";
            let original = dir.0.join(format!("{stem}.{extension}"));
            std::fs::write(&original, b"original capture").unwrap();
            for number in 2..=3 {
                let (path, file) = reserve_output(&dir.0, stem, extension).unwrap();
                assert_eq!(path, dir.0.join(format!("{stem} ({number}).{extension}")));
                drop(file);
            }
            assert_eq!(std::fs::read(original).unwrap(), b"original capture");
        }
    }

    #[test]
    fn concurrent_saves_reserve_distinct_files() {
        let dir = TestDir::new();
        let paths = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..16)
                .map(|_| {
                    let dir = &dir.0;
                    scope.spawn(move || {
                        let (path, file) = reserve_output(dir, "Oculus same second", "png").unwrap();
                        drop(file);
                        path
                    })
                })
                .collect();
            threads.into_iter().map(|t| t.join().unwrap()).collect::<std::collections::HashSet<_>>()
        });
        assert_eq!(paths.len(), 16);
        assert!(paths.iter().all(|p| p.is_file()));
        assert!(paths.contains(&dir.0.join("Oculus same second.png")));
    }

    #[test]
    fn reservation_reports_missing_directory() {
        let dir = TestDir::new();
        let error = reserve_output(&dir.0.join("missing"), "Oculus", "mp4").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
