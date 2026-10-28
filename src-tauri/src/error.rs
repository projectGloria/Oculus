use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}", windows_message(.0))]
    Win(#[from] windows::core::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Image(#[from] image::ImageError),
    #[error("{0}")]
    Msg(String),
    #[error("{context}: {source}")]
    Context { context: &'static str, source: Box<Error> },
}

// Tauri commands need errors that serialize; the UI only ever shows the message.
impl Serialize for Error {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn msg(s: impl Into<String>) -> Error {
    Error::Msg(s.into())
}

pub trait ResultContext<T> {
    fn context(self, context: &'static str) -> Result<T>;
}

impl<T, E: Into<Error>> ResultContext<T> for std::result::Result<T, E> {
    fn context(self, context: &'static str) -> Result<T> {
        self.map_err(|source| Error::Context { context, source: Box::new(source.into()) })
    }
}

fn windows_message(error: &windows::core::Error) -> String {
    let guidance = match error.code().0 as u32 {
        0x80070005 => {
            "Windows denied access. Check capture/microphone privacy permissions and the target app's privilege level."
        }
        0xC00D36B4 | 0xC00D6D60 => {
            "The encoder does not support this media format. Try H.264 or a lower frame rate/resolution."
        }
        0x887A0004 => {
            "The graphics driver does not support this capture/encoding mode. Update the driver or use another display."
        }
        0x887A0005..=0x887A0007 => {
            "The graphics device was reset or disconnected. Restart recording after the display driver recovers."
        }
        0x8007000E => "Windows could not allocate enough memory. Close other apps or reduce the capture resolution.",
        0x80070070 => "The output disk is full. Free space or choose another output folder.",
        _ => return error.to_string(),
    };
    format!("{guidance} Windows details: {error} (0x{:08X})", error.code().0 as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contextual_windows_errors_keep_hresult_and_recovery_guidance() {
        let error = windows::core::Error::from_hresult(windows::core::HRESULT(0x80070005u32 as i32));
        let result: std::result::Result<(), _> = Err(error);
        let message = result.context("Opening microphone").unwrap_err().to_string();
        assert!(message.contains("Opening microphone"));
        assert!(message.contains("privacy"));
        assert!(message.contains("0x80070005"));
    }
}
