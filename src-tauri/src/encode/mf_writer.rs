use std::path::Path;
use std::time::Instant;

use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoTaskMemFree, IPersist};
use windows::Win32::System::Variant::VARIANT;

use crate::capture::convert::{Converter, Nv12Pool};
use crate::capture::gpu::Gpu;
use crate::error::{Result, ResultContext};

pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub constant_frame_rate: bool,
    pub bitrate: u32,
    pub codec: Codec,
    pub rate_control: crate::settings::RateControl,
}

#[derive(Clone, Copy, Default)]
pub struct WriteTimings {
    pub video_max_us: u64,
    pub audio_max_us: u64,
    pub slow_calls: u64,
}

/// Integer frame grid: no per-frame rounding error accumulates over long recordings.
struct FrameGrid {
    fps: u32,
    next: u64,
}

impl FrameGrid {
    fn new(fps: u32) -> Self {
        Self { fps, next: 0 }
    }
    fn nearest_slot(&self, pts: i64) -> u64 {
        ((pts.max(0) as u128 * self.fps as u128 + 5_000_000) / 10_000_000) as u64
    }
    fn ceil_slot(&self, pts: i64) -> u64 {
        (pts.max(0) as u128 * self.fps as u128).div_ceil(10_000_000) as u64
    }
    fn take(&mut self) -> (i64, i64) {
        let start = (self.next as u128 * 10_000_000 / self.fps as u128) as i64;
        self.next += 1;
        let end = (self.next as u128 * 10_000_000 / self.fps as u128) as i64;
        (start, end - start)
    }
}

/// Two-stream MP4 writer: stream 0 video (HW H.264/HEVC fed with D3D11 NV12 surfaces),
/// stream 1 AAC (fed with PCM16). Owned by exactly one thread (the session writer loop).
pub struct MfWriter {
    writer: IMFSinkWriter,
    _manager: IMFDXGIDeviceManager,
    video: u32,
    audio: Option<u32>,
    last_video: i64,
    pending_video: Option<(IMFSample, i64, i64)>,
    frame_grid: Option<FrameGrid>,
    repeats: Option<(Gpu, Converter, Nv12Pool)>,
    last_audio: i64,
    video_end: i64,
    audio_end: i64,
    pub encoder_name: Option<String>,
    pub timings: WriteTimings,
    pub submitted_video_frames: u64,
    // Declared last: COM objects are released before the matching MFShutdown call.
    _runtime: MfRuntime,
}

struct MfRuntime;

impl MfRuntime {
    fn new() -> Result<Self> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        }
        Ok(Self)
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

unsafe impl Send for MfWriter {}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl MfWriter {
    pub fn new(path: &Path, gpu: &Gpu, v: &VideoConfig, with_audio: bool) -> Result<Self> {
        if v.fps == 0 || v.fps > 360 {
            return Err(crate::error::msg("Frame rate must be between 1 and 360 fps."));
        }
        if v.codec == Codec::H264 && !v.constant_frame_rate {
            return Err(crate::error::msg("H.264 requires constant frame rate to preserve fragmented MP4 timing."));
        }
        unsafe {
            let runtime = MfRuntime::new()?;

            // Hand our device to MF so the HW encoder MFT reads our NV12 textures in place.
            let (mut token, mut manager) = (0u32, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.unwrap();
            manager.ResetDevice(&gpu.device, token)?;

            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 4)?;
            let attrs = attrs.unwrap();
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
            // H.264 fragments survive a process kill without Finalize(). The Windows
            // fragmented sink rejects HEVC (MF_E_INVALIDMEDIATYPE); keep HEVC working
            // through the regular sink until a crash-safe HEVC muxer is available.
            attrs.SetGUID(
                &MF_TRANSCODE_CONTAINERTYPE,
                if v.codec == Codec::Hevc { &MFTranscodeContainerType_MPEG4 } else { &MFTranscodeContainerType_FMPEG4 },
            )?;
            // Throttling stays on: if the encoder falls behind, WriteSample blocks, the NV12
            // pool drains and frames get dropped (bounded memory) instead of queueing forever.

            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)?;

            // ---- video
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(
                &MF_MT_SUBTYPE,
                if v.codec == Codec::Hevc { &MFVideoFormat_HEVC } else { &MFVideoFormat_H264 },
            )?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, v.bitrate)?;
            if v.codec == Codec::H264 {
                out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            }
            set_video_common(&out, v)?;
            let video = writer.AddStream(&out)?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            set_video_common(&input, v)?;
            writer.SetInputMediaType(video, &input, None).context("Configuring video encoder input")?;

