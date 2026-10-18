//! Locating and running external executables (ffmpeg, yt-dlp, chromium, ...).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

use crate::{Config, Paths};

/// Resolved executable paths. `None` = not installed.
#[derive(Debug, Clone, Serialize)]
pub struct Tools {
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub yt_dlp: Option<PathBuf>,
    pub chromium: Option<PathBuf>,
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub espeak_ng: Option<PathBuf>,
    pub podman: Option<PathBuf>,
    pub docker: Option<PathBuf>,
    /// JS runtime yt-dlp uses for YouTube's player challenges.
    pub deno: Option<PathBuf>,
    pub node: Option<PathBuf>,
}

fn find(overrides: &BTreeMap<String, String>, key: &str, extra_dirs: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    if let Some(path) = overrides.get(key).filter(|p| !p.trim().is_empty()) {
        let p = PathBuf::from(path);
        return p.exists().then_some(p).or_else(|| which::which(path).ok());
    }
    for dir in extra_dirs {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    names.iter().find_map(|n| which::which(n).ok())
}

/// macOS browsers live inside app bundles, which are never on PATH.
fn mac_browser() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = dirs::home_dir().unwrap_or_default();
    [
        "Google Chrome.app/Contents/MacOS/Google Chrome",
        "Chromium.app/Contents/MacOS/Chromium",
        "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "Brave Browser.app/Contents/MacOS/Brave Browser",
    ]
    .iter()
    .flat_map(|app| [PathBuf::from("/Applications").join(app), home.join("Applications").join(app)])
    .find(|p| p.is_file())
}

/// Install command for a system package on this OS, for doctor hints.
pub fn install_hint(package: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("brew install {package}")
    } else {
        format!("install {package} with your package manager (sudo apt install {package} / sudo dnf install {package})")
    }
}

/// The programs `[tools]` in config.toml can point at, as `detect` names them.
pub const TOOL_NAMES: [&str; 11] =
    ["ffmpeg", "ffprobe", "yt-dlp", "chromium", "claude", "codex", "espeak-ng", "podman", "docker", "deno", "node"];

impl Tools {
    pub fn detect(config: &Config, paths: &Paths) -> Self {
        let o = &config.tools;
        let home = dirs::home_dir().unwrap_or_default();
        let extra = vec![
            paths.bin(),
            home.join(".local/bin"),
            home.join(".deno/bin"),
            PathBuf::from("/home/linuxbrew/.linuxbrew/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        Self {
            ffmpeg: find(o, "ffmpeg", &extra, &["ffmpeg"]),
            ffprobe: find(o, "ffprobe", &extra, &["ffprobe"]),
            yt_dlp: find(o, "yt-dlp", &extra, &["yt-dlp", "yt-dlp_linux"]),
            chromium: find(
                o,
                "chromium",
                &extra,
                &["chromium-browser", "chromium", "google-chrome", "google-chrome-stable", "microsoft-edge", "brave-browser", "brave"],
            )
            .or_else(mac_browser),
            claude: find(o, "claude", &extra, &["claude"]),
            codex: find(o, "codex", &extra, &["codex"]),
            espeak_ng: find(o, "espeak-ng", &extra, &["espeak-ng", "espeak"]),
            podman: find(o, "podman", &extra, &["podman"]),
            docker: find(o, "docker", &extra, &["docker"]),
            deno: find(o, "deno", &extra, &["deno"]),
            node: find(o, "node", &extra, &["node"]),
        }
    }

    pub fn require<'a>(tool: &'a Option<PathBuf>, name: &str) -> anyhow::Result<&'a Path> {
        tool.as_deref()
            .ok_or_else(|| anyhow::anyhow!("{name} is not installed. Run `clipcut doctor` to see how to install it."))
    }
}

#[derive(Debug)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

/// Run a command to completion with a timeout; non-zero exit is an error that
/// includes the tail of stderr.
pub async fn run(program: &Path, args: &[&str], cwd: Option<&Path>, timeout: Duration) -> anyhow::Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let child = cmd.spawn().map_err(|e| anyhow::anyhow!("failed to start {}: {e}", program.display()))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("{} timed out after {}s", program.display(), timeout.as_secs()))??;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        let tail: String = stderr.chars().rev().take(800).collect::<Vec<_>>().into_iter().rev().collect();
        anyhow::bail!("{} exited with {}: {}", program.file_name().unwrap_or_default().to_string_lossy(), output.status, tail.trim());
    }
    Ok(Output { stdout, stderr })
}
