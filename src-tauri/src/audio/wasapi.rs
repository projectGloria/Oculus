use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Serialize;
use windows::core::{implement, PCWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use crate::encode::mf_writer::{AUDIO_CHANNELS, AUDIO_RATE};
use crate::error::Result;
use crate::queue::Sender;
use crate::util::com_init;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// What the speakers play (WASAPI loopback on the default render endpoint).
    System = 0,
    Mic = 1,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Status {
    Off,
    Starting,
    Ok,
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct Health {
    pub system: Status,
    pub mic: Status,
}

impl Health {
    pub fn new(system: bool, mic: bool) -> Self {
        Self {
            system: if system { Status::Starting } else { Status::Off },
            mic: if mic { Status::Starting } else { Status::Off },
        }
    }

    fn set(&mut self, source: Source, status: Status) {
        match source {
            Source::System => self.system = status,
            Source::Mic => self.mic = status,
        }
    }
}

#[implement(IMMNotificationClient)]
struct EndpointChanges {
    flow: EDataFlow,
    changed: Arc<AtomicBool>,
    follow_default: bool,
}

impl IMMNotificationClient_Impl for EndpointChanges_Impl {
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> windows::core::Result<()> {
        if self.follow_default && flow == self.flow && role == eConsole {
            self.changed.store(true, Ordering::Release);
        }
        Ok(())
    }
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        self.changed.store(true, Ordering::Release);
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.changed.store(true, Ordering::Release);
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.changed.store(true, Ordering::Release);
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

struct Notifications {
    enumerator: IMMDeviceEnumerator,
    callback: IMMNotificationClient,
}

impl Drop for Notifications {
    fn drop(&mut self) {
        unsafe {
            let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.callback);
        }
    }
}

struct RunningClient(IAudioClient);

impl Drop for RunningClient {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.Stop();
        }
    }
}

pub struct Packet {
    pub source: Source,
    /// QPC (100ns) of the first frame in `samples`.
    pub qpc: i64,
    /// Interleaved stereo f32 @ 48 kHz.
    pub samples: Vec<f32>,
}

pub fn spawn(
    source: Source,
    device: Option<String>,
    tx: Sender<Packet>,
    stop: Arc<AtomicBool>,
    health: Arc<Mutex<Health>>,
) -> (JoinHandle<()>, Receiver<()>) {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name(format!("oculus-audio-{source:?}"))
        .spawn(move || {
            com_init();
            let mut ready = Some(ready_tx);
            if let Err(e) = run(source, device.as_deref(), &tx, &stop, &health, &mut ready) {
                health.lock().unwrap().set(source, Status::Failed { message: audio_error(source, &e) });
                tracing::error!(?source, error = %e, "Audio capture stopped");
            }
            // Also unblock setup if enumerator/notification registration failed.
            if let Some(ready) = ready {
                let _ = ready.send(());
            }
        })
        .expect("spawn audio thread");
    (thread, ready_rx)
}

fn audio_error(source: Source, error: &crate::error::Error) -> String {
    if source == Source::Mic {
        if let crate::error::Error::Win(e) = error {
            if e.code() == windows::Win32::Foundation::E_ACCESSDENIED {
                return "Microphone access denied. Open microphone privacy settings and allow desktop apps to access the microphone.".into();
            }
        }
    }
    let label = if source == Source::Mic { "Microphone" } else { "System audio" };
    format!("{label} unavailable: {error}")
}

fn run(
    source: Source,
    device: Option<&str>,
    tx: &Sender<Packet>,
    stop: &AtomicBool,
    health: &Mutex<Health>,
    ready: &mut Option<mpsc::SyncSender<()>>,
) -> Result<()> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let flow = if source == Source::System { eRender } else { eCapture };
        let changed = Arc::new(AtomicBool::new(false));
        let callback: IMMNotificationClient =
            EndpointChanges { flow, changed: changed.clone(), follow_default: device.is_none() }.into();
        en.RegisterEndpointNotificationCallback(&callback)?;
        let _notifications = Notifications { enumerator: en.clone(), callback };
        while !stop.load(Ordering::Relaxed) {
            changed.store(false, Ordering::Release);
            health.lock().unwrap().set(source, Status::Starting);
            let result = run_endpoint(Endpoint { source, device }, tx, stop, &en, &changed, health, ready);
            if let Err(e) = result {
                // Includes AUDCLNT_E_DEVICE_INVALIDATED from either packet call. Reopen
                // the current default instead of abandoning this source for the session.
                health.lock().unwrap().set(source, Status::Failed { message: audio_error(source, &e) });
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                for _ in 0..100 {
                    if stop.load(Ordering::Relaxed) || changed.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            } else if !changed.load(Ordering::Acquire) {
                break;
            }
        }
    }
    Ok(())
}