            // ---- audio
            let audio = if with_audio {
                let out = MFCreateMediaType()?;
                out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
                out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
                // 192 kbps; the MS AAC encoder only accepts 96/128/160/192.
                out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?;
                let idx = writer.AddStream(&out)?;

                let input = MFCreateMediaType()?;
                input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_RATE)?;
                input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
                input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2 * AUDIO_CHANNELS)?;
                input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 2 * AUDIO_CHANNELS * AUDIO_RATE)?;
                writer.SetInputMediaType(idx, &input, None)?;
                Some(idx)
            } else {
                None
            };

            configure_encoder(&writer, video, v)?;
            writer.BeginWriting().context("Starting media writer")?;
            let encoder_name = actual_encoder_name(&writer, video, v.codec);
            tracing::info!(
                encoder = encoder_name.as_deref().unwrap_or("name not exposed by encoder"),
                "Video encoder selected"
            );
            let repeats = if v.constant_frame_rate {
                let converter = Converter::new(gpu, v.width, v.height, v.width, v.height, v.fps, None)?;
                let pool = Nv12Pool::new(gpu, &converter, v.width, v.height)?;
                Some((gpu.clone(), converter, pool))
            } else {
                None
            };
            Ok(Self {
                writer,
                _manager: manager,
                video,
                audio,
                last_video: -1,
                pending_video: None,
                frame_grid: v.constant_frame_rate.then(|| FrameGrid::new(v.fps)),
                repeats,
                last_audio: -1,
                video_end: 0,
                audio_end: 0,
                encoder_name,
                timings: WriteTimings::default(),
                submitted_video_frames: 0,
                _runtime: runtime,
            })
        }
    }

    pub fn write_video(&mut self, sample: &IMFSample, pts: i64, duration: i64) -> Result<()> {
        // MP4 sink rejects non-increasing timestamps; clamp instead of failing the recording.
        let pts = pts.max(self.last_video + 1);
        // Retain one sample until the following timestamp supplies its actual VFR duration.
        // A nominal 1/fps duration leaves holes between sparse frames; the fragmented MP4
        // sink can collapse those holes, making video run shorter than the audio timeline.
        if let Some((previous, start, _)) = self.pending_video.take() {
            if self.frame_grid.is_some() {
                self.repeat_until(&previous, pts, false)?;
            } else {
                self.submit_video(&previous, start, pts - start)?;
            }
        }
        self.pending_video = Some((sample.clone(), pts, duration));
        self.last_video = pts;
        Ok(())
    }

    fn submit_video(&mut self, sample: &IMFSample, pts: i64, duration: i64) -> Result<()> {
        unsafe {
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            let started = Instant::now();
            let result = self.writer.WriteSample(self.video, sample);
            self.record_write_time(true, started, pts);
            result?;
        }
        self.video_end = pts + duration;
        self.submitted_video_frames += 1;
        Ok(())
    }

    pub fn finish_video_at(&mut self, end: i64) -> Result<()> {
        if let Some((sample, pts, _)) = self.pending_video.take() {
            if self.frame_grid.is_some() {
                self.repeat_until(&sample, end, true)?;
            } else {
                self.submit_video(&sample, pts, (end - pts).max(1))?;
            }
        }
        Ok(())
    }

    fn repeat_until(&mut self, source: &IMFSample, end: i64, finishing: bool) -> Result<()> {
        let grid = self.frame_grid.as_mut().unwrap();
        let limit = if finishing { grid.ceil_slot(end) } else { grid.nearest_slot(end) };
        // Keep catch-up bounded if the writer/device has stalled. Normal source gaps
        // are covered by the 200ms keep-alive; a multi-second backlog must not run away.
        if limit.saturating_sub(grid.next) > grid.fps as u64 * 2 {
            return Err(crate::error::msg("Recording stopped because video fell more than two seconds behind."));
        }
        while self.frame_grid.as_ref().unwrap().next < limit {
            let (pts, duration) = self.frame_grid.as_mut().unwrap().take();
            let (gpu, converter, pool) = self.repeats.as_mut().unwrap();
            let deadline = Instant::now() + std::time::Duration::from_millis(250);
            let frame = loop {
                if let Some(frame) = pool.acquire(converter)? {
                    break frame;
                }
                if Instant::now() >= deadline {
                    return Err(crate::error::msg(
                        "Encoder retained all output surfaces; recording stopped to keep memory bounded.",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            };
            unsafe {
                let mut raw = std::ptr::null_mut();
                source.GetBufferByIndex(0)?.cast::<IMFDXGIBuffer>()?.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
                let surface = ID3D11Texture2D::from_raw(raw);
                gpu.context.CopyResource(&frame.texture, &surface);
            }
            self.submit_video(&frame.sample, pts, duration)?;
        }
        Ok(())
    }

    pub fn write_audio(&mut self, pcm: &[i16], pts: i64) -> Result<()> {
        let Some(idx) = self.audio else { return Ok(()) };
        if pcm.is_empty() {
            return Ok(());
        }
        let pts = pts.max(self.last_audio + 1);
        let bytes = std::mem::size_of_val(pcm) as u32;
        let frames = pcm.len() as i64 / AUDIO_CHANNELS as i64;
        unsafe {
            let buf = MFCreateMemoryBuffer(bytes)?;
            let mut ptr = std::ptr::null_mut();
            buf.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, ptr, bytes as usize);
            buf.Unlock()?;
            buf.SetCurrentLength(bytes)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(frames * 10_000_000 / AUDIO_RATE as i64)?;
            let started = Instant::now();
            let result = self.writer.WriteSample(idx, &sample);
            self.record_write_time(false, started, pts);
            result?;
        }
        self.last_audio = pts;
        self.audio_end = pts + frames * 10_000_000 / AUDIO_RATE as i64;
        Ok(())
    }

    pub fn has_video(&self) -> bool {
        self.last_video >= 0
    }

    pub fn video_end(&self) -> i64 {
        self.video_end
    }

    pub fn audio_end(&self) -> i64 {
        self.audio_end
    }

    fn record_write_time(&mut self, video: bool, started: Instant, pts: i64) {
        let elapsed_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let maximum = if video { &mut self.timings.video_max_us } else { &mut self.timings.audio_max_us };
        *maximum = (*maximum).max(elapsed_us);
        if elapsed_us >= 100_000 {
            self.timings.slow_calls += 1;
            tracing::warn!(
                stream = if video { "video" } else { "audio" },
                elapsed_us,
                pts,
                video_end = self.video_end,
                audio_end = self.audio_end,
                "Slow Media Foundation sample write"
            );
        }
    }

    pub fn duration_ms(&self) -> u64 {
        self.video_end.max(0) as u64 / 10_000
    }

    fn pad_audio_to_video(&mut self) -> Result<()> {
        if self.audio.is_none() {
            return Ok(());
        }
        loop {
            let frames = padding_frames(self.audio_end, self.video_end);
            if frames == 0 {
                return Ok(());
            }
            self.write_audio(&vec![0; frames * AUDIO_CHANNELS as usize], self.audio_end)?;
        }
    }

    pub fn finalize(mut self) -> Result<()> {
        #[cfg(debug_assertions)]
        {
            let args: Vec<String> = std::env::args().collect();
            if args.iter().any(|a| a == "--selftest-app") {
                if let Some(delay) = args
                    .iter()
                    .position(|a| a == "--finalize-delay-ms")
                    .and_then(|i| args.get(i + 1))
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                }
            }
        }
        // Error paths may not have reached finish_video_at; retain the final accepted frame
        // using its nominal duration before attempting to finalize the partial recording.
        let pending = if let Some((sample, pts, duration)) = self.pending_video.take() {
            if self.frame_grid.is_some() {
                self.repeat_until(&sample, pts + duration, true)
            } else {
                self.submit_video(&sample, pts, duration)
            }
        } else {
            Ok(())
        };
        let padding = self.pad_audio_to_video();
        // Still attempt to finalize existing samples if tail padding fails.
        let r = unsafe { self.writer.Finalize() };
        pending?;
        padding?;
        Ok(r?)
    }
}

