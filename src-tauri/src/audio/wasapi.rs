use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use crate::encode::mf_writer::{AUDIO_CHANNELS, AUDIO_RATE};
use crate::error::Result;
use crate::util::com_init;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// What the speakers play (WASAPI loopback on the default render endpoint).
    System = 0,
    Mic = 1,
}

pub struct Packet {
    pub source: Source,
    /// QPC (100ns) of the first frame in `samples`.
    pub qpc: i64,
    /// Interleaved stereo f32 @ 48 kHz.
    pub samples: Vec<f32>,
}

pub fn spawn(source: Source, tx: Sender<Packet>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::Builder::new()
        .name(format!("oculus-audio-{source:?}"))
        .spawn(move || {
            com_init();
            // A missing/unplugged device must not kill the recording: the mixer zero-fills
            // this source and the video keeps going.
            if let Err(e) = run(source, &tx, &stop) {
                eprintln!("[audio {source:?}] {e}");
            }
        })
        .expect("spawn audio thread")
}

fn run(source: Source, tx: &Sender<Packet>, stop: &AtomicBool) -> Result<()> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let flow = if source == Source::System { eRender } else { eCapture };
        let device = en.GetDefaultAudioEndpoint(flow, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

        // Ask for exactly what the mixer wants and let the audio engine convert: AUTOCONVERTPCM
        // inserts its own resampler + channel matrixer (a 16 kHz mono BT headset comes out as
        // 48 kHz stereo). That's the resampler the plan had rubato for, without the dependency.
        let block = (AUDIO_CHANNELS * 4) as u16;
        let fmt = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: AUDIO_CHANNELS as u16,
                nSamplesPerSec: AUDIO_RATE,
                nAvgBytesPerSec: AUDIO_RATE * block as u32,
                nBlockAlign: block,
                wBitsPerSample: 32,
                cbSize: 22,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
            dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
            SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        };
        let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        if source == Source::System {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        // 200ms engine buffer; we drain every 5ms so it never comes close to overflowing.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &fmt.Format, None)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        // Polling, not event-driven: loopback events are unreliable across Windows builds,
        // and 5ms sleeps cost nothing measurable.
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(5));
            while capture.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut bflags, mut qpc) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                capture.GetBuffer(&mut data, &mut frames, &mut bflags, None, Some(&mut qpc))?;
                let n = frames as usize * AUDIO_CHANNELS as usize;
                let samples = if bflags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    vec![0.0; n]
                } else {
                    std::slice::from_raw_parts(data as *const f32, n).to_vec()
                };
                capture.ReleaseBuffer(frames)?;
                if tx.send(Packet { source, qpc: qpc as i64, samples }).is_err() {
                    return Ok(());
                }
            }
        }
        let _ = client.Stop();
    }
    Ok(())
}
