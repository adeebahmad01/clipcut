//! Live smoke test against the real providers.
//!
//!     cargo run -p clipcut-sources --example smoke [-- "query"] [--no-setup]
//!
//! Installs yt-dlp / starts SearXNG (idempotent), then exercises every
//! public method. Output goes to /tmp/clipcut-sources-test/.

use std::path::Path;
use std::time::Instant;

use clipcut_core::{Asset, Candidate, Config, KeyPool, Paths, Progress, Tools, VisualKind, VisualPlan};
use clipcut_sources::{SearchCategory, Sources, setup};
use serde_json::json;

fn plan(kind: VisualKind, query: &str, alt: &[&str], extra: serde_json::Value) -> VisualPlan {
    let mut v = json!({"kind": kind, "query": query, "alt_queries": alt});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    serde_json::from_value(v).expect("plan json")
}

fn show_asset(dest: &Path, a: &Asset) {
    let full = dest.parent().unwrap().join(&a.path);
    let size = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
    println!(
        "    -> {} [{:?}] {}x{} dur={:?} trim={:.2} focus={:?} {:.1} KB\n       credit: {}",
        a.path,
        a.kind,
        a.width,
        a.height,
        a.duration_s,
        a.trim_start_s,
        a.focus,
        size as f64 / 1024.0,
        a.credit
    );
}

fn show_cands(c: &[Candidate]) {
    for c in c.iter().take(3) {
        println!(
            "    - [{}] {} | {} | media={} {}",
            c.provider,
            c.title.chars().take(70).collect::<String>(),
            c.page_url,
            c.media_url.as_deref().unwrap_or("-").chars().take(90).collect::<String>(),
            match (c.width, c.height) {
                (Some(w), Some(h)) => format!("{w}x{h}"),
                _ => String::new(),
            }
        );
    }
}

