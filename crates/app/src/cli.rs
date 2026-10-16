//! Command-line batch, queue, clip, publish, ideas, list, show and delete, and
//! finding a project from part of its ID. When the app is already running on this
//! machine they go through its API, so the two never write the same queue or
//! sign-in at once; otherwise they run here.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use clipcut_core::config::Format;
use clipcut_core::timeline::CaptionPreset;
use clipcut_core::{Project, StageState, Status};
use clipcut_sources::IdeaSource;
use serde_json::{Value, json};

use crate::clips::{self, ClipRequest};
use crate::publish;
use crate::queue::{self, AddBody, ItemOptions, ItemStatus, PublishPlan, QueueItem, QueueState};
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
        println!(
            "Queued {n} topics in the running app; follow them in its Queue tab or with `clipcut queue`."
        );
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
        println!(
            "Queued in the running app; follow them in its Queue tab or with `clipcut queue`."
        );
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
    let id = resolve_id(&state, &id)?;
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

// ---------------------------------------------------------------------------
// list

const TOPIC_WIDTH: usize = 60;

/// Where a project stands, in a few words.
pub fn project_status(p: &Project) -> String {
    let stage = |status: Status| p.stages.iter().find(|s| s.status == status);
    if let Some(s) = stage(Status::Running) {
        return format!("running: {}", s.stage.label().to_lowercase());
    }
    if let Some(s) = stage(Status::Failed) {
        return format!("failed: {}", s.stage.label().to_lowercase());
    }
    if p.render.is_some() {
        return if p.published.is_empty() {
            "rendered".into()
        } else {
            "published".into()
        };
    }
    if p.stages.iter().all(|s| s.status == Status::Pending) {
        return "not started".into();
    }
    match p
        .stages
        .iter()
        .find(|s| !matches!(s.status, Status::Done | Status::Skipped))
    {
        Some(s) => format!("stopped before {}", s.stage.label().to_lowercase()),
        None => "not rendered".into(),
    }
}

/// `0:42` for a rendered project, `-` otherwise.
fn length(p: &Project) -> String {
    match &p.render {
        Some(r) => clock(r.duration_s),
        None => "-".into(),
    }
}

