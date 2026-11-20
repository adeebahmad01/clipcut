//! Rotating log file writer and recent log reader.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Result as IoResult, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::EnvFilter;

/// Default size limit before rotating: 5 MB.
pub const DEFAULT_MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// Number of old rotated log files to retain (`clipcut.log.1`, `.2`, `.3`).
pub const DEFAULT_MAX_LOG_FILES: usize = 3;

/// Strip ANSI escape sequences (colors, cursor control) from raw bytes.
pub fn strip_ansi(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == 0x1b && i + 1 < buf.len() && buf[i + 1] == b'[' {
            i += 2;
            while i < buf.len() && (0x20..=0x3f).contains(&buf[i]) {
                i += 1;
            }
            if i < buf.len() && (0x40..=0x7e).contains(&buf[i]) {
                i += 1;
            }
        } else {
            out.push(buf[i]);
            i += 1;
        }
    }
    out
}

/// A rotating log file writer that bounds disk usage to `(1 + max_files) * max_bytes`.
pub struct RotatingLogWriter {
    path: PathBuf,
    max_bytes: u64,
    max_files: usize,
    current_size: u64,
    file: Option<File>,
}

impl RotatingLogWriter {
    pub fn new(path: PathBuf) -> IoResult<Self> {
        Self::with_limits(path, DEFAULT_MAX_LOG_BYTES, DEFAULT_MAX_LOG_FILES)
    }

    pub fn with_limits(path: PathBuf, max_bytes: u64, max_files: usize) -> IoResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let current_size = match std::fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            max_bytes,
            max_files,
            current_size,
            file: Some(file),
        })
    }

    pub fn rotate(&mut self) -> IoResult<()> {
        self.file = None;

        let base = &self.path;
        // Oldest file beyond limit is removed
        let oldest = format!("{}.{}", base.display(), self.max_files);
        let _ = std::fs::remove_file(&oldest);

        // Shift existing rotated files: .2 -> .3, .1 -> .2
        for i in (1..self.max_files).rev() {
            let src = format!("{}.{}", base.display(), i);
            let dst = format!("{}.{}", base.display(), i + 1);
            if Path::new(&src).exists() {
                let _ = std::fs::rename(&src, &dst);
            }
        }

        // Current file becomes .1
        if base.exists() {
            let first = format!("{}.1", base.display());
            let _ = std::fs::rename(base, &first);
        }

        let new_file = OpenOptions::new().create(true).append(true).open(base)?;
        self.current_size = 0;
        self.file = Some(new_file);
        Ok(())
    }
}

impl Write for RotatingLogWriter {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        let clean = strip_ansi(buf);
        let len = clean.len() as u64;
        if self.current_size + len > self.max_bytes {
            self.rotate()?;
        }
        if let Some(file) = &mut self.file {
            file.write_all(&clean)?;
            self.current_size += len;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> IoResult<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }
}

/// Dual writer that sends unformatted output to stderr and plain clean logs to the file.
#[derive(Clone)]
pub struct MultiWriter {
    file: Arc<Mutex<RotatingLogWriter>>,
}

impl Write for MultiWriter {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        let _ = std::io::stderr().write_all(buf);
        if let Ok(mut writer) = self.file.lock() {
            let _ = writer.write(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> IoResult<()> {
        let _ = std::io::stderr().flush();
        if let Ok(mut writer) = self.file.lock() {
            let _ = writer.flush();
        }
        Ok(())
    }
}

/// Initialize tracing subscriber with both stderr and rotating file output.
pub fn init_logging(log_file: PathBuf) -> IoResult<Arc<Mutex<RotatingLogWriter>>> {
    let rotating = RotatingLogWriter::new(log_file)?;
    let shared = Arc::new(Mutex::new(rotating));
    let writer_shared = shared.clone();
    let make_writer = move || MultiWriter {
        file: writer_shared.clone(),
    };

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn,hyper=warn")),
        )
        .with_target(false)
        .with_writer(make_writer)
        .try_init();

    Ok(shared)
}

/// Read the last `max_lines` from the log directory in chronological order.
pub fn read_recent_lines(logs_dir: &Path, max_lines: usize) -> IoResult<Vec<String>> {
    let mut files_to_read = Vec::new();
    // In chronological order: clipcut.log.3, .2, .1, clipcut.log
    for i in (1..=DEFAULT_MAX_LOG_FILES).rev() {
        let path = logs_dir.join(format!("clipcut.log.{i}"));
        if path.exists() {
            files_to_read.push(path);
        }
    }
    let current = logs_dir.join("clipcut.log");
    if current.exists() {
        files_to_read.push(current);
    }

    let mut lines = Vec::new();
    for path in files_to_read {
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            lines.push(line);
        }
    }

    if lines.len() > max_lines {
        let skip = lines.len() - max_lines;
        Ok(lines.split_off(skip))
    } else {
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi() {
        let colored = b"\x1b[32mOK\x1b[0m \x1b[1;31mFAIL\x1b[0m plain";
        assert_eq!(strip_ansi(colored), b"OK FAIL plain");
    }

    #[test]
    fn test_rotation_with_small_size_limit() {
        let dir = std::env::temp_dir().join(format!("clipcut-log-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_file = dir.join("clipcut.log");

        // Small limit: 50 bytes per file, retain 3
        let mut writer = RotatingLogWriter::with_limits(log_file.clone(), 50, 3).unwrap();

        // Write line 1 (40 bytes)
        writeln!(writer, "012345678901234567890123456789012345678").unwrap();
        assert!(log_file.exists());
        assert!(!dir.join("clipcut.log.1").exists());

        // Write line 2 (causes rotation because 40 + 40 > 50)
        writeln!(writer, "line 2 ---------------------------------").unwrap();
        assert!(dir.join("clipcut.log.1").exists());
        assert!(!dir.join("clipcut.log.2").exists());

        // Write line 3 (rotates again: log -> log.1, log.1 -> log.2)
        writeln!(writer, "line 3 ---------------------------------").unwrap();
        assert!(dir.join("clipcut.log.1").exists());
        assert!(dir.join("clipcut.log.2").exists());
        assert!(!dir.join("clipcut.log.3").exists());

        // Write line 4 (rotates again: log.2 -> log.3)
        writeln!(writer, "line 4 ---------------------------------").unwrap();
        assert!(dir.join("clipcut.log.1").exists());
        assert!(dir.join("clipcut.log.2").exists());
        assert!(dir.join("clipcut.log.3").exists());

        // Write line 5 (rotates again: oldest log.3 removed, log.2 -> log.3)
        writeln!(writer, "line 5 ---------------------------------").unwrap();
        assert!(dir.join("clipcut.log.1").exists());
        assert!(dir.join("clipcut.log.2").exists());
        assert!(dir.join("clipcut.log.3").exists());

        // Verify recent lines reads all lines in order
        let lines = read_recent_lines(&dir, 10).unwrap();
        assert_eq!(lines.len(), 4); // line 1 was in the pruned oldest file, lines 2, 3, 4, 5 remain
        assert!(lines[0].starts_with("line 2"));
        assert!(lines[3].starts_with("line 5"));

        // Limit reading to last 2 lines
        let last_two = read_recent_lines(&dir, 2).unwrap();
        assert_eq!(last_two.len(), 2);
        assert!(last_two[0].starts_with("line 4"));
        assert!(last_two[1].starts_with("line 5"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
