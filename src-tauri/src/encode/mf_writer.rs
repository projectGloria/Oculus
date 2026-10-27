use std::path::Path;

use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::*;

use crate::capture::gpu::Gpu;
use crate::error::Result;

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
    pub bitrate: u32,
    pub codec: Codec,
}

/// Two-stream MP4 writer: stream 0 video (HW H.264/HEVC fed with D3D11 NV12 surfaces),
/// stream 1 AAC (fed with PCM16). Owned by exactly one thread (the session writer loop).
pub struct MfWriter {
    writer: IMFSinkWriter,
    _manager: IMFDXGIDeviceManager,
    video: u32,
    audio: Option<u32>,
    last_video: i64,
    last_audio: i64,
}

unsafe impl Send for MfWriter {}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl MfWriter {
    pub fn new(path: &Path, gpu: &Gpu, v: &VideoConfig, with_audio: bool) -> Result<Self> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

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
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            // Throttling stays on: if the encoder falls behind, WriteSample blocks, the NV12
            // pool drains and frames get dropped (bounded memory) instead of queueing forever.

            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)?;

            // ---- video
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, if v.codec == Codec::Hevc { &MFVideoFormat_HEVC } else { &MFVideoFormat_H264 })?;
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
            writer.SetInputMediaType(video, &input, None)?;

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

            writer.BeginWriting()?;
            Ok(Self { writer, _manager: manager, video, audio, last_video: -1, last_audio: -1 })
        }
    }

    pub fn write_video(&mut self, sample: &IMFSample, pts: i64, duration: i64) -> Result<()> {
        // MP4 sink rejects non-increasing timestamps; clamp instead of failing the recording.
        let pts = pts.max(self.last_video + 1);
        self.last_video = pts;
        unsafe {
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            self.writer.WriteSample(self.video, sample)?;
        }
        Ok(())
    }

    pub fn write_audio(&mut self, pcm: &[i16], pts: i64) -> Result<()> {
        let Some(idx) = self.audio else { return Ok(()) };
        if pcm.is_empty() {
            return Ok(());
        }
        let pts = pts.max(self.last_audio + 1);
        self.last_audio = pts;
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
            self.writer.WriteSample(idx, &sample)?;
        }
        Ok(())
    }

    pub fn has_video(&self) -> bool {
        self.last_video >= 0
    }

    pub fn finalize(self) -> Result<()> {
        let r = unsafe { self.writer.Finalize() };
        drop(self.writer);
        unsafe {
            let _ = MFShutdown();
        }
        Ok(r?)
    }
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
