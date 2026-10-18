use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use crate::timeline::CaptionPreset;

/// `config.toml` in the ClipCut home. Missing fields use their defaults;
/// explicitly configured values are checked by `validate`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub llm: LlmConfig,
    pub video: VideoConfig,
    pub search: SearchConfig,
    pub youtube: YoutubeConfig,
    pub tts: TtsConfig,
    pub audio: AudioConfig,
    pub publish: PublishConfig,
    /// Executable overrides by tool name: ffmpeg, ffprobe, yt-dlp, chromium,
    /// claude, codex, espeak-ng, podman.
    pub tools: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Open the UI in a Chromium `--app` window on start.
    pub open_window: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { host: "0.0.0.0".into(), port: 7878, open_window: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// `claude` (Claude Code CLI) or `codex` (Codex CLI).
    pub provider: String,
    /// Model for creative stages (hooks, script, beat sheet).
    pub model: String,
    /// Model for cheap structured picks (meme template, clip moment).
    pub fast_model: String,
    pub timeout_seconds: u64,
    /// unslop preset: crisp | warm | expert | story
    pub unslop_preset: String,
    /// Skip the unslop pass (faster, lower quality).
    pub skip_unslop: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "claude".into(),
            model: "opus".into(),
            fast_model: "sonnet".into(),
            timeout_seconds: 600,
            unslop_preset: "crisp".into(),
            skip_unslop: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// 1080x1920 Shorts / Reels / TikTok
    #[default]
    Vertical,
    /// 1920x1080 long-form
    Horizontal,
    /// 1080x1080 feed posts
    Square,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Vertical, Format::Horizontal, Format::Square];

    pub fn size(self) -> (u32, u32) {
        match self {
            Format::Vertical => (1080, 1920),
            Format::Horizontal => (1920, 1080),
            Format::Square => (1080, 1080),
        }
    }

    /// snake_case name, as used in serde and in variant file names.
    pub fn slug(self) -> &'static str {
        match self {
            Format::Vertical => "vertical",
            Format::Horizontal => "horizontal",
            Format::Square => "square",
        }
    }

    /// Width:height, for labels.
    pub fn ratio(self) -> &'static str {
        match self {
            Format::Vertical => "9:16",
            Format::Horizontal => "16:9",
            Format::Square => "1:1",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    pub format: Format,
    pub fps: u32,
    pub target_seconds: u32,
    pub min_clip_seconds: f32,
    pub max_clip_seconds: f32,
    pub captions: bool,
    /// Caption look for new projects: pop, hormozi, mrbeast, minimal, karaoke.
    pub caption_preset: CaptionPreset,
    /// x264 CRF (lower = better quality, bigger file).
    pub crf: u8,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            format: Format::Vertical,
            fps: 30,
            target_seconds: 45,
            min_clip_seconds: 1.0,
            max_clip_seconds: 3.0,
            captions: true,
            caption_preset: CaptionPreset::Pop,
            crf: 20,
        }
    }
}

/// Defaults for uploads to YouTube.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PublishConfig {
    /// private, unlisted or public.
    pub privacy: String,
    /// YouTube category id; 22 is People & Blogs, 24 Entertainment, 27 Education.
    pub category_id: String,
    /// Declare uploads as made for kids (COPPA).
    pub made_for_kids: bool,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self { privacy: "private".into(), category_id: "22".into(), made_for_kids: false }
    }
}

