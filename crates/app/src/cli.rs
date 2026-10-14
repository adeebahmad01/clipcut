//! Command-line batch, clip, publish, ideas, list and delete, and finding a
//! project from part of its ID. When the app is already running on this
//! machine they go through its API, so the two never write the same queue or
//! sign-in at once; otherwise they run here.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use clipcut_core::config::Format;
use clipcut_core::timeline::CaptionPreset;
use clipcut_core::{Project, Status};
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
        Some(r) => {
            let s = r.duration_s.round() as u32;
            format!("{}:{:02}", s / 60, s % 60)
        }
        None => "-".into(),
    }
}

fn shorten(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
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
        let set_port = |port: u16| {
            let mut config = state.config();
            config.server.port = port;
            state.set_config(config).unwrap();
        };
        let delete_args = |id: &str| DeleteArgs {
            ids: vec![id.into()],
            yes: true,
            force: false,
        };
        let serve = async |app_state: AppState| {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            app_state.set_bound_port(port);
            let app = crate::server::router(app_state);
            (
                port,
                tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
            )
        };

        // Nothing listening: the folder is removed here.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        set_port(free.local_addr().unwrap().port());
        drop(free);
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
}
