//! Kokoro model files: which variant to use, where they live and how to fetch
//! them (resumable, size-verified downloads from the kokoro-onnx release).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use clipcut_core::Progress;
use futures::StreamExt;
use tokio::io::AsyncWriteExt;

pub const KOKORO_BASE_URL: &str =
    "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/";
pub const VOICES_FILE: &str = "voices-v1.0.bin";
pub const VOICES_SIZE: u64 = 28_214_398;
/// Env override for the model variant: `fp32`, `fp16` or `int8`.
pub const KOKORO_VARIANT_ENV: &str = "CLIPCUT_KOKORO_VARIANT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KokoroVariant {
    Fp32,
    Fp16,
    Int8,
}

/// Variant downloaded when nothing is configured. Prefer the smaller floating
/// point export; actual CPU performance depends on hardware and ORT kernels.
pub const DEFAULT_KOKORO_VARIANT: KokoroVariant = KokoroVariant::Fp16;

impl KokoroVariant {
    pub const ALL: [KokoroVariant; 3] = [
        KokoroVariant::Fp16,
        KokoroVariant::Fp32,
        KokoroVariant::Int8,
    ];

    pub fn file_name(self) -> &'static str {
        match self {
            KokoroVariant::Fp32 => "kokoro-v1.0.onnx",
            KokoroVariant::Fp16 => "kokoro-v1.0.fp16.onnx",
            KokoroVariant::Int8 => "kokoro-v1.0.int8.onnx",
        }
    }

    pub fn size(self) -> u64 {
        match self {
            KokoroVariant::Fp32 => 325_532_387,
            KokoroVariant::Fp16 => 177_464_787,
            KokoroVariant::Int8 => 92_361_271,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            KokoroVariant::Fp32 => "fp32",
            KokoroVariant::Fp16 => "fp16",
            KokoroVariant::Int8 => "int8",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fp32" | "f32" | "full" => Some(KokoroVariant::Fp32),
            "fp16" | "f16" | "half" => Some(KokoroVariant::Fp16),
            "int8" | "i8" | "q8" | "quantized" => Some(KokoroVariant::Int8),
            _ => None,
        }
    }

    /// The env override, else [`DEFAULT_KOKORO_VARIANT`].
    pub fn configured() -> Self {
        std::env::var(KOKORO_VARIANT_ENV)
            .ok()
            .and_then(|v| Self::parse(&v))
            .unwrap_or(DEFAULT_KOKORO_VARIANT)
    }
}

/// Model + voices locations, if both are present and complete.
#[derive(Debug, Clone)]
pub struct ModelFiles {
    pub model: PathBuf,
    pub voices: PathBuf,
    pub variant: KokoroVariant,
}

fn complete(path: &Path, size: u64) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() == size)
        .unwrap_or(false)
}

/// Find usable files: the configured variant first; without an explicit env
/// override, any other complete variant is accepted too.
pub fn locate(models_dir: &Path) -> Option<ModelFiles> {
    let voices = models_dir.join(VOICES_FILE);
    if !complete(&voices, VOICES_SIZE) {
        return None;
    }
    let (model, variant) = locate_model(models_dir)?;
    Some(ModelFiles {
        model,
        voices,
        variant,
    })
}

/// Locate the model independently of voices, so an incomplete voice download
/// never causes a redundant download of another already-installed variant.
fn locate_model(models_dir: &Path) -> Option<(PathBuf, KokoroVariant)> {
    let wanted = KokoroVariant::configured();
    let explicit = std::env::var(KOKORO_VARIANT_ENV)
        .ok()
        .and_then(|v| KokoroVariant::parse(&v))
        .is_some();
    let mut order = vec![wanted];
    if !explicit {
        order.extend(KokoroVariant::ALL.iter().copied().filter(|v| *v != wanted));
    }
    order.into_iter().find_map(|variant| {
        let model = models_dir.join(variant.file_name());
        complete(&model, variant.size()).then_some((model, variant))
    })
}