async fn fetch_first(s: &Sources, cands: &[Candidate], dest: &Path, stem: &str, window: Option<(f32, f32)>, quote: Option<&str>, tries: usize) -> bool {
    for (i, c) in cands.iter().take(tries).enumerate() {
        let t = Instant::now();
        match s.fetch(c, dest, stem, window, quote).await {
            Ok(a) => {
                println!("  fetch #{i} ok in {:.1}s ({})", t.elapsed().as_secs_f32(), c.page_url);
                show_asset(dest, &a);
                return true;
            }
            Err(e) => println!("  fetch #{i} FAILED in {:.1}s ({}): {e:#}", t.elapsed().as_secs_f32(), c.page_url),
        }
    }
    false
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info,clipcut_sources=debug".into())).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_setup = args.iter().any(|a| a == "--no-setup");
    let query = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| "James Webb telescope".into());

    let paths = Paths::resolve(None);
    paths.ensure()?;
    let config = Config::load(&paths.config_file())?;
    let progress = Progress::log_only("smoke");
    let http = clipcut_core::http::client();
    if !no_setup {
        println!("== setup");
        match setup::ensure_yt_dlp(&paths, &http, &progress).await {
            Ok(p) => println!("  yt-dlp: {}", p.display()),
            Err(e) => println!("  yt-dlp setup FAILED: {e:#}"),
        }
        let tools = Tools::detect(&config, &paths);
        match setup::ensure_searxng(&config, &paths, &tools, &progress).await {
            Ok(()) => println!("  searxng ok"),
            Err(e) => println!("  searxng setup FAILED: {e:#}"),
        }
    }
    let tools = Tools::detect(&config, &paths);
    let keys = KeyPool::load(paths.keys_file())?;
    let s = Sources::new(config, keys, paths, tools);

    let out = Path::new("/tmp/clipcut-sources-test");
    let dest = out.join("project").join("assets");
    std::fs::create_dir_all(&dest)?;

    println!("== health");
    for h in s.health().await {
        println!("  {:<18} {:<5} {}", h.name, h.ok, h.detail);
    }

    println!("== web_search {query:?}");
    for cat in [SearchCategory::General, SearchCategory::News, SearchCategory::Images, SearchCategory::Videos] {
        let t = Instant::now();
        match s.web_search(&query, cat, 8).await {
            Ok(h) => {
                println!("  {cat:?}: {} hits in {:.1}s (engine {})", h.len(), t.elapsed().as_secs_f32(), h.first().map(|h| h.engine.as_str()).unwrap_or("-"));
                for h in h.iter().take(2) {
                    println!("    - {} | {}", h.title.chars().take(70).collect::<String>(), h.url);
                }
            }
            Err(e) => println!("  {cat:?}: FAILED {e:#}"),
        }
    }

    let topic_alts = ["JWST", "James Webb Space Telescope"];
    let mut by_kind = std::collections::HashMap::new();
    println!("== candidates");
    for kind in [VisualKind::Youtube, VisualKind::Article, VisualKind::Meme, VisualKind::Gif, VisualKind::Image, VisualKind::Stock, VisualKind::Text] {
        let p = match kind {
            VisualKind::Meme => plan(kind, "mind blown", &[], json!({"meme": {"template": "Expanding Brain", "caption": "x"}})),
            VisualKind::Gif => plan(kind, "mind blown", &["wow"], json!({})),
            VisualKind::Stock => plan(kind, "night sky stars", &["galaxy"], json!({})),
            _ => plan(kind, &query, &topic_alts, json!({})),
        };
        let t = Instant::now();
        match s.candidates(kind, &p, 6).await {
            Ok(c) => {
                println!("  {kind:?}: {} candidates in {:.1}s", c.len(), t.elapsed().as_secs_f32());
                show_cands(&c);
                by_kind.insert(format!("{kind:?}"), c);
            }
            Err(e) => println!("  {kind:?}: FAILED {e:#}"),
        }
    }
    let get = |k: &str| by_kind.get(k).cloned().unwrap_or_default();

    println!("== youtube captions + 2 s section");
    let mut yt_done = false;
    for c in get("Youtube").iter().take(4) {
        let t = Instant::now();
        let cues = match s.youtube_captions(&c.page_url).await {
            Ok(c) => c,
            Err(e) => {
                println!("  captions FAILED for {}: {e:#}", c.page_url);
                continue;
            }
        };
        println!("  {} cues for {} in {:.1}s", cues.len(), c.page_url, t.elapsed().as_secs_f32());
        if cues.is_empty() {
            continue;
        }
        for q in cues.iter().take(4) {
            println!("    {:>7.2}-{:<7.2} {}", q.start_s, q.end_s, q.text);
        }
        let quote = cues.get(cues.len() / 3).map(|q| q.text.clone());
        let window = s
            .best_moment_heuristic(&cues, "the telescope sees the early universe in infrared", quote.as_deref(), 2.0)
            .unwrap_or((30.0, 32.0));
        println!("  heuristic window {window:?} for quote {quote:?}");
        let w = (window.0, window.0 + 2.0);
        if fetch_first(&s, std::slice::from_ref(c), &dest, "yt", Some(w), None, 1).await {
            yt_done = true;
            break;
        }
    }
    if !yt_done {
        println!("  youtube section: NOT FETCHED");
    }

    println!("== article capture");
    let articles = get("Article");
    let mut art_ok = false;
    for c in articles.iter().take(5) {
        // Quote: the first sentence of the search snippet, else the headline.
        let snippet = c.extra["snippet"].as_str().unwrap_or_default();
        let quote: String = snippet.split(['.', '!', '?']).next().unwrap_or_default().split_whitespace().take(10).collect::<Vec<_>>().join(" ");
        println!("  trying {} with quote {quote:?}", c.page_url);
        if fetch_first(&s, std::slice::from_ref(c), &dest, "article", None, Some(&quote), 1).await {
            art_ok = true;
            break;
        }
    }
    if !art_ok {
        println!("  article: NOT CAPTURED");
    }

    println!("== icons");
    for q in ["telescope", "mdi:fire", "rocket launch", "zzzzqqqnonsense"] {
        match s.icon_svg(q).await {
            Ok(p) => println!("  {q:?} -> {:?}", p),
            Err(e) => println!("  {q:?} FAILED {e:#}"),
        }
    }

    println!("== memes");
    match s.meme_templates().await {
        Ok(t) => println!("  {} templates; first: {:?}", t.len(), t.first().map(|t| &t.name)),
        Err(e) => println!("  meme_templates FAILED {e:#}"),
    }
    fetch_first(&s, &get("Meme"), &dest, "meme", None, None, 2).await;

    println!("== gif");
    fetch_first(&s, &get("Gif"), &dest, "gif", None, None, 4).await;
    println!("== image");
    fetch_first(&s, &get("Image"), &dest, "image", None, None, 4).await;
    println!("== stock");
    fetch_first(&s, &get("Stock"), &dest, "stock", None, None, 3).await;

    clipcut_sources::shutdown_browser().await;
    println!("== done; files in {}", dest.display());
    Ok(())
}
