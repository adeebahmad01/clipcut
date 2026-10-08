//! Same-directory temporary files for atomic replacement. The guard also
//! removes partial downloads when an async operation is cancelled.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) struct TemporaryFile {
    path: PathBuf,
    committed: bool,
}

impl TemporaryFile {
    /// Create synchronously so cancellation cannot strand an in-flight async
    /// file creation before its cleanup guard exists.
    pub(crate) fn create(dest: &Path, private: bool) -> std::io::Result<(Self, File)> {
        let parent = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        for _ in 0..8 {
            let path = parent.join(format!(".clipcut-{}.tmp", uuid::Uuid::new_v4().simple()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            if private {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            #[cfg(not(unix))]
            let _ = private;
            match options.open(&path) {
                Ok(file) => {
                    let temp = Self { path, committed: false };
                    #[cfg(unix)]
                    if private {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                    }
                    return Ok((temp, file));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "could not create a unique temporary file"))
    }

    pub(crate) fn commit(mut self, dest: &Path) -> std::io::Result<()> {
        std::fs::rename(&self.path, dest)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn write(path: &Path, bytes: &[u8], private: bool) -> anyhow::Result<()> {
    let (temp, mut file) = TemporaryFile::create(path, private)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    temp.commit(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn live_temporary_files_have_distinct_paths() {
        let dir = TestDir::new();
        let dest = dir.path().join("config.json");
        let (first, first_file) = TemporaryFile::create(&dest, false).unwrap();
        let (second, second_file) = TemporaryFile::create(&dest, false).unwrap();
        assert_ne!(first.path, second.path);
        assert!(first.path.exists());
        assert!(second.path.exists());
        drop((first_file, second_file));
        drop((first, second));
        assert_eq!(dir.entries().len(), 0);
    }

    #[test]
    fn failed_rename_removes_temporary_file() {
        let dir = TestDir::new();
        let dest = dir.path().join("existing-directory");
        std::fs::create_dir(&dest).unwrap();
        assert!(write(&dest, b"must not replace a directory", false).is_err());
        assert_eq!(dir.entries(), vec![dest]);
    }

    #[test]
    fn concurrent_writers_leave_one_complete_file_and_no_temps() {
        let dir = TestDir::new();
        let dest = dir.path().join("shared.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        std::thread::scope(|scope| {
            for byte in 0u8..8 {
                let dest = &dest;
                let barrier = barrier.clone();
                scope.spawn(move || {
                    let bytes = vec![byte; 16 * 1024];
                    barrier.wait();
                    for _ in 0..8 {
                        write(dest, &bytes, false).unwrap();
                    }
                });
            }
        });
        let bytes = std::fs::read(&dest).unwrap();
        assert_eq!(bytes.len(), 16 * 1024);
        assert!(bytes.iter().all(|b| *b == bytes[0]));
        assert_eq!(dir.entries(), vec![dest]);
    }

    #[test]
    fn writer_creates_nested_parents() {
        let dir = TestDir::new();
        let dest = dir.path().join("nested/deeper/config.json");
        write(&dest, b"complete", false).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"complete");
        assert_eq!(std::fs::read_dir(dest.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn secret_temporary_file_is_private_before_any_write() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let (temp, file) = TemporaryFile::create(&dir.path().join("keys.json"), true).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&temp.path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
