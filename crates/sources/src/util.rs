//! Small text and file helpers shared by the providers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "can", "could", "did", "do", "does", "for", "from",
    "had", "has", "have", "he", "her", "his", "i", "if", "in", "into", "is", "it", "its", "just", "me", "my", "no",
    "not", "of", "on", "or", "our", "out", "she", "so", "than", "that", "the", "their", "them", "then", "there",
    "these", "they", "this", "those", "to", "too", "up", "us", "was", "we", "were", "what", "when", "which", "who",
    "will", "with", "would", "you", "your", "about", "all", "also", "how", "like", "more", "now", "only", "one",
    "some", "very", "get", "got", "im", "dont", "thats", "its", "gonna", "uh", "um", "yeah", "oh", "s", "t",
];

/// Lowercase alphanumeric tokens ("Webb's" -> "webbs").
pub fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect()
}

pub fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

/// Tokens worth matching on: no stopwords, no single letters (digits kept).
pub fn content_words(s: &str) -> Vec<String> {
    words(s)
        .into_iter()
        .filter(|w| !is_stopword(w) && (w.len() > 1 || w.chars().all(|c| c.is_ascii_digit())))
        .collect()
}

fn trigrams(s: &str) -> Vec<String> {
    let joined: Vec<char> = format!("  {} ", words(s).join(" ")).chars().collect();
    joined.windows(3).map(|w| w.iter().collect()).collect()
}

/// Fuzzy similarity 0..=1: the better of token Jaccard and trigram Dice,
/// plus a containment bonus ("drake" inside "Drake Hotline Bling").
pub fn similarity(a: &str, b: &str) -> f32 {
    use std::collections::HashSet;
    let wa: HashSet<String> = words(a).into_iter().collect();
    let wb: HashSet<String> = words(b).into_iter().collect();
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let inter = wa.intersection(&wb).count() as f32;
    let jaccard = inter / wa.union(&wb).count() as f32;
    let ta = trigrams(a);
    let tb = trigrams(b);
    let mut tb_left = tb.clone();
    let mut common = 0usize;
    for t in &ta {
        if let Some(i) = tb_left.iter().position(|x| x == t) {
            tb_left.swap_remove(i);
            common += 1;
        }
    }
    let dice = 2.0 * common as f32 / (ta.len() + tb.len()).max(1) as f32;
    let na = words(a).join(" ");
    let nb = words(b).join(" ");
    let contain = if na == nb {
        1.0
    } else if (nb.contains(&na) && na.len() >= 4) || (na.contains(&nb) && nb.len() >= 4) {
        0.8
    } else {
        0.0
    };
    jaccard.max(dice).max(contain)
}

/// Filesystem-safe slug for cache file names.
pub fn slug(s: &str) -> String {
    let mut out: String = words(s).join("-");
    let mut end = out.len().min(80);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    if out.is_empty() { "x".into() } else { out }
}

/// Control-free, whitespace-collapsed text cut to at most `max_chars` chars.
pub(crate) fn clean_text(s: &str, max_chars: usize) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().filter(|c| !c.is_control()).take(max_chars).collect::<String>().trim_end().to_string()
}

/// Lowercased host without `www.`.
pub fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_lowercase()))
        .unwrap_or_default()
}

pub(crate) fn http_url(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https") && u.host_str().is_some()
            && u.username().is_empty() && u.password().is_none()
    })
}

pub(crate) fn validate_stem(stem: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !stem.is_empty() && stem.len() <= 100
            && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "asset stem must be 1-100 ASCII letters, digits, '-' or '_'"
    );
    Ok(())
}

/// Explicit diagnostic override. Only numeric private/loopback addresses are
/// accepted, so a typo cannot redirect provider requests to a public service.
pub(crate) fn mock_base() -> Option<String> {
    let raw = std::env::var("CLIPCUT_SOURCES_MOCK_BASE").ok()?;
    let u = url::Url::parse(&raw).ok()?;
    let ip: std::net::IpAddr = u.host_str()?.parse().ok()?;
    let local = match ip {
        std::net::IpAddr::V4(v) => v.is_private() || v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_loopback() || v.is_unique_local(),
    };
    (local && u.scheme() == "http" && u.username().is_empty() && u.password().is_none()
        && u.path() == "/" && u.query().is_none() && u.fragment().is_none())
        .then(|| u.as_str().trim_end_matches('/').to_string())
}

