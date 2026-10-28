//! Headless pipeline check (debug builds only): `oculus --selftest <out.mp4> [flags]`.
//! Runs the exact session path the UI uses, minus Tauri, so A/V sync, pause math and
//! keep-alive can be verified with ffprobe before any UI exists.
//!
//! Flags: --secs N  --pause A,B (pause at A s for B s)  --sys  --mic  --fps N
//!        --hevc  --window <title substring>  --region x,y,w,h  --shot <out.png>
//!        --kill-at T (hard exit after T wall seconds, without finalizing)
//!        --stall-writer-ms N / --stall-mixer-ms N (one-time backpressure injection)

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::audio::mixer::Gains;
use crate::capture::wgc::Target;
use crate::encode::mf_writer::Codec;
use crate::session::{self, Config};
use crate::sources;
use crate::util::com_init;
use windows::Win32::Foundation::RECT;

pub fn inject_delay(flag: &str) {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|arg| arg == "--selftest") {
        return;
    }
    if let Some(ms) = args
        .iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(ms.min(60_000)));
    }
}

/// Exercise the real Recorder state machine, including its startup and quit callbacks.
/// `--selftest-app <temporary output directory> [--cancel-start] [--finalize-delay-ms N]`
/// changes settings in memory only and exits automatically. No input injection is used.
pub fn install_app_driver(app: &tauri::AppHandle) {
    use crate::recorder::Recorder;
    use tauri::Manager;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("--selftest-app") {
        return;
    }
    let Some(output) = args.get(1) else {
        app.exit(2);
        return;
    };
    app.state::<crate::settings::SettingsStore>().configure_selftest(PathBuf::from(output));
    let cancel = args.iter().any(|a| a == "--cancel-start");
    let app = app.clone();
    std::thread::spawn(move || {
        let recorder = app.state::<Recorder>();
        if cancel {
            let a = app.clone();
            let starter = std::thread::spawn(move || a.state::<Recorder>().start(&a));
            let deadline = Instant::now() + Duration::from_secs(15);
            while !recorder.is_active() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            let before = Instant::now();
            recorder.stop(&app);
            let stop_ms = before.elapsed().as_millis();
            println!("stop during startup returned in {stop_ms}ms");
            if stop_ms > 100 || !matches!(starter.join(), Ok(Ok(()))) {
                eprintln!("startup cancellation failed");
                app.exit(1);
                return;
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            while recorder.is_active() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if recorder.is_active() {
                eprintln!("cancelled startup never finished");
                app.exit(1);
                return;
            }
            println!("startup cancellation completed");
        } else {
            if let Err(e) = recorder.start(&app) {
                eprintln!("app recording failed: {e}");
                app.exit(1);
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
            println!("requesting quit with an active recording");
        }
        recorder.request_exit(&app);
    });
}

pub fn run(args: &[String]) -> i32 {
    com_init();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let has = |name: &str| args.iter().any(|a| a == name);
    if has("--panic") {
        panic!("Oculus selftest panic hook");
    }

    // Input injection for driving the real app end-to-end: hotkeys go through the actual
    // RegisterHotKey path, drags through the actual overlay wndproc.
    if let Some(k) = flag("--keys") {
        send_hotkey(k.as_bytes()[0]);
        return 0;
    }
    if let Some(d) = flag("--drag") {
        let v: Vec<i32> = d.split(',').map(|x| x.trim().parse().unwrap()).collect();
        drag(v[0], v[1], v[2], v[3]);
        return 0;
    }
    if let Some(p) = flag("--cursor") {
        let v: Vec<i32> = p.split(',').map(|x| x.trim().parse().unwrap()).collect();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetCursorPos(v[0], v[1]);
        }
        return 0;
    }
    if let Some(t) = flag("--close") {
        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        for w in sources::list().windows.iter().filter(|w| w.title == t) {
            let _ = unsafe {
                PostMessageW(
                    Some(windows::Win32::Foundation::HWND(w.id as _)),
                    WM_CLOSE,
                    Default::default(),
                    Default::default(),
                )
            };
            println!("closed {}", w.title);
        }
        return 0;
    }
    if has("--mem") {
        mem_report();
        return 0;
    }
    if has("--list") {
        for m in sources::list().monitors {
            println!("{} at {},{} {}x{} primary={}", m.name, m.x, m.y, m.width, m.height, m.primary);
        }
        return 0;
    }

    let Some(out) = args.first().cloned() else {
        eprintln!("usage: oculus --selftest <out.mp4> [--secs N] [--pause A,B] [--sys] [--mic] [--fps N] [--hevc] [--window TITLE] [--region x,y,w,h] [--shot out.png]");
        return 2;
    };
    let src = sources::list();
    let target = match flag("--window") {
        Some(t) => match src.windows.iter().find(|w| w.title.to_lowercase().contains(&t.to_lowercase())) {
            Some(w) => {
                println!("window: {} ({})", w.title, w.exe);
                Target::Window(w.id)
            }
            None => {
                eprintln!("no window matching {t:?}; have:");
                for w in &src.windows {
                    eprintln!("  {:?} ({})", w.title, w.exe);
                }
                return 1;
            }
        },
        None => Target::Monitor(sources::primary_monitor().expect("monitor")),
    };
    let crop = flag("--region").map(|r| {
        let v: Vec<i32> = r.split(',').map(|x| x.trim().parse().unwrap()).collect();
        RECT { left: v[0], top: v[1], right: v[0] + v[2], bottom: v[1] + v[3] }
    });

    if let Some(shot) = flag("--shot") {
        let p = PathBuf::from(shot);
        return match crate::screenshot::capture(target, crop, true, p.parent().unwrap(), false) {
            Ok(saved) => {
                println!("screenshot: {}", saved.display());
                0
            }
            Err(e) => {
                eprintln!("screenshot failed: {e}");
                1
            }
        };
    }

    let secs: f64 = flag("--secs").and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let kill_at = flag("--kill-at").and_then(|s| s.parse::<f64>().ok());
    let pause: Option<(f64, f64)> = flag("--pause").map(|p| {
        let v: Vec<f64> = p.split(',').map(|x| x.parse().unwrap()).collect();
        (v[0], v[1])
    });
    let cfg = Config {
        target,
        crop,
        fps: flag("--fps").and_then(|s| s.parse().ok()).unwrap_or(60),
        constant_frame_rate: !has("--vfr"),
        output_height: flag("--height").and_then(|s| s.parse().ok()).unwrap_or(0),
        custom_bitrate_mbps: flag("--bitrate").and_then(|s| s.parse().ok()),
        rate_control: if has("--cbr") {
            crate::settings::RateControl::Cbr
        } else if has("--vbr") {
            crate::settings::RateControl::Vbr
        } else {
            crate::settings::RateControl::Default
        },
        bpp: 0.08,
        codec: if has("--hevc") { Codec::Hevc } else { Codec::H264 },
        cursor: true,
        system_audio: has("--sys"),
        mic: has("--mic"),
        system_device: None,
        mic_device: None,
        gains: Gains { system: 1.0, mic: 1.0 },
        path: PathBuf::from(&out),
    };

    let (tx, rx) = mpsc::channel();
    let t0 = Instant::now();
    let handle = match session::start(cfg, move |o| {
        let _ = tx.send(o);
    }) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("start failed: {e}");
            return 1;
        }
    };
    println!("recording {secs}s -> {out} (setup {:?})", t0.elapsed());

    let started = Instant::now();
    // --resize-at T,W,H: resize the target window mid-recording (letterbox check).
    let mut resize: Option<(f64, i32, i32)> = flag("--resize-at").map(|r| {
        let v: Vec<f64> = r.split(',').map(|x| x.parse().unwrap()).collect();
        (v[0], v[1] as i32, v[2] as i32)
    });
    let mut paused_at: Option<Instant> = None;
    let mut ended = None;
    while started.elapsed().as_secs_f64() < secs {
        if kill_at.is_some_and(|t| started.elapsed().as_secs_f64() >= t) {
            // No unwinding, stop message or Finalize: reproduce Task Manager termination.
            std::process::exit(86);
        }
        if let Ok(o) = rx.recv_timeout(Duration::from_millis(20)) {
            ended = Some(o);
            break;
        }
        if let (Some((at, w, h)), Target::Window(hwnd)) = (resize, target) {
            if started.elapsed().as_secs_f64() >= at {
                use windows::Win32::UI::WindowsAndMessaging::MoveWindow;
                let _ = unsafe { MoveWindow(windows::Win32::Foundation::HWND(hwnd as _), 100, 100, w, h, true) };
                println!("resized to {w}x{h}");
                resize = None;
            }
        }
        if let Some((a, b)) = pause {
            let t = started.elapsed().as_secs_f64();
            if paused_at.is_none() && t >= a && t < a + b {
                handle.clock.pause();
                paused_at = Some(Instant::now());
                println!("paused at {t:.2}s");
            } else if paused_at.is_some() && t >= a + b && handle.clock.paused() {
                handle.clock.resume();
                println!("resumed at {t:.2}s");
            }
        }
    }
    handle.stop();
    let o = match ended {
        Some(o) => o,
        None => rx.recv().expect("session outcome"),
    };
    let f = handle.stats.frames.load(std::sync::atomic::Ordering::Relaxed);
    let d = handle.stats.dropped.load(std::sync::atomic::Ordering::Relaxed);
    println!(
        "queues: writer peak {}, audio peak {}, rejected writer {}, audio {}",
        handle.stats.writer_queue.peak.load(std::sync::atomic::Ordering::Relaxed),
        handle.stats.audio_queue.peak.load(std::sync::atomic::Ordering::Relaxed),
        handle.stats.writer_queue.full.load(std::sync::atomic::Ordering::Relaxed),
        handle.stats.audio_queue.full.load(std::sync::atomic::Ordering::Relaxed)
    );
    let timings = *handle.stats.write_timings.lock().unwrap();
    println!("timeline: video end {}ms, clock {:?}ms", o.duration_ms, handle.clock.now().map(|pts| pts / 10_000));
    println!(
        "sample writes: video max {}us, audio max {}us, slow calls {}",
        timings.video_max_us, timings.audio_max_us, timings.slow_calls
    );
    match o.result {
        Ok(()) => {
            let wall = started.elapsed().as_secs_f64();
            println!("ok: {} frames, {} dropped, reason {:?}, wall {:.2}s", f, d, o.reason, wall);
            println!("wgc delivered {} frames", handle.stats.arrived.load(std::sync::atomic::Ordering::Relaxed));
            println!("cpu: {:.1}% of one core (whole process, incl. setup)", cpu_secs() / wall * 100.0);
            0
        }
        Err(e) => {
            eprintln!("failed: {e}");
            1
        }
    }
}

