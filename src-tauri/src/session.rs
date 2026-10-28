//! One recording: WGC → GPU NV12 → IMFSinkWriter, plus WASAPI → mixer → AAC.
//! The writer thread below owns the sink writer; everything else talks to it by channel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::IMFSample;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use crate::audio::{mixer, wasapi};
use crate::capture::convert::{Converter, Nv12Pool};
use crate::capture::gpu::Gpu;
use crate::capture::wgc::{create_item, CaptureSession, Frame, Target};
use crate::clock::Clock;
use crate::encode::mf_writer::{Codec, MfWriter, VideoConfig};
use crate::error::{msg, Result, ResultContext};
use crate::queue;
use crate::util::{com_init, even, Mta};

pub struct Config {
    pub target: Target,
    /// Region mode: pixel-exact RGB crop inside the monitor. Encoded dimensions are even-aligned separately.
    pub crop: Option<RECT>,
    pub fps: u32,
    pub constant_frame_rate: bool,
    pub output_height: u32,
    pub custom_bitrate_mbps: Option<u32>,
    pub rate_control: crate::settings::RateControl,
    /// Bits per pixel per frame; bitrate scales with resolution × fps.
    pub bpp: f32,
    pub codec: Codec,
    pub cursor: bool,
    pub system_audio: bool,
    pub mic: bool,
    pub system_device: Option<String>,
    pub mic_device: Option<String>,
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
    pub duration_ms: u64,
    pub encoder_name: Option<String>,
}

#[derive(Default)]
pub struct Stats {
    pub duration_ms: AtomicU64,
    pub write_timings: Mutex<crate::encode::mf_writer::WriteTimings>,
    pub encoder_name: Mutex<Option<String>>,
    /// Frames WGC delivered (before the fps cap). Diagnostic.
    pub arrived: AtomicU64,
    pub frames: AtomicU64,
    pub dropped: AtomicU64,
    pub keepalives: AtomicU64,
    pub mixer: Arc<mixer::Stats>,
    pub writer_queue: Arc<queue::Metrics>,
    pub audio_queue: Arc<queue::Metrics>,
}

enum Msg {
    Video(Mta<IMFSample>, i64),
    Audio(Vec<i16>, i64),
}

enum Control {
    TargetClosed,
    CaptureError(String),
    Stop,
}

pub struct Handle {
    pub audio_controls: Arc<mixer::Controls>,
    tx: SyncSender<Control>,
    pub clock: Arc<Clock>,
    pub stats: Arc<Stats>,
    pub audio: Arc<Mutex<wasapi::Health>>,
    pub path: PathBuf,
}

impl Handle {
    pub fn stop(&self) {
        self.clock.finish();
        // The first pending end request wins. Never wait for the media writer or space
        // in its queue; repeated Stop clicks need no additional messages.
        let _ = self.tx.try_send(Control::Stop);
    }
}

struct Channels {
    tx: queue::Sender<Msg>,
    rx: queue::Receiver<Msg>,
    control_tx: SyncSender<Control>,
    control_rx: mpsc::Receiver<Control>,
    audio_tx: queue::Sender<wasapi::Packet>,
    audio_rx: queue::Receiver<wasapi::Packet>,
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
    let (tx, rx) = queue::bounded::<Msg>(128);
    let (audio_tx, audio_rx) = queue::bounded(64);
    let (control_tx, control_rx) = mpsc::sync_channel(1);
    let clock = Arc::new(Clock::default());
    let stats = Arc::new(Stats { writer_queue: tx.metrics(), audio_queue: audio_tx.metrics(), ..Stats::default() });
    let audio = Arc::new(Mutex::new(wasapi::Health::new(cfg.system_audio, cfg.mic)));
    let audio_controls = Arc::new(mixer::Controls::default());
    let controls = audio_controls.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<()>>(1);

    let (c, s) = (clock.clone(), stats.clone());
    let channels = Channels { tx, rx, control_tx: control_tx.clone(), control_rx, audio_tx, audio_rx };
    let health = audio.clone();
    let output_path = cfg.path.clone();
    thread::Builder::new()
        .name("oculus-writer".into())
        .spawn(move || {
            com_init();
            let path = cfg.path.clone();
            let mut ready = Some(ready_tx);
            let duration = s.clone();
            let result = run(cfg, channels, c, s, health, controls, &mut ready);
            let duration_ms = duration.duration_ms.load(Ordering::Relaxed);
            let encoder_name = duration.encoder_name.lock().unwrap().clone();
            match (ready.take(), result) {
                // Failed during setup: report to start(), no "ended" event for a session
                // that never began.
                (Some(r), Err(e)) => {
                    let _ = std::fs::remove_file(&path);
                    let _ = r.send(Err(e));
                }
                (_, Ok(reason)) => on_end(Outcome { path, reason, result: Ok(()), duration_ms, encoder_name }),
                (None, Err(e)) => {
                    // Completed fragments can still be playable after a write/finalize
                    // failure. Never delete the user's recording on a runtime error.
                    on_end(Outcome { path, reason: EndReason::User, result: Err(e), duration_ms, encoder_name })
                }
            }
        })
        .map_err(|e| msg(e.to_string()))?;