pub(crate) fn endpoint(service: &str, default: &str) -> String {
    if let Some(base) = mock_base() {
        let path = url::Url::parse(default).map(|u| u.path().to_string()).unwrap_or_default();
        format!("{base}/sources-mock/{service}{path}")
    } else {
        default.to_string()
    }
}

pub(crate) fn check_mock_key(key: &str) -> anyhow::Result<()> {
    anyhow::ensure!(mock_base().is_none() || key.starts_with("clipcut-mock-"),
        "mock mode refuses to send real provider keys");
    Ok(())
}

async fn response_bytes(mut r: reqwest::Response, max: usize) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(r.content_length().is_none_or(|n| n <= max as u64), "HTTP body exceeds {max} bytes");
    let mut out = Vec::new();
    while let Some(b) = r.chunk().await? {
        anyhow::ensure!(out.len().saturating_add(b.len()) <= max, "HTTP body exceeds {max} bytes");
        out.extend_from_slice(&b);
    }
    Ok(out)
}

/// Preserve ProviderError for key rotation, but never buffer an unbounded
/// error page (or expose signed URLs / keys from its body).
pub(crate) async fn check(service: &str, r: reqwest::Response) -> anyhow::Result<reqwest::Response> {
    if r.status().is_success() {
        return Ok(r);
    }
    Err(clipcut_core::keys::ProviderError {
        service: service.into(), status: r.status().as_u16(), message: "request failed".into(),
    }.into())
}

pub(crate) async fn json_response(service: &str, r: reqwest::Response) -> anyhow::Result<serde_json::Value> {
    let bytes = response_bytes(check(service, r).await?, 8 * 1024 * 1024).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) async fn text_response(service: &str, r: reqwest::Response) -> anyhow::Result<String> {
    let bytes = response_bytes(check(service, r).await?, 3 * 1024 * 1024).await?;
    Ok(String::from_utf8(bytes)?)
}

/// A guard also removes partial downloads when an outer timeout cancels us.
struct PartialFile(PathBuf);
impl Drop for PartialFile {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
}

async fn read_capped(reader: impl AsyncRead + Unpin, max: usize) -> anyhow::Result<String> {
    let mut buf = Vec::new();
    reader.take(max as u64 + 1).read_to_end(&mut buf).await?;
    anyhow::ensure!(buf.len() <= max, "subprocess output exceeded {max} bytes");
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Unlike a direct-child timeout, a cancelled yt-dlp also kills its ffmpeg
/// children. Output pipes are capped to prevent malformed metadata flooding RAM.
pub(crate) async fn run(
    program: &Path, args: &[&str], cwd: Option<&Path>, timeout: Duration,
) -> anyhow::Result<clipcut_core::tools::Output> {
    use std::process::Stdio;
    struct Group(u32);
    impl Drop for Group {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                // SAFETY: kill only our child's dedicated process group.
                unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
                if self.0 > 0 { unsafe { kill(-(self.0 as i32), 9); } }
            }
        }
    }
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    if let Some(dir) = cwd { cmd.current_dir(dir); }
    let mut child = cmd.spawn()?;
    let group = Group(child.id().unwrap_or(0));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let result = tokio::time::timeout(timeout, async {
        let (status, stdout, stderr) = tokio::try_join!(
            async { child.wait().await.map_err(anyhow::Error::from) },
            read_capped(stdout, 4 * 1024 * 1024), read_capped(stderr, 256 * 1024)
        )?;
        anyhow::ensure!(status.success(), "{} exited with {status}: {}",
            program.file_name().unwrap_or_default().to_string_lossy(),
            stderr.chars().rev().take(600).collect::<String>().chars().rev().collect::<String>());
        Ok::<_, anyhow::Error>(clipcut_core::tools::Output { stdout, stderr })
    }).await.map_err(|_| anyhow::anyhow!("{} timed out after {}s", program.display(), timeout.as_secs()))?;
    drop(group);
    result
}

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

pub(crate) async fn bounded<T>(
    name: &str, timeout: Duration, future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::time::timeout(timeout, future).await
        .map_err(|_| anyhow::anyhow!("{name} exceeded its {}s budget", timeout.as_secs()))?
}

/// Worth fetching the same thing again: a timeout, a dropped connection,
/// HTTP 408/425/429/5xx, or yt-dlp reporting one of those. A 404, a size
/// limit or a file that isn't usable media is final.
pub fn is_transient(e: &anyhow::Error) -> bool {
    use std::io::ErrorKind;
    e.chain().any(|cause| {
        if let Some(p) = cause.downcast_ref::<clipcut_core::keys::ProviderError>() {
            return transient_status(p.status);
        }
        if let Some(r) = cause.downcast_ref::<reqwest::Error>() {
            if let Some(status) = r.status() { return transient_status(status.as_u16()); }
            if r.is_timeout() || r.is_connect() || r.is_request() || r.is_body() { return true; }
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && matches!(io.kind(), ErrorKind::TimedOut | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe)
        {
            return true;
        }
        transient_message(&cause.to_string())
    })
}

fn transient_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500..=599)
}

