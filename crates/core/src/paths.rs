use std::path::{Path, PathBuf};

/// Every on-disk location ClipCut uses. The home directory defaults to
/// `$CLIPCUT_HOME`, then `~/.local/share/clipcut`.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn resolve(explicit: Option<PathBuf>) -> Self {
        let home = explicit
            .or_else(|| std::env::var_os("CLIPCUT_HOME").map(PathBuf::from))
            .or_else(|| dirs::data_dir().map(|d| d.join("clipcut")))
            .unwrap_or_else(|| PathBuf::from(".clipcut"));
        Self { home }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            self.projects(),
            self.cache(),
            self.bin(),
            self.models(),
            self.sfx(),
            self.music(),
            self.searxng(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        self.home.join("config.toml")
    }
    pub fn keys_file(&self) -> PathBuf {
        self.home.join("keys.json")
    }
    pub fn projects(&self) -> PathBuf {
        self.home.join("projects")
    }
    pub fn project(&self, id: &str) -> PathBuf {
        self.projects().join(id)
    }
    /// Shared download cache (meme templates, iconify SVGs, search results).
    pub fn cache(&self) -> PathBuf {
        self.home.join("cache")
    }
    /// Self-managed binaries such as the standalone `yt-dlp`.
    pub fn bin(&self) -> PathBuf {
        self.home.join("bin")
    }
    /// Kokoro ONNX model + voices.
    pub fn models(&self) -> PathBuf {
        self.home.join("models")
    }
    /// Optional user SFX overrides: `<kind>*.wav|mp3` (e.g. `whoosh-2.wav`).
    pub fn sfx(&self) -> PathBuf {
        self.home.join("sfx")
    }
    /// Optional background music beds (any ffmpeg-readable audio).
    pub fn music(&self) -> PathBuf {
        self.home.join("music")
    }
    /// SearXNG settings mounted into the podman container.
    pub fn searxng(&self) -> PathBuf {
        self.home.join("searxng")
    }
}

/// Files inside one project directory.
pub struct ProjectPaths<'a>(pub &'a Path);

impl ProjectPaths<'_> {
    pub fn json(&self) -> PathBuf {
        self.0.join("project.json")
    }
    pub fn assets(&self) -> PathBuf {
        self.0.join("assets")
    }
    pub fn voice(&self) -> PathBuf {
        self.0.join("voice")
    }
    pub fn render(&self) -> PathBuf {
        self.0.join("render")
    }
}