    ready_rx.recv().map_err(|_| msg("recorder thread died"))??;
    Ok(Handle { tx: control_tx, clock, stats, audio, audio_controls, path: output_path })
}

fn encoder_dims(w: i32, h: i32, codec: Codec) -> (u32, u32) {
    // Common HW encoder ceilings (H.264 level 5.1/5.2 vs HEVC). Bigger sources (5120×1440
    // ultrawide, 8K) are downscaled by the video processor in the same pass.
    let (mw, mh) = if codec == Codec::Hevc { (8192.0, 4320.0) } else { (4096.0, 2304.0) };
    let s = (mw / w as f64).min(mh / h as f64).min(1.0);
    (even((w as f64 * s) as i32) as u32, even((h as f64 * s) as i32) as u32)
}

fn requested_dims(w: i32, h: i32, codec: Codec, maximum_height: u32) -> (u32, u32) {
    let scale = if maximum_height == 0 { 1.0 } else { (maximum_height as f64 / h as f64).min(1.0) };
    encoder_dims(even((w as f64 * scale) as i32), even((h as f64 * scale) as i32), codec)
}

fn run(
    cfg: Config,
    channels: Channels,
    clock: Arc<Clock>,
    stats: Arc<Stats>,
    health: Arc<Mutex<wasapi::Health>>,
    controls: Arc<mixer::Controls>,
    ready: &mut Option<mpsc::SyncSender<Result<()>>>,
) -> Result<EndReason> {
    let Channels { tx, rx, control_tx, control_rx, audio_tx: atx, audio_rx: arx } = channels;
    let gpu = Gpu::new().context("Creating recording graphics device")?;
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
    let (out_w, out_h) = requested_dims(src_w, src_h, cfg.codec, cfg.output_height);
    let bitrate = cfg
        .custom_bitrate_mbps
        .map(|value| value.clamp(2, 150) * 1_000_000)
        .unwrap_or_else(|| (out_w as f64 * out_h as f64 * cfg.fps as f64 * cfg.bpp as f64).clamp(2e6, 150e6) as u32);
    let with_audio = cfg.system_audio || cfg.mic;

    let mut writer = MfWriter::new(
        &cfg.path,
        &gpu,
        &VideoConfig {
            width: out_w,
            height: out_h,
            fps: cfg.fps,
            constant_frame_rate: cfg.constant_frame_rate,
            bitrate,
            codec: cfg.codec,
            rate_control: cfg.rate_control,
        },
        with_audio,
    )
    .context("Setting up recording encoder")?;
    *stats.encoder_name.lock().unwrap() = writer.encoder_name.clone();

    let conv = Converter::new(&gpu, size.Width as u32, size.Height as u32, out_w, out_h, cfg.fps, cfg.crop)?;
    let pool = Nv12Pool::new(&gpu, &conv, out_w, out_h)?;
    let pipe = Arc::new(Mutex::new(Pipe { conv, pool, last: None, last_pts: -1, last_slot: -1 }));
    let interval = 10_000_000 / cfg.fps as i64;

    let capture = {
        let (pipe, clock, stats, tx) = (pipe.clone(), clock.clone(), stats.clone(), tx.clone());
        let closed_tx = control_tx.clone();
        let failures = AtomicU32::new(0);
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
                // measured 47 fps on 60-on-60 that way. On 144Hz, picking the nearest vblank per
                // slot alternates 13.9/20.8ms gaps but averages exactly `fps`.
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
                    Err(e) => return capture_failed(&control_tx, &failures, format!("Capture texture pool: {e}")),
                };
                if let Err(e) = p.conv.blit(f.texture, f.width, f.height, &frame.view) {
                    return capture_failed(&control_tx, &failures, format!("Converting captured frame: {e}"));
                }
                failures.store(0, Ordering::Relaxed);
                p.last = Some(frame.texture);
                p.last_pts = pts;
                drop(guard);
                let _ = tx.try_send(Msg::Video(Mta(frame.sample), pts));
            },
            move || {
                let _ = closed_tx.try_send(Control::TargetClosed);
            },
        )?
    };

    // ---- audio
    let stop = Arc::new(AtomicBool::new(false));
    let mut audio_threads = Vec::new();
    let mut audio_ready = Vec::new();
    if with_audio {
        if cfg.system_audio {
            let (thread, ready) = wasapi::spawn(
                wasapi::Source::System,
                cfg.system_device.clone(),
                atx.clone(),
                stop.clone(),
                health.clone(),
            );
            audio_threads.push(thread);
            audio_ready.push(ready);
        }
        if cfg.mic {
            let (thread, ready) =
                wasapi::spawn(wasapi::Source::Mic, cfg.mic_device.clone(), atx.clone(), stop.clone(), health.clone());
            audio_threads.push(thread);
            audio_ready.push(ready);
        }
        drop(atx);
        let tx = tx.clone();
        audio_threads.push(mixer::spawn(
            arx,
            clock.clone(),
            cfg.gains,
            controls,
            stop.clone(),
            stats.mixer.clone(),
            move |pcm, pts| {
                let _ = tx.try_send(Msg::Audio(pcm, pts));
            },
        ));
    }
    drop(tx);

    for ready in audio_ready {
        let _ = ready.recv();
    }

    if let Some(r) = ready.take() {
        let _ = r.send(Ok(()));
    }

    #[cfg(debug_assertions)]
    crate::selftest::inject_delay("--stall-writer-ms");

    // ---- writer loop
    let write = |m: Msg, writer: &mut MfWriter| -> Result<()> {
        match m {
            Msg::Video(s, pts) => {
                writer.write_video(&s.0, pts, interval)?;
                stats.frames.store(writer.submitted_video_frames, Ordering::Relaxed);
            }
            Msg::Audio(pcm, pts) => writer.write_audio(&pcm, pts)?,
        }
        Ok(())
    };

    let loop_result: Result<EndReason> = (|| loop {
        check_overflow(&stats)?;
        match control_rx.try_recv() {
            Ok(Control::Stop) => return Ok(EndReason::User),
            Ok(Control::TargetClosed) => return Ok(EndReason::TargetClosed),
            Ok(Control::CaptureError(error)) => return Err(msg(error)),
            Err(_) => {}
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => write(m, &mut writer)?,
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
    clock.finish();
    drop(capture);
    stop.store(true, Ordering::Relaxed);
    for t in audio_threads {
        let _ = t.join();
    }
    let mut result = loop_result;
    while let Ok(m) = rx.try_recv() {
        if let Err(error) = write(m, &mut writer) {
            if result.is_ok() {
                result = Err(error);
            }
            break;
        }
    }
    // A final audio flush can overflow after Stop was observed. Do not report a
    // successful save if any media was rejected during teardown.
    if result.is_ok() {
        if let Err(error) = check_overflow(&stats) {
            result = Err(error);
        }
    }
    if result.is_ok() {
        if let Some(end) = clock.now() {
            if let Err(error) = writer.finish_video_at(end) {
                result = Err(error);
            }
        }
    }
    let had_video = writer.has_video();
    stats.frames.store(writer.submitted_video_frames, Ordering::Relaxed);
    stats.duration_ms.store(writer.duration_ms(), Ordering::Relaxed);
    let timings = writer.timings;
    *stats.write_timings.lock().unwrap() = timings;
    let video_end = writer.video_end();
    let audio_end = writer.audio_end();
    let fin = writer.finalize();
    tracing::info!(path = %cfg.path.display(), arrived = stats.arrived.load(Ordering::Relaxed),
        encoded = stats.frames.load(Ordering::Relaxed), dropped = stats.dropped.load(Ordering::Relaxed),
        keepalives = stats.keepalives.load(Ordering::Relaxed),
        audio_corrections = stats.mixer.corrections.load(Ordering::Relaxed),
        audio_zero_filled_frames = stats.mixer.zero_filled.load(Ordering::Relaxed),
        audio_overlap_dropped_frames = stats.mixer.overlap_dropped.load(Ordering::Relaxed),
        max_writer_queue = stats.writer_queue.peak.load(Ordering::Relaxed),
        max_audio_queue = stats.audio_queue.peak.load(Ordering::Relaxed),
        rejected_writer_packets = stats.writer_queue.full.load(Ordering::Relaxed),
        rejected_audio_packets = stats.audio_queue.full.load(Ordering::Relaxed),
        video_write_max_us = timings.video_max_us, audio_write_max_us = timings.audio_max_us,
        slow_sample_writes = timings.slow_calls,
        video_end_hns = video_end, audio_end_hns = audio_end, timeline_end_hns = clock.now(),
        finalized = fin.is_ok(), "Recording session ended");
    let reason = result?;
    if !had_video {
        return Err(msg("No frames were captured"));
    }
    fin?;
    Ok(reason)
}

fn check_overflow(stats: &Stats) -> Result<()> {
    if stats.writer_queue.full.load(Ordering::Relaxed) != 0 {
        return Err(msg(
            "Recording stopped because the encoder or output disk could not keep up. The recording buffer filled.",
        ));
    }
    if stats.audio_queue.full.load(Ordering::Relaxed) != 0 {
        return Err(msg("Recording stopped because audio capture could not keep up. The audio buffer filled."));
    }
    Ok(())
}

fn capture_failed(tx: &SyncSender<Control>, failures: &AtomicU32, message: String) {
    let count = failures.fetch_add(1, Ordering::Relaxed) + 1;
    if count == 1 {
        tracing::warn!(%message, "Capture frame failed");
    }
    if count == 5 {
        tracing::error!(%message, "Stopping after five consecutive capture failures");
        let _ = tx.try_send(Control::CaptureError(message));
    }
}

fn keepalive(
    gpu: &Gpu,
    pipe: &Mutex<Pipe>,
    writer: &mut MfWriter,
    now: i64,
    interval: i64,
    stats: &Stats,
) -> Result<()> {
    let mut guard = pipe.lock().unwrap();
    let p = &mut *guard;
    let Some(last) = p.last.clone() else { return Ok(()) };
    if now - p.last_pts < KEEPALIVE {
        return Ok(());
    }
    // Copy into a fresh pooled texture rather than re-submitting the same IMFSample: the
    // encoder may still hold it, and re-stamping its time would race the MFT.
    let Some(frame) = p.pool.acquire(&p.conv)? else { return Ok(()) };
    if frame.texture != last {
        unsafe { gpu.context.CopyResource(&frame.texture, &last) };
    }
    p.last = Some(frame.texture);
    p.last_pts = now;
    drop(guard);
    writer.write_video(&frame.sample, now, interval)?;
    stats.frames.store(writer.submitted_video_frames, Ordering::Relaxed);
    stats.keepalives.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_does_not_wait_for_a_full_media_or_control_queue() {
        let (media_tx, _media_rx) = queue::bounded(1);
        media_tx.try_send(1).unwrap();
        let (control_tx, control_rx) = mpsc::sync_channel(1);
        let handle = Handle {
            audio_controls: Arc::new(mixer::Controls::default()),
            tx: control_tx,
            clock: Arc::new(Clock::default()),
            stats: Arc::new(Stats::default()),
            audio: Arc::new(Mutex::new(wasapi::Health::new(false, false))),
            path: PathBuf::new(),
        };
        handle.stop();
        handle.stop(); // Full control queue must also return immediately.
        assert!(matches!(control_rx.try_recv(), Ok(Control::Stop)));
        assert!(control_rx.try_recv().is_err());
    }

    #[test]
    fn buffer_overload_identifies_the_full_queue() {
        let stats = Stats::default();
        assert!(check_overflow(&stats).is_ok());
        stats.writer_queue.full.store(1, Ordering::Relaxed);
        assert!(check_overflow(&stats).unwrap_err().to_string().contains("output disk"));
        stats.writer_queue.full.store(0, Ordering::Relaxed);
        stats.audio_queue.full.store(1, Ordering::Relaxed);
        assert!(check_overflow(&stats).unwrap_err().to_string().contains("audio buffer"));
    }

    #[test]
    fn repeated_capture_failures_end_the_session_once() {
        let (tx, rx) = mpsc::sync_channel(1);
        let failures = AtomicU32::new(0);
        for _ in 0..4 {
            capture_failed(&tx, &failures, "device removed".into());
        }
        assert!(rx.try_recv().is_err());
        capture_failed(&tx, &failures, "device removed".into());
        assert!(matches!(rx.try_recv(), Ok(Control::CaptureError(error)) if error == "device removed"));
        capture_failed(&tx, &failures, "device removed".into());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn encoder_dimensions_fit_ultrawide_portrait_and_odd_sources() {
        assert_eq!(encoder_dims(1920, 1080, Codec::H264), (1920, 1080));
        assert_eq!(encoder_dims(5120, 1440, Codec::H264), (4096, 1152));
        assert_eq!(encoder_dims(7680, 4320, Codec::H264), (4096, 2304));
        assert_eq!(encoder_dims(2160, 3840, Codec::H264), (1296, 2304));
        assert_eq!(encoder_dims(1921, 1081, Codec::H264), (1920, 1080));
        assert_eq!(encoder_dims(7680, 4320, Codec::Hevc), (7680, 4320));
    }

    #[test]
    fn requested_output_height_scales_without_upscaling_and_stays_encoder_safe() {
        assert_eq!(requested_dims(1920, 1080, Codec::H264, 720), (1280, 720));
        assert_eq!(requested_dims(1280, 720, Codec::H264, 1080), (1280, 720));
        assert_eq!(requested_dims(5120, 1440, Codec::H264, 1080), (3840, 1080));
        let (width, height) = requested_dims(1080, 1920, Codec::Hevc, 1080);
        assert_eq!(height, 1080);
        assert_eq!(width % 2, 0);
        assert!(width > 600 && width < 610);
    }
}