fn configure_encoder(writer: &IMFSinkWriter, video: u32, config: &VideoConfig) -> Result<()> {
    use crate::settings::RateControl;
    unsafe {
        let mut raw = std::ptr::null_mut();
        if writer.GetServiceForStream(video, &windows::core::GUID::zeroed(), &ICodecAPI::IID, &mut raw).is_err()
            || raw.is_null()
        {
            if config.rate_control != RateControl::Default {
                return Err(crate::error::msg(
                    "This encoder does not expose rate-control settings. Choose Encoder default.",
                ));
            }
            tracing::warn!("Encoder does not expose GOP controls; using encoder defaults");
            return Ok(());
        }
        let codec = ICodecAPI::from_raw(raw);
        if config.rate_control != RateControl::Default {
            let mode = if config.rate_control == RateControl::Cbr {
                eAVEncCommonRateControlMode_CBR
            } else {
                eAVEncCommonRateControlMode_PeakConstrainedVBR
            };
            codec
                .SetValue(&CODECAPI_AVEncCommonRateControlMode, &VARIANT::from(mode.0 as u32))
                .context("Selected encoder does not support the requested rate-control mode; choose Encoder default")?;
            codec
                .SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(config.bitrate))
                .context("Configuring encoder bitrate")?;
            if config.rate_control == RateControl::Vbr {
                codec
                    .SetValue(
                        &CODECAPI_AVEncCommonMaxBitRate,
                        &VARIANT::from(config.bitrate.saturating_mul(2).min(150_000_000)),
                    )
                    .context("Configuring encoder peak bitrate")?;
            }
        }
        let gop = config.fps.saturating_mul(2);
        match codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(gop)) {
            Ok(()) => tracing::info!(gop_frames = gop, "Configured two-second keyframe interval"),
            Err(error) => tracing::warn!(%error, "Encoder rejected GOP interval; keeping vendor default"),
        }
        Ok(())
    }
}