/// `1:05` for 65 seconds.
fn clock(seconds: f32) -> String {
    let s = seconds.max(0.0).round() as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `text` on one line, with runs of whitespace collapsed.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shorten(text: &str, max: usize) -> String {
    let text = one_line(text);
    if text.chars().count() <= max {
        return text;
    }
    let cut: String = text.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The `clipcut list` table, newest first.
pub fn project_table(projects: &[Project]) -> String {
    let rows: Vec<[String; 4]> = projects
        .iter()
        .map(|p| {
            [
                p.id.clone(),
                project_status(p),
                length(p),
                shorten(&p.topic, TOPIC_WIDTH),
            ]
        })
        .collect();
    let header = ["ID", "STATUS", "LENGTH", "TOPIC"].map(String::from);
    let width = |col: usize| {
        std::iter::once(&header)
            .chain(&rows)
            .map(|r| r[col].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (id_w, status_w, length_w) = (width(0), width(1), width(2));
    std::iter::once(&header)
        .chain(&rows)
        .map(|[id, status, length, topic]| {
            format!("{id:<id_w$}  {status:<status_w$}  {length:>length_w$}  {topic}\n")
        })
        .collect()
}

pub fn list(state: &AppState, limit: Option<usize>) -> anyhow::Result<()> {
    let mut projects = state.store.list();
    if projects.is_empty() {
        println!("No projects yet. Make one with: clipcut new \"<topic>\"");
        return Ok(());
    }
    let total = projects.len();
    projects.truncate(limit.unwrap_or(total));
    print!("{}", project_table(&projects));
    if projects.len() < total {
        println!(
            "\nShowing {} of {total}; use --limit to see more.",
            projects.len()
        );
    }
    println!("\nProjects are in {}", state.paths.projects().display());
    Ok(())
}

// ---------------------------------------------------------------------------
// show

/// `12s`, `3m 05s`, `1h 02m`.
fn took(seconds: i64) -> String {
    let s = seconds.max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// How a stage went: `done in 12s`, `running 40%: Downloading`, `failed: …`.
fn stage_line(s: &StageState) -> String {
    match s.status {
        Status::Pending => "pending".into(),
        Status::Skipped => "skipped".into(),
        Status::Done => match (s.started_at, s.finished_at) {
            (Some(start), Some(end)) if end >= start => {
                format!("done in {}", took((end - start).num_seconds()))
            }
            _ => "done".into(),
        },
        Status::Running => {
            let percent = (s.progress.clamp(0.0, 1.0) * 100.0).round();
            match one_line(&s.message) {
                m if m.is_empty() => format!("running {percent}%"),
                m => format!("running {percent}%: {m}"),
            }
        }
        Status::Failed => match s.error.as_deref().map(one_line) {
            Some(e) if !e.is_empty() => format!("failed: {e}"),
            _ => "failed".into(),
        },
    }
}

fn local_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

/// The `clipcut show` report: what the project is, how each stage went, the
/// files it made (flagging any that are gone) and where it was published.
pub fn project_details(p: &Project, dir: &Path) -> String {
    let mut about: Vec<(&str, String)> = vec![("Topic", one_line(&p.topic))];
    if let Some(title) = p.script.as_ref().map(|s| one_line(&s.title))
        && !title.is_empty()
    {
        about.push(("Title", title));
    }
    if let Some(clip) = &p.clip {
        let by = match one_line(&clip.channel) {
            c if c.is_empty() => String::new(),
            c => format!(" by {c}"),
        };
        about.push((
            "Clip of",
            format!(
                "{}{by}, {}–{}",
                one_line(&clip.title),
                clock(clip.start_s),
                clock(clip.end_s)
            ),
        ));
        about.push(("Source", clip.url.clone()));
    }
    let mut video = p.format.slug().to_string();
    if !p.extra_formats.is_empty() {
        let also: Vec<&str> = p.extra_formats.iter().map(|f| f.slug()).collect();
        video += &format!(" (also {})", also.join(", "));
    }
    video += &format!(", {}s target", p.target_seconds);
    if let Some(preset) = p.caption_preset {
        video += &format!(", {} captions", preset.slug());
    }
    let voice = p
        .voice_track
        .as_ref()
        .map(|v| v.voice.clone())
        .or_else(|| p.voice.clone());
    if let Some(voice) = voice.filter(|v| !v.is_empty()) {
        video += &format!(", voice {voice}");
    }
    about.push(("Video", video));
    about.push(("Created", local_time(p.created_at)));

    let mut out = format!("{}  {}\n\n", p.id, project_status(p));
    for (name, value) in about {
        out += &format!("{:<9}{value}\n", format!("{name}:"));
    }

    out += "\nStages\n";
    for s in &p.stages {
        out += &format!("  {:<12}{}\n", s.stage.label(), stage_line(s));
    }

    let file = |rel: &str, note: String| {
        let path = dir.join(rel);
        let missing = if path.exists() { "" } else { "  (missing)" };
        format!("{}{note}{missing}", path.display())
    };
    let mut files: Vec<(String, String)> = Vec::new();
    if let Some(r) = &p.render {
        files.push((
            "Video".into(),
            file(
                &r.path,
                format!("  ({}, {}x{})", clock(r.duration_s), r.width, r.height),
            ),
        ));
        for v in &r.variants {
            let mut name = v.format.slug().to_string();
            name[..1].make_ascii_uppercase();
            files.push((name, file(&v.path, format!("  ({}x{})", v.width, v.height))));
        }
        if let Some(subs) = &r.subtitles {
            files.push(("Captions".into(), file(&subs.srt, String::new())));
            files.push((String::new(), file(&subs.vtt, String::new())));
        }
        if let Some(thumb) = &r.thumbnail_jpg {
            files.push(("Thumbnail".into(), file(thumb, String::new())));
        }
        files.push(("Credits".into(), file(&r.credits_path, String::new())));
    }
    files.push((
        "Folder".into(),
        format!("{}  ({})", dir.display(), human_size(disk_usage(dir))),
    ));
    out += "\nFiles\n";
    for (name, value) in files {
        out += &format!("  {name:<11}{value}\n");
    }

    if !p.published.is_empty() {
        out += "\nPublished\n";
        for r in &p.published {
            let when = match r.publish_at {
                Some(at) => format!("releases {}", local_time(at)),
                None => format!("uploaded {}", local_time(r.uploaded_at)),
            };
            out += &format!(
                "  {} {}  {}  {}  {when}\n",
                r.platform,
                r.format.slug(),
                r.privacy,
                r.url
            );
        }
    }
    out
}

pub fn show(state: &AppState, query: &str, path: bool, json: bool) -> anyhow::Result<()> {
    use std::io::Write;
    let id = resolve_id(state, query)?;
    let project = state.store.get(&id)?;
    let dir = state.store.dir(&id);
    let text = if json {
        serde_json::to_string_pretty(&project)? + "\n"
    } else if path {
        let render = project.render.as_ref().with_context(|| {
            format!(
                "{id} has no video yet ({}); `clipcut run {id}` finishes it",
                project_status(&project)
            )
        })?;
        format!("{}\n", dir.join(&render.path).display())
    } else {
        project_details(&project, &dir)
    };
    // `clipcut show latest --json | head` closes the pipe early; that's fine.
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        written => Ok(written?),
    }
}

// ---------------------------------------------------------------------------
// delete

/// Bytes used by the files under `path`. Symlinks are counted, not followed.
pub fn disk_usage(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| disk_usage(&e.path()))
        .sum()
}

/// `512 B`, `12 KB`, `420 MB`, `1.3 GB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64 / 1000.0;
    let mut unit = 0;
    while size >= 999.5 && unit < UNITS.len() - 1 {
        size /= 1000.0;
        unit += 1;
    }
    if size < 9.95 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.0} {}", UNITS[unit])
    }
}

pub struct DeleteArgs {
    pub ids: Vec<String>,
    pub yes: bool,
    pub force: bool,
}

/// The projects `ids` name, once each, in the order given. A project with a
/// running stage is refused unless `force`: it may be running in another
/// `clipcut` command, or be left over from a crash.
pub fn projects_to_delete(
    state: &AppState,
    ids: &[String],
    force: bool,
) -> anyhow::Result<Vec<Project>> {
    let mut projects: Vec<Project> = Vec::new();
    for query in ids {
        let id = resolve_id(state, query)?;
        if projects.iter().any(|p| p.id == id) {
            continue;
        }
        let project = state.store.get(&id)?;
        if !force && let Some(s) = project.stages.iter().find(|s| s.status == Status::Running) {
            anyhow::bail!(
                "{id} is running ({}); stop it first, or pass --force if ClipCut quit \
                 while it ran",
                s.stage.label().to_lowercase()
            );
        }
        projects.push(project);
    }
    Ok(projects)
}

