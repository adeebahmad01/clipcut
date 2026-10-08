use std::path::{Path, PathBuf};

pub(crate) struct TestDir(PathBuf);

impl TestDir {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("clipcut-core-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn entries(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = std::fs::read_dir(&self.0).unwrap().map(|e| e.unwrap().path()).collect();
        paths.sort();
        paths
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