/// yt-dlp and timeouts only leave text ("HTTP Error 503", "Read timed out").
fn transient_message(message: &str) -> bool {
    const NETWORK: &[&str] = &[
        "timed out", "connection reset", "connection aborted", "broken pipe", "remote end closed",
        "remotedisconnected", "incompleteread", "temporary failure in name resolution",
    ];
    let m = message.to_ascii_lowercase();
    NETWORK.iter().any(|n| m.contains(n))
        || m.match_indices("http error ").any(|(i, p)| {
            m[i + p.len()..].get(..3).and_then(|code| code.parse().ok()).is_some_and(transient_status)
        })
}

/// `dest_dir/<stem>.<ext>` relative to the project dir (`dest_dir`'s parent).
pub fn relative_to_project(dest_dir: &Path, file: &Path) -> String {
    let base = dest_dir.parent().unwrap_or(dest_dir);
    file.strip_prefix(base).unwrap_or(file).to_string_lossy().replace('\\', "/")
}

/// Stream `url` to `dest` atomically with an explicit per-request timeout
/// (the shared client caps whole requests at 60 s, too short for stock video).
pub async fn download_to(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    max_bytes: u64,
    timeout: Duration,
    referer: Option<&str>,
) -> anyhow::Result<u64> {
    use futures::StreamExt;
    anyhow::ensure!(http_url(url), "download URL must be HTTP(S), without embedded credentials");
    let mut req = http.get(url).timeout(timeout);
    if let Some(r) = referer {
        req = req.header(reqwest::header::REFERER, r);
    }
    let response = check("download", req.send().await?).await?;
    if let Some(len) = response.content_length() {
        anyhow::ensure!(len <= max_bytes, "{url} is {len} bytes (limit {max_bytes})");
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = dest.with_extension(format!("{}.part", uuid::Uuid::new_v4().simple()));
    let _guard = PartialFile(tmp.clone());
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut written = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e.into());
            }
        };
        written += chunk.len() as u64;
        if written > max_bytes {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp).await;
            anyhow::bail!("{url} exceeded {max_bytes} bytes");
        }
        file.write_all(&chunk).await?;
    }
    anyhow::ensure!(written > 0, "download returned an empty body");
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp, dest).await?;
    Ok(written)
}

