//! One-time installs for the sources backends: yt-dlp (standalone binary,
//! PyPI wheel fallback) and a local SearXNG container (podman or docker).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use clipcut_core::tools::run;
use clipcut_core::{Config, Paths, Progress, Tools};

pub const SEARXNG_CONTAINER: &str = "clipcut-searxng";
pub const SEARXNG_IMAGE: &str = "docker.io/searxng/searxng:latest";

fn yt_dlp_asset() -> anyhow::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("yt-dlp_linux"),
        ("linux", "aarch64") => Ok("yt-dlp_linux_aarch64"),
        ("macos", _) => Ok("yt-dlp_macos"),
        (os, arch) => anyhow::bail!("no yt-dlp standalone build for {os}/{arch}; install it with `pipx install yt-dlp`"),
    }
}

async fn yt_dlp_version(path: &Path) -> anyhow::Result<String> {
    let out = run(path, &["--version"], None, Duration::from_secs(60)).await?;
    let v = out.stdout.trim().to_string();
    anyhow::ensure!(!v.is_empty(), "{} --version printed nothing", path.display());
    Ok(v)
}

fn make_executable(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = path;
    Ok(())
}

/// Download with a few retries (GitHub's release CDN resets connections on
/// some networks).
async fn download_retry(http: &reqwest::Client, url: &str, dest: &Path, progress: &Progress) -> anyhow::Result<u64> {
    let mut last = None;
    for attempt in 1..=3 {
        match crate::util::download_to(http, url, dest, 300 * 1024 * 1024, Duration::from_secs(900), None).await {
            Ok(n) => return Ok(n),
            Err(e) => {
                progress.warn(format!("download attempt {attempt}/3 of {url} failed: {e:#}"));
                last = Some(e);
                tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
            }
        }
    }
    Err(last.unwrap())
}

/// The official standalone build from GitHub releases.
async fn install_standalone(bin: &Path, http: &reqwest::Client, progress: &Progress) -> anyhow::Result<(PathBuf, String)> {
    let asset = yt_dlp_asset()?;
    let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}");
    progress.info(format!("downloading {url}"));
    let tmp = bin.join("yt-dlp.download");
    let bytes = download_retry(http, &url, &tmp, progress).await.context("downloading yt-dlp from GitHub")?;
    make_executable(&tmp)?;
    let version = match yt_dlp_version(&tmp).await {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.context("downloaded yt-dlp does not run"));
        }
    };
    let dest = bin.join("yt-dlp");
    std::fs::rename(&tmp, &dest)?;
    let _ = std::fs::remove_file(bin.join("yt-dlp.whl"));
    progress.info(format!("installed yt-dlp {version} ({:.1} MB) at {}", bytes as f64 / 1e6, dest.display()));
    Ok((dest, version))
}

fn find_python() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for name in ["python3", "python"] {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Fallback: the pure-Python wheel from PyPI run through a tiny wrapper
/// script (`python3 -m yt_dlp` with the wheel on PYTHONPATH).
async fn install_wheel(bin: &Path, http: &reqwest::Client, progress: &Progress) -> anyhow::Result<(PathBuf, String)> {
    anyhow::ensure!(cfg!(unix), "the PyPI fallback needs a unix shell");
    let python = find_python().context("python3 is needed for the PyPI yt-dlp fallback")?;
    let meta: serde_json::Value = clipcut_core::http::check("pypi", http.get("https://pypi.org/pypi/yt-dlp/json").send().await?)
        .await?
        .json()
        .await?;
    let wheel = meta["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["packagetype"] == "bdist_wheel" && f["filename"].as_str().is_some_and(|n| n.ends_with("py3-none-any.whl")))
        .and_then(|f| f["url"].as_str())
        .context("no universal yt-dlp wheel on PyPI")?
        .to_string();
    progress.info(format!("downloading {wheel}"));
    let whl = bin.join("yt-dlp.whl");
    let tmp = bin.join("yt-dlp.whl.download");
    download_retry(http, &wheel, &tmp, progress).await.context("downloading the yt-dlp wheel")?;
    std::fs::rename(&tmp, &whl)?;
    let script = format!(
        "#!/bin/sh\n# Written by ClipCut: yt-dlp from the PyPI wheel (GitHub was unreachable).\n\
         PYTHONPATH=\"{}${{PYTHONPATH:+:$PYTHONPATH}}\" exec \"{}\" -m yt_dlp \"$@\"\n",
        whl.display(),
        python.display()
    );
    let tmp_script = bin.join("yt-dlp.download");
    std::fs::write(&tmp_script, script)?;
    make_executable(&tmp_script)?;
    let version = match yt_dlp_version(&tmp_script).await {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_script);
            return Err(e.context("yt-dlp wheel does not run with python3"));
        }
    };
    let dest = bin.join("yt-dlp");
    std::fs::rename(&tmp_script, &dest)?;
    progress.info(format!("installed yt-dlp {version} (PyPI wheel via {}) at {}", python.display(), dest.display()));
    Ok((dest, version))
}

