use windows::core::Interface;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

use crate::error::Result;

/// The one D3D11 device of a capture session. WGC frames, the video processor, D2D and the
/// MF encoder all live on it: a texture from device A can't be bound to a view on device B,
/// so sharing this is what makes the zero-copy path possible at all.
#[derive(Clone)]
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    /// WinRT projection of the same device, required by `Direct3D11CaptureFramePool`.
    pub winrt: IDirect3DDevice,
}

// Multithread-protected below, so the immediate context serializes cross-thread use.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

impl Gpu {
    pub fn new() -> Result<Self> {
        unsafe {
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                // BGRA: WGC + D2D surfaces. VIDEO: ID3D11VideoDevice for the NV12 converter.
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.unwrap();
            let context: ID3D11DeviceContext = context.unwrap();

            // The WGC callback thread (Blt), the writer thread (keep-alive copies) and the
            // MF encoder all hit the immediate context. MF requires this flag anyway.
            let mt: ID3D11Multithread = context.cast()?;
            let _ = mt.SetMultithreadProtected(true);

            let dxgi: IDXGIDevice = device.cast()?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            Ok(Self { device, context, winrt })
        }
    }

    pub fn texture(
        &self,
        w: u32,
        h: u32,
        format: DXGI_FORMAT,
        bind: D3D11_BIND_FLAG,
        usage: D3D11_USAGE,
        cpu: D3D11_CPU_ACCESS_FLAG,
    ) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: usage,
            BindFlags: bind.0 as u32,
            CPUAccessFlags: cpu.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
        Ok(tex.unwrap())
    }
}

pub fn texture_size(tex: &ID3D11Texture2D) -> (u32, u32) {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut d) };
    (d.Width, d.Height)
}