/// Download whatever is missing for the configured variant.
pub async fn ensure(models_dir: &Path, progress: &Progress) -> anyhow::Result<ModelFiles> {
    if let Some(found) = locate(models_dir) {
        return Ok(found);
    }
    // Multiple previews/jobs can ask for first-use installation concurrently.
    // Recheck after taking the lock instead of appending to one .part twice.
    static INSTALL_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _guard = INSTALL_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    if let Some(found) = locate(models_dir) {
        return Ok(found);
    }
    tokio::fs::create_dir_all(models_dir).await?;
    let variant = locate_model(models_dir)
        .map(|(_, v)| v)
        .unwrap_or_else(KokoroVariant::configured);
    let wanted = [
        (VOICES_FILE, VOICES_SIZE),
        (variant.file_name(), variant.size()),
    ];
    let missing: Vec<(&str, u64)> = wanted
        .iter()
        .copied()
        .filter(|(name, size)| !complete(&models_dir.join(name), *size))
        .collect();
    let total: u64 = missing.iter().map(|(_, s)| s).sum();
    if !missing.is_empty() {
        progress.info(format!(
            "Downloading Kokoro voice model ({}, {:.0} MB) into {}",
            variant.name(),
            total as f64 / 1e6,
            models_dir.display()
        ));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent("clipcut-tts")
        .build()?;
    let mut done_before = 0u64;
    for (name, size) in missing {
        let url = format!("{KOKORO_BASE_URL}{name}");
        let dest = models_dir.join(name);
        download_resumable(&client, &url, &dest, size, |got| {
            let frac = (done_before + got) as f32 / total.max(1) as f32;
            progress.fraction(
                frac.min(1.0),
                format!(
                    "Downloading {name}: {:.0}/{:.0} MB",
                    got as f64 / 1e6,
                    size as f64 / 1e6
                ),
            );
        })
        .await?;
        done_before += size;
    }
    locate(models_dir)
        .ok_or_else(|| anyhow::anyhow!("Kokoro model files are incomplete after download"))
}

/// GET `url` into `dest.part` (resuming with Range), verify the size, rename.
pub async fn download_resumable(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected: u64,
    mut on_progress: impl FnMut(u64),
) -> anyhow::Result<()> {
    anyhow::ensure!(expected > 0, "download size must be positive");
    if complete(dest, expected) {
        on_progress(expected);
        return Ok(());
    }
    if let Some(dir) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut part_name = dest.as_os_str().to_os_string();
    part_name.push(".part");
    let part = PathBuf::from(part_name);
    let mut last_err = None;
    for attempt in 0..4u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
        }
        match download_once(client, url, &part, expected, &mut on_progress).await {
            Ok(()) => {
                let len = tokio::fs::metadata(&part).await?.len();
                if len != expected {
                    let _ = tokio::fs::remove_file(&part).await;
                    last_err = Some(anyhow::anyhow!(
                        "{url}: got {len} bytes, expected {expected}"
                    ));
                    continue;
                }
                tokio::fs::rename(&part, dest).await?;
                return Ok(());
            }
            Err(e) => {
                tracing::warn!("download {url} attempt {} failed: {e:#}", attempt + 1);
                if e.downcast_ref::<PermanentDownloadError>().is_some() {
                    return Err(e.context(format!("downloading {url}")));
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("download failed"))
        .context(format!("downloading {url}")))
}

#[derive(Debug)]
struct PermanentDownloadError(String);

impl std::fmt::Display for PermanentDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for PermanentDownloadError {}

fn permanent(message: impl Into<String>) -> anyhow::Error {
    PermanentDownloadError(message.into()).into()
}

fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (bounds, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = bounds.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

async fn download_once(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    expected: u64,
    on_progress: &mut impl FnMut(u64),
) -> anyhow::Result<()> {
    let mut have = tokio::fs::metadata(part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if have > expected {
        tokio::fs::remove_file(part).await?;
        have = 0;
    }
    if have == expected {
        on_progress(have);
        return Ok(());
    }
    let mut req = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            anyhow::bail!("HTTP {status}");
        }
        return Err(permanent(format!("HTTP {status}")));
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let response_size = if resumed {
        let range = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|h| h.to_str().ok())
            .and_then(content_range)
            .ok_or_else(|| permanent("206 response has no valid Content-Range"))?;
        if range.0 != have || range.1 < range.0 || range.1 >= expected || range.2 != expected {
            return Err(permanent(format!(
                "bad resume Content-Range {range:?}; wanted bytes {have}-/{expected}"
            )));
        }
        range.1 - range.0 + 1
    } else if status == reqwest::StatusCode::OK {
        expected
    } else {
        return Err(permanent(format!("unexpected download status {status}")));
    };
    if let Some(len) = resp.content_length() {
        if len != response_size {
            return Err(permanent(format!(
                "HTTP body size {len} does not match expected {response_size}"
            )));
        }
    }
    let mut file = if resumed && have > 0 {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(part)
            .await?
    } else {
        have = 0;
        tokio::fs::File::create(part).await?
    };
    on_progress(have);
    let mut stream = resp.bytes_stream();
    let mut last_report = have;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() as u64 > expected - have {
            return Err(permanent(format!(
                "download exceeds expected {expected} bytes"
            )));
        }
        file.write_all(&chunk).await?;
        have += chunk.len() as u64;
        if have - last_report >= 1 << 20 {
            last_report = have;
            on_progress(have);
        }
    }
    file.flush().await?;
    file.sync_data().await?;
    on_progress(have);
    anyhow::ensure!(
        have == expected,
        "connection closed at {have}/{expected} bytes"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants() {
        assert_eq!(KokoroVariant::parse("FP16"), Some(KokoroVariant::Fp16));
        assert_eq!(KokoroVariant::parse("q8"), Some(KokoroVariant::Int8));
        assert_eq!(KokoroVariant::parse("nope"), None);
        assert!(KokoroVariant::Int8.size() < KokoroVariant::Fp16.size());
    }

    #[test]
    fn locate_requires_complete_files() {
        let dir = std::env::temp_dir().join(format!("clipcut-tts-locate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(VOICES_FILE), b"short").unwrap();
        assert!(locate(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn reuse_installed_models_without_network() {
        let dir = std::env::temp_dir().join(format!(
            "clipcut-tts-existing-models-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let variant = KokoroVariant::configured();
        std::fs::File::create(dir.join(variant.file_name()))
            .unwrap()
            .set_len(variant.size())
            .unwrap();
        assert_eq!(locate_model(&dir).unwrap().1, variant);
        // Models are still discoverable while the voice file is missing.
        assert!(locate(&dir).is_none());
        std::fs::File::create(dir.join(VOICES_FILE))
            .unwrap()
            .set_len(VOICES_SIZE)
            .unwrap();
        let files = ensure(&dir, &Progress::log_only("existing-models-test"))
            .await
            .unwrap();
        assert_eq!(files.variant, variant);
        std::fs::remove_dir_all(dir).unwrap();
    }

    async fn response_fixture(response: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0; 1];
            while !request.ends_with(b"\r\n\r\n") {
                assert!(socket.read(&mut byte).await.unwrap() > 0);
                request.push(byte[0]);
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (format!("http://{addr}/model"), task)
    }

    #[tokio::test]
    async fn resumed_download_checks_ranges_and_restarts_if_ignored() {
        for (id, response) in [
            (
                "range",
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nConnection: close\r\n\r\ndef",
            ),
            (
                "ignored",
                "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef",
            ),
        ] {
            let dir = std::env::temp_dir()
                .join(format!("clipcut-tts-download-{id}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let dest = dir.join("model");
            std::fs::write(dir.join("model.part"), b"abc").unwrap();
            let (url, task) = response_fixture(response).await;
            let mut progress = Vec::new();
            download_resumable(&reqwest::Client::new(), &url, &dest, 6, |n| {
                progress.push(n)
            })
            .await
            .unwrap();
            assert!(
                task.await
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("range: bytes=3-")
            );
            assert_eq!(std::fs::read(&dest).unwrap(), b"abcdef");
            assert_eq!(progress.last(), Some(&6));
            assert!(!dir.join("model.part").exists());
            // Public downloader is idempotent and must not GET again.
            download_resumable(
                &reqwest::Client::new(),
                "http://invalid.invalid/",
                &dest,
                6,
                |_| {},
            )
            .await
            .unwrap();
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[tokio::test]
    async fn incorrect_resume_range_never_publishes_or_changes_valid_partial() {
        let dir =
            std::env::temp_dir().join(format!("clipcut-tts-bad-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("model");
        std::fs::write(dir.join("model.part"), b"abc").unwrap();
        let (url, task) = response_fixture(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 0-2/6\r\nConnection: close\r\n\r\nxyz"
        ).await;
        let err = download_resumable(&reqwest::Client::new(), &url, &dest, 6, |_| {})
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("Content-Range"), "{err:#}");
        task.await.unwrap();
        assert!(!dest.exists());
        assert_eq!(std::fs::read(dir.join("model.part")).unwrap(), b"abc");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wrong_body_size_is_rejected_before_writing() {
        let dir = std::env::temp_dir().join(format!("clipcut-tts-bad-size-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("model");
        let (url, task) = response_fixture(
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n123456789",
        )
        .await;
        assert!(
            download_resumable(&reqwest::Client::new(), &url, &dest, 6, |_| {})
                .await
                .is_err()
        );
        task.await.unwrap();
        assert!(!dest.exists());
        assert!(!dir.join("model.part").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
