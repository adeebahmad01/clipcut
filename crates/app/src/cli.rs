//! Command-line batch, clip, publish and ideas. When the app is already
//! running on this machine they go through its API, so the two never write
//! the same queue or sign-in at once; otherwise they run here.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use clipcut_core::config::Format;
use clipcut_core::timeline::CaptionPreset;
use clipcut_sources::IdeaSource;
use serde_json::{Value, json};

use crate::clips::{self, ClipRequest};
use crate::publish;
use crate::queue::{self, AddBody, ItemOptions, ItemStatus, PublishPlan, QueueItem};
use crate::state::AppState;

pub fn parse_format(s: &str) -> Result<Format, String> {
    serde_json::from_value(Value::String(s.trim().to_ascii_lowercase()))
        .map_err(|_| "one of vertical, horizontal, square".to_string())
}

pub fn parse_captions(s: &str) -> Result<CaptionPreset, String> {
    serde_json::from_value(Value::String(s.trim().to_ascii_lowercase()))
        .map_err(|_| "one of pop, hormozi, mrbeast, minimal, karaoke".to_string())
}

/// Shape and caption options shared by `new`, `batch` and `clip`.
#[derive(clap::Args, Debug, Default)]
pub struct VideoOpts {
    /// Shortcut for --format horizontal.
    #[arg(long, conflicts_with = "format")]
    pub horizontal: bool,
    /// vertical (9:16), horizontal (16:9) or square (1:1).
    #[arg(long, value_parser = parse_format)]
    pub format: Option<Format>,
    /// Caption style: pop, hormozi, mrbeast, minimal or karaoke.
    #[arg(long, value_parser = parse_captions)]
    pub captions: Option<CaptionPreset>,
    /// Also export these formats from the same edit, e.g. --also horizontal,square.
    #[arg(long, value_delimiter = ',', value_parser = parse_format)]
    pub also: Vec<Format>,
}

impl VideoOpts {
    pub fn format(&self) -> Option<Format> {
        self.format
            .or(self.horizontal.then_some(Format::Horizontal))
    }
}

/// `2026-10-12T15:00:00Z`, or local time as `2026-10-12 15:00`.
pub fn parse_time(s: &str) -> Result<DateTime<Utc>, String> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .and_then(|t| t.and_local_timezone(Local).single())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| "use 2026-10-12 15:00 (local) or 2026-10-12T15:00:00Z".into())
}

/// `90m`, `2h`, `1d` or plain minutes.
pub fn parse_every(s: &str) -> Result<u32, String> {
    let s = s.trim().to_ascii_lowercase();
    let (n, unit) = match s.char_indices().last() {
        Some((i, c @ ('m' | 'h' | 'd'))) => (&s[..i], c),
        _ => (s.as_str(), 'm'),
    };
    let n: u32 = n.trim().parse().map_err(|_| "use 90m, 2h or 1d".to_string())?;
    let minutes = match unit {
        'h' => n.checked_mul(60),
        'd' => n.checked_mul(24 * 60),
        _ => Some(n),
    };
    minutes
        .filter(|m| *m > 0)
        .ok_or_else(|| "the interval must be at least 1 minute".into())
}

fn privacy_plan(
    privacy: Option<String>,
    start: Option<DateTime<Utc>>,
) -> anyhow::Result<Option<PublishPlan>> {
    let Some(privacy) = privacy else {
        anyhow::ensure!(start.is_none(), "--start needs --publish");
        return Ok(None);
    };
    let plan = PublishPlan {
        privacy: privacy.to_ascii_lowercase(),
        publish_at: start,
    };
    queue::validate_plan(&plan).map_err(anyhow::Error::msg)?;
    Ok(Some(plan))
}

// ---------------------------------------------------------------------------
// Talking to a running app