/// Whether the running app keeps this project in the same folder: a one-off
/// file written there must come back through the app. A different data
/// directory (`--home`) can hold a copy of the same project, and the app must
/// not delete its copy instead.
async fn app_serves_folder(base: &str, id: &str, dir: &Path) -> bool {
    let token = uuid::Uuid::new_v4().to_string();
    let name = format!(".delete-check-{token}");
    let marker = dir.join(&name);
    if std::fs::write(&marker, &token).is_err() {
        return false;
    }
    let seen = async {
        let resp = reqwest::Client::new()
            .get(format!("{base}/projects/{id}/files/{name}"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .ok()?;
        resp.status().is_success().then_some(())?;
        resp.text().await.ok()
    }
    .await;
    let _ = std::fs::remove_file(&marker);
    seen.as_deref() == Some(token.as_str())
}

pub async fn delete(state: &AppState, args: DeleteArgs) -> anyhow::Result<()> {
    let projects = projects_to_delete(state, &args.ids, args.force)?;
    let size: u64 = projects
        .iter()
        .map(|p| disk_usage(&state.store.dir(&p.id)))
        .sum();
    print!("{}", project_table(&projects));
    if !args.yes {
        use std::io::{IsTerminal, Write};
        anyhow::ensure!(
            std::io::stdin().is_terminal(),
            "pass --yes to delete without being asked"
        );
        let what = match projects.len() {
            1 => "this project".to_string(),
            n => format!("these {n} projects"),
        };
        print!("\nDelete {what} and free {}? [y/N] ", human_size(size));
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("Nothing deleted.");
            return Ok(());
        }
    }
    // The app stops a project it is running before deleting it.
    let app = running_app(state).await;
    for p in &projects {
        let dir = state.store.dir(&p.id);
        match &app {
            Some(base) if app_serves_folder(base, &p.id, &dir).await => {
                api(
                    reqwest::Method::DELETE,
                    &format!("{base}/projects/{}", p.id),
                    None,
                )
                .await
                .with_context(|| format!("could not delete {}", p.id))?;
            }
            _ => state
                .store
                .delete(&p.id)
                .await
                .with_context(|| format!("could not delete {}", p.id))?,
        }
    }
    let n = projects.len();
    let noun = if n == 1 { "project" } else { "projects" };
    println!("Deleted {n} {noun}, freeing {}.", human_size(size));
    Ok(())
}

// ---------------------------------------------------------------------------
// queue

#[derive(clap::Subcommand, Debug, Clone)]
pub enum QueueAction {
    /// Start no new videos; the one being made finishes.
    Pause,
    /// Start making videos again.
    Resume,
    /// Put a failed or cancelled item back in line: `clipcut queue retry 3`.
    Retry {
        /// Its number in `clipcut queue`, or its ID.
        item: String,
    },
    /// Take an item out of the queue; one being made is stopped.
    Remove {
        /// Its number in `clipcut queue`, or its ID.
        item: String,
    },
    /// Remove done, failed and cancelled items.
    Clear,
}

fn item_status(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Waiting => "waiting",
        ItemStatus::Running => "running",
        ItemStatus::Done => "done",
        ItemStatus::Failed => "failed",
        ItemStatus::Cancelled => "cancelled",
    }
}

/// `unlisted, 2026-10-13 15:00`, or `-` when the item isn't published.
fn publish_plan(item: &QueueItem) -> String {
    match &item.options.publish {
        None => "-".into(),
        Some(plan) => match plan.publish_at {
            Some(at) => format!("{}, {}", plan.privacy, local_time(at)),
            None => plan.privacy.clone(),
        },
    }
}

/// The `clipcut queue` table: each item's number, status, project, publish
/// plan and topic, with the video's link or the error below it.
pub fn queue_table(queue: &QueueState) -> String {
    let rows: Vec<[String; 5]> = queue
        .items
        .iter()
        .enumerate()
        .map(|(n, item)| {
            let project = item.project_id.as_deref().map_or("-".into(), |id| {
                let skip = id.chars().count().saturating_sub(6);
                id.chars().skip(skip).collect()
            });
            [
                (n + 1).to_string(),
                item_status(item.status).into(),
                project,
                publish_plan(item),
                shorten(&item.topic, TOPIC_WIDTH),
            ]
        })
        .collect();
    let header = ["#", "STATUS", "PROJECT", "PUBLISH", "TOPIC"].map(String::from);
    let width = |col: usize| {
        std::iter::once(&header)
            .chain(&rows)
            .map(|r| r[col].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2), width(3)];
    let line = |[n, status, project, publish, topic]: &[String; 5]| {
        let [n_w, status_w, project_w, publish_w] = widths;
        format!(
            "{n:>n_w$}  {status:<status_w$}  {project:<project_w$}  {publish:<publish_w$}  {topic}\n"
        )
    };
    let indent = " ".repeat(widths[0] + 2);
    let mut out = line(&header);
    for (row, item) in rows.iter().zip(&queue.items) {
        out += &line(row);
        let note = match item.status {
            ItemStatus::Done => item.video_url.clone(),
            ItemStatus::Failed | ItemStatus::Cancelled => {
                item.error.as_deref().map(|e| shorten(e, 100))
            }
            _ => None,
        };
        if let Some(note) = note {
            out += &format!("{indent}{note}\n");
        }
    }
    out
}

