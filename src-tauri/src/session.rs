//! One recording: WGC → GPU NV12 → IMFSinkWriter, plus WASAPI → mixer → AAC.
//! The writer thread below owns the sink writer; everything else talks to it by channel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::IMFSample;

use crate::audio::{mixer, wasapi};
use crate::capture::convert::{Converter, Nv12Pool};
use crate::capture::gpu::Gpu;
use crate::capture::wgc::{create_item, CaptureSession, Frame, Target};
use crate::clock::Clock;
use crate::encode::mf_writer::{Codec, MfWriter, VideoConfig};
use crate::error::{msg, Result};
use crate::util::{com_init, even, Mta};

pub struct Config {
    pub target: Target,
    /// Region mode: physical-px rect inside the monitor (already even-aligned).
    pub crop: Option<RECT>,
    pub fps: u32,
    /// Bits per pixel per frame; bitrate scales with resolution × fps.
    pub bpp: f32,
    pub codec: Codec,
    pub cursor: bool,
    pub system_audio: bool,
    pub mic: bool,
    pub gains: mixer::Gains,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    User,
    TargetClosed,
}

pub struct Outcome {
    pub path: PathBuf,
    pub reason: EndReason,
    pub result: Result<()>,
}

#[derive(Default)]
pub struct Stats {
    /// Frames WGC delivered (before the fps cap). Diagnostic.
    pub arrived: AtomicU64,
    pub frames: AtomicU64,
    pub dropped: AtomicU64,
}

enum Msg {
    Video(Mta<IMFSample>, i64),
    Audio(Vec<i16>, i64),
    TargetClosed,
    Stop,
}

pub struct Handle {
    tx: Sender<Msg>,
    pub clock: Arc<Clock>,
    pub stats: Arc<Stats>,
}

