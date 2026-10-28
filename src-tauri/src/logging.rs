//! Bounded local diagnostics. A synchronous writer keeps panic/abort diagnostics on disk;
//! events are small and capture callbacks log only failures, never individual frames.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use tracing_subscriber::util::SubscriberInitExt;

const MAX_BYTES: u64 = 1024 * 1024;
const FILES: usize = 5;

struct RotatingLog {
    dir: PathBuf,
    file: Option<File>,
    size: u64,
    limit: u64,
}

impl RotatingLog {
    fn open(dir: &Path, limit: u64) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new().create(true).append(true).open(dir.join("oculus.log"))?;
        let size = file.metadata()?.len();
        Ok(Self { dir: dir.to_path_buf(), file: Some(file), size, limit })
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.take(); // Close before renaming on Windows.
        let oldest = self.dir.join(format!("oculus.{}.log", FILES - 1));
        match std::fs::remove_file(oldest) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        for index in (1..FILES).rev() {
            let source = if index == 1 {
                self.dir.join("oculus.log")
            } else {
                self.dir.join(format!("oculus.{}.log", index - 1))
            };
            let target = self.dir.join(format!("oculus.{index}.log"));
            match std::fs::rename(source, target) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        self.file = Some(OpenOptions::new().create(true).append(true).open(self.dir.join("oculus.log"))?);
        self.size = 0;
        Ok(())
    }
}

impl Write for RotatingLog {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            if self.size >= self.limit {
                self.rotate()?;
            }
            let count = remaining.len().min((self.limit - self.size) as usize);
            let file = self.file.as_mut().ok_or_else(|| io::Error::other("Log rotation failed"))?;
            file.write_all(&remaining[..count])?;
            self.size += count as u64;
            remaining = &remaining[count..];
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct LogWriter(Arc<Mutex<RotatingLog>>);

struct LockedWriter<'a>(MutexGuard<'a, RotatingLog>);

impl Write for LockedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LockedWriter<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        LockedWriter(self.0.lock().unwrap_or_else(|poison| poison.into_inner()))
    }
}

pub fn init() -> crate::error::Result<()> {
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Oculus")
        .join("logs");
    let log = RotatingLog::open(&dir, MAX_BYTES)?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(LogWriter(Arc::new(Mutex::new(log))))
        .finish()
        .try_init()
        .map_err(|e| crate::error::msg(format!("Cannot initialize logging: {e}")))?;

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Do not acquire the tracing mutex: the panic might have happened inside it.
        // A separate bounded emergency file also survives panic = abort.
        let path = dir.join("panic.log");
        let report = format!("{info}\n{}\n", std::backtrace::Backtrace::force_capture());
        let bytes = &report.as_bytes()[..report.len().min(MAX_BYTES as usize)];
        let truncate = std::fs::metadata(&path).map(|m| m.len() + bytes.len() as u64 > MAX_BYTES).unwrap_or(false);
        if let Ok(mut file) =
            OpenOptions::new().create(true).write(true).append(!truncate).truncate(truncate).open(path)
        {
            let _ = file.write_all(bytes);
            let _ = file.sync_all();
        }
        default_hook(info);
    }));
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Oculus started");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::tests::TestDir;

    #[test]
    fn rotation_is_bounded_and_keeps_newest_bytes() {
        let dir = TestDir::new();
        let mut log = RotatingLog::open(&dir.0, 4).unwrap();
        log.write_all(b"0000111122223333444455556666").unwrap();
        log.flush().unwrap();
        drop(log);
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), FILES);
        assert_eq!(std::fs::read(dir.0.join("oculus.log")).unwrap(), b"6666");
        assert_eq!(std::fs::read(dir.0.join("oculus.4.log")).unwrap(), b"2222");
        assert!(std::fs::read_dir(&dir.0).unwrap().all(|entry| entry.unwrap().metadata().unwrap().len() <= 4));
    }

    #[test]
    fn reopening_a_full_log_rotates_before_writing() {
        let dir = TestDir::new();
        std::fs::write(dir.0.join("oculus.log"), b"old!").unwrap();
        let mut log = RotatingLog::open(&dir.0, 4).unwrap();
        log.write_all(b"new").unwrap();
        drop(log);
        assert_eq!(std::fs::read(dir.0.join("oculus.1.log")).unwrap(), b"old!");
        assert_eq!(std::fs::read(dir.0.join("oculus.log")).unwrap(), b"new");
    }
}
