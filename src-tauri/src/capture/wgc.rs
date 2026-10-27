use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::core::Interface;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use super::gpu::{texture_size, Gpu};
use crate::error::{msg, Result};
use crate::util::Mta;

#[derive(Clone, Copy, Debug)]
pub enum Target {
    Monitor(isize),
    Window(isize),
}

pub fn create_item(target: Target) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    unsafe {
        Ok(match target {
            Target::Monitor(h) => interop.CreateForMonitor(HMONITOR(h as _))?,
            Target::Window(h) => interop.CreateForWindow(HWND(h as _))?,
        })
    }
}

pub struct Frame<'a> {
    /// Pool-owned BGRA texture. Valid only inside the callback: the pool has 2 buffers and
    /// holding one past the callback stalls capture.
    pub texture: &'a ID3D11Texture2D,
    /// QPC, 100ns.
    pub time: i64,
    /// Valid region of `texture` (smaller than it while a window is shrinking).
    pub width: u32,
    pub height: u32,
}

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

/// Raw WGC session. ~What windows-capture wraps, minus its private device and CPU paths.
pub struct CaptureSession {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    item: GraphicsCaptureItem,
    frame_token: i64,
    closed_token: i64,
}

impl CaptureSession {
    pub fn start<F, C>(
        gpu: &Gpu,
        item: GraphicsCaptureItem,
        cursor: bool,
        on_frame: F,
        on_closed: C,
    ) -> Result<Self>
    where
        F: Fn(Frame) + Send + 'static,
        C: Fn() + Send + 'static,
    {
        let size = item.Size()?;
        // Free-threaded: FrameArrived fires on a WGC worker thread, no DispatcherQueue needed.
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&gpu.winrt, FORMAT, 2, size)?;
        let device = Mta(gpu.winrt.clone());
        let pool_size = Mutex::new(size);

        let frame_token = pool.FrameArrived(&TypedEventHandler::new(
            move |pool: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
                let pool = pool.ok()?;
                let Ok(frame) = pool.TryGetNextFrame() else { return Ok(()) };
                let content = frame.ContentSize()?;
                let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
                let (tw, th) = texture_size(&texture);
                on_frame(Frame {
                    texture: &texture,
                    time: frame.SystemRelativeTime()?.Duration,
                    width: (content.Width as u32).min(tw),
                    height: (content.Height as u32).min(th),
                });
                drop(texture);
                frame.Close()?;

                // Window resized: grow/shrink the pool so later frames aren't clipped.
                // The encoder canvas stays fixed; the converter letterboxes into it.
                let mut ps = pool_size.lock().unwrap();
                if content.Width != ps.Width || content.Height != ps.Height {
                    if content.Width > 0 && content.Height > 0 {
                        // Bind the wrapper, not `device.0`: edition-2021 closures capture
                        // disjoint fields, which would capture the !Send interface directly.
                        let device = &device;
                        pool.Recreate(&device.0, FORMAT, 2, content)?;
                        *ps = content;
                    }
                }
                Ok(())
            },
        ))?;

        let closed_token = item.Closed(&TypedEventHandler::new(move |_, _| {
            on_closed();
            Ok(())
        }))?;

        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        // Win11: no yellow capture border. Older builds reject it; not fatal.
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture()?;
        Ok(Self { pool, session, item, frame_token, closed_token })
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.frame_token);
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// One frame, copied out of the WGC pool into a texture we own (BGRA, shader-readable so D2D
/// can draw it). Used by screenshots and the region overlay's freeze frame.
pub fn grab(gpu: &Gpu, target: Target, cursor: bool) -> Result<ID3D11Texture2D> {
    let item = create_item(target)?;
    let (tx, rx) = mpsc::sync_channel::<Result<Mta<ID3D11Texture2D>>>(1);
    let tx = Arc::new(Mutex::new(Some(tx)));
    let g = gpu.clone();
    let session = CaptureSession::start(
        gpu,
        item,
        cursor,
        move |f| {
            let Some(tx) = tx.lock().unwrap().take() else { return };
            let copy = (|| {
                let tex = g.texture(
                    f.width,
                    f.height,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    D3D11_BIND_SHADER_RESOURCE,
                    D3D11_USAGE_DEFAULT,
                    D3D11_CPU_ACCESS_FLAG(0),
                )?;
                let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: f.width, bottom: f.height, back: 1 };
                unsafe { g.context.CopySubresourceRegion(&tex, 0, 0, 0, 0, f.texture, 0, Some(&bx)) };
                Ok(Mta(tex))
            })();
            let _ = tx.send(copy);
        },
        || {},
    )?;
    let out = rx
        .recv_timeout(Duration::from_millis(1500))
        .map_err(|_| msg("Capture timed out (is the target minimized?)"))?;
    drop(session);
    Ok(out?.0)
}