/// Read a cache file if it is younger than `max_age`.
pub fn read_fresh(path: &Path, max_age: Duration) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 8 * 1024 * 1024 { return None; }
    let age = meta.modified().ok()?.elapsed().unwrap_or(Duration::MAX);
    if age > max_age {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Remove any `<stem>.*` leftovers in `dir` (previous fetches of the same beat).
pub fn clear_stem(dir: &Path, stem: &str) {
    if validate_stem(stem).is_err() { return; }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{stem}.")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

pub fn with_ext(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    dir.join(format!("{stem}.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_ranks_close_names() {
        assert!(similarity("Drake Hotline Bling", "drake hotline bling") > 0.99);
        assert!(similarity("distracted boyfriend", "Distracted Boyfriend") > 0.99);
        assert!(similarity("drake", "Drake Hotline Bling") >= 0.8);
        assert!(similarity("change my mind", "Change My Mind") > similarity("change my mind", "Two Buttons"));
        assert!(similarity("Distracted Boyfriend", "Two Buttons") < 0.3);
    }

    #[test]
    fn content_words_drop_stopwords() {
        assert_eq!(content_words("The James Webb telescope is huge"), vec!["james", "webb", "telescope", "huge"]);
    }

    #[test]
    fn unicode_slug_and_safe_paths() {
        assert!(slug(&"界".repeat(100)).len() <= 80);
        assert_eq!(clean_text("  a\n\tb \u{7}c\u{0}d  ", 10), "a b cd");
        assert_eq!(clean_text(&"界".repeat(400), 300).chars().count(), 300);
        assert_eq!(clean_text("ab cd", 3), "ab");
        for bad in ["", "../beat", ".", "/tmp/a", "a.b", "a/b"] {
            assert!(validate_stem(bad).is_err());
        }
        assert!(!http_url("file:///etc/passwd"));
        assert!(!http_url("https://secret@example.com/"));
        assert!(http_url("https://example.com/image.png"));
    }

    #[test]
    fn transient_failures_are_told_apart_from_final_ones() {
        use clipcut_core::keys::ProviderError;
        let status = |status| anyhow::Error::from(ProviderError { service: "download".into(), status, message: String::new() });
        for code in [408, 429, 500, 502, 503] {
            assert!(is_transient(&status(code)), "{code}");
        }
        for code in [400, 401, 403, 404, 410] {
            assert!(!is_transient(&status(code)), "{code}");
        }
        assert!(is_transient(&status(503).context("Downloading GIPHY")));
        let reset = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        assert!(is_transient(&anyhow::Error::from(reset).context("reading body")));
        assert!(!is_transient(&std::io::Error::from(std::io::ErrorKind::NotFound).into()));
        for line in [
            "yt-dlp exited with exit status: 1: ERROR: unable to download video data: HTTP Error 503: Service Unavailable",
            "ERROR: [youtube] abc: Unable to download API page: HTTP Error 429: Too Many Requests",
            "ERROR: [download] Got error: HTTPSConnectionPool(host='rr1.googlevideo.com', port=443): Read timed out.",
            "ERROR: [youtube] abc: Unable to download webpage: <urlopen error [Errno 104] Connection reset by peer>",
            "ERROR: [youtube] abc: Unable to download webpage: <urlopen error [Errno -3] Temporary failure in name resolution>",
            "ERROR: Got error: ('Connection aborted.', RemoteDisconnected('Remote end closed connection without response'))",
            "/home/me/.local/share/clipcut/bin/yt-dlp timed out after 90s",
        ] {
            assert!(is_transient(&anyhow::anyhow!("{line}")), "{line}");
        }
        for line in [
            "yt-dlp exited with exit status: 1: ERROR: unable to download video data: HTTP Error 404: Not Found",
            "ERROR: unable to download video data: HTTP Error 403: Forbidden",
            "ERROR: [youtube] abc: Video unavailable. This video has been removed by the uploader",
            "ERROR: [youtube] abc: Sign in to confirm you're not a bot",
            "yt-dlp produced no file: ",
            "https://example.com/a.mp4 is 90000000 bytes (limit 40000000)",
            "image is only 8x8",
            "HTTP error 50",
        ] {
            assert!(!is_transient(&anyhow::anyhow!("{line}")), "{line}");
        }
    }

    /// Answers on all interfaces, reached via the Network URL. The first
    /// connection is dropped once its request arrives; later ones get `response`.
    async fn drop_first_then(response: &'static [u8]) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        socket.connect("192.0.2.1:80").unwrap();
        let ip = socket.local_addr().unwrap().ip();
        let url = format!("http://{ip}:{}/clip.gif", listener.local_addr().unwrap().port());
        let task = tokio::spawn(async move {
            for first in std::iter::once(true).chain(std::iter::repeat(false)) {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buffer[..n]),
                    }
                }
                if !first {
                    let _ = stream.write_all(response).await;
                }
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn a_dropped_connection_is_transient_and_the_next_try_downloads() {
        let (url, task) = drop_first_then(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nGIF8").await;
        let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        let dest = std::env::temp_dir().join(format!("clipcut-retry-{}.gif", uuid::Uuid::new_v4().simple()));
        let first = download_to(&http, &url, &dest, 1024, Duration::from_secs(5), None).await.unwrap_err();
        assert!(is_transient(&first), "{first:#}");
        assert!(!dest.exists());
        assert_eq!(download_to(&http, &url, &dest, 1024, Duration::from_secs(5), None).await.unwrap(), 4);
        assert_eq!(std::fs::read(&dest).unwrap(), b"GIF8");
        let _ = std::fs::remove_file(&dest);
        task.abort();

        let (url, task) = drop_first_then(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        let _ = download_to(&http, &url, &dest, 1024, Duration::from_secs(5), None).await;
        let missing = download_to(&http, &url, &dest, 1024, Duration::from_secs(5), None).await.unwrap_err();
        assert!(!is_transient(&missing), "{missing:#}");
        task.abort();
    }

    #[tokio::test]
    async fn subprocess_is_bounded() {
        #[cfg(unix)]
        {
            assert!(run(Path::new("/bin/sh"), &["-c", "sleep 20"], None, Duration::from_millis(50)).await.is_err());
            assert!(run(Path::new("/bin/sh"), &["-c", "head -c 5000000 /dev/zero"], None, Duration::from_secs(3)).await.is_err());
        }
    }
}