/// The API of the app running on this machine, if any.
async fn running_app(state: &AppState) -> Option<String> {
    let port = state.config().server.port;
    let base = format!("http://127.0.0.1:{port}/api");
    let resp = reqwest::Client::new()
        .get(format!("{base}/queue"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .ok()?;
    resp.status().is_success().then_some(base)
}

async fn api(method: reqwest::Method, url: &str, body: Option<Value>) -> anyhow::Result<Value> {
    let mut req = reqwest::Client::new().request(method, url);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await.context("could not reach the running app")?;
    let status = resp.status();
    let value: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        anyhow::bail!("{}", value["error"].as_str().unwrap_or(status.as_str()));
    }
    Ok(value)
}

fn ensure_connected(state: &AppState) -> anyhow::Result<()> {
    anyhow::ensure!(
        state.publish.status().connected,
        "YouTube isn't connected; run `clipcut publish --connect` first"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// batch and clip

pub struct BatchArgs {
    pub file: std::path::PathBuf,
    pub publish: Option<String>,
    pub every: Option<u32>,
    pub start: Option<DateTime<Utc>>,
    pub video: VideoOpts,
    pub seconds: Option<u32>,
}

/// One topic per line; blank lines and `#` comments are skipped.
pub fn read_topics(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

pub async fn batch(state: AppState, args: BatchArgs) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(&args.file)
        .with_context(|| format!("could not read {}", args.file.display()))?;
    let topics = read_topics(&text);
    anyhow::ensure!(!topics.is_empty(), "{} has no topics", args.file.display());
    let plan = privacy_plan(args.publish, args.start)?;
    anyhow::ensure!(
        args.every.is_none() || plan.is_some(),
        "--every needs --publish"
    );
    let body = AddBody {
        items: Vec::new(),
        topics,
        options: ItemOptions {
            format: args.video.format(),
            target_seconds: args.seconds,
            voice: None,
            caption_preset: args.video.captions,
            extra_formats: args.video.also.clone(),
            publish: plan,
        },
        publish_every_minutes: args.every,
    };
    queue::validate(&body).map_err(anyhow::Error::msg)?;

    if let Some(base) = running_app(&state).await {
        let added = api(
            reqwest::Method::POST,
            &format!("{base}/queue"),
            Some(json!({
                "topics": body.topics,
                "format": body.options.format,
                "target_seconds": body.options.target_seconds,
                "caption_preset": body.options.caption_preset,
                "extra_formats": body.options.extra_formats,
                "publish": body.options.publish,
                "publish_every_minutes": body.publish_every_minutes,
            })),
        )
        .await?;
        let n = added.as_array().map_or(0, Vec::len);
        println!("Queued {n} topics in the running app; follow them in its Queue tab.");
        return Ok(());
    }

    if body.options.publish.is_some() {
        ensure_connected(&state)?;
    }
    let added = state.queue.add(body)?;
    println!("Queued {} topics.", added.len());
    drain(&state, &added).await
}

pub struct ClipArgs {
    pub url: String,
    pub count: usize,
    pub min: f32,
    pub max: f32,
    pub video: VideoOpts,
    pub publish: Option<String>,
    pub every: Option<u32>,
    pub start: Option<DateTime<Utc>>,
}

pub async fn clip(state: AppState, args: ClipArgs) -> anyhow::Result<()> {
    let plan = privacy_plan(args.publish, args.start)?;
    anyhow::ensure!(
        args.every.is_none() || plan.is_some(),
        "--every needs --publish"
    );
    queue::validate_spacing(args.every).map_err(anyhow::Error::msg)?;
    let mut req = ClipRequest::new(args.url.trim());
    req.count = args.count;
    req.min_s = args.min;
    req.max_s = args.max;
    req.format = args.video.format();
    req.caption_preset = args.video.captions;
    req.extra_formats = args.video.also.clone();
    anyhow::ensure!(
        (1..=clips::MAX_CLIPS).contains(&req.count),
        "--count must be between 1 and {}",
        clips::MAX_CLIPS
    );
    anyhow::ensure!(
        (5.0..=180.0).contains(&req.min_s) && (5.0..=180.0).contains(&req.max_s),
        "--min and --max must be 5–180 seconds"
    );
    anyhow::ensure!(req.min_s <= req.max_s, "--min must not exceed --max");

    if let Some(base) = running_app(&state).await {
        println!("Finding moments in the running app (this can take a few minutes)…");
        let projects = api(
            reqwest::Method::POST,
            &format!("{base}/clips"),
            Some(json!({
                "url": req.url,
                "count": req.count,
                "min_s": req.min_s,
                "max_s": req.max_s,
                "format": req.format,
                "caption_preset": req.caption_preset,
                "extra_formats": req.extra_formats,
                "publish": plan,
                "publish_every_minutes": args.every,
            })),
        )
        .await?;
        for p in projects.as_array().into_iter().flatten() {
            println!(
                "  {}  {}",
                p["id"].as_str().unwrap_or_default(),
                p["topic"].as_str().unwrap_or_default()
            );
        }
        println!("Queued in the running app; follow them in its Queue tab.");
        return Ok(());
    }

    if plan.is_some() {
        ensure_connected(&state)?;
    }
    let progress = state.progress("clips");
    let projects = clips::create(&state, &req, &progress).await?;
    anyhow::ensure!(!projects.is_empty(), "no moments were found");
    for p in &projects {
        println!("  {}  {}", p.id, p.topic);
    }
    let added = state.queue.add_projects(&projects, plan, args.every)?;
    drain(&state, &added).await
}

/// Run the queue here until it is empty, then report on `ours`.
async fn drain(state: &AppState, ours: &[QueueItem]) -> anyhow::Result<()> {
    let snapshot = state.queue.snapshot();
    let earlier = snapshot
        .items
        .iter()
        .filter(|i| i.status == ItemStatus::Waiting && !ours.iter().any(|o| o.id == i.id))
        .count();
    if earlier > 0 {
        println!("The queue also has {earlier} earlier items; they run first.");
    }
    if snapshot.paused {
        println!("Resuming the paused queue.");
        state.queue.set_paused(false)?;
    }
    queue::worker(state.clone(), true).await;

    let items = state.queue.snapshot().items;
    let mut failed = 0;
    println!();
    for item in ours {
        let Some(item) = items.iter().find(|i| i.id == item.id) else {
            continue;
        };
        let video = item
            .project_id
            .as_deref()
            .and_then(|id| Some((id, state.store.get(id).ok()?.render?)))
            .map(|(id, r)| state.store.dir(id).join(r.path).display().to_string());
        match item.status {
            ItemStatus::Done => {
                println!("[done]  {}", item.topic);
                if let Some(video) = video {
                    println!("        {video}");
                }
                if let Some(url) = &item.video_url {
                    println!("        {url}");
                }
            }
            status => {
                failed += 1;
                println!("[{}]  {}", format!("{status:?}").to_lowercase(), item.topic);
                if let Some(e) = &item.error {
                    println!("        {e}");
                }
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} of {} videos didn't finish", ours.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// publish

pub struct PublishArgs {
    pub id: Option<String>,
    pub privacy: Option<String>,
    pub at: Option<DateTime<Utc>>,
    pub format: Option<Format>,
    pub connect: bool,
    pub client: Option<std::path::PathBuf>,
    pub disconnect: bool,
}

pub async fn publish(state: AppState, args: PublishArgs) -> anyhow::Result<()> {
    if let Some(file) = &args.client {
        set_client(&state, file).await?;
    }
    if args.disconnect {
        state.publish.disconnect(false).await?;
        println!("YouTube disconnected.");
        return Ok(());
    }
    if args.connect {
        return connect(&state).await;
    }
    let Some(id) = args.id else {
        if args.client.is_some() {
            println!("Next: clipcut publish --connect");
            return Ok(());
        }
        anyhow::bail!("give a project id, or --connect to sign in to YouTube");
    };
    let privacy = args
        .privacy
        .unwrap_or_else(|| state.config().publish.privacy);
    let plan = PublishPlan {
        privacy: privacy.to_ascii_lowercase(),
        publish_at: args.at,
    };
    queue::validate_plan(&plan).map_err(anyhow::Error::msg)?;

    let url = if let Some(base) = running_app(&state).await {
        println!("Uploading through the running app…");
        let record = api(
            reqwest::Method::POST,
            &format!("{base}/projects/{id}/publish"),
            Some(json!({
                "format": args.format,
                "privacy": plan.privacy,
                "publish_at": plan.publish_at,
            })),
        )
        .await?;
        record["url"].as_str().unwrap_or_default().to_string()
    } else {
        anyhow::ensure!(state.store.exists(&id), "project {id} not found");
        ensure_connected(&state)?;
        let progress = state.progress(&id);
        publish::upload(
            &state,
            &id,
            args.format,
            &plan.privacy,
            plan.publish_at,
            &progress,
        )
        .await?
        .url
    };
    println!("Published: {url}");
    if let Some(at) = plan.publish_at.filter(|at| *at > Utc::now()) {
        println!(
            "It stays private until {}.",
            at.with_timezone(&Local).format("%Y-%m-%d %H:%M")
        );
    }
    Ok(())
}

async fn set_client(state: &AppState, file: &Path) -> anyhow::Result<()> {
    let json = std::fs::read_to_string(file)
        .with_context(|| format!("could not read {}", file.display()))?;
    let body = publish::ClientBody {
        client_json: Some(json),
        ..Default::default()
    };
    let (id, secret) = publish::parse_client(&body).map_err(anyhow::Error::msg)?;
    state.publish.set_client(id, secret).await?;
    println!("Saved the Google OAuth client.");
    Ok(())
}

/// Sign in to YouTube: open Google's page, then wait for its redirect, or for
/// the redirected address pasted here when the browser is on another device.
async fn connect(state: &AppState) -> anyhow::Result<()> {
    let app = running_app(state).await;
    let (url, listener) = match &app {
        Some(base) => {
            let v = api(reqwest::Method::POST, &format!("{base}/publish/youtube/connect"), None)
                .await?;
            (v["url"].as_str().unwrap_or_default().to_string(), None)
        }
        None => {
            let (url, listener) = publish::begin_connect(state).await?;
            (url, Some(listener))
        }
    };
    println!("Sign in to YouTube here:\n\n  {url}\n");
    open_browser(&url);
    println!(
        "Waiting for Google… If the browser is on another device, paste the address \
         it ends up on (http://127.0.0.1…) here and press Enter."
    );

    // A plain thread, so a read still waiting at exit can't hold the runtime open.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if !line.trim().is_empty() && tx.send(line).is_err() {
                break;
            }
        }
    });
    let pasted = async move {
        match rx.recv().await {
            Some(line) => Some(line),
            // No terminal: just wait for the redirect.
            None => std::future::pending().await,
        }
    };
    let status = match (&app, listener) {
        (Some(base), _) => {
            let waited = async {
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let s = api(reqwest::Method::GET, &format!("{base}/publish/youtube"), None)
                        .await?;
                    if s["connected"].as_bool() == Some(true) {
                        return anyhow::Ok(s);
                    }
                    anyhow::ensure!(
                        s["connecting"].as_bool() == Some(true),
                        "the sign-in ended without connecting; run --connect again"
                    );
                }
            };
            let s = tokio::select! {
                s = waited => s?,
                Some(line) = pasted => {
                    api(
                        reqwest::Method::POST,
                        &format!("{base}/publish/youtube/callback-url"),
                        Some(json!({ "url": line.trim() })),
                    )
                    .await?
                }
            };
            s["channel"].as_str().map(str::to_string)
        }
        (None, Some(listener)) => {
            let s = tokio::select! {
                s = publish::serve_callback(state, listener) => s?,
                Some(line) = pasted => publish::finish_from_url(state, &line).await?,
            };
            s.channel
        }
        (None, None) => unreachable!("a local sign-in always has a listener"),
    };
    match status {
        Some(channel) => println!("Connected to YouTube as {channel}."),
        None => println!("Connected to YouTube."),
    }
    Ok(())
}

fn open_browser(url: &str) {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("cmd", vec!["/C", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    let _ = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ---------------------------------------------------------------------------
// ideas

pub async fn ideas(
    state: &AppState,
    news: bool,
    q: Option<String>,
    geo: &str,
    limit: usize,
) -> anyhow::Result<()> {
    let source = if news || q.is_some() {
        IdeaSource::News
    } else {
        IdeaSource::Trends
    };
    let ideas = state
        .services()
        .sources
        .ideas(source, q.as_deref(), geo, limit.clamp(1, 50))
        .await?;
    anyhow::ensure!(!ideas.is_empty(), "no ideas came back; try again later");
    for (n, idea) in ideas.iter().enumerate() {
        println!("{:>2}. {}", n + 1, idea.title);
        if let Some(detail) = &idea.detail {
            println!("    {detail}");
        }
    }
    println!("\nSave the ones you like to a file and run: clipcut batch <file>");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_skip_blank_and_comment_lines() {
        let text = "# October\nFirst topic\n\n  Second topic  \n#skip\n";
        assert_eq!(read_topics(text), vec!["First topic", "Second topic"]);
    }

    #[test]
    fn intervals_parse_with_units() {
        assert_eq!(parse_every("90m"), Ok(90));
        assert_eq!(parse_every("2h"), Ok(120));
        assert_eq!(parse_every("1D"), Ok(1440));
        assert_eq!(parse_every("45"), Ok(45));
        assert!(parse_every("0h").is_err());
        assert!(parse_every("soon").is_err());
        assert!(parse_every("99999999d").is_err());
    }

    #[test]
    fn times_parse_utc_and_local() {
        assert_eq!(
            parse_time("2026-10-12T15:00:00Z").unwrap().to_rfc3339(),
            "2026-10-12T15:00:00+00:00"
        );
        let local = parse_time("2026-10-12 15:00").unwrap();
        assert_eq!(
            local.with_timezone(&Local).format("%H:%M").to_string(),
            "15:00"
        );
        assert!(parse_time("tomorrow").is_err());
    }

    #[test]
    fn formats_parse_case_insensitively() {
        assert_eq!(parse_format("Square"), Ok(Format::Square));
        assert!(parse_format("wide").is_err());
    }

    #[test]
    fn a_start_time_needs_publish() {
        assert!(privacy_plan(None, Some(Utc::now())).is_err());
        assert!(privacy_plan(Some("secret".into()), None).is_err());
        let plan = privacy_plan(Some("Unlisted".into()), None).unwrap().unwrap();
        assert_eq!(plan.privacy, "unlisted");
    }
}
