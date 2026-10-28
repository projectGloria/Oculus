use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::HSTRING;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{Common::ITEMIDLIST, SHOpenFolderAndSelectItems, SHParseDisplayName};

use crate::error::{msg, Result};

/// Compare resolved path components, not string prefixes. This also resolves junctions
/// and symlinks before allowing a path from the webview to reach the Shell.
pub fn output_file(dir: &Path, path: &Path) -> Result<PathBuf> {
    let dir = dir.canonicalize()?;
    let path = path.canonicalize().map_err(|_| msg("File no longer exists or cannot be accessed"))?;
    if !path.starts_with(&dir) {
        return Err(msg("Only files in the current output folder can be shown"));
    }
    if !path.is_file() {
        return Err(msg("File no longer exists or is not a regular file"));
    }
    Ok(path)
}

pub(crate) struct Apartment;
impl Apartment {
    pub(crate) fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct ShellItem(*mut ITEMIDLIST);

pub(crate) fn shell_name(path: &Path) -> HSTRING {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    // canonicalize returns extended Win32 paths, which the Shell parser rejects.
    // Translate only the canonical disk/UNC prefixes; preserve the filename's UTF-16.
    let disk_prefix: Vec<u16> = "\\\\?\\".encode_utf16().collect();
    let unc_prefix: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
    if let Some(rest) = wide.strip_prefix(unc_prefix.as_slice()) {
        let mut name: Vec<u16> = "\\\\".encode_utf16().collect();
        name.extend_from_slice(rest);
        HSTRING::from_wide(&name)
    } else if let Some(rest) = wide.strip_prefix(disk_prefix.as_slice()) {
        HSTRING::from_wide(rest)
    } else {
        HSTRING::from_wide(&wide)
    }
}

impl ShellItem {
    fn parse(path: &Path) -> Result<Self> {
        let mut item = Self(std::ptr::null_mut());
        let name = shell_name(path);
        unsafe {
            SHParseDisplayName(&name, None, &mut item.0, 0, None)?;
        }
        if item.0.is_null() {
            return Err(msg("Windows could not resolve the saved file"));
        }
        Ok(item)
    }
}
impl Drop for ShellItem {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

pub fn reveal_file(path: PathBuf) -> Result<()> {
    // Shell APIs use an STA. A dedicated thread avoids changing the Tauri worker's
    // apartment, and all COM allocations are released before that apartment ends.
    std::thread::Builder::new()
        .name("reveal-file".into())
        .spawn(move || {
            let _apartment = Apartment::new()?;
            let item = ShellItem::parse(&path)?;
            // With no child array, Windows opens the parent and selects this absolute PIDL.
            // There is no command line to quote or interpret.
            unsafe {
                SHOpenFolderAndSelectItems(item.0, None, 0)?;
            }
            Ok(())
        })?
        .join()
        .map_err(|_| msg("Could not show the saved file"))?
}

/// Run on the UI STA: OLE owns a modal message loop until drop or Escape.
pub fn drag_file(path: &Path) -> Result<()> {
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Ole::{OleInitialize, OleUninitialize, DROPEFFECT_COPY};
    use windows::Win32::UI::Shell::{ILFindLastID, SHCreateDataObject, SHDoDragDrop};
    struct Ole;
    impl Drop for Ole {
        fn drop(&mut self) {
            unsafe {
                OleUninitialize();
            }
        }
    }
    unsafe {
        OleInitialize(None)?;
        let _ole = Ole;
        let file = ShellItem::parse(path)?;
        let parent = ShellItem::parse(path.parent().ok_or_else(|| msg("Missing file folder"))?)?;
        let child = ILFindLastID(file.0);
        let data: IDataObject = SHCreateDataObject(Some(parent.0), Some(&[child as *const ITEMIDLIST]), None)?;
        // Copy only: dragging a saved recording must never move or remove the original.
        SHDoDragDrop(None, &data, None, DROPEFFECT_COPY)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_names_translate_extended_disk_and_unc_prefixes() {
        for (input, expected) in [
            (r"\\?\C:\captures\Oculus, İstanbul.png", r"C:\captures\Oculus, İstanbul.png"),
            (r"\\?\UNC\server\share\shot.png", r"\\server\share\shot.png"),
            (r"C:\captures\shot.png", r"C:\captures\shot.png"),
        ] {
            assert_eq!(shell_name(Path::new(input)).to_string_lossy(), expected);
        }
    }

    #[test]
    fn reveal_rejects_outside_paths_and_prefix_siblings() {
        let temp = crate::util::tests::TestDir::new();
        let root = &temp.0;
        let dir = root.join("output");
        let sibling = root.join("output-other");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let outside = sibling.join("private.mp4");
        std::fs::write(&outside, b"test").unwrap();
        assert!(output_file(&dir, &outside).is_err());
        assert!(output_file(&dir, &dir.join("..\\output-other\\private.mp4")).is_err());
        assert!(output_file(&dir, &dir).is_err());
        assert!(output_file(&dir, &dir.join("missing.mp4")).is_err());
    }

    #[test]
    fn reveal_accepts_nested_unicode_and_comma_filename_as_one_shell_item() {
        let temp = crate::util::tests::TestDir::new();
        let root = &temp.0;
        let dir = root.join("nested");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Oculus, İstanbul (2).mp4");
        std::fs::write(&path, b"test").unwrap();
        let path = output_file(root, &path).unwrap();
        std::thread::spawn(move || {
            let _apartment = Apartment::new().unwrap();
            let item = ShellItem::parse(&path).unwrap();
            assert!(!item.0.is_null());
        })
        .join()
        .unwrap();
    }

    #[test]
    #[ignore = "Opens Explorer; run only in an interactive Windows session"]
    fn reveal_opens_explorer_for_a_saved_file() {
        let temp = crate::util::tests::TestDir::new();
        let path = temp.0.join("Oculus, İstanbul (2).png");
        std::fs::write(&path, b"shell selection test").unwrap();
        reveal_file(output_file(&temp.0, &path).unwrap()).unwrap();
    }
}
