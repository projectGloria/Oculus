use std::borrow::Cow;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use image::ImageEncoder;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use crate::capture::gpu::{texture_size, Gpu};
use crate::capture::wgc::{self, Target};
use crate::error::{msg, Result};
use crate::util::{local_stamp, reserve_output};

pub fn play_sound() {
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    static SOUND: &[u8] = include_bytes!("../assets/shutter.wav");
    unsafe {
        let _ = PlaySoundW(PCWSTR(SOUND.as_ptr().cast()), None, SND_ASYNC | SND_MEMORY | SND_NODEFAULT);
    }
}

pub fn capture(target: Target, crop: Option<RECT>, cursor: bool, dir: &Path, copy: bool) -> Result<PathBuf> {
    let gpu = Gpu::new()?;
    let tex = wgc::grab(&gpu, target, cursor)?;
    save(&gpu, &tex, crop, dir, copy)
}

/// The one deliberate GPU→CPU readback in the app: PNG encoding needs the pixels.
pub fn save(gpu: &Gpu, tex: &ID3D11Texture2D, crop: Option<RECT>, dir: &Path, copy: bool) -> Result<PathBuf> {
    let (tw, th) = texture_size(tex);
    let r = crop.unwrap_or(RECT { left: 0, top: 0, right: tw as i32, bottom: th as i32 });
    let (l, t) = (r.left.clamp(0, tw as i32) as u32, r.top.clamp(0, th as i32) as u32);
    let (w, h) = ((r.right as u32).min(tw).saturating_sub(l), (r.bottom as u32).min(th).saturating_sub(t));
    if w == 0 || h == 0 {
        return Err(msg("Empty capture area"));
    }

    let staging =
        gpu.texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_FLAG(0), D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ)?;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    unsafe {
        let bx = D3D11_BOX { left: l, top: t, front: 0, right: l + w, bottom: t + h, back: 1 };
        gpu.context.CopySubresourceRegion(&staging, 0, 0, 0, 0, tex, 0, Some(&bx));
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
        // RowPitch ≥ width*4 (driver alignment), so copy row by row. BGRA → RGBA, and force
        // opaque: WGC leaves alpha undefined for some windows.
        for y in 0..h as usize {
            let src = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), w as usize * 4);
            let dst = &mut rgba[y * w as usize * 4..(y + 1) * w as usize * 4];
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        gpu.context.Unmap(&staging, 0);
    }

    std::fs::create_dir_all(dir)?;
    let (path, file) = reserve_output(dir, &format!("Oculus {}", local_stamp()), "png")?;
    let result = (|| -> Result<()> {
        let mut output = BufWriter::new(file);
        image::codecs::png::PngEncoder::new_with_quality(
            &mut output,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(&rgba, w, h, image::ExtendedColorType::Rgba8)?;
        output.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    if copy {
        // Clipboard failure (another app holding it open) shouldn't fail the screenshot.
        let image_copied = arboard::Clipboard::new()
            .and_then(|mut cb| {
                cb.set_image(arboard::ImageData { width: w as usize, height: h as usize, bytes: Cow::Owned(rgba) })
            })
            .is_ok();
        if let Ok(_clipboard) = clipboard_win::Clipboard::new_attempts(10) {
            let name = crate::shell::shell_name(&path).to_string_lossy();
            if image_copied {
                // Append file-drop data without discarding the image representation.
                let _ = clipboard_win::raw::set_file_list(&[name]);
            } else {
                let _ = clipboard_win::raw::set_file_list_with(&[name], clipboard_win::options::DoClear);
            }
        }
    }
    Ok(path)
}