struct Endpoint<'a> {
    source: Source,
    device: Option<&'a str>,
}

fn run_endpoint(
    endpoint: Endpoint<'_>,
    tx: &Sender<Packet>,
    stop: &AtomicBool,
    en: &IMMDeviceEnumerator,
    changed: &AtomicBool,
    health: &Mutex<Health>,
    ready: &mut Option<mpsc::SyncSender<()>>,
) -> Result<()> {
    unsafe {
        let Endpoint { source, device } = endpoint;
        let flow = if source == Source::System { eRender } else { eCapture };
        let device = match device {
            Some(id) => en.GetDevice(&windows::core::HSTRING::from(id))?,
            None => en.GetDefaultAudioEndpoint(flow, eConsole)?,
        };
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
        let _running = RunningClient(client);
        health.lock().unwrap().set(source, Status::Ok);
        // Initial device open is the preflight: the first snapshot already includes
        // permission/device failures instead of briefly claiming all audio is healthy.
        if let Some(ready) = ready.take() {
            let _ = ready.send(());
        }

        // Polling, not event-driven: loopback events are unreliable across Windows builds,
        // and 5ms sleeps cost nothing measurable.
        while !stop.load(Ordering::Relaxed) && !changed.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(5));
            while !stop.load(Ordering::Relaxed) && !changed.load(Ordering::Acquire) && capture.GetNextPacketSize()? > 0
            {
                let (mut data, mut frames, mut bflags, mut qpc) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                capture.GetBuffer(&mut data, &mut frames, &mut bflags, None, Some(&mut qpc))?;
                let n = frames as usize * AUDIO_CHANNELS as usize;
                let samples = if bflags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    vec![0.0; n]
                } else {
                    std::slice::from_raw_parts(data as *const f32, n).to_vec()
                };
                capture.ReleaseBuffer(frames)?;
                match tx.try_send(Packet { source, qpc: qpc as i64, samples }) {
                    Ok(()) => {}
                    Err(TrySendError::Disconnected(_)) => return Ok(()),
                    Err(TrySendError::Full(_)) => return Err(crate::error::msg("Audio capture buffer filled")),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_notifications_follow_only_our_flow_and_role() {
        let changed = Arc::new(AtomicBool::new(false));
        let callback: IMMNotificationClient =
            EndpointChanges { flow: eRender, changed: changed.clone(), follow_default: true }.into();
        unsafe {
            callback.OnDefaultDeviceChanged(eCapture, eConsole, PCWSTR::null()).unwrap();
            callback.OnDefaultDeviceChanged(eRender, eCommunications, PCWSTR::null()).unwrap();
            assert!(!changed.load(Ordering::Acquire));
            callback.OnDefaultDeviceChanged(eRender, eConsole, PCWSTR::null()).unwrap();
            assert!(changed.load(Ordering::Acquire));
        }
    }

    #[test]
    fn audio_failure_is_serialized_per_source() {
        let mut health = Health::new(true, true);
        health.set(Source::System, Status::Ok);
        health.set(Source::Mic, Status::Failed { message: "permission denied".into() });
        let json = serde_json::to_value(health).unwrap();
        assert_eq!(json["system"]["state"], "ok");
        assert_eq!(json["mic"]["state"], "failed");
        assert_eq!(json["mic"]["message"], "permission denied");
    }

    #[test]
    fn denied_microphone_has_actionable_privacy_guidance() {
        let error = windows::core::Error::from_hresult(windows::Win32::Foundation::E_ACCESSDENIED).into();
        assert!(audio_error(Source::Mic, &error).contains("privacy settings"));
    }
}