fn key(vk: u16, up: bool) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                ..Default::default()
            },
        },
    }
}

/// Ctrl+Shift+<letter>, exactly as a user would press it.
fn send_hotkey(letter: u8) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let (c, s, k) = (VK_CONTROL.0, VK_SHIFT.0, letter.to_ascii_uppercase() as u16);
    let seq = [key(c, false), key(s, false), key(k, false), key(k, true), key(s, true), key(c, true)];
    unsafe { SendInput(&seq, std::mem::size_of::<INPUT>() as i32) };
}

fn mouse(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let i =
        INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: flags, ..Default::default() } } };
    unsafe { SendInput(&[i], std::mem::size_of::<INPUT>() as i32) };
}

/// Left-drag between two physical screen points, in steps so the overlay sees WM_MOUSEMOVE.
fn drag(x1: i32, y1: i32, x2: i32, y2: i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
    unsafe {
        let _ = SetCursorPos(x1, y1);
        std::thread::sleep(Duration::from_millis(50));
        mouse(MOUSEEVENTF_LEFTDOWN);
        for i in 1..=20 {
            let _ = SetCursorPos(x1 + (x2 - x1) * i / 20, y1 + (y2 - y1) * i / 20);
            mouse(MOUSEEVENTF_MOVE);
            std::thread::sleep(Duration::from_millis(15));
        }
        mouse(MOUSEEVENTF_LEFTUP);
    }
}