impl Handle {
    pub fn stop(&self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

/// GPU state touched per frame. Behind one mutex because both the WGC callback (new frames)
/// and the writer thread (keep-alive repeats) acquire pool textures.
struct Pipe {
    conv: Converter,
    pool: Nv12Pool,
    last: Option<ID3D11Texture2D>,
    last_pts: i64,
    last_slot: i64,
}

/// Static screens produce zero WGC frames. Re-submit the last image at this floor so editors
/// don't choke on multi-second frame gaps.
const KEEPALIVE: i64 = 2_000_000; // 200ms → 5 fps floor

pub fn start<F>(cfg: Config, on_end: F) -> Result<Handle>
where
    F: FnOnce(Outcome) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Msg>();
    let clock = Arc::new(Clock::default());
    let stats = Arc::new(Stats::default());
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<()>>(1);

    let (t, c, s) = (tx.clone(), clock.clone(), stats.clone());
    thread::Builder::new()
        .name("oculus-writer".into())
        .spawn(move || {
            com_init();
            let path = cfg.path.clone();
            let mut ready = Some(ready_tx);
            let result = run(cfg, t, rx, c, s, &mut ready);
            match (ready.take(), result) {
                // Failed during setup: report to start(), no "ended" event for a session
                // that never began.
                (Some(r), Err(e)) => {
                    let _ = std::fs::remove_file(&path);
                    let _ = r.send(Err(e));
                }
                (_, Ok(reason)) => on_end(Outcome { path, reason, result: Ok(()) }),
                (None, Err(e)) => {
                    let _ = std::fs::remove_file(&path);
                    on_end(Outcome { path, reason: EndReason::User, result: Err(e) })
                }
            }
        })
        .map_err(|e| msg(e.to_string()))?;

    ready_rx.recv().map_err(|_| msg("recorder thread died"))??;
    Ok(Handle { tx, clock, stats })
}

fn encoder_dims(w: i32, h: i32, codec: Codec) -> (u32, u32) {
    // Common HW encoder ceilings (H.264 level 5.1/5.2 vs HEVC). Bigger sources (5120×1440
    // ultrawide, 8K) are downscaled by the video processor in the same pass.
    let (mw, mh) = if codec == Codec::Hevc { (8192.0, 4320.0) } else { (4096.0, 2304.0) };
    let s = (mw / w as f64).min(mh / h as f64).min(1.0);
    (even((w as f64 * s) as i32) as u32, even((h as f64 * s) as i32) as u32)
}

fn run(
    cfg: Config,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    clock: Arc<Clock>,
    stats: Arc<Stats>,
    ready: &mut Option<mpsc::SyncSender<Result<()>>>,
) -> Result<EndReason> {
    let gpu = Gpu::new()?;
    let target = cfg.target;
    let item = create_item(target)?;
    let size = item.Size()?;
    let (src_w, src_h) = match cfg.crop {
        Some(c) => (c.right - c.left, c.bottom - c.top),
        None => (size.Width, size.Height),
    };
    if src_w < 2 || src_h < 2 {
        return Err(msg("Capture target has no area"));
    }
    let (out_w, out_h) = encoder_dims(src_w, src_h, cfg.codec);
    let bitrate = ((out_w * out_h * cfg.fps) as f32 * cfg.bpp).clamp(2e6, 150e6) as u32;
    let with_audio = cfg.system_audio || cfg.mic;

    let mut writer = MfWriter::new(
        &cfg.path,
        &gpu,
        &VideoConfig { width: out_w, height: out_h, fps: cfg.fps, bitrate, codec: cfg.codec },
        with_audio,
    )?;

    let conv = Converter::new(&gpu, size.Width as u32, size.Height as u32, out_w, out_h, cfg.fps, cfg.crop)?;
    let pool = Nv12Pool::new(&gpu, &conv, out_w, out_h)?;
    let pipe = Arc::new(Mutex::new(Pipe { conv, pool, last: None, last_pts: -1, last_slot: -1 }));
    let interval = 10_000_000 / cfg.fps as i64;

    let capture = {
        let (pipe, clock, stats, tx) = (pipe.clone(), clock.clone(), stats.clone(), tx.clone());
        let closed_tx = tx.clone();
        CaptureSession::start(
            &gpu,
            item,
            cfg.cursor,
            move |f: Frame| {
                stats.arrived.fetch_add(1, Ordering::Relaxed);
                let Some(pts) = clock.video_pts(f.time) else { return };
                let mut guard = pipe.lock().unwrap();
                let p = &mut *guard;
                // FPS cap on a fixed grid: one frame per 1/fps slot (rounded). Comparing against
                // the previous frame instead lets one early (jittered) frame cost a whole slot:
                // measured 47 fps on 60-on-60 that way. A 144Hz monitor still yields ≤ fps.
                let slot = (pts + interval / 2) / interval;
                if p.last_slot >= 0 && slot <= p.last_slot {
                    return;
                }
                p.last_slot = slot;
                let frame = match p.pool.acquire(&p.conv) {
                    Ok(Some(fr)) => fr,
                    Ok(None) => {
                        stats.dropped.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    Err(e) => return eprintln!("[capture] pool: {e}"),
                };
                if let Err(e) = p.conv.blit(f.texture, f.width, f.height, &frame.view) {
                    return eprintln!("[capture] blit: {e}");
                }
                p.last = Some(frame.texture);
                p.last_pts = pts;
                drop(guard);
                let _ = tx.send(Msg::Video(Mta(frame.sample), pts));
            },
            move || {
                let _ = closed_tx.send(Msg::TargetClosed);
            },
        )?
    };

    // ---- audio
    let stop = Arc::new(AtomicBool::new(false));
    let mut audio_threads = Vec::new();
    if with_audio {
        let (atx, arx) = mpsc::channel();
        if cfg.system_audio {
            audio_threads.push(wasapi::spawn(wasapi::Source::System, atx.clone(), stop.clone()));
        }
        if cfg.mic {
            audio_threads.push(wasapi::spawn(wasapi::Source::Mic, atx.clone(), stop.clone()));
        }
        drop(atx);
        let tx = tx.clone();
        audio_threads.push(mixer::spawn(arx, clock.clone(), cfg.gains, stop.clone(), move |pcm, pts| {
            let _ = tx.send(Msg::Audio(pcm, pts));
        }));
    }
    drop(tx);

    if let Some(r) = ready.take() {
        let _ = r.send(Ok(()));
    }

    // ---- writer loop
    let write = |m: Msg, writer: &mut MfWriter| -> Result<Option<EndReason>> {
        match m {
            Msg::Video(s, pts) => {
                writer.write_video(&s.0, pts, interval)?;
                stats.frames.fetch_add(1, Ordering::Relaxed);
            }
            Msg::Audio(pcm, pts) => writer.write_audio(&pcm, pts)?,
            Msg::Stop => return Ok(Some(EndReason::User)),
            Msg::TargetClosed => return Ok(Some(EndReason::TargetClosed)),
        }
        Ok(None)
    };

    let loop_result: Result<EndReason> = (|| loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => {
                if let Some(reason) = write(m, &mut writer)? {
                    return Ok(reason);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(EndReason::User),
        }
        // WGC's item.Closed doesn't reliably fire on a thread without a message pump, so
        // poll too. IsWindow is a cheap user32 handle check.
        if let Target::Window(h) = target {
            if !unsafe { IsWindow(Some(HWND(h as _))) }.as_bool() {
                return Ok(EndReason::TargetClosed);
            }
        }
        if !clock.paused() {
            if let Some(now) = clock.now() {
                keepalive(&gpu, &pipe, &mut writer, now, interval, &stats)?;
            }
        }
    })();

    // ---- teardown: capture first (no new frames), then audio (mixer flushes), then drain
    drop(capture);
    stop.store(true, Ordering::Relaxed);
    for t in audio_threads {
        let _ = t.join();
    }
    while let Ok(m) = rx.try_recv() {
        if matches!(m, Msg::Video(..) | Msg::Audio(..)) {
            let _ = write(m, &mut writer);
        }
    }
    let had_video = writer.has_video();
    let fin = writer.finalize();
    let reason = loop_result?;
    if !had_video {
        return Err(msg("No frames were captured"));
    }
    fin?;
    Ok(reason)
}

fn keepalive(gpu: &Gpu, pipe: &Mutex<Pipe>, writer: &mut MfWriter, now: i64, interval: i64, stats: &Stats) -> Result<()> {
    let mut guard = pipe.lock().unwrap();
    let p = &mut *guard;
    let Some(last) = p.last.clone() else { return Ok(()) };
    if now - p.last_pts < KEEPALIVE {
        return Ok(());
    }
    // Copy into a fresh pooled texture rather than re-submitting the same IMFSample: the
    // encoder may still hold it, and re-stamping its time would race the MFT.
    let Some(frame) = p.pool.acquire(&p.conv)? else { return Ok(()) };
    unsafe { gpu.context.CopyResource(&frame.texture, &last) };
    p.last = Some(frame.texture);
    p.last_pts = now;
    drop(guard);
    writer.write_video(&frame.sample, now, interval)?;
    stats.frames.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
