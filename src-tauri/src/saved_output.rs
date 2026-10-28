use std::path::{Path, PathBuf};

use base64::Engine;
use image::ImageEncoder;
use serde::Serialize;
use windows::Win32::Foundation::{HWND, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, ShellExecuteW, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::error::{msg, Result};
use crate::shell::{shell_name, Apartment};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedOutput {
    pub path: String,
    pub kind: &'static str,
    pub duration_ms: Option<u64>,
    pub complete: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Details {
    pub size_bytes: u64,
    pub thumbnail: Option<String>,
}

pub fn details(path: PathBuf) -> Result<Details> {
    let size_bytes = path.metadata()?.len();
    // A missing codec or shell thumbnail provider must not hide saved-file actions.
    let thumbnail = std::thread::spawn(move || thumbnail(&path)).join().ok().and_then(|r| r.ok());
    Ok(Details { size_bytes, thumbnail })
}

pub fn open(path: PathBuf) -> Result<()> {
    std::thread::spawn(move || {
        let _apartment = Apartment::new()?;
        let path = shell_name(&path);
        let result = unsafe { ShellExecuteW(None, windows::core::w!("open"), &path, None, None, SW_SHOWNORMAL) };
        if result.0 as isize <= 32 {
            return Err(msg("Windows could not open the file. Check its default app."));
        }
        Ok(())
    })
    .join()
    .map_err(|_| msg("Could not open saved file"))?
}

pub fn copy(path: &Path) -> Result<()> {
    let name = shell_name(path).to_string_lossy();
    let _clipboard =
        clipboard_win::Clipboard::new_attempts(10).map_err(|e| msg(format!("Could not open clipboard: {e}")))?;
    clipboard_win::raw::set_file_list_with(&[name], clipboard_win::options::DoClear)
        .map_err(|e| msg(format!("Could not copy file to clipboard: {e}")))
}

pub fn rename(path: &Path, stem: &str) -> Result<PathBuf> {
    validate_stem(stem)?;
    let extension = path.extension().and_then(|s| s.to_str()).ok_or_else(|| msg("Missing file extension"))?;
    let destination = path.with_file_name(format!("{stem}.{extension}"));
    if destination == path {
        return Ok(destination);
    }
    // No REPLACE_EXISTING flag: the existence check and move must be atomic.
    unsafe {
        MoveFileExW(&shell_name(path), &shell_name(&destination), MOVE_FILE_FLAGS(0))?;
    }
    Ok(destination)
}

fn validate_stem(stem: &str) -> Result<()> {
    let base = stem.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(prefix)
                .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
        });
    if stem.trim().is_empty()
        || stem.ends_with([' ', '.'])
        || stem.encode_utf16().count() > 180
        || stem.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || reserved
    {
        return Err(msg("Choose a filename without reserved names, control characters or <>:\"/\\|?*. Do not end it with a space or dot."));
    }
    Ok(())
}

struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}
struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

fn thumbnail(path: &Path) -> Result<String> {
    let _apartment = Apartment::new()?;
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&shell_name(path), None)?;
        let bitmap = Bitmap(factory.GetImage(SIZE { cx: 320, cy: 180 }, SIIGBF_THUMBNAILONLY)?);
        let mut object = BITMAP::default();
        if GetObjectW(bitmap.0.into(), std::mem::size_of::<BITMAP>() as i32, Some((&mut object as *mut BITMAP).cast()))
            == 0
        {
            return Err(msg("Could not read thumbnail"));
        }
        let (width, height) = (object.bmWidth, object.bmHeight.abs());
        if width <= 0 || height <= 0 || width > 1024 || height > 1024 {
            return Err(msg("Invalid thumbnail size"));
        }
        let dc = ScreenDc(GetDC(Some(HWND::default())));
        if dc.0.is_invalid() {
            return Err(msg("Could not read thumbnail pixels"));
        }
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        if GetDIBits(dc.0, bitmap.0, 0, height as u32, Some(rgba.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS)
            != height
        {
            return Err(msg("Could not read thumbnail pixels"));
        }
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png).write_image(
            &rgba,
            width as u32,
            height as u32,
            image::ExtendedColorType::Rgba8,
        )?;
        Ok(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_rejects_paths_and_windows_reserved_names() {
        for name in [
            "",
            "  ",
            ".",
            "..",
            "../escape",
            "C:\\escape",
            "NUL",
            "con.txt",
            "CON .txt",
            "COM1",
            "LPT²",
            "trailing.",
            "trailing ",
            "a\nb",
        ] {
            assert!(validate_stem(name).is_err(), "{name:?}");
        }
        assert!(validate_stem("Demo, İstanbul (2)").is_ok());
    }

    #[test]
    fn rename_preserves_extension_and_never_overwrites() {
        let dir = crate::util::tests::TestDir::new();
        let original = dir.0.join("original.mp4");
        let existing = dir.0.join("existing.mp4");
        std::fs::write(&original, b"original").unwrap();
        std::fs::write(&existing, b"existing").unwrap();
        assert!(rename(&original, "existing").is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"existing");
        let renamed = rename(&original, "Demo, İstanbul").unwrap();
        assert_eq!(renamed.extension().unwrap(), "mp4");
        assert_eq!(std::fs::read(renamed).unwrap(), b"original");
    }
}