/// Working set + private bytes of every running oculus.exe (except us) and all descendants
/// (the WebView2 browser/GPU/renderer processes).
fn mem_report() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let mut all = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).unwrap();
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            all.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let me = unsafe { GetCurrentProcessId() };
    let mut set: Vec<u32> =
        all.iter().filter(|p| p.2.eq_ignore_ascii_case("oculus.exe") && p.0 != me).map(|p| p.0).collect();
    loop {
        let more: Vec<u32> = all.iter().filter(|p| set.contains(&p.1) && !set.contains(&p.0)).map(|p| p.0).collect();
        if more.is_empty() {
            break;
        }
        set.extend(more);
    }
    let (mut ws, mut pv) = (0usize, 0usize);
    for pid in &set {
        let name = &all.iter().find(|p| p.0 == *pid).unwrap().2;
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, *pid) else { continue };
            let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
            if GetProcessMemoryInfo(
                h,
                &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
            .is_ok()
            {
                println!(
                    "  {name:<22} ws {:>6.1} MB  private {:>6.1} MB",
                    c.WorkingSetSize as f64 / 1048576.0,
                    c.PrivateUsage as f64 / 1048576.0
                );
                ws += c.WorkingSetSize;
                pv += c.PrivateUsage;
            }
            let _ = CloseHandle(h);
        }
    }
    println!("TOTAL {} procs: ws {:.1} MB, private {:.1} MB", set.len(), ws as f64 / 1048576.0, pv as f64 / 1048576.0);
}

fn cpu_secs() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
    t(k) + t(u)
}
