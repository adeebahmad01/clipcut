//! Shared cache for downloaded visuals (YouTube cuts, GIFs, stock, images).
//!
//! Visuals are keyed by a SHA-256 of their canonical definition: provider and
//! source URL (and segment start/end seconds for YouTube). Files are stored in
//! `<home>/cache/assets/<prefix>/<hash>.<ext>` with an atomic JSON sidecar.
//! Files are linked into projects via hard link where supported (or copied),
//! and least-recently-used files are evicted when the cache exceeds `cache.max_gb`.

use std::path::{Path, PathBuf};

use clipcut_core::AssetKind;
use serde::{Deserialize, Serialize};

/// Canonical SHA-256 key for a visual asset.
pub fn asset_key(provider: &str, url: &str, youtube_window: Option<(f32, f32)>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(provider.as_bytes());
    hasher.update(b"\n");
    hasher.update(url.as_bytes());
    if let Some((start, end)) = youtube_window {
        hasher.update(b"\n");
        hasher.update(format!("{start:.3}-{end:.3}").as_bytes());
    }
    let hash = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// JSON sidecar stored alongside cached visual assets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssetSidecar {
    pub provider: String,
    pub url: String,
    pub size: u64,
    pub last_used: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub ext: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub duration_s: Option<f32>,
    #[serde(default)]
    pub asset_kind: Option<AssetKind>,
}

/// Hard link `src` to `dest` if the filesystem supports it, otherwise copy.
pub fn link_or_copy(src: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(dest);
    match std::fs::hard_link(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => std::fs::copy(src, dest).map(|_| ()),
    }
}

/// Shared asset cache inside `<home>/cache/assets`.
#[derive(Clone, Debug)]
pub struct AssetCache {
    root: PathBuf,
    max_gb: u32,
}