/// The item `query` names: its number in the table, its ID, or the start of
/// its ID.
fn find_item<'a>(queue: &'a QueueState, query: &str) -> anyhow::Result<(usize, &'a QueueItem)> {
    let query = query.trim();
    let numbered = queue.items.iter().enumerate().map(|(n, i)| (n + 1, i));
    if let Ok(n) = query.parse::<usize>() {
        let item = n.checked_sub(1).and_then(|i| queue.items.get(i));
        return item.map(|i| (n, i)).with_context(|| {
            format!(
                "the queue has no item {n}; it has {} (`clipcut queue` numbers them)",
                queue.items.len()
            )
        });
    }
    if let Some(found) = numbered.clone().find(|(_, i)| i.id == query) {
        return Ok(found);
    }
    anyhow::ensure!(
        query.chars().count() >= MIN_ID_PART,
        "no queue item {query:?}; give its number from `clipcut queue` or its ID"
    );
    let matches: Vec<_> = numbered.filter(|(_, i)| i.id.starts_with(query)).collect();
    match matches[..] {
        [found] => Ok(found),
        [] => anyhow::bail!("no queue item {query:?}; `clipcut queue` numbers them"),
        _ => anyhow::bail!(
            "{query:?} matches {} queue items; give more of the ID",
            matches.len()
        ),
    }
}

