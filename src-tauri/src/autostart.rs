use crate::error::Result;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::*;

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn command(path: &Path) -> Vec<u16> {
    let mut text = vec![b'"' as u16];
    text.extend(path.as_os_str().encode_wide());
    text.extend("\" --tray\0".encode_utf16());
    text
}

pub fn configure(enabled: bool) -> Result<()> {
    if enabled {
        let exe = std::env::current_exe()?;
        let bytes: Vec<u8> = command(&exe).into_iter().flat_map(u16::to_le_bytes).collect();
        write_value(Some((REG_SZ, &bytes)))
    } else {
        write_value(None)
    }
}

pub type StartupValue = Option<(REG_VALUE_TYPE, Vec<u8>)>;

pub fn snapshot() -> Result<StartupValue> {
    unsafe {
        let mut key = HKEY::default();
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        );
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok()?;
        let key = Key(key);
        let mut kind = REG_SZ;
        let mut len = 0;
        let status = RegQueryValueExW(key.0, w!("Oculus"), None, Some(&mut kind), None, Some(&mut len));
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok()?;
        if len > 65536 {
            return Err(crate::error::msg("Existing startup entry is too large to preserve safely"));
        }
        let mut bytes = vec![0; len as usize];
        RegQueryValueExW(key.0, w!("Oculus"), None, Some(&mut kind), Some(bytes.as_mut_ptr()), Some(&mut len)).ok()?;
        bytes.truncate(len as usize);
        Ok(Some((kind, bytes)))
    }
}

pub fn restore(value: &StartupValue) -> Result<()> {
    write_value(value.as_ref().map(|(kind, bytes)| (*kind, bytes.as_slice())))
}

fn write_value(value: Option<(REG_VALUE_TYPE, &[u8])>) -> Result<()> {
    let enabled = value.is_some();
    unsafe {
        let mut key = HKEY::default();
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            None,
            KEY_SET_VALUE,
            &mut key,
        );
        if !enabled && status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if enabled && status == ERROR_FILE_NOT_FOUND {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
            .ok()?;
        } else {
            status.ok()?;
        }
        let key = Key(key);
        if let Some((kind, bytes)) = value {
            RegSetValueExW(key.0, w!("Oculus"), None, kind, Some(bytes)).ok()?;
        } else {
            let status = RegDeleteValueW(key.0, PCWSTR(w!("Oculus").0));
            if status != ERROR_FILE_NOT_FOUND {
                status.ok()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_command_quotes_the_executable_and_starts_in_tray() {
        let value = command(Path::new(r"C:\Program Files\İstanbul Oculus\oculus.exe"));
        assert_eq!(
            String::from_utf16(&value[..value.len() - 1]).unwrap(),
            "\"C:\\Program Files\\İstanbul Oculus\\oculus.exe\" --tray"
        );
        assert_eq!(value.last(), Some(&0));
    }
}