async fn install_yt_dlp(bin: &Path, http: &reqwest::Client, progress: &Progress) -> anyhow::Result<PathBuf> {
    match install_standalone(bin, http, progress).await {
        Ok((p, _)) => Ok(p),
        Err(gh) => {
            progress.warn(format!("{gh:#}; falling back to the PyPI wheel"));
            install_wheel(bin, http, progress)
                .await
                .map(|(p, _)| p)
                .map_err(|e| anyhow::anyhow!("could not install yt-dlp. GitHub: {gh:#}. PyPI: {e:#}"))
        }
    }
}

/// Install yt-dlp into `paths.bin()/yt-dlp` (kept if it already works) and
/// return its path. Uses the official standalone build from GitHub, falling
/// back to the PyPI wheel + python3 when GitHub is unreachable.
pub async fn ensure_yt_dlp(paths: &Paths, http: &reqwest::Client, progress: &Progress) -> anyhow::Result<PathBuf> {
    let bin = paths.bin();
    tokio::fs::create_dir_all(&bin).await?;
    let dest = bin.join("yt-dlp");
    if dest.is_file() {
        if let Ok(v) = yt_dlp_version(&dest).await {
            progress.info(format!("yt-dlp {v} already installed at {}", dest.display()));
            return Ok(dest);
        }
        progress.warn("existing yt-dlp is broken; reinstalling");
    }
    install_yt_dlp(&bin, http, progress).await
}

/// Update the managed yt-dlp in `paths.bin()` (installing it when missing).
/// The standalone build self-updates with `-U`; the PyPI-wheel fallback is
/// replaced by a fresh install. Returns its path.
pub async fn update_yt_dlp(paths: &Paths, http: &reqwest::Client, progress: &Progress) -> anyhow::Result<PathBuf> {
    let bin = paths.bin();
    let yt_dlp = ensure_yt_dlp(paths, http, progress).await?;
    let before = yt_dlp_version(&yt_dlp).await.unwrap_or_default();
    if bin.join("yt-dlp.whl").is_file() {
        let p = install_yt_dlp(&bin, http, progress).await?;
        progress.info(format!("yt-dlp {before} -> {}", yt_dlp_version(&p).await?));
        return Ok(p);
    }
    let out = run(&yt_dlp, &["-U"], None, Duration::from_secs(600)).await?;
    for line in out.stdout.lines().chain(out.stderr.lines()).filter(|l| !l.trim().is_empty()) {
        progress.info(format!("yt-dlp: {}", line.trim()));
    }
    progress.info(format!("yt-dlp {before} -> {}", yt_dlp_version(&yt_dlp).await?));
    Ok(yt_dlp)
}

/// `(host, port)` the configured SearXNG URL points at.
pub fn searxng_host_port(searxng_url: &str) -> anyhow::Result<(String, u16)> {
    let u = url::Url::parse(searxng_url.trim()).with_context(|| format!("bad searxng_url {searxng_url:?}"))?;
    let host = u.host_str().unwrap_or("127.0.0.1").to_string();
    let port = u.port_or_known_default().unwrap_or(8888);
    Ok((host, port))
}

fn settings_yml(secret: &str) -> String {
    format!(
        r#"# Written by ClipCut. Extends SearXNG's defaults.
use_default_settings: true
general:
  instance_name: "ClipCut SearXNG"
server:
  secret_key: "{secret}"
  limiter: false
  image_proxy: false
  public_instance: false
search:
  safe_search: 0
  formats:
    - html
    - json
"#
    )
}

/// Write `settings.yml` unless a usable one exists (keeps its secret).
fn write_settings(dir: &Path) -> anyhow::Result<bool> {
    std::fs::create_dir_all(dir)?;
    let file = dir.join("settings.yml");
    if let Ok(existing) = std::fs::read_to_string(&file) {
        let json_on = existing.lines().any(|l| l.trim() == "- json");
        if json_on && existing.contains("use_default_settings") {
            return Ok(false);
        }
    }
    let secret = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    clipcut_core::model::write_atomic(&file, settings_yml(&secret).as_bytes())?;
    Ok(true)
}