pub async fn queue(
    state: &AppState,
    action: Option<QueueAction>,
    json: bool,
) -> anyhow::Result<()> {
    let app = running_app(state).await;
    let snapshot: QueueState = match &app {
        Some(base) => {
            serde_json::from_value(api(reqwest::Method::GET, &format!("{base}/queue"), None).await?)
                .context("the running app sent a queue this version can't read")?
        }
        None => state.queue.snapshot(),
    };
    let Some(action) = action else {
        if json {
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            return Ok(());
        }
        if snapshot.items.is_empty() {
            println!("The queue is empty. Add topics with: clipcut batch topics.txt");
            return Ok(());
        }
        print!("{}", queue_table(&snapshot));
        let waiting = snapshot
            .items
            .iter()
            .any(|i| i.status == ItemStatus::Waiting);
        if snapshot.paused {
            println!("\nPaused: no new video starts until `clipcut queue resume`.");
        } else if waiting && app.is_none() {
            println!("\nClipCut isn't running; waiting videos start when you run `clipcut`.");
        }
        return Ok(());
    };

    match action {
        QueueAction::Pause | QueueAction::Resume => {
            let paused = matches!(action, QueueAction::Pause);
            match &app {
                Some(base) => {
                    api(
                        reqwest::Method::PATCH,
                        &format!("{base}/queue"),
                        Some(json!({ "paused": paused })),
                    )
                    .await?;
                }
                None => state.queue.set_paused(paused)?,
            }
            let running = snapshot
                .items
                .iter()
                .any(|i| i.status == ItemStatus::Running);
            println!(
                "{}",
                match (paused, running) {
                    (true, true) => "Paused the queue; the video being made finishes first.",
                    (true, false) => "Paused the queue.",
                    (false, _) => "Resumed the queue.",
                }
            );
        }
        QueueAction::Retry { item } => {
            let (n, item) = find_item(&snapshot, &item)?;
            anyhow::ensure!(
                matches!(item.status, ItemStatus::Failed | ItemStatus::Cancelled),
                "item {n} is {}; only failed and cancelled items can be retried",
                item_status(item.status)
            );
            match &app {
                Some(base) => {
                    api(
                        reqwest::Method::POST,
                        &format!("{base}/queue/{}/retry", item.id),
                        None,
                    )
                    .await?;
                }
                None => {
                    state.queue.retry(&item.id)?;
                }
            }
            println!(
                "Item {n} is back in line: {}",
                shorten(&item.topic, TOPIC_WIDTH)
            );
        }
        QueueAction::Remove { item } => {
            let (n, item) = find_item(&snapshot, &item)?;
            match &app {
                Some(base) => {
                    api(
                        reqwest::Method::DELETE,
                        &format!("{base}/queue/{}", item.id),
                        None,
                    )
                    .await?;
                }
                None => {
                    queue::remove(state, &item.id).await?;
                }
            }
            let topic = shorten(&item.topic, TOPIC_WIDTH);
            if item.status == ItemStatus::Running {
                println!(
                    "Stopped item {n}: {topic}\nIt stays in the queue as cancelled; \
                     `clipcut queue retry {n}` picks it up again."
                );
            } else {
                println!("Removed item {n}: {topic}");
            }
        }
        QueueAction::Clear => {
            let removed = match &app {
                Some(base) => api(reqwest::Method::POST, &format!("{base}/queue/clear"), None)
                    .await?["removed"]
                    .as_u64()
                    .unwrap_or(0) as usize,
                None => state.queue.clear_finished()?,
            };
            match removed {
                0 => println!("Nothing to clear: no item is done, failed or cancelled."),
                1 => println!("Cleared 1 finished item."),
                n => println!("Cleared {n} finished items."),
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// project ids

/// Shortest start or end of an ID that `resolve_id` matches on.
const MIN_ID_PART: usize = 4;

/// The project `query` names: its full ID, `latest` for the newest project,
/// or the start or end of one ID (at least four characters), such as the
/// six random characters at the end.
pub fn resolve_id(state: &AppState, query: &str) -> anyhow::Result<String> {
    let query = query.trim();
    if state.store.exists(query) {
        return Ok(query.to_string());
    }
    let projects = state.store.list();
    if query.eq_ignore_ascii_case("latest") {
        return projects
            .first()
            .map(|p| p.id.clone())
            .context("there are no projects yet");
    }
    anyhow::ensure!(
        query.chars().count() >= MIN_ID_PART,
        "no project has the ID {query:?}; give the full ID, at least its first or last \
         {MIN_ID_PART} characters, or `latest` (`clipcut list` shows the IDs)"
    );
    let query = query.to_ascii_lowercase();
    let matches: Vec<&str> = projects
        .iter()
        .map(|p| p.id.as_str())
        .filter(|id| id.starts_with(&query) || id.ends_with(&query))
        .collect();
    match matches[..] {
        [id] => Ok(id.to_string()),
        [] => anyhow::bail!("no project matches {query:?}; `clipcut list` shows the IDs"),
        _ => {
            let shown = matches[..matches.len().min(5)].join(", ");
            let more = if matches.len() > 5 { ", …" } else { "" };
            anyhow::bail!(
                "{query:?} matches {} projects ({shown}{more}); give more of the ID",
                matches.len()
            )
        }
    }
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

    fn at(p: &mut Project, stage: clipcut_core::Stage, status: Status) {
        p.stages
            .iter_mut()
            .find(|s| s.stage == stage)
            .unwrap()
            .status = status;
    }

    #[test]
    fn status_says_where_a_project_stands() {
        use crate::test_support::{clip_project, finished_project};
        use clipcut_core::Stage;

        let mut p = finished_project();
        assert_eq!(project_status(&p), "rendered");
        p.published = serde_json::from_value(json!([{
            "platform": "youtube", "format": "vertical", "video_id": "x",
            "url": "https://youtu.be/x", "privacy": "unlisted", "uploaded_at": Utc::now()
        }]))
        .unwrap();
        assert_eq!(project_status(&p), "published");

        p.render = None;
        at(&mut p, Stage::Assets, Status::Running);
        assert_eq!(project_status(&p), "running: visuals");
        at(&mut p, Stage::Assets, Status::Done);
        at(&mut p, Stage::Voice, Status::Failed);
        assert_eq!(project_status(&p), "failed: voiceover");
        at(&mut p, Stage::Voice, Status::Pending);
        at(&mut p, Stage::Render, Status::Pending);
        assert_eq!(project_status(&p), "stopped before voiceover");

        let new = Project::new("Fresh", "", Format::Vertical, 30);
        assert_eq!(project_status(&new), "not started");
        // Clips skip research, hooks and script, and start with their beat.
        assert_eq!(project_status(&clip_project()), "stopped before visuals");
    }

    #[test]
    fn table_aligns_columns_and_shortens_topics() {
        let mut done = crate::test_support::finished_project();
        done.render.as_mut().unwrap().duration_s = 61.6;
        let mut new = Project::new(
            format!("A very long topic {}", "that keeps going ".repeat(10)),
            "",
            Format::Vertical,
            30,
        );
        new.id = "short".into();
        let table = project_table(&[done.clone(), new]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("ID "));
        let cells: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(&cells[1..], ["rendered", "1:02", "A", "fixture", "topic"]);
        assert_eq!(lines[0].find("STATUS"), lines[1].find("rendered"));
        assert_eq!(lines[0].find("TOPIC"), lines[1].find("A fixture"));
        assert_eq!(lines[2].find("not started"), lines[1].find("rendered"));
        let topic = lines[2].rsplit("  ").next().unwrap();
        assert_eq!(topic.chars().count(), TOPIC_WIDTH);
        assert!(topic.ends_with('…'));
    }

    #[test]
    fn stage_lines_say_how_each_went() {
        assert_eq!(took(12), "12s");
        assert_eq!(took(185), "3m 05s");
        assert_eq!(took(3720), "1h 02m");
        assert_eq!(took(-4), "0s");

        let mut s: StageState = serde_json::from_value(json!({
            "stage": "assets", "status": "pending"
        }))
        .unwrap();
        assert_eq!(stage_line(&s), "pending");
        s.status = Status::Running;
        s.progress = 0.4;
        s.message = "Downloading\n beat 3".into();
        assert_eq!(stage_line(&s), "running 40%: Downloading beat 3");
        s.status = Status::Failed;
        s.error = Some("yt-dlp: 403\nForbidden".into());
        assert_eq!(stage_line(&s), "failed: yt-dlp: 403 Forbidden");
        s.status = Status::Done;
        assert_eq!(stage_line(&s), "done");
        let start = Utc::now();
        s.started_at = Some(start);
        s.finished_at = Some(start + chrono::Duration::seconds(75));
        assert_eq!(stage_line(&s), "done in 1m 15s");
    }

    #[test]
    fn details_list_stages_files_and_uploads() {
        let home = crate::test_support::TestHome::new();
        let dir = home.0.join("p");
        std::fs::create_dir_all(dir.join("render")).unwrap();
        std::fs::write(dir.join("render/final.mp4"), [0u8; 1200]).unwrap();
        std::fs::write(dir.join("render/credits.txt"), "Sources").unwrap();

        let mut p = crate::test_support::finished_project();
        p.extra_formats = vec![Format::Square];
        p.caption_preset = Some(CaptionPreset::Hormozi);
        p.script.as_mut().unwrap().title = "The fixture title".into();
        let render = p.render.as_mut().unwrap();
        render.duration_s = 42.4;
        render.variants = serde_json::from_value(json!([
            {"format": "square", "path": "render/final-square.mp4", "width": 1080, "height": 1080}
        ]))
        .unwrap();
        render.subtitles = serde_json::from_value(
            json!({"srt": "render/captions.srt", "vtt": "render/captions.vtt"}),
        )
        .unwrap();
        p.published = serde_json::from_value(json!([{
            "platform": "youtube", "format": "vertical", "video_id": "x",
            "url": "https://youtu.be/x", "privacy": "private",
            "publish_at": Utc::now(), "uploaded_at": Utc::now()
        }]))
        .unwrap();
        p.stages[1].status = Status::Skipped;

        let text = project_details(&p, &dir);
        let line = |start: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(start))
                .unwrap_or_else(|| panic!("no {start:?} line in:\n{text}"))
                .to_string()
        };
        assert_eq!(text.lines().next().unwrap(), format!("{}  published", p.id));
        assert_eq!(line("Topic:"), "Topic:   A fixture topic");
        assert_eq!(line("Title:"), "Title:   The fixture title");
        assert_eq!(
            line("Video:"),
            "Video:   vertical (also square), 15s target, hormozi captions, voice fixture"
        );
        assert_eq!(line("Hook"), "  Hook        skipped");
        assert_eq!(line("Render"), "  Render      done");
        let main = dir.join("render/final.mp4");
        assert_eq!(
            line("Video "),
            format!("  Video      {}  (0:42, 1080x1920)", main.display())
        );
        assert!(line("Square").ends_with("final-square.mp4  (1080x1080)  (missing)"));
        assert!(line("Captions").ends_with("captions.srt  (missing)"));
        assert!(text.contains("captions.vtt  (missing)"));
        assert!(!text.contains("Thumbnail"));
        assert!(line("Credits").ends_with("credits.txt"));
        assert!(line("Folder").ends_with("(1.2 KB)"));
        assert!(
            line("youtube")
                .starts_with("  youtube vertical  private  https://youtu.be/x  releases ")
        );

        // A clip names its source; nothing rendered means no file rows.
        let clip = crate::test_support::clip_project();
        let text = project_details(&clip, &dir);
        assert!(text.contains("Clip of: Long talk by Some Channel, 1:40–2:20\n"));
        assert!(text.contains("Source:  https://www.youtube.com/watch?v=abc\n"));
        assert!(!text.contains("Credits") && !text.contains("Published"));
    }

    #[tokio::test]
    async fn show_path_needs_a_render() {
        let home = crate::test_support::TestHome::new();
        let state = home.state();
        fixture_projects(&state, &["20261012-091500-3fa9c1"]).await;
        let err = show(&state, "3fa9c1", true, false).unwrap_err().to_string();
        assert!(err.contains("has no video yet (not started)"), "{err}");
        show(&state, "latest", false, false).unwrap();
        show(&state, "latest", false, true).unwrap();
    }

    #[tokio::test]
    async fn ids_resolve_from_their_start_or_end_or_latest() {
        let home = crate::test_support::TestHome::new();
        let state = home.state();
        let err = |q: &str| resolve_id(&state, q).unwrap_err().to_string();
        assert!(err("latest").contains("no projects"));

        let started = Utc::now();
        for (n, id) in [
            "20261011-120033-9d21aa",
            "20261011-180210-b07e44",
            "20261012-091500-3fa9c1",
        ]
        .into_iter()
        .enumerate()
        {
            let mut p = Project::new("Fixture", "", Format::Vertical, 30);
            p.id = id.into();
            p.created_at = started + chrono::Duration::minutes(n as i64);
            state.store.create(p).await.unwrap();
        }
        let found = |q: &str| resolve_id(&state, q).unwrap();

        assert_eq!(found("20261011-120033-9d21aa"), "20261011-120033-9d21aa");
        assert_eq!(found(" 3fa9c1 "), "20261012-091500-3fa9c1");
        assert_eq!(found("B07E44"), "20261011-180210-b07e44");
        assert_eq!(found("7e44"), "20261011-180210-b07e44");
        assert_eq!(found("20261012"), "20261012-091500-3fa9c1");
        assert_eq!(found("Latest"), "20261012-091500-3fa9c1");

        assert!(err("20261011").contains("matches 2 projects"));
        assert!(err("ffffff").contains("no project matches"));
        // Too short to be trusted, even though only one ID ends this way.
        assert!(err("aa").contains("at least its first or last 4"));
        // Only the start or end counts, never the middle.
        assert!(err("091500").contains("no project matches"));
        assert!(err("../projects").contains("no project matches"));
    }

    #[test]
    fn sizes_read_in_round_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_000), "1.0 KB");
        assert_eq!(human_size(12_400), "12 KB");
        assert_eq!(human_size(420_000_000), "420 MB");
        assert_eq!(human_size(999_600), "1.0 MB");
        assert_eq!(human_size(1_340_000_000), "1.3 GB");
        assert_eq!(human_size(5_000_000_000_000_000), "5000 TB");
    }

    #[test]
    fn disk_usage_adds_up_nested_files() {
        let home = crate::test_support::TestHome::new();
        let dir = home.0.join("p");
        std::fs::create_dir_all(dir.join("render")).unwrap();
        std::fs::write(dir.join("project.json"), [0u8; 300]).unwrap();
        std::fs::write(dir.join("render/final.mp4"), [0u8; 1200]).unwrap();
        assert_eq!(disk_usage(&dir), 1500);
        assert_eq!(disk_usage(&dir.join("render/final.mp4")), 1200);
        assert_eq!(disk_usage(&dir.join("missing")), 0);
    }

    async fn fixture_projects(state: &AppState, ids: &[&str]) {
        let started = Utc::now();
        for (n, id) in ids.iter().enumerate() {
            let mut p = Project::new("Fixture", "", Format::Vertical, 30);
            p.id = id.to_string();
            p.created_at = started + chrono::Duration::minutes(n as i64);
            state.store.create(p).await.unwrap();
        }
    }

    #[tokio::test]
    async fn delete_picks_each_project_once_and_spares_running_ones() {
        let home = crate::test_support::TestHome::new();
        let state = home.state();
        fixture_projects(
            &state,
            &["20261011-120033-9d21aa", "20261012-091500-3fa9c1"],
        )
        .await;
        let ids = |qs: &[&str]| qs.iter().map(|q| q.to_string()).collect::<Vec<_>>();

        let picked = projects_to_delete(&state, &ids(&["3fa9c1", "latest", "9d21aa"]), false)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect::<Vec<_>>();
        assert_eq!(picked, ["20261012-091500-3fa9c1", "20261011-120033-9d21aa"]);
        // One bad ID stops the whole command before anything is deleted.
        assert!(projects_to_delete(&state, &ids(&["3fa9c1", "ffffff"]), false).is_err());

        state
            .store
            .update("20261012-091500-3fa9c1", |p| {
                at(p, clipcut_core::Stage::Script, Status::Running);
                Ok(())
            })
            .await
            .unwrap();
        let err = projects_to_delete(&state, &ids(&["3fa9c1"]), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is running (script)"), "{err}");
        assert_eq!(
            projects_to_delete(&state, &ids(&["3fa9c1"]), true)
                .unwrap()
                .len(),
            1
        );
    }

    /// Point `state`'s CLI at an app on `port`.
    fn set_port(state: &AppState, port: u16) {
        let mut config = state.config();
        config.server.port = port;
        state.set_config(config).unwrap();
    }

    /// A port nothing listens on.
    fn free_port() -> u16 {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        free.local_addr().unwrap().port()
    }

    /// Serve the app's API for `app_state` on a free port.
    async fn serve(app_state: AppState) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        app_state.set_bound_port(port);
        let app = crate::server::router(app_state);
        (
            port,
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
        )
    }

    #[tokio::test]
    async fn delete_removes_folders_here_or_through_the_running_app() {
        let home = crate::test_support::TestHome::new();
        let state = home.state();
        let ids = [
            "20261011-120033-9d21aa",
            "20261012-091500-3fa9c1",
            "20261013-100000-c0ffee",
        ];
        fixture_projects(&state, &ids).await;
        let set_port = |port: u16| set_port(&state, port);
        let delete_args = |id: &str| DeleteArgs {
            ids: vec![id.into()],
            yes: true,
            force: false,
        };

        // Nothing listening: the folder is removed here.
        set_port(free_port());
        assert!(running_app(&state).await.is_none());
        delete(&state, delete_args("9d21aa")).await.unwrap();
        assert!(!state.store.exists(ids[0]));

        // The app is up on this home: it deletes the project.
        let (port, server) = serve(state.clone()).await;
        set_port(port);
        let base = running_app(&state).await.unwrap();
        assert!(app_serves_folder(&base, ids[1], &state.store.dir(ids[1])).await);
        delete(&state, delete_args("3fa9c1")).await.unwrap();
        server.abort();
        assert!(!state.store.exists(ids[1]));

        // An app on another home with a copy of the project keeps its copy.
        let other_home = crate::test_support::TestHome::new();
        let other = other_home.state();
        fixture_projects(&other, &ids[2..]).await;
        let (port, server) = serve(other.clone()).await;
        set_port(port);
        assert!(running_app(&state).await.is_some());
        delete(&state, delete_args("c0ffee")).await.unwrap();
        server.abort();
        assert!(!state.store.exists(ids[2]));
        assert!(other.store.exists(ids[2]));
        let leftovers = std::fs::read_dir(other.store.dir(ids[2])).unwrap().count();
        assert_eq!(leftovers, 1, "only project.json is left");
    }

    /// A queue with one item in each state, as `queue.json` holds it.
    fn fixture_queue() -> QueueState {
        let at = "2026-10-12T09:15:00Z";
        serde_json::from_value(json!({
            "paused": false,
            "items": [
                {"id": "a1b2c3d4e5", "topic": "Why the Moon is drifting away", "status": "done",
                 "project_id": "20261012-091500-3fa9c1", "added_at": at,
                 "publish": {"privacy": "public"}, "video_url": "https://youtu.be/moon"},
                {"id": "b2c3d4e5f6", "topic": "How Australia lost   the Great Emu War",
                 "status": "failed", "project_id": "20261012-093000-b07e44", "added_at": at,
                 "error": "Voiceover failed: espeak-ng\nis missing"},
                {"id": "c3d4e5f6a7", "topic": "The loudest sound ever recorded",
                 "status": "waiting", "added_at": at,
                 "publish": {"privacy": "unlisted", "publish_at": "2026-10-13T15:00:00Z"}},
                {"id": "c3d4e5f6b8", "topic": "Cancelled one", "status": "cancelled",
                 "project_id": "20261012-094500-9d21aa", "added_at": at, "error": "cancelled"}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn queue_table_numbers_items_with_links_and_errors_below() {
        let queue = fixture_queue();
        let table = queue_table(&queue);
        let lines: Vec<&str> = table.lines().collect();
        let release = local_time("2026-10-13T15:00:00Z".parse().unwrap());
        let publish_w = format!("unlisted, {release}").len();
        let row = |n: &str, status: &str, project: &str, publish: &str, topic: &str| {
            format!("{n}  {status:<9}  {project:<7}  {publish:<publish_w$}  {topic}")
        };
        assert_eq!(lines[0], row("#", "STATUS", "PROJECT", "PUBLISH", "TOPIC"));
        assert_eq!(
            lines[1],
            row(
                "1",
                "done",
                "3fa9c1",
                "public",
                "Why the Moon is drifting away"
            )
        );
        assert_eq!(lines[2], "   https://youtu.be/moon");
        assert_eq!(
            lines[3],
            row(
                "2",
                "failed",
                "b07e44",
                "-",
                "How Australia lost the Great Emu War"
            )
        );
        assert_eq!(lines[4], "   Voiceover failed: espeak-ng is missing");
        assert_eq!(
            lines[5],
            row(
                "3",
                "waiting",
                "-",
                &format!("unlisted, {release}"),
                "The loudest sound ever recorded"
            )
        );
        assert_eq!(
            lines[6],
            row("4", "cancelled", "9d21aa", "-", "Cancelled one")
        );
        assert_eq!(lines[7], "   cancelled");
        assert_eq!(lines.len(), 8);
    }

    #[test]
    fn queue_items_resolve_by_number_or_id() {
        let queue = fixture_queue();
        let id = |q: &str| find_item(&queue, q).map(|(n, i)| (n, i.id.as_str()));
        assert_eq!(id("1").unwrap(), (1, "a1b2c3d4e5"));
        assert_eq!(id(" 4 ").unwrap(), (4, "c3d4e5f6b8"));
        assert_eq!(id("b2c3d4e5f6").unwrap(), (2, "b2c3d4e5f6"));
        assert_eq!(id("b2c3").unwrap(), (2, "b2c3d4e5f6"));
        for (query, error) in [
            ("0", "no item 0; it has 4"),
            ("5", "no item 5; it has 4"),
            ("c3d4", "matches 2 queue items"),
            ("ab", "give its number"),
            ("ffff", "no queue item"),
        ] {
            let err = id(query).unwrap_err().to_string();
            assert!(err.contains(error), "{query}: {err}");
        }
    }

    #[tokio::test]
    async fn queue_actions_run_here_or_through_the_running_app() {
        let homes = [(); 2].map(|_| crate::test_support::TestHome::new());
        let bytes = serde_json::to_vec(&fixture_queue()).unwrap();
        let [state, other] = homes.each_ref().map(|home| {
            std::fs::write(home.0.join("queue.json"), &bytes).unwrap();
            home.state()
        });
        let ids = |state: &AppState| {
            let items = state.queue.snapshot().items;
            items
                .iter()
                .map(|i| format!("{}:{}", &i.id[..4], item_status(i.status)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let retry = |item: &str| QueueAction::Retry { item: item.into() };
        let remove = |item: &str| QueueAction::Remove { item: item.into() };

        // Nothing listening: queue.json is changed here.
        set_port(&state, free_port());
        queue(&state, None, false).await.unwrap();
        queue(&state, Some(retry("2")), false).await.unwrap();
        let err = queue(&state, Some(retry("3")), false).await.unwrap_err();
        assert!(err.to_string().contains("item 3 is waiting"), "{err}");
        queue(&state, Some(QueueAction::Pause), false)
            .await
            .unwrap();
        assert!(state.queue.snapshot().paused);
        queue(&state, Some(remove("c3d4e5f6a7")), false)
            .await
            .unwrap();
        queue(&state, Some(QueueAction::Clear), false)
            .await
            .unwrap();
        assert_eq!(ids(&state), "b2c3:waiting");
        let saved: QueueState =
            serde_json::from_slice(&std::fs::read(homes[0].0.join("queue.json")).unwrap()).unwrap();
        assert_eq!(saved.items.len(), 1);
        assert!(saved.paused);

        // The app is up: it changes its own queue, not this one.
        let (port, server) = serve(other.clone()).await;
        set_port(&state, port);
        queue(&state, None, true).await.unwrap();
        queue(&state, Some(retry("4")), false).await.unwrap();
        queue(&state, Some(remove("1")), false).await.unwrap();
        queue(&state, Some(QueueAction::Pause), false)
            .await
            .unwrap();
        assert!(other.queue.snapshot().paused);
        queue(&state, Some(QueueAction::Resume), false)
            .await
            .unwrap();
        queue(&state, Some(QueueAction::Clear), false)
            .await
            .unwrap();
        server.abort();
        assert_eq!(ids(&other), "c3d4:waiting c3d4:waiting");
        assert!(!other.queue.snapshot().paused);
        assert_eq!(ids(&state), "b2c3:waiting");
    }
}