unsafe fn attribute_name(attributes: &IMFAttributes) -> Option<String> {
    attribute_string(attributes, &MFT_FRIENDLY_NAME_Attribute)
}

unsafe fn attribute_string(attributes: &IMFAttributes, key: &windows::core::GUID) -> Option<String> {
    let mut text = PWSTR::null();
    let mut len = 0;
    if attributes.GetAllocatedString(key, &mut text, &mut len).is_err() {
        return None;
    }
    let name = text.to_string().ok();
    CoTaskMemFree(Some(text.0.cast()));
    name
}

fn actual_encoder_name(writer: &IMFSinkWriter, video: u32, codec: Codec) -> Option<String> {
    unsafe {
        let mut raw = std::ptr::null_mut();
        if writer.GetServiceForStream(video, &windows::core::GUID::zeroed(), &IMFTransform::IID, &mut raw).is_ok()
            && !raw.is_null()
        {
            let transform = IMFTransform::from_raw(raw);
            if let Some(name) = transform_name(&transform, codec) {
                return Some(name);
            }
        }
        let writer: IMFSinkWriterEx = writer.cast().ok()?;
        for index in 0..8 {
            let mut transform = None;
            let mut category = windows::core::GUID::zeroed();
            if writer.GetTransformForStream(video, index, Some(&mut category), &mut transform).is_err() {
                break;
            }
            if category == MFT_CATEGORY_VIDEO_ENCODER {
                return transform_name(&transform?, codec);
            }
        }
        None
    }
}

