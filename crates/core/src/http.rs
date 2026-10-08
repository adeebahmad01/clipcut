use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::keys::ProviderError;

pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36 ClipCut/0.1";

/// Shared HTTP client: browser-like UA, gzip, sane timeouts.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .gzip(true)
        .build()
        .expect("reqwest client")
}

/// Turn a non-2xx response into a `ProviderError` so `KeyPool` can rotate.
pub async fn check(service: &str, response: reqwest::Response) -> anyhow::Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    Err(ProviderError {
        service: service.to_string(),
        status: status.as_u16(),
        message: body.chars().take(300).collect(),
    }
    .into())
}

/// Stream a URL to `dest` (atomic), refusing bodies over `max_bytes`.
/// Unique partial files are removed on errors and future cancellation.
pub async fn download(client: &reqwest::Client, url: &str, dest: &Path, max_bytes: u64) -> anyhow::Result<u64> {
    use futures::StreamExt;
    let response = check("download", client.get(url).send().await?).await?;
    if let Some(len) = response.content_length() {
        anyhow::ensure!(len <= max_bytes, "{url} is {len} bytes (limit {max_bytes})");
    }
    let (temp, file) = crate::atomic::TemporaryFile::create(dest, false)?;
    let mut file = tokio::fs::File::from_std(file);
    let mut written = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let len = chunk.len() as u64;
        anyhow::ensure!(len <= max_bytes - written, "{url} exceeded {max_bytes} bytes");
        file.write_all(&chunk).await?;
        written += len;
    }
    file.flush().await?;
    drop(file);
    temp.commit(dest)?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use tokio::io::AsyncReadExt;

    struct Server {
        url: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// Tiny HTTP fixture on all interfaces, reached via the Network URL.
    /// UDP connect chooses the LAN interface without sending any packets.
    async fn serve(response: &'static [u8], hold_open: bool) -> Server {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        socket.connect("192.0.2.1:80").unwrap();
        let ip = socket.local_addr().unwrap().ip();
        assert!(!ip.is_loopback() && !ip.is_unspecified());
        let url = format!("http://{ip}:{}/asset", listener.local_addr().unwrap().port());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buffer).await.unwrap();
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..n]);
            }
            stream.write_all(response).await.unwrap();
            if hold_open {
                std::future::pending::<()>().await;
            }
        });
        Server { url, task }
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap()
    }

    #[tokio::test]
    async fn successful_download_atomically_replaces_existing_file() {
        let server = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndone", false).await;
        let dir = TestDir::new();
        let dest = dir.path().join("asset.bin");
        std::fs::write(&dest, b"original").unwrap();
        assert_eq!(download(&test_client(), &server.url, &dest, 4).await.unwrap(), 4);
        assert_eq!(std::fs::read(&dest).unwrap(), b"done");
        assert_eq!(dir.entries(), vec![dest]);
    }

    #[tokio::test]
    async fn oversized_or_failed_bodies_leave_existing_file_and_no_partials() {
        for response in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\noversize".as_slice(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\npart\r\n4\r\nmore\r\n0\r\n\r\n".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nx".as_slice(),
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
        ] {
            let server = serve(response, false).await;
            let dir = TestDir::new();
            let dest = dir.path().join("asset.bin");
            std::fs::write(&dest, b"original").unwrap();
            assert!(download(&test_client(), &server.url, &dest, 4).await.is_err());
            assert_eq!(std::fs::read(&dest).unwrap(), b"original");
            assert_eq!(dir.entries(), vec![dest]);
        }
    }

    #[tokio::test]
    async fn failed_download_commit_cleans_up_partial_file() {
        let server = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndone", false).await;
        let dir = TestDir::new();
        let dest = dir.path().join("existing-directory");
        std::fs::create_dir(&dest).unwrap();
        assert!(download(&test_client(), &server.url, &dest, 4).await.is_err());
        assert_eq!(dir.entries(), vec![dest]);
    }

    #[tokio::test]
    async fn cancelling_download_removes_partial_and_preserves_destination() {
        let server =
            serve(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\npart\r\n", true)
                .await;
        let dir = TestDir::new();
        let dest = dir.path().join("asset.bin");
        std::fs::write(&dest, b"original").unwrap();
        let task = tokio::spawn({
            let dest = dest.clone();
            let url = server.url.clone();
            async move { download(&test_client(), &url, &dest, 100).await }
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if dir.entries().iter().any(|p| p != &dest && std::fs::metadata(p).unwrap().len() == 4) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert_eq!(dir.entries(), vec![dest]);
    }

    #[tokio::test]
    async fn concurrent_downloads_use_distinct_temporary_files() {
        let first = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n1111", false).await;
        let second = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n2222", false).await;
        let dir = TestDir::new();
        let dest = dir.path().join("shared.bin");
        let client = test_client();
        let (a, b) = tokio::join!(download(&client, &first.url, &dest, 4), download(&client, &second.url, &dest, 4));
        assert_eq!(a.unwrap(), 4);
        assert_eq!(b.unwrap(), 4);
        let bytes = std::fs::read(&dest).unwrap();
        assert!(bytes == b"1111" || bytes == b"2222");
        assert_eq!(dir.entries(), vec![dest]);
    }
}