/// True when SearXNG answers a JSON search.
pub async fn searxng_json_ok(http: &reqwest::Client, base: &str) -> bool {
    let url = format!("{}/search", base.trim_end_matches('/'));
    let Ok(resp) = http.get(&url).query(&[("q", "test"), ("format", "json")]).timeout(Duration::from_secs(8)).send().await else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    resp.json::<serde_json::Value>().await.map(|v| v.get("results").is_some()).unwrap_or(false)
}

/// Make sure a local SearXNG with the JSON API is running at
/// `config.search.searxng_url`, managing a podman (or docker) container.
pub async fn ensure_searxng(config: &Config, paths: &Paths, tools: &Tools, progress: &Progress) -> anyhow::Result<()> {
    let http = clipcut_core::http::client();
    let base = config.search.searxng_url.trim().trim_end_matches('/').to_string();
    let (host, port) = searxng_host_port(&base)?;
    let local = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0.0.0.0");
    let dir = paths.searxng();
    let settings_changed = write_settings(&dir)?;

    if searxng_json_ok(&http, &base).await && !(settings_changed && local && config.search.manage_searxng) {
        progress.info(format!("SearXNG is up at {base} with JSON enabled"));
        return Ok(());
    }
    if !config.search.manage_searxng || !local {
        anyhow::bail!(
            "SearXNG at {base} is not answering JSON searches and ClipCut does not manage it \
             (enable `search.formats: [html, json]` in its settings.yml)"
        );
    }
    let engine = tools
        .podman
        .as_deref()
        .or(tools.docker.as_deref())
        .ok_or_else(|| anyhow::anyhow!("podman (or docker) is needed to run SearXNG; install podman"))?;
    let engine_name = engine.file_name().unwrap_or_default().to_string_lossy().to_string();

    let inspect = run(engine, &["container", "inspect", "--format", "{{.State.Running}}", SEARXNG_CONTAINER], None, Duration::from_secs(30)).await;
    match inspect {
        Ok(out) => {
            if settings_changed {
                progress.info("SearXNG settings changed; restarting the container");
                run(engine, &["restart", SEARXNG_CONTAINER], None, Duration::from_secs(120)).await?;
            } else if out.stdout.trim() != "true" {
                progress.info(format!("starting {SEARXNG_CONTAINER} with {engine_name}"));
                run(engine, &["start", SEARXNG_CONTAINER], None, Duration::from_secs(120)).await?;
            } else {
                progress.info(format!("{SEARXNG_CONTAINER} is running; waiting for it to answer"));
            }
        }
        Err(_) => {
            progress.info(format!("pulling and starting SearXNG ({SEARXNG_IMAGE}) with {engine_name}; first run downloads the image"));
            let publish = format!("127.0.0.1:{port}:8080");
            let volume = format!("{}:/etc/searxng:Z", dir.display());
            let base_env = format!("SEARXNG_BASE_URL={base}/");
            run(
                engine,
                &[
                    "run",
                    "-d",
                    "--name",
                    SEARXNG_CONTAINER,
                    "--restart",
                    "unless-stopped",
                    "-p",
                    &publish,
                    "-v",
                    &volume,
                    "-e",
                    &base_env,
                    SEARXNG_IMAGE,
                ],
                None,
                Duration::from_secs(900),
            )
            .await
            .context("starting the SearXNG container")?;
        }
    }

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        if searxng_json_ok(&http, &base).await {
            progress.info(format!("SearXNG is up at {base} ({:.0}s)", started.elapsed().as_secs_f32()));
            return Ok(());
        }
        progress.fraction(started.elapsed().as_secs_f32() / 60.0, "waiting for SearXNG");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let logs = run(engine, &["logs", "--tail", "20", SEARXNG_CONTAINER], None, Duration::from_secs(20))
        .await
        .map(|o| format!("{}{}", o.stdout, o.stderr))
        .unwrap_or_default();
    anyhow::bail!("SearXNG did not answer JSON at {base} within 60 s. Container logs:\n{}", logs.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_from_url() {
        assert_eq!(searxng_host_port("http://127.0.0.1:8888").unwrap(), ("127.0.0.1".into(), 8888));
        assert_eq!(searxng_host_port("http://localhost/").unwrap().1, 80);
    }

    #[test]
    fn settings_have_json() {
        let s = settings_yml("abc");
        assert!(s.contains("- json") && s.contains("limiter: false") && s.contains("image_proxy: false"));
    }
}
