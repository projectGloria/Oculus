//! A replay session keeps independently decodable MP4 segments in a private working
//! directory. A save pins the segments intersecting the keypress and trims both ends.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use tempfile::TempDir;
use windows::core::HSTRING;
use windows::Foundation::TimeSpan;
use windows::Media::Editing::{MediaClip, MediaComposition, MediaTrimmingPreference};
use windows::Media::MediaProperties::MediaEncodingProfile;
use windows::Media::Transcoding::TranscodeFailureReason;
use windows::Storage::StorageFile;
use windows::Win32::Media::MediaFoundation::IMFSample;

use crate::capture::gpu::Gpu;
use crate::encode::mf_writer::{MfWriter, VideoConfig, AUDIO_RATE};
use crate::error::{msg, Result, ResultContext};
use crate::saved_output::SavedOutput;
use crate::util::{local_stamp, reserve_output};

const SECOND: i64 = 10_000_000;
const SEGMENT: i64 = 5 * SECOND;

struct Segment {
    path: PathBuf,
    start: i64,
    end: i64,
}
impl Drop for Segment {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Default)]
struct History {
    segments: VecDeque<Arc<Segment>>,
    finished: bool,
    requested_ends: Vec<i64>,
}

pub struct Buffer {
    directory: TempDir,
    length: i64,
    history: Mutex<History>,
    changed: Condvar,
    export: Mutex<()>,
}

impl Buffer {
    pub fn new(output: &Path, seconds: u32) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            directory: tempfile::Builder::new().prefix(".oculus-replay-").tempdir_in(output)?,
            length: i64::from(seconds) * SECOND,
            history: Mutex::default(),
            changed: Condvar::new(),
            export: Mutex::default(),
        }))
    }

    pub fn directory(&self) -> &Path {
        self.directory.path()
    }

    fn publish(&self, path: PathBuf, start: i64, end: i64) {
        let mut history = self.history.lock().unwrap();
        let oldest = history.requested_ends.iter().copied().min().unwrap_or(end).min(end) - self.length;
        history.segments.retain(|s| s.end > oldest);
        history.segments.push_back(Arc::new(Segment { path, start, end }));
        self.changed.notify_all();
    }

    pub fn finish(&self) {
        self.history.lock().unwrap().finished = true;
        self.changed.notify_all();
    }

    fn should_rotate(&self, start: i64, now: i64) -> bool {
        now - start >= SEGMENT
            || self.history.lock().unwrap().requested_ends.iter().any(|&end| start < end && end <= now)
    }

    pub fn save(&self, end: i64) -> Result<SavedOutput> {
        let start = (end - self.length).max(0);
        let mut h = self.history.lock().unwrap();
        h.requested_ends.push(end);
        let mut h =
            self.changed.wait_while(h, |h| !h.finished && h.segments.back().is_none_or(|s| s.end < end)).unwrap();
        let request = h.requested_ends.iter().position(|&value| value == end).unwrap();
        h.requested_ends.swap_remove(request);
        let clips: Vec<_> = h.segments.iter().filter(|s| s.end > start && s.start < end).cloned().collect();
        drop(h);
        if clips.first().is_none_or(|s| s.start > start) || clips.last().is_none_or(|s| s.end < end) {
            return Err(msg("The capture ended before the requested replay was available"));
        }
        // Pin the interval before queuing behind another export.
        let _export = self.export.lock().unwrap();
        let (path, file) =
            reserve_output(self.directory().parent().unwrap(), &format!("Oculus Replay {}", local_stamp()), "mp4")?;
        drop(file);
        if let Err(error) = render(&clips, start, end, &path) {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(SavedOutput {
            path: path.to_string_lossy().into_owned(),
            kind: "recording",
            duration_ms: Some(((end - start) / 10_000) as u64),
            complete: true,
        })
    }
}

fn storage_file(path: &Path) -> Result<StorageFile> {
    Ok(StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.join()?)
}

fn render(segments: &[Arc<Segment>], start: i64, end: i64, path: &Path) -> Result<()> {
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
    let result = render_initialized(segments, start, end, path);
    unsafe { RoUninitialize() };
    result
}

