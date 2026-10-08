//! Backend checks for the doctor page.

use std::path::Path;
use std::time::Duration;

use clipcut_core::tools::run;

use crate::{SourceHealth, Sources};

fn h(name: &str, ok: bool, detail: impl Into<String>) -> SourceHealth {
    SourceHealth { name: name.into(), ok, detail: detail.into() }
}

async fn version_of(tool: Option<&Path>, name: &str, args: &[&str]) -> SourceHealth {
    let Some(path) = tool else { return h(name, false, "not installed") };
    match run(path, args, None, Duration::from_secs(20)).await {
        Ok(o) => {
            let v = o.stdout.lines().next().unwrap_or_default().trim().to_string();
            h(name, true, format!("{v} ({})", path.display()))
        }
        Err(e) => h(name, false, format!("{} fails: {e:#}", path.display())),
    }
}

/// Optional keyed services and what replaces them when no key is set.
const KEYED: &[(&str, &str)] = &[
    ("giphy", "keyless Tenor/web GIF search"),
    ("tenor", "keyless Tenor/web GIF search"),
    ("pexels", "Wikimedia/Openverse/web images and Commons video"),
    ("pixabay", "Wikimedia Commons video"),
    ("unsplash", "Wikimedia/Openverse/web images"),
    ("brave", "SearXNG / DuckDuckGo"),
    ("tavily", "SearXNG / DuckDuckGo"),
];

impl Sources {
    pub(crate) async fn health_impl(&self) -> Vec<SourceHealth> {
        let base = self.searxng_base();
        let searxng = async {
            if crate::setup::searxng_json_ok(&self.http, &base).await {
                h("searxng", true, format!("{base} answers JSON searches"))
            } else {
                let reachable = self.http.get(&base).timeout(Duration::from_secs(5)).send().await.is_ok();
                let why = if reachable { "reachable but the JSON format is disabled" } else { "not reachable" };
                let fix = if self.config.search.manage_searxng { " (run setup to start the container)" } else { "" };
                h("searxng", false, format!("{base} {why}{fix}"))
            }
        };
        let ddg = async {
            if !self.config.search.duckduckgo_fallback {
                return h("duckduckgo", true, "fallback disabled in config");
            }
            match tokio::time::timeout(Duration::from_secs(20), self.duckduckgo("james webb telescope", crate::SearchCategory::General)).await {
                Ok(Ok(hits)) if !hits.is_empty() => h("duckduckgo", true, format!("{} results", hits.len())),
                Ok(Ok(_)) => h("duckduckgo", false, "reachable but returned no results (rate limited?)"),
                Ok(Err(e)) => h("duckduckgo", false, format!("{e:#}")),
                Err(_) => h("duckduckgo", false, "timed out"),
            }
        };
        let yt = version_of(self.tools.yt_dlp.as_deref(), "yt-dlp", &["--version"]);
        let chromium = version_of(self.tools.chromium.as_deref(), "chromium", &["--version"]);
        let ffmpeg = version_of(self.tools.ffmpeg.as_deref(), "ffmpeg", &["-version"]);
        let (searxng, ddg, yt, chromium, ffmpeg) = tokio::join!(searxng, ddg, yt, chromium, ffmpeg);
        let js = match (&self.tools.deno, &self.tools.node) {
            (Some(d), _) => h("yt-dlp js runtime", true, format!("deno ({})", d.display())),
            (None, Some(n)) => h("yt-dlp js runtime", true, format!("node ({}), passed via --js-runtimes", n.display())),
            _ => h("yt-dlp js runtime", false, "neither deno nor node found; some YouTube formats will be missing"),
        };
        let mut out = vec![searxng, ddg, yt, js, chromium, ffmpeg];
        for (service, fallback) in KEYED {
            let detail = if self.keys.has(service) { "key configured".to_string() } else { format!("no key; using {fallback}") };
            out.push(h(&format!("{service} key"), true, detail));
        }
        out
    }
}