pub const PRIVACY_VALUES: [&str; 3] = ["private", "unlisted", "public"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Self-hosted SearXNG with the JSON format enabled.
    pub searxng_url: String,
    /// Let `clipcut setup` / the server manage a podman container for it.
    pub manage_searxng: bool,
    /// Fall back to DuckDuckGo HTML results when SearXNG is unreachable.
    pub duckduckgo_fallback: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { searxng_url: "http://127.0.0.1:8888".into(), manage_searxng: true, duckduckgo_fallback: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct YoutubeConfig {
    /// Search results considered per beat.
    pub max_results: u32,
    /// Skip videos longer than this (minutes); long streams make caption search slow.
    pub max_video_minutes: u32,
    /// Passed to `yt-dlp --cookies-from-browser` when YouTube asks for sign-in.
    pub cookies_from_browser: Option<String>,
    /// Channel names or ids never to clip from (e.g. known claimers).
    pub channel_blocklist: Vec<String>,
}

impl Default for YoutubeConfig {
    fn default() -> Self {
        Self { max_results: 8, max_video_minutes: 40, cookies_from_browser: None, channel_blocklist: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TtsConfig {
    /// `kokoro` (local ONNX), `espeak` (robotic fallback) or `command`.
    pub engine: String,
    /// Kokoro voice id, e.g. am_michael, af_heart, bm_george.
    pub voice: String,
    pub speed: f32,
    /// For `engine = "command"`: shell template with `{text_file}` and `{out_wav}`.
    pub command: String,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self { engine: "kokoro".into(), voice: "am_michael".into(), speed: 1.1, command: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub voice_volume: f32,
    pub sfx_volume: f32,
    pub music_volume: f32,
    /// Lower the music under narration.
    pub duck_music: bool,
    /// Pick a random bed from the music folder when present.
    pub use_music: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { voice_volume: 1.0, sfx_volume: 0.55, music_volume: 0.14, duck_music: true, use_music: true }
    }
}

impl Config {
    /// Validate user-editable settings before persisting or using them.
    /// No values are silently clamped and all errors identify the field.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.server.port != 0, "server.port must be nonzero");
        let host = &self.server.host;
        let hostname = host.strip_suffix('.').unwrap_or(host);
        let valid_hostname = !hostname.is_empty()
            && hostname.len() <= 253
            && hostname.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        anyhow::ensure!(
            host.parse::<std::net::IpAddr>().is_ok() || valid_hostname,
            "server.host must be an IP address or hostname without a scheme, port, or whitespace"
        );
        anyhow::ensure!(
            matches!(self.llm.provider.trim().to_ascii_lowercase().as_str(), "claude" | "codex"),
            "llm.provider must be claude or codex"
        );
        anyhow::ensure!(
            (30..=1800).contains(&self.llm.timeout_seconds),
            "llm.timeout_seconds must be between 30 and 1800"
        );
        anyhow::ensure!(
            matches!(self.llm.unslop_preset.trim(), "crisp" | "warm" | "expert" | "story"),
            "llm.unslop_preset must be crisp, warm, expert, or story"
        );
        anyhow::ensure!((1..=60).contains(&self.video.fps), "video.fps must be between 1 and 60");
        anyhow::ensure!(
            (10..=600).contains(&self.video.target_seconds),
            "video.target_seconds must be between 10 and 600"
        );
        finite_range("video.min_clip_seconds", self.video.min_clip_seconds, 1.0, 3.0)?;
        finite_range("video.max_clip_seconds", self.video.max_clip_seconds, 1.0, 3.0)?;
        anyhow::ensure!(
            self.video.min_clip_seconds <= self.video.max_clip_seconds,
            "video.min_clip_seconds must not exceed video.max_clip_seconds"
        );
        anyhow::ensure!(self.video.crf <= 51, "video.crf must be between 0 and 51");
        let engine = self.tts.engine.trim().to_ascii_lowercase();
        anyhow::ensure!(
            matches!(engine.as_str(), "kokoro" | "espeak" | "command"),
            "tts.engine must be kokoro, espeak, or command"
        );
        if engine == "command" {
            anyhow::ensure!(
                self.tts.command.contains("{text_file}") && self.tts.command.contains("{out_wav}"),
                "tts.command must contain {{text_file}} and {{out_wav}} when tts.engine is command"
            );
        }
        finite_range("tts.speed", self.tts.speed, 0.5, 2.0)?;
        finite_range("audio.voice_volume", self.audio.voice_volume, 0.0, 2.0)?;
        finite_range("audio.sfx_volume", self.audio.sfx_volume, 0.0, 2.0)?;
        finite_range("audio.music_volume", self.audio.music_volume, 0.0, 2.0)?;
        anyhow::ensure!(
            PRIVACY_VALUES.contains(&self.publish.privacy.as_str()),
            "publish.privacy must be private, unlisted, or public"
        );
        let category = &self.publish.category_id;
        anyhow::ensure!(
            (1..=3).contains(&category.len()) && category.bytes().all(|b| b.is_ascii_digit()),
            "publish.category_id must be a numeric YouTube category id such as 22"
        );

        let endpoint = &self.search.searxng_url;
        anyhow::ensure!(
            !endpoint.chars().any(char::is_whitespace) && !endpoint.chars().any(char::is_control),
            "search.searxng_url must not contain whitespace or control characters"
        );
        let url = reqwest::Url::parse(endpoint)
            .map_err(|_| anyhow::anyhow!("search.searxng_url must be an absolute HTTP(S) URL"))?;
        let authority = endpoint.split_once("://").map(|(_, rest)| rest.split('/').next().unwrap_or(""));
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && authority.is_some_and(|s| !s.is_empty())
                && url.port_or_known_default() != Some(0)
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "search.searxng_url must be an absolute HTTP(S) base URL with a host and nonzero port, without credentials, query, or fragment"
        );
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let config = Self::read(path)?;
        config.validate()?;
        Ok(config)
    }

    /// Read `path`, or the defaults when it doesn't exist, without checking the
    /// values, so `clipcut config` can still show and fix one that is out of range.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        self.validate()?;
        crate::model::write_atomic(path, self.to_toml()?.as_bytes())
    }

    /// The settings as config.toml holds them.
    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }
}

fn finite_range(field: &str, value: f32, min: f32, max: f32) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.is_finite() && (min..=max).contains(&value),
        "{field} must be finite and between {min} and {max}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn defaults_and_partial_configs_are_valid() {
        Config::default().validate().unwrap();
        let dir = TestDir::new();
        let path = dir.path().join("config.toml");
        Config::load(&path).unwrap().validate().unwrap();
        std::fs::write(&path, "[video]\nfps = 60\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.video.fps, 60);
        assert_eq!(config.video.target_seconds, 45);
        assert_eq!(config.tts.speed, 1.1);
    }

    #[test]
    fn valid_range_endpoints_are_inclusive() {
        for upper in [false, true] {
            let mut config = Config::default();
            config.server.port = if upper { u16::MAX } else { 1 };
            config.video.fps = if upper { 60 } else { 1 };
            config.video.target_seconds = if upper { 600 } else { 10 };
            config.video.min_clip_seconds = if upper { 3.0 } else { 1.0 };
            config.video.max_clip_seconds = config.video.min_clip_seconds;
            config.video.crf = if upper { 51 } else { 0 };
            config.tts.speed = if upper { 2.0 } else { 0.5 };
            config.llm.timeout_seconds = if upper { 1800 } else { 30 };
            config.audio.voice_volume = if upper { 2.0 } else { 0.0 };
            config.audio.sfx_volume = config.audio.voice_volume;
            config.audio.music_volume = config.audio.voice_volume;
            config.validate().unwrap();
        }
    }

    #[test]
    fn rejects_invalid_integer_ranges_with_field_names() {
        let cases: &[(&str, fn(&mut Config))] = &[
            ("server.port", |c| c.server.port = 0),
            ("video.fps", |c| c.video.fps = 0),
            ("video.fps", |c| c.video.fps = 61),
            ("video.target_seconds", |c| c.video.target_seconds = 9),
            ("video.target_seconds", |c| c.video.target_seconds = 601),
            ("video.crf", |c| c.video.crf = 52),
            ("llm.timeout_seconds", |c| c.llm.timeout_seconds = 29),
            ("llm.timeout_seconds", |c| c.llm.timeout_seconds = 1801),
        ];
        for (field, change) in cases {
            let mut config = Config::default();
            change(&mut config);
            assert!(config.validate().unwrap_err().to_string().contains(field));
        }
    }

    #[test]
    fn rejects_nonfinite_and_out_of_range_floats() {
        let cases: &[(&str, fn(&mut Config, f32), f32, f32)] = &[
            ("video.min_clip_seconds", |c, v| c.video.min_clip_seconds = v, 1.0, 3.0),
            ("video.max_clip_seconds", |c, v| c.video.max_clip_seconds = v, 1.0, 3.0),
            ("tts.speed", |c, v| c.tts.speed = v, 0.5, 2.0),
            ("audio.voice_volume", |c, v| c.audio.voice_volume = v, 0.0, 2.0),
            ("audio.sfx_volume", |c, v| c.audio.sfx_volume = v, 0.0, 2.0),
            ("audio.music_volume", |c, v| c.audio.music_volume = v, 0.0, 2.0),
        ];
        for (field, change, min, max) in cases {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, min - 0.01, max + 0.01] {
                let mut config = Config::default();
                change(&mut config, value);
                assert!(config.validate().unwrap_err().to_string().contains(field), "{field} accepted {value}");
            }
        }
    }

    #[test]
    fn rejects_reversed_clip_bounds() {
        let mut config = Config::default();
        config.video.min_clip_seconds = 2.5;
        config.video.max_clip_seconds = 2.0;
        assert!(config.validate().unwrap_err().to_string().contains("must not exceed"));
    }

    #[test]
    fn supported_enum_values_are_valid() {
        let mut config = Config::default();
        for provider in ["claude", "codex", " Codex "] {
            config.llm.provider = provider.into();
            config.validate().unwrap();
        }
        for preset in ["crisp", "warm", "expert", "story"] {
            config.llm.unslop_preset = preset.into();
            config.validate().unwrap();
        }
        for engine in ["kokoro", "espeak", "command"] {
            config.tts.engine = engine.into();
            config.tts.command = "voice --input {text_file} --output {out_wav}".into();
            config.validate().unwrap();
        }
    }

    #[test]
    fn rejects_unknown_enum_values() {
        let cases: &[(&str, fn(&mut Config))] = &[
            ("llm.provider", |c| c.llm.provider = "unknown".into()),
            ("llm.unslop_preset", |c| c.llm.unslop_preset = "unknown".into()),
            ("tts.engine", |c| c.tts.engine = "unknown".into()),
        ];
        for (field, change) in cases {
            let mut config = Config::default();
            change(&mut config);
            assert!(config.validate().unwrap_err().to_string().contains(field));
        }
    }

    #[test]
    fn publish_settings_are_checked() {
        let mut config = Config::default();
        for privacy in PRIVACY_VALUES {
            config.publish.privacy = privacy.into();
            config.validate().unwrap();
        }
        config.publish.privacy = "secret".into();
        assert!(config.validate().unwrap_err().to_string().contains("publish.privacy"));
        config.publish.privacy = "private".into();
        for category in ["", "abc", "1234", "2 2"] {
            config.publish.category_id = category.into();
            assert!(config.validate().unwrap_err().to_string().contains("publish.category_id"));
        }
    }

    #[test]
    fn formats_and_presets_round_trip_through_toml() {
        let dir = TestDir::new();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[video]\nformat = \"square\"\ncaption_preset = \"hormozi\"\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.video.format, Format::Square);
        assert_eq!(config.video.format.size(), (1080, 1080));
        assert_eq!(config.video.caption_preset, CaptionPreset::Hormozi);
        for format in Format::ALL {
            let (w, h) = format.size();
            assert!(w % 2 == 0 && h % 2 == 0);
            assert_eq!(serde_json::to_value(format).unwrap(), format.slug());
        }
    }

    #[test]
    fn command_engine_requires_both_placeholders() {
        let mut config = Config::default();
        config.tts.engine = "command".into();
        for command in ["", "voice {text_file}", "voice {out_wav}", "voice fixed.wav"] {
            config.tts.command = command.into();
            assert!(config.validate().unwrap_err().to_string().contains("tts.command"));
        }
        config.tts.command = "voice {text_file} > {out_wav}".into();
        config.validate().unwrap();
    }

    #[test]
    fn validates_bind_hosts_without_dns_lookups() {
        let mut config = Config::default();
        for host in ["0.0.0.0", "127.0.0.1", "::", "::1", "localhost", "studio.local", "studio.local."] {
            config.server.host = host.into();
            config.validate().unwrap();
        }
        for host in [
            "",
            " ",
            " localhost",
            "http://localhost",
            "localhost:7878",
            "user@host",
            "host/name",
            "-host",
            "host..local",
        ] {
            config.server.host = host.into();
            assert!(config.validate().unwrap_err().to_string().contains("server.host"));
        }
    }

    #[test]
    fn accepts_http_endpoints_including_lan_ipv6_and_path_prefixes() {
        let mut config = Config::default();
        for endpoint in [
            "http://127.0.0.1:8888",
            "http://192.168.1.20:8888/",
            "http://[::1]:8888",
            "https://search.example.test/searxng",
        ] {
            config.search.searxng_url = endpoint.into();
            config.validate().unwrap();
        }
    }

    #[test]
    fn rejects_invalid_or_ambiguous_search_endpoints() {
        let mut config = Config::default();
        for endpoint in [
            "",
            "localhost:8888",
            "/search",
            "ftp://search.test",
            "file:///tmp/search",
            "http:///search",
            "http://search.test:0",
            "http://user:password@search.test",
            "http://search.test?query=x",
            "http://search.test#fragment",
            " http://search.test",
            "http://search.test\n",
            "http://search.test/a b",
        ] {
            config.search.searxng_url = endpoint.into();
            assert!(config.validate().unwrap_err().to_string().contains("search.searxng_url"), "{endpoint}");
        }
    }

    #[test]
    fn invalid_save_preserves_existing_config() {
        let dir = TestDir::new();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        config.video.fps = 0;
        assert!(config.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(dir.entries(), vec![path]);
    }

    #[test]
    fn invalid_loaded_config_is_rejected() {
        let dir = TestDir::new();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[video]\nfps = 0\n").unwrap();
        assert!(Config::load(&path).unwrap_err().to_string().contains("video.fps"));
    }
}