fn render_initialized(segments: &[Arc<Segment>], start: i64, end: i64, path: &Path) -> Result<()> {
    let composition = MediaComposition::new()?;
    let clips = composition.Clips()?;
    let profile = MediaEncodingProfile::CreateFromFileAsync(&storage_file(&segments[0].path)?)?
        .join()
        .context("Reading replay encoding profile")?;
    for segment in segments {
        let clip = MediaClip::CreateFromFileAsync(&storage_file(&segment.path)?)?
            .join()
            .context("Opening a buffered replay segment")?;
        let duration = clip.OriginalDuration()?.Duration;
        let from = (start - segment.start).max(0);
        let until = (segment.end.min(end) - segment.start).min(duration);
        if until > from {
            clip.SetTrimTimeFromStart(TimeSpan { Duration: from })?;
            clip.SetTrimTimeFromEnd(TimeSpan { Duration: duration - until })?;
            clips.Append(&clip)?;
        }
    }
    let reason = composition
        .RenderToFileWithProfileAsync(&storage_file(path)?, MediaTrimmingPreference::Precise, &profile)
        .context("Starting replay export")?
        .join()
        .context("Rendering replay")?;
    if reason != TranscodeFailureReason::None {
        return Err(msg(format!("Windows could not export the replay ({reason:?})")));
    }
    Ok(())
}

// The mixer trails video. Keep the preceding writer until its final PCM packet
// arrives and split straddling packets on a stereo sample boundary.
struct Previous {
    writer: MfWriter,
    path: PathBuf,
    start: i64,
}

pub struct Writer<'a> {
    pub current: MfWriter,
    path: PathBuf,
    start: i64,
    previous: Option<Previous>,
    completed_frames: u64,
    buffer: Option<Arc<Buffer>>,
    gpu: &'a Gpu,
    config: VideoConfig,
    audio: bool,
}

impl<'a> Writer<'a> {
    pub fn new(
        path: &Path,
        gpu: &'a Gpu,
        config: VideoConfig,
        audio: bool,
        buffer: Option<Arc<Buffer>>,
    ) -> Result<Self> {
        Ok(Self {
            current: MfWriter::new(path, gpu, &config, audio)?,
            gpu,
            config,
            audio,
            buffer,
            path: path.to_owned(),
            start: 0,
            previous: None,
            completed_frames: 0,
        })
    }

    pub fn frames(&self) -> u64 {
        self.completed_frames + self.current.submitted_video_frames
    }

    fn close_previous(&mut self) -> Result<()> {
        if let Some(p) = self.previous.take() {
            p.writer.finalize()?;
            self.buffer.as_ref().unwrap().publish(p.path, p.start, self.start);
        }
        Ok(())
    }

    pub fn write_video(&mut self, sample: &IMFSample, pts: i64, duration: i64) -> Result<()> {
        if self.current.has_video() && self.buffer.as_ref().is_some_and(|b| b.should_rotate(self.start, pts)) {
            self.close_previous()?;
            let path = self.buffer.as_ref().unwrap().directory().join(format!("{pts}.mp4"));
            self.current.finish_video_at(pts - self.start)?;
            self.completed_frames += self.current.submitted_video_frames;
            let next = MfWriter::new(&path, self.gpu, &self.config, self.audio)?;
            self.previous = Some(Previous {
                writer: std::mem::replace(&mut self.current, next),
                path: std::mem::replace(&mut self.path, path),
                start: std::mem::replace(&mut self.start, pts),
            });
            if !self.audio {
                self.close_previous()?;
            }
        }
        self.current.write_video(sample, pts - self.start, duration)
    }

    pub fn write_audio(&mut self, pcm: &[i16], pts: i64) -> Result<()> {
        let mut split = 0;
        if let Some(p) = &mut self.previous {
            split = (((self.start - pts).max(0) * i64::from(AUDIO_RATE) / SECOND) as usize * 2).min(pcm.len());
            p.writer.write_audio(&pcm[..split], pts - p.start)?;
            if split < pcm.len() {
                self.close_previous()?;
            }
        }
        self.current.write_audio(&pcm[split..], (pts - self.start).max(0))
    }

    pub fn finish_video_at(&mut self, end: i64) -> Result<()> {
        self.current.finish_video_at(end - self.start)
    }

    pub fn finalize(mut self) -> Result<()> {
        self.close_previous()?;
        let end = self.start + self.current.video_end();
        self.current.finalize()?;
        if let Some(b) = self.buffer {
            b.publish(self.path, self.start, end);
        }
        Ok(())
    }
}
