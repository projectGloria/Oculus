use serde::Serialize;
use windows::core::{Interface, HSTRING};
use windows::Foundation::Metadata::ApiInformation;
use windows::Graphics::Capture::GraphicsCaptureSession;
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

use crate::error::{msg, Result};

pub fn windows_version() -> Result<(u32, u32, u32)> {
    let mut version =
        OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    unsafe {
        RtlGetVersion(&mut version).ok()?;
    }
    Ok((version.dwMajorVersion, version.dwMinorVersion, version.dwBuildNumber))
}

pub fn require_supported_windows() -> Result<()> {
    let (major, _, build) = windows_version()?;
    if major < 10 || build < 19041 {
        return Err(msg(format!(
            "Oculus requires Windows 10 version 2004 (build 19041) or later. This system reports build {build}."
        )));
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    version: String,
    windows: String,
    capture_supported: bool,
    capture_border_control: bool,
    min_update_interval: bool,
    graphics: Vec<String>,
    audio: Vec<String>,
    encoder_candidates: Vec<String>,
    current_encoder: Option<String>,
    notes: Vec<String>,
}

pub fn diagnostics(app: &tauri::AppHandle) -> Diagnostics {
    use tauri::Manager;
    let _com = crate::util::ComScope::enter().ok();
    let mut notes = Vec::new();
    let windows = windows_version()
        .map(|(major, minor, build)| format!("{major}.{minor} build {build}"))
        .unwrap_or_else(|e| format!("Unavailable: {e}"));
    let graphics = graphics().unwrap_or_else(|e| {
        notes.push(format!("GPU enumeration: {e}"));
        Vec::new()
    });
    let audio = audio_devices()
        .map(|devices| {
            devices
                .into_iter()
                .map(|device| {
                    format!("{}: {}{}", device.source, device.name, if device.default { " (default)" } else { "" })
                })
                .collect()
        })
        .unwrap_or_else(|e| {
            notes.push(format!("Audio enumeration: {e}"));
            Vec::new()
        });
    let encoder_candidates = crate::encode::mf_writer::encoder_names().unwrap_or_else(|e| {
        notes.push(format!("Encoder enumeration: {e}"));
        Vec::new()
    });
    let has_property = |name| {
        ApiInformation::IsPropertyPresent(
            &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
            &HSTRING::from(name),
        )
        .unwrap_or(false)
    };
    Diagnostics {
        version: app.package_info().version.to_string(),
        windows,
        capture_supported: GraphicsCaptureSession::IsSupported().unwrap_or(false),
        capture_border_control: has_property("IsBorderRequired"),
        min_update_interval: has_property("MinUpdateInterval"),
        graphics,
        audio,
        encoder_candidates,
        current_encoder: app.state::<crate::recorder::Recorder>().encoder_name(),
        notes,
    }
}

fn graphics() -> Result<Vec<String>> {
    let mut graphics = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut index = 0;
        while let Ok(adapter) = factory.EnumAdapters1(index) {
            let desc = adapter.GetDesc1()?;
            let name = String::from_utf16_lossy(
                &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len())],
            );
            let driver = adapter
                // DXGI's version query does not accept D3D11+ interface IDs.
                .CheckInterfaceSupport(&IDXGIDevice::IID)
                .map(|version| {
                    let value = version as u64;
                    format!("{}.{}.{}.{}", value >> 48, (value >> 32) & 0xffff, (value >> 16) & 0xffff, value & 0xffff)
                })
                .unwrap_or_else(|_| "unavailable".into());
            graphics.push(format!("{name} — driver {driver}"));
            index += 1;
        }
    }
    Ok(graphics)
}

#[derive(Serialize)]
pub struct AudioDevice {
    id: String,
    name: String,
    source: &'static str,
    default: bool,
}

fn device_id(device: &IMMDevice) -> Result<String> {
    unsafe {
        let id = device.GetId()?;
        let text = id.to_string();
        CoTaskMemFree(Some(id.0.cast()));
        text.map_err(|e| msg(format!("Audio device identifier is invalid: {e}")))
    }
}

pub fn audio_devices() -> Result<Vec<AudioDevice>> {
    let _com = crate::util::ComScope::enter()?;
    let mut devices = Vec::new();
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        for (flow, source) in [(eRender, "system"), (eCapture, "mic")] {
            let default =
                enumerator.GetDefaultAudioEndpoint(flow, eConsole).ok().and_then(|device| device_id(&device).ok());
            let collection = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
            for index in 0..collection.GetCount()? {
                let device = collection.Item(index)?;
                let id = device_id(&device)?;
                let properties = device.OpenPropertyStore(STGM_READ)?;
                let mut value = properties.GetValue(&PKEY_Device_FriendlyName)?;
                let name = PropVariantToStringAlloc(&value);
                let _ = PropVariantClear(&mut value);
                if let Ok(name) = name {
                    let text = name.to_string();
                    CoTaskMemFree(Some(name.0.cast()));
                    devices.push(AudioDevice {
                        default: default.as_deref() == Some(&id),
                        id,
                        source,
                        name: text.unwrap_or_else(|_| "Unknown endpoint".into()),
                    });
                }
            }
        }
    }
    Ok(devices)
}