impl AssetCache {
    pub fn new(cache_root: &Path, max_gb: u32) -> Self {
        Self {
            root: cache_root.join("assets"),
            max_gb,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn key_paths(&self, key: &str, ext: &str) -> (PathBuf, PathBuf) {
        let prefix = if key.len() >= 2 { &key[..2] } else { "xx" };
        let dir = self.root.join(prefix);
        (
            dir.join(format!("{key}.{ext}")),
            dir.join(format!("{key}.json")),
        )
    }

    /// Retrieve an asset from the cache by key.
    ///
    /// If found, copies or hard-links the cached file into `dest_dir` under
    /// `stem.<ext>`, updates the sidecar's `last_used` timestamp, and returns
    /// the destination path and metadata.
    pub fn get(&self, key: &str, dest_dir: &Path, stem: &str) -> Option<(PathBuf, AssetSidecar)> {
        let prefix = if key.len() >= 2 { &key[..2] } else { "xx" };
        let sidecar_path = self.root.join(prefix).join(format!("{key}.json"));
        if !sidecar_path.exists() {
            return None;
        }
        let sidecar_bytes = std::fs::read(&sidecar_path).ok()?;
        let mut sidecar: AssetSidecar = serde_json::from_slice(&sidecar_bytes).ok()?;
        let ext = if sidecar.ext.is_empty() {
            "bin"
        } else {
            &sidecar.ext
        };
        let asset_path = self.root.join(prefix).join(format!("{key}.{ext}"));
        if !asset_path.exists() {
            let _ = std::fs::remove_file(&sidecar_path);
            return None;
        }

        // Update last_used
        sidecar.last_used = chrono::Utc::now();
        if let Ok(bytes) = serde_json::to_vec_pretty(&sidecar) {
            let _ = clipcut_core::atomic::write(&sidecar_path, &bytes, false);
        }

        let dest_file = dest_dir.join(format!("{stem}.{ext}"));
        if let Err(e) = link_or_copy(&asset_path, &dest_file) {
            tracing::warn!(
                "failed to link or copy cached asset {}: {e:#}",
                asset_path.display()
            );
            return None;
        }
        Some((dest_file, sidecar))
    }

    /// Store a processed asset into the cache and update its sidecar atomically.
    pub fn put(
        &self,
        key: &str,
        src_file: &Path,
        provider: &str,
        url: &str,
        width: Option<u32>,
        height: Option<u32>,
        duration_s: Option<f32>,
        kind: Option<AssetKind>,
    ) -> anyhow::Result<PathBuf> {
        let ext = src_file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let (cached_file, sidecar_file) = self.key_paths(key, &ext);
        if let Some(parent) = cached_file.parent() {
            std::fs::create_dir_all(parent)?;
        }

        clipcut_core::atomic::copy_atomic(src_file, &cached_file)?;
        let size = std::fs::metadata(&cached_file)?.len();
        let sidecar = AssetSidecar {
            provider: provider.to_string(),
            url: url.to_string(),
            size,
            last_used: chrono::Utc::now(),
            ext,
            width,
            height,
            duration_s,
            asset_kind: kind,
        };
        let bytes = serde_json::to_vec_pretty(&sidecar)?;
        clipcut_core::atomic::write(&sidecar_file, &bytes, false)?;

        let max_bytes = (self.max_gb as u64) * 1024 * 1024 * 1024;
        let _ = evict_lru(&self.root, max_bytes);
        Ok(cached_file)
    }

    pub fn evict(&self) -> std::io::Result<usize> {
        let max_bytes = (self.max_gb as u64) * 1024 * 1024 * 1024;
        evict_lru(&self.root, max_bytes)
    }
}

struct CacheEntry {
    sidecar_path: PathBuf,
    asset_path: PathBuf,
    last_used: chrono::DateTime<chrono::Utc>,
    total_bytes: u64,
}

/// Evict least-recently-used cache files until total size is within `max_bytes`.
pub fn evict_lru(root: &Path, max_bytes: u64) -> std::io::Result<usize> {
    if !root.exists() {
        return Ok(0);
    }

    let mut entries = Vec::new();
    let mut total_bytes = 0u64;

    let subdirs = std::fs::read_dir(root)?;
    for subdir in subdirs.flatten() {
        if !subdir.file_type()?.is_dir() {
            continue;
        }
        let prefix_dir = subdir.path();
        let files = std::fs::read_dir(&prefix_dir)?;
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(sidecar) = serde_json::from_slice::<AssetSidecar>(&bytes) {
                        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        let ext = if sidecar.ext.is_empty() {
                            "bin"
                        } else {
                            &sidecar.ext
                        };
                        let asset_path = prefix_dir.join(format!("{stem}.{ext}"));
                        if asset_path.exists() {
                            let asset_size =
                                std::fs::metadata(&asset_path).map(|m| m.len()).unwrap_or(0);
                            let sidecar_size = bytes.len() as u64;
                            let entry_size = asset_size + sidecar_size;
                            total_bytes += entry_size;
                            entries.push(CacheEntry {
                                sidecar_path: path,
                                asset_path,
                                last_used: sidecar.last_used,
                                total_bytes: entry_size,
                            });
                        } else {
                            // Clean up orphan sidecar
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
            }
        }
    }

    if total_bytes <= max_bytes {
        return Ok(0);
    }

    // Sort by last_used ascending: oldest (least recently used) first.
    entries.sort_by_key(|e| e.last_used);

    let mut evicted_count = 0;
    for entry in entries {
        if total_bytes <= max_bytes {
            break;
        }
        let _ = std::fs::remove_file(&entry.asset_path);
        let _ = std::fs::remove_file(&entry.sidecar_path);
        if let Some(parent) = entry.asset_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        total_bytes = total_bytes.saturating_sub(entry.total_bytes);
        evicted_count += 1;
    }

    Ok(evicted_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempTestDir {
        path: PathBuf,
    }

    impl TempTestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "clipcut-cache-test-{name}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn asset_key_is_deterministic_and_window_sensitive() {
        let key1 = asset_key(
            "youtube",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            Some((10.0, 13.0)),
        );
        let key2 = asset_key(
            "youtube",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            Some((10.0, 13.0)),
        );
        let key3 = asset_key(
            "youtube",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            Some((10.0, 14.0)),
        );
        let key4 = asset_key("pexels", "https://example.com/photo.jpg", None);

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
        assert_ne!(key1, key4);
        assert_eq!(key1.len(), 64);
    }

    #[test]
    fn cache_miss_put_and_hit() {
        let test_dir = TempTestDir::new("miss-put-hit");
        let cache = AssetCache::new(&test_dir.path, 5);

        let key = asset_key("test_provider", "https://example.com/image.png", None);
        let project_assets = test_dir.path.join("project").join("assets");
        std::fs::create_dir_all(&project_assets).unwrap();

        // 1. Miss initially
        assert!(cache.get(&key, &project_assets, "b01").is_none());

        // 2. Put into cache
        let temp_src = test_dir.path.join("sample.png");
        std::fs::write(&temp_src, b"fake png bytes 123456").unwrap();
        let cached = cache
            .put(
                &key,
                &temp_src,
                "test_provider",
                "https://example.com/image.png",
                Some(640),
                Some(480),
                None,
                Some(AssetKind::Image),
            )
            .unwrap();
        assert!(cached.exists());

        // 3. Hit
        let (dest, sidecar) = cache.get(&key, &project_assets, "b01").expect("cache hit");
        assert_eq!(dest, project_assets.join("b01.png"));
        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"fake png bytes 123456");
        assert_eq!(sidecar.provider, "test_provider");
        assert_eq!(sidecar.width, Some(640));
        assert_eq!(sidecar.height, Some(480));
    }

    #[test]
    fn hard_link_fallback_and_independent_project_files() {
        let test_dir = TempTestDir::new("hardlink");
        let cache = AssetCache::new(&test_dir.path, 5);

        let key = asset_key("giphy", "https://giphy.com/sample.mp4", None);
        let src = test_dir.path.join("download.mp4");
        std::fs::write(&src, b"mp4 video bytes").unwrap();

        cache
            .put(
                &key,
                &src,
                "giphy",
                "https://giphy.com/sample.mp4",
                Some(1920),
                Some(1080),
                Some(2.5),
                Some(AssetKind::Video),
            )
            .unwrap();

        let project1_dir = test_dir.path.join("proj1").join("assets");
        let (dest1, _) = cache.get(&key, &project1_dir, "b01").unwrap();
        assert!(dest1.exists());

        // Removing project 1 does not break cache or project 2
        std::fs::remove_file(&dest1).unwrap();

        let project2_dir = test_dir.path.join("proj2").join("assets");
        let (dest2, _) = cache.get(&key, &project2_dir, "b02").unwrap();
        assert!(dest2.exists());
        assert_eq!(std::fs::read(&dest2).unwrap(), b"mp4 video bytes");
    }

    #[test]
    fn evicts_least_recently_used_first() {
        let test_dir = TempTestDir::new("eviction");
        let cache_root = test_dir.path.join("cache");
        let assets_dir = cache_root.join("assets");
        std::fs::create_dir_all(&assets_dir).unwrap();

        let key1 = asset_key("p1", "http://example.com/1", None);
        let key2 = asset_key("p2", "http://example.com/2", None);
        let key3 = asset_key("p3", "http://example.com/3", None);

        // Put 3 files of 1000 bytes each
        let data = vec![b'x'; 1000];
        let file1 = test_dir.path.join("1.mp4");
        let file2 = test_dir.path.join("2.mp4");
        let file3 = test_dir.path.join("3.mp4");
        std::fs::write(&file1, &data).unwrap();
        std::fs::write(&file2, &data).unwrap();
        std::fs::write(&file3, &data).unwrap();

        let cache = AssetCache::new(&cache_root, 5);
        cache
            .put(
                &key1,
                &file1,
                "p1",
                "http://example.com/1",
                None,
                None,
                None,
                None,
            )
            .unwrap();
        cache
            .put(
                &key2,
                &file2,
                "p2",
                "http://example.com/2",
                None,
                None,
                None,
                None,
            )
            .unwrap();
        cache
            .put(
                &key3,
                &file3,
                "p3",
                "http://example.com/3",
                None,
                None,
                None,
                None,
            )
            .unwrap();

        // Adjust sidecars to force deterministic last_used times:
        // key1 is oldest, key2 is middle, key3 is newest
        let now = chrono::Utc::now();
        let (_, sidecar1) = cache.key_paths(&key1, "mp4");
        let (_, sidecar2) = cache.key_paths(&key2, "mp4");
        let (_, sidecar3) = cache.key_paths(&key3, "mp4");

        let mut s1: AssetSidecar =
            serde_json::from_slice(&std::fs::read(&sidecar1).unwrap()).unwrap();
        s1.last_used = now - chrono::Duration::hours(3);
        std::fs::write(&sidecar1, serde_json::to_vec(&s1).unwrap()).unwrap();

        let mut s2: AssetSidecar =
            serde_json::from_slice(&std::fs::read(&sidecar2).unwrap()).unwrap();
        s2.last_used = now - chrono::Duration::hours(2);
        std::fs::write(&sidecar2, serde_json::to_vec(&s2).unwrap()).unwrap();

        let mut s3: AssetSidecar =
            serde_json::from_slice(&std::fs::read(&sidecar3).unwrap()).unwrap();
        s3.last_used = now - chrono::Duration::hours(1);
        std::fs::write(&sidecar3, serde_json::to_vec(&s3).unwrap()).unwrap();

        // Total size ~ 3600 bytes (3 x (1000 bytes asset + ~200 bytes JSON sidecar)).
        // If limit is 2600 bytes, key1 must be evicted and key2 + key3 remain (~2400 bytes).
        let evicted = evict_lru(&assets_dir, 2600).unwrap();
        assert_eq!(evicted, 1);

        let (path1, _) = cache.key_paths(&key1, "mp4");
        let (path2, _) = cache.key_paths(&key2, "mp4");
        let (path3, _) = cache.key_paths(&key3, "mp4");

        assert!(!path1.exists(), "oldest file key1 should be evicted");
        assert!(path2.exists(), "newer file key2 should remain");
        assert!(path3.exists(), "newest file key3 should remain");
    }

    #[test]
    fn cache_never_exceeds_limit_by_more_than_one_file() {
        let test_dir = TempTestDir::new("cap-invariant");
        let cache_root = test_dir.path.join("cache");
        let cache = AssetCache::new(&cache_root, 5);

        // Put files with evict_lru called with a limit of 1500 bytes.
        let data = vec![b'a'; 800];
        let file = test_dir.path.join("temp.bin");
        std::fs::write(&file, &data).unwrap();

        for i in 0..5 {
            let key = asset_key("provider", &format!("http://example.com/asset-{i}"), None);
            cache
                .put(
                    &key,
                    &file,
                    "provider",
                    &format!("http://example.com/asset-{i}"),
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
            // Evict to 1500 bytes max
            let _ = evict_lru(cache.root(), 1500);

            // Calculate total bytes
            let mut total = 0u64;
            for prefix_entry in std::fs::read_dir(cache.root()).unwrap().flatten() {
                if prefix_entry.path().is_dir() {
                    for f in std::fs::read_dir(prefix_entry.path()).unwrap().flatten() {
                        total += f.metadata().unwrap().len();
                    }
                }
            }
            // Each entry is ~800 bytes asset + ~200 bytes JSON ~ 1000 bytes.
            // With a 1500 bytes cap, it will hold at most 1 full entry (~1000 bytes) after eviction,
            // never exceeding 1500 bytes.
            assert!(total <= 1500, "cache total {total} exceeds max limit 1500");
        }
    }
}