unsafe fn transform_name(transform: &IMFTransform, codec: Codec) -> Option<String> {
    let attributes = transform.GetAttributes().ok();
    if let Some(name) = attributes.as_ref().and_then(|a| attribute_name(a)) {
        return Some(name);
    }
    let clsid = attributes
        .as_ref()
        .and_then(|a| a.GetGUID(&MFT_TRANSFORM_CLSID_Attribute).ok())
        .or_else(|| transform.cast::<IPersist>().ok()?.GetClassID().ok());
    let hardware_url = attributes.as_ref().and_then(|a| attribute_string(a, &MFT_ENUM_HARDWARE_URL_Attribute));
    for activation in video_encoders_for(Some(codec)).ok()? {
        let attributes: IMFAttributes = activation.cast().ok()?;
        let same_class = clsid.is_some_and(|id| attributes.GetGUID(&MFT_TRANSFORM_CLSID_Attribute).ok() == Some(id));
        let same_hardware = hardware_url
            .as_ref()
            .is_some_and(|url| attribute_string(&attributes, &MFT_ENUM_HARDWARE_URL_Attribute).as_ref() == Some(url));
        if same_class || same_hardware {
            if let Some(name) = attribute_name(&attributes) {
                return Some(name);
            }
        }
    }
    clsid.map(|id| format!("Video encoder {id:?}"))
}

pub fn encoder_names() -> Result<Vec<String>> {
    let _runtime = MfRuntime::new()?;
    unsafe {
        let mut names: Vec<_> = video_encoders()?
            .into_iter()
            .filter_map(|activation| {
                activation.cast::<IMFAttributes>().ok().and_then(|attributes| attribute_name(&attributes))
            })
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
}

unsafe fn video_encoders() -> Result<Vec<IMFActivate>> {
    video_encoders_for(None)
}

unsafe fn video_encoders_for(codec: Option<Codec>) -> Result<Vec<IMFActivate>> {
    let mut activations = std::ptr::null_mut();
    let mut count = 0;
    let output = codec.map(|codec| MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: if codec == Codec::Hevc { MFVideoFormat_HEVC } else { MFVideoFormat_H264 },
    });
    MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_ALL,
        None,
        output.as_ref().map(|info| info as *const _),
        &mut activations,
        &mut count,
    )?;
    let mut result = Vec::new();
    if !activations.is_null() {
        for activation in std::slice::from_raw_parts_mut(activations, count as usize) {
            if let Some(activation) = activation.take() {
                result.push(activation);
            }
        }
        CoTaskMemFree(Some(activations.cast()));
    }
    Ok(result)
}

fn padding_frames(audio_end: i64, video_end: i64) -> usize {
    let gap = (video_end as i128 - audio_end as i128).max(0);
    // Round up to cover the final video duration, but allocate at most one second.
    ((gap * AUDIO_RATE as i128 + 9_999_999) / 10_000_000).min(AUDIO_RATE as i128) as usize
}

unsafe fn set_video_common(t: &IMFMediaType, v: &VideoConfig) -> Result<()> {
    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    t.SetUINT64(&MF_MT_FRAME_SIZE, pack(v.width, v.height))?;
    t.SetUINT64(&MF_MT_FRAME_RATE, pack(v.fps, 1))?;
    t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
    // Must match what the video processor actually produced (BT.709 limited range), or
    // players apply the wrong matrix on decode.
    t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
    t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
    t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
    t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_grid_has_no_hour_long_rounding_drift_and_covers_sparse_endpoints() {
        for fps in [24, 30, 48, 50, 60, 72, 120] {
            let mut grid = FrameGrid::new(fps);
            let mut end = 0;
            for _ in 0..fps as u64 * 3600 {
                let (pts, duration) = grid.take();
                assert_eq!(pts, end);
                assert!(duration > 0);
                end = pts + duration;
            }
            assert_eq!(end, 36_000_000_000);
            let final_slot = grid.ceil_slot(10_100_000);
            assert!((final_slot as u128 * 10_000_000 / fps as u128) >= 10_100_000);
            assert_eq!(grid.nearest_slot(10_000_000), fps as u64);
        }
    }

    #[test]
    fn tail_padding_rounds_up_to_one_sample_and_bounds_allocations() {
        assert_eq!(padding_frames(100, 100), 0);
        assert_eq!(padding_frames(200, 100), 0);
        assert_eq!(padding_frames(0, 1), 1);
        assert_eq!(padding_frames(0, 100_000), 480);
        assert_eq!(padding_frames(0, 100_001), 481);
        assert_eq!(padding_frames(0, i64::MAX), AUDIO_RATE as usize);
    }
}
