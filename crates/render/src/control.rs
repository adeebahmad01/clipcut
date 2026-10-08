//! Cancellation shared by the async API, blocking compositor and ffmpeg.
//! Dropping a render future stops its children, even though spawn_blocking
//! itself cannot be aborted. Publication is serialized with cancellation.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};

#[derive(Default)]
pub(crate) struct Control {
    cancelled: AtomicBool,
    publication: Mutex<()>,
    children: Mutex<Vec<Weak<Mutex<Child>>>>,
}

impl Control {
    pub fn check(&self) -> anyhow::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            bail!("render cancelled");
        }
        Ok(())
    }

    fn register(&self, child: Child) -> Arc<Mutex<Child>> {
        let child = Arc::new(Mutex::new(child));
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        children.retain(|c| c.strong_count() > 0);
        children.push(Arc::downgrade(&child));
        // Covers a cancellation racing with Command::spawn.
        if self.cancelled.load(Ordering::Acquire) {
            let _ = child.lock().unwrap_or_else(|e| e.into_inner()).kill();
        }
        child
    }

    pub fn cancel(&self) {
        {
            let _publication = self.publication.lock().unwrap_or_else(|e| e.into_inner());
            self.cancelled.store(true, Ordering::Release);
        }
        let children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        for child in children.iter().filter_map(Weak::upgrade) {
            let _ = child.lock().unwrap_or_else(|e| e.into_inner()).kill();
        }
    }

    pub fn publish(&self, staged: &Path, destination: &Path) -> anyhow::Result<()> {
        let _publication = self.publication.lock().unwrap_or_else(|e| e.into_inner());
        self.check()?;
        std::fs::rename(staged, destination)
            .with_context(|| format!("publishing {}", destination.display()))
    }
}

pub(crate) struct CancelOnDrop(pub Arc<Control>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// The control retains weak handles only. Each owner must reap its child.
pub(crate) struct ManagedChild(Arc<Mutex<Child>>);

impl ManagedChild {
    pub fn spawn(command: &mut Command, control: &Control) -> anyhow::Result<Self> {
        control.check()?;
        Ok(Self(control.register(command.spawn()?)))
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut Child) -> T) -> T {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn kill(&self) {
        self.with(|c| {
            let _ = c.kill();
        });
    }

    pub fn wait(&self) -> anyhow::Result<ExitStatus> {
        // Never hold the child mutex in a blocking wait: cancellation needs
        // the same handle to terminate an encoder blocked on pipe writes.
        loop {
            if let Some(status) = self.with(Child::try_wait)? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.kill();
        self.with(|c| {
            let _ = c.wait();
        });
    }
}

/// Same-directory staging makes publication atomic. The requested output is
/// never touched by a failed/cancelled attempt, and scratch files are unique
/// across simultaneous renders, threads and processes.
pub(crate) struct StagedFile {
    pub path: PathBuf,
}

impl StagedFile {
    pub fn new(destination: &Path, extension: &str) -> anyhow::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        let stem = destination
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let tick = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{stem}.clipcut-{}-{tick}-{id}.{extension}",
            std::process::id()
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self { path })
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_work_cannot_publish() {
        let out =
            std::env::temp_dir().join(format!("clipcut-publication-{}.mp4", std::process::id()));
        let staged = StagedFile::new(&out, "mp4").unwrap();
        let control = Arc::new(Control::default());
        let guard = CancelOnDrop(control.clone());
        drop(guard);
        assert!(
            control
                .publish(&staged.path, &out)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert!(staged.path.exists());
    }

    #[test]
    fn staging_files_are_unique_and_removed_on_drop() {
        let out = std::env::temp_dir().join("clipcut-staging.mp4");
        let a = StagedFile::new(&out, "wav").unwrap();
        let b = StagedFile::new(&out, "wav").unwrap();
        assert_ne!(a.path, b.path);
        let path = a.path.clone();
        drop(a);
        assert!(!path.exists());
    }
}
