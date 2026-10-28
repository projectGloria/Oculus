use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex, Weak};

use windows::core::{implement, Interface, Ref, GUID};
use windows::Win32::Foundation::{E_NOTIMPL, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Media::MediaFoundation::*;

use super::gpu::Gpu;
use crate::error::Result;
use crate::util::Mta;

/// One GPU pass per frame: crop (source rect) + scale/letterbox (dest rect) + BGRA→NV12 with
/// explicit BT.709 matrices. Runs on the GPU's video engine, not the 3D queue.
pub struct Converter {
    vctx: ID3D11VideoContext1,
    vdev: ID3D11VideoDevice,
    enumerator: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    out_w: u32,
    out_h: u32,
    /// Region mode: sub-rect of the monitor frame. None = whole content.
    crop: Option<RECT>,
    /// Input views keyed by texture pointer. WGC cycles 2 textures (more after a Recreate);
    /// each cached view holds a ref, so a pointer can't be recycled while it's in here.
    views: Vec<(usize, ID3D11VideoProcessorInputView)>,
}

unsafe impl Send for Converter {}

impl Converter {
    pub fn new(gpu: &Gpu, in_w: u32, in_h: u32, out_w: u32, out_h: u32, fps: u32, crop: Option<RECT>) -> Result<Self> {
        unsafe {
            let vdev: ID3D11VideoDevice = gpu.device.cast()?;
            let vctx: ID3D11VideoContext1 = gpu.context.cast()?;
            let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
            // Input size is a hint for the driver; Blt accepts other sizes, which is what
            // lets a resizing window keep flowing through the same processor.
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: in_w,
                InputHeight: in_h,
                OutputFrameRate: rate,
                OutputWidth: out_w,
                OutputHeight: out_h,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = vdev.CreateVideoProcessorEnumerator(&desc)?;
            let vp = vdev.CreateVideoProcessor(&enumerator, 0)?;

            // Without these, drivers commonly assume BT.601 / limited-range RGB and the
            // recording comes out with lifted blacks and shifted reds.
            vctx.VideoProcessorSetStreamColorSpace1(&vp, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            vctx.VideoProcessorSetOutputColorSpace1(&vp, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            vctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            // No driver "enhancements" (sharpening, noise reduction) on screen content.
            vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
            let full = RECT { left: 0, top: 0, right: out_w as i32, bottom: out_h as i32 };
            vctx.VideoProcessorSetOutputTargetRect(&vp, true, Some(&full));
            // Letterbox bars: studio-range black in YCbCr.
            let mut black = D3D11_VIDEO_COLOR::default();
            black.Anonymous.YCbCr = D3D11_VIDEO_COLOR_YCbCrA { Y: 16.0 / 255.0, Cb: 0.5, Cr: 0.5, A: 1.0 };
            vctx.VideoProcessorSetOutputBackgroundColor(&vp, true, &black);

            Ok(Self { vctx, vdev, enumerator, vp, out_w, out_h, crop, views: Vec::new() })
        }
    }

    pub fn enumerator(&self) -> &ID3D11VideoProcessorEnumerator {
        &self.enumerator
    }

    fn input_view(&mut self, tex: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView> {
        let key = tex.as_raw() as usize;
        if let Some((_, v)) = self.views.iter().find(|(k, _)| *k == key) {
            return Ok(v.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut view = None;
        unsafe { self.vdev.CreateVideoProcessorInputView(tex, &self.enumerator, &desc, Some(&mut view))? };
        let view = view.unwrap();
        if self.views.len() >= 6 {
            self.views.remove(0);
        }
        self.views.push((key, view.clone()));
        Ok(view)
    }

    pub fn blit(
        &mut self,
        src: &ID3D11Texture2D,
        content_w: u32,
        content_h: u32,
        dst: &ID3D11VideoProcessorOutputView,
    ) -> Result<()> {
        let src_rect = match self.crop {
            Some(c) => RECT {
                left: c.left.min(content_w as i32),
                top: c.top.min(content_h as i32),
                right: c.right.min(content_w as i32),
                bottom: c.bottom.min(content_h as i32),
            },
            None => RECT { left: 0, top: 0, right: content_w as i32, bottom: content_h as i32 },
        };
        let (sw, sh) = (src_rect.right - src_rect.left, src_rect.bottom - src_rect.top);
        if sw <= 0 || sh <= 0 {
            return Ok(());
        }
        let dst_rect = fit(sw, sh, self.out_w as i32, self.out_h as i32);
        let view = self.input_view(src)?;
        unsafe {
            self.vctx.VideoProcessorSetStreamSourceRect(&self.vp, 0, true, Some(&src_rect));
            self.vctx.VideoProcessorSetStreamDestRect(&self.vp, 0, true, Some(&dst_rect));
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(view)),
                ..Default::default()
            };
            let r = self.vctx.VideoProcessorBlt(&self.vp, dst, 0, std::slice::from_ref(&stream));
            // The struct field is ManuallyDrop (C layout); release our view ref ourselves.
            ManuallyDrop::drop(&mut stream.pInputSurface);
            r?;
        }
        Ok(())
    }
}

/// Aspect-fit (w,h) into (W,H), centered, even-aligned. Identity when sizes match, which is
/// the monitor/region case; only a resized window or a downscaled huge monitor letterboxes.
fn fit(w: i32, h: i32, out_w: i32, out_h: i32) -> RECT {
    let scale = (out_w as f64 / w as f64).min(out_h as f64 / h as f64);
    let (fw, fh) = (((w as f64 * scale) as i32) & !1, ((h as f64 * scale) as i32) & !1);
    let (x, y) = (((out_w - fw) / 2) & !1, ((out_h - fh) / 2) & !1);
    RECT { left: x, top: y, right: x + fw, bottom: y + fh }
}

// Sample attribute keys: each pooled IMFSample carries its own texture + output view, so the
// free list is just a Vec<IMFSample> with no side table to keep in sync.
const KEY_TEXTURE: GUID = GUID::from_u128(0x6f1c_2a7e_4d0b_4b52_9a3e_0c1a_51d0_e001);
const KEY_VIEW: GUID = GUID::from_u128(0x6f1c_2a7e_4d0b_4b52_9a3e_0c1a_51d0_e002);

type FreeList = Mutex<Vec<Mta<IMFSample>>>;

/// IMFTrackedSample allocator callback: fires when the encoder (and everyone else) has
/// released the sample. Only then is the NV12 texture safe to overwrite: WriteSample
/// returning just means "queued" for an async hardware MFT.
#[implement(IMFAsyncCallback)]
struct Returner {
    // Weak: samples hold this callback, the free list holds samples. A strong ref would be a
    // cycle that leaks every texture when the session ends.
    free: Weak<FreeList>,
}

impl IMFAsyncCallback_Impl for Returner_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Invoke(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        let sample: IMFSample = unsafe { result.ok()?.GetObject()? }.cast()?;
        if let Some(free) = self.free.upgrade() {
            free.lock().unwrap().push(Mta(sample));
        }
        Ok(())
    }
}

pub struct Nv12Pool {
    gpu: Gpu,
    free: Arc<FreeList>,
    callback: IMFAsyncCallback,
    created: usize,
    w: u32,
    h: u32,
}

unsafe impl Send for Nv12Pool {}

pub struct PooledFrame {
    pub sample: IMFSample,
    pub texture: ID3D11Texture2D,
    pub view: ID3D11VideoProcessorOutputView,
}

const POOL_START: usize = 6;
const POOL_CAP: usize = 12;

impl Nv12Pool {
    pub fn new(gpu: &Gpu, conv: &Converter, w: u32, h: u32) -> Result<Self> {
        let free = Arc::new(Mutex::new(Vec::new()));
        let callback: IMFAsyncCallback = Returner { free: Arc::downgrade(&free) }.into();
        let mut pool = Self { gpu: gpu.clone(), free, callback, created: 0, w, h };
        for _ in 0..POOL_START {
            let s = pool.create(conv)?;
            pool.free.lock().unwrap().push(Mta(s));
        }
        Ok(pool)
    }

    fn create(&mut self, conv: &Converter) -> Result<IMFSample> {
        unsafe {
            let tex = self.gpu.texture(
                self.w,
                self.h,
                DXGI_FORMAT_NV12,
                D3D11_BIND_FLAG(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0),
                D3D11_USAGE_DEFAULT,
                D3D11_CPU_ACCESS_FLAG(0),
            )?;
            let vdev: ID3D11VideoDevice = self.gpu.device.cast()?;
            let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut view = None;
            vdev.CreateVideoProcessorOutputView(&tex, conv.enumerator(), &desc, Some(&mut view))?;

            // DXGI surface buffer: MF hands the texture itself to the HW encoder, no readback.
            let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &tex, 0, false)?;
            let len = buf.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buf.SetCurrentLength(len)?;
            let sample: IMFSample = MFCreateTrackedSample()?.cast()?;
            sample.AddBuffer(&buf)?;
            sample.SetUnknown(&KEY_TEXTURE, &tex)?;
            sample.SetUnknown(&KEY_VIEW, &view.unwrap())?;
            self.created += 1;
            Ok(sample)
        }
    }

    /// A free NV12 frame, or None when all are in flight at the cap: the caller drops the
    /// frame (counted as dropped) instead of overwriting a texture the encoder is reading.
    pub fn acquire(&mut self, conv: &Converter) -> Result<Option<PooledFrame>> {
        let popped = self.free.lock().unwrap().pop();
        let sample = match popped {
            Some(s) => s.0,
            None if self.created < POOL_CAP => self.create(conv)?,
            None => return Ok(None),
        };
        Ok(Some(self.arm(sample)?))
    }

    fn arm(&self, sample: IMFSample) -> Result<PooledFrame> {
        unsafe {
            // One-shot: must be re-armed every time the sample goes out.
            sample.cast::<IMFTrackedSample>()?.SetAllocator(&self.callback, None)?;
            Ok(PooledFrame { texture: sample.GetUnknown(&KEY_TEXTURE)?, view: sample.GetUnknown(&KEY_VIEW)?, sample })
        }
    }
}

impl Drop for Nv12Pool {
    fn drop(&mut self) {
        self.free.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterboxes_landscape_portrait_and_odd_inputs_on_even_boundaries() {
        for (w, h, ow, oh) in [(1920, 1080, 1280, 720), (1080, 1920, 1920, 1080), (1919, 1079, 1280, 720)] {
            let r = fit(w, h, ow, oh);
            assert!(r.left >= 0 && r.top >= 0 && r.right <= ow && r.bottom <= oh);
            assert!(r.right > r.left && r.bottom > r.top);
            for edge in [r.left, r.top, r.right, r.bottom] {
                assert_eq!(edge % 2, 0);
            }
            let ratio = (r.right - r.left) as f64 / (r.bottom - r.top) as f64;
            assert!((ratio - w as f64 / h as f64).abs() < 0.01);
        }
    }
}
