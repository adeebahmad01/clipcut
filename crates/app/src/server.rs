//! HTTP API (see docs/API.md) and the embedded web UI.

use std::convert::Infallible;
use std::path::{Component, Path as FsPath, PathBuf};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use clipcut_core::config::Format;
use clipcut_core::keys::{KNOWN_SERVICES, MAX_KEYS_PER_SERVICE};
use clipcut_core::model::{ProjectKind, PublishRecord};
use clipcut_core::paths::ProjectPaths;
use clipcut_core::timeline::CaptionPreset;
use clipcut_core::{
    Beat, BeatStatus, Config, Project, Script, Stage, StageState, VisualKind, VisualPlan,
};
use clipcut_sources::{Idea, IdeaSource};
use futures::{Stream, StreamExt};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};
use tower::ServiceExt;
use tower_http::compression::Predicate;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType};
use tower_http::services::ServeFile;

use crate::clips::{self, ClipRequest};
use crate::doctor;
use crate::pipeline::{self, Outcome};
use crate::publish::{self, AuthStatus, ClientBody};
use crate::queue::{self, AddBody, PublishPlan, QueueItem, QueueState};
use crate::state::{AppState, BeatClaim, ServerEvent};

const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
const UPLOAD_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "webm", "mkv", "gif", "m4v", "avi", "png", "jpg", "jpeg", "webp", "bmp",
];

#[derive(RustEmbed)]
#[folder = "web/"]
struct Web;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/config", get(get_config).put(put_config))
        .route("/keys", get(get_keys))
        .route("/keys/{service}", put(put_keys))
        .route("/voices", get(voices))
        .route("/voices/preview", post(voice_preview))
        .route("/setup", post(setup))
        .route("/events", get(events))
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{id}",
            get(get_project).patch(patch_project).delete(delete_project),
        )
        .route("/projects/{id}/run", post(run_project))
        .route("/projects/{id}/cancel", post(cancel_project))
        .route("/projects/{id}/duplicate", post(duplicate_project))
        .route("/projects/{id}/beats/{beat}", patch(patch_beat))
        .route("/projects/{id}/beats/{beat}/search", post(search_beat))
        .route("/projects/{id}/beats/{beat}/choose", post(choose_beat))
        .route("/projects/{id}/beats/{beat}/auto", post(auto_beat))
        .route(
            "/projects/{id}/beats/{beat}/upload",
            post(upload_beat).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 64 * 1024)),
        )
        .route("/projects/{id}/files/{*path}", get(project_file))
        .route("/projects/{id}/still", get(still))
        .route("/projects/{id}/download", get(download))
        .route("/projects/{id}/publish", post(publish_project))
        .route("/clips", post(create_clips))
        .route("/ideas", get(ideas))
        .route("/queue", get(get_queue).post(add_queue).patch(patch_queue))
        .route("/queue/clear", post(clear_queue))
        .route("/queue/{item}", axum::routing::delete(remove_queue_item))
        .route("/queue/{item}/retry", post(retry_queue_item))
        .route(
            "/publish/youtube",
            get(youtube_status)
                .put(put_youtube_client)
                .delete(disconnect_youtube),
        )
        .route("/publish/youtube/connect", post(connect_youtube))
        .route("/publish/youtube/callback-url", post(youtube_callback_url))
        .fallback(|| async { ApiError::not_found("no such API route") });
    Router::new()
        .nest("/api", api)
        .fallback(static_file)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            same_origin_mutations,
        ))
        // The default predicate already skips SSE and images; media is served with Range.
        .layer(
            tower_http::compression::CompressionLayer::new()
                .gzip(true)
                .compress_when(
                    DefaultPredicate::new()
                        .and(NotForContentType::new("video/"))
                        .and(NotForContentType::new("audio/")),
                ),
        )
        .with_state(state)
}

/// This is a trusted-LAN app, not an authenticated public service. Still reject
/// browser drive-by writes from another origin. CLI requests without Origin
/// remain supported; this is CSRF defense, not user authentication.
async fn same_origin_mutations(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    // Reject attacker-controlled DNS names even for reads. Numeric LAN hosts
    // and the explicitly configured hostname work without a domain allowlist.
    let host_allowed = req
        .headers()
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .and_then(|host| host.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|host| {
            let name = host.host().trim_start_matches('[').trim_end_matches(']');
            (name.eq_ignore_ascii_case("localhost")
                || name.parse::<std::net::IpAddr>().is_ok()
                || name.eq_ignore_ascii_case(&state.config().server.host))
                && host.port_u16().unwrap_or(80) == state.bound_port()
        });
    if !host_allowed {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "use the printed Local or Network URL (unrecognized Host)"
            })),
        )
            .into_response();
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        let cross_site = req
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site");
        let origin_matches = req
            .headers()
            .get(header::ORIGIN)
            .map(|origin| {
                let host = req
                    .headers()
                    .get(header::HOST)
                    .and_then(|h| h.to_str().ok());
                let url = origin
                    .to_str()
                    .ok()
                    .and_then(|s| reqwest::Url::parse(s).ok());
                match (host, url) {
                    (Some(host), Some(url))
                        if matches!(url.scheme(), "http" | "https")
                            && url.username().is_empty()
                            && url.password().is_none() =>
                    {
                        let authority = url.host_str().map(|h| {
                            let h = if h.contains(':') {
                                format!("[{h}]")
                            } else {
                                h.to_string()
                            };
                            url.port().map_or(h.clone(), |port| format!("{h}:{port}"))
                        });
                        authority.is_some_and(|h| h.eq_ignore_ascii_case(host))
                    }
                    _ => false,
                }
            })
            .unwrap_or(true);
        if cross_site || !origin_matches {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "cross-origin mutations are not allowed" })),
            )
                .into_response();
        }
    }
    next.run(req).await
}

// ---------------------------------------------------------------------------
// Errors

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn not_found(msg: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, msg.into())
    }
    fn conflict(msg: impl Into<String>) -> Self {
        Self(StatusCode::CONFLICT, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn project_or_404(state: &AppState, id: &str) -> ApiResult<Project> {
    if !state.store.exists(id) {
        return Err(ApiError::not_found(format!("project {id} not found")));
    }
    Ok(state.store.get(id)?)
}

fn beat_or_404<'a>(project: &'a Project, beat: &str) -> ApiResult<&'a Beat> {
    project
        .beats
        .iter()
        .find(|b| b.id == beat)
        .ok_or_else(|| ApiError::not_found(format!("beat {beat} not found")))
}

fn ensure_editable(state: &AppState, id: &str) -> ApiResult<()> {
    if state.store.is_running(id) {
        return Err(ApiError::conflict(
            "the pipeline is running; cancel it before editing",
        ));
    }
    if state.project_busy(id) {
        return Err(ApiError::conflict(
            "a visual request is running; wait before editing the project",
        ));
    }
    ensure_not_uploading(state, id)
}

fn ensure_not_uploading(state: &AppState, id: &str) -> ApiResult<()> {
    if state.publish.is_uploading(id) {
        return Err(ApiError::conflict(
            "this project is uploading to YouTube; wait for it to finish",
        ));
    }
    Ok(())
}

/// A clip's narration and footage are its source video.
fn ensure_story(project: &Project, what: &str) -> ApiResult<()> {
    if project.kind == ProjectKind::Clip {
        return Err(ApiError::bad_request(format!(
            "can't edit the {what} of a clip; it comes from the source video"
        )));
    }
    Ok(())
}

/// Claim atomically with the running check. Long network work does not hold
/// the operation/persistence mutex; the claim keeps starts/edits out.
async fn manual_beat(
    state: &AppState,
    id: &str,
    beat_id: &str,
) -> ApiResult<(Project, Beat, BeatClaim)> {
    let _operation = state.store.operation(id).await;
    let project = project_or_404(state, id)?;
    if state.store.is_running(id) || state.beat_busy(id, "_preview") {
        return Err(ApiError::conflict(
            "the pipeline or frame preview is running; wait before changing visuals",
        ));
    }
    ensure_not_uploading(state, id)?;
    ensure_story(&project, "footage")?;
    let beat = beat_or_404(&project, beat_id)?.clone();
    let claim = state
        .claim_beat(id, beat_id)
        .ok_or_else(|| ApiError::conflict("this beat is busy; wait for its current request"))?;
    Ok((project, beat, claim))
}

fn limited_text(value: &str, max: usize, name: &str) -> ApiResult<()> {
    if value.len() > max {
        return Err(ApiError::bad_request(format!(
            "{name} is too long (maximum {max} bytes)"
        )));
    }
    Ok(())
}

fn validate_visual(visual: &VisualPlan) -> ApiResult<()> {
    limited_text(&visual.query, 2000, "visual query")?;
    if visual.alt_queries.len() > 8 {
        return Err(ApiError::bad_request("at most 8 alternative queries"));
    }
    for query in &visual.alt_queries {
        limited_text(query, 2000, "alternative query")?;
    }
    for text in [&visual.on_screen_text, &visual.icon, &visual.quote]
        .into_iter()
        .flatten()
    {
        limited_text(text, 4000, "visual text")?;
    }
    if let Some(meme) = &visual.meme {
        for text in [
            &meme.template,
            &meme.caption,
            &meme.top_text,
            &meme.bottom_text,
        ] {
            limited_text(text, 2000, "meme text")?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Status and settings

async fn status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let checks = doctor::checks(&state).await;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "home": state.paths.home,
        "network_url": crate::net::network_url(state.bound_port()),
        "tools": state.services().tools,
        "ready": doctor::ready(&checks),
        "checks": checks,
    }))
}

async fn get_config(State(state): State<AppState>) -> Json<Config> {
    Json(state.config())
}

async fn put_config(
    State(state): State<AppState>,
    Json(config): Json<Config>,
) -> ApiResult<Json<Config>> {
    config
        .validate()
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    let _settings = state.settings_lock.lock().await;
    state.set_config(config)?;
    Ok(Json(state.config()))
}

fn keys_json(state: &AppState) -> serde_json::Value {
    let status = state.keys.status();
    let services: Vec<_> = KNOWN_SERVICES
        .iter()
        .map(|(id, label)| {
            let slots: Vec<_> = status
                .iter()
                .filter(|s| s.service == *id)
                .map(|s| json!({ "slot": s.slot, "hint": s.hint, "state": s.state }))
                .collect();
            json!({ "id": id, "label": label, "slots": slots })
        })
        .collect();
    json!({ "services": services })
}

async fn get_keys(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(keys_json(&state))
}

#[derive(Deserialize)]
struct KeysBody {
    keys: Vec<String>,
}

/// `""` keeps the key already in that slot; `"__remove__"` deletes it.
async fn put_keys(
    State(state): State<AppState>,
    Path(service): Path<String>,
    Json(body): Json<KeysBody>,
) -> ApiResult<Json<serde_json::Value>> {
    if !KNOWN_SERVICES.iter().any(|(id, _)| *id == service) {
        return Err(ApiError::bad_request(format!(
            "unknown service {service:?}"
        )));
    }
    if body.keys.len() > MAX_KEYS_PER_SERVICE {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_KEYS_PER_SERVICE} keys per service"
        )));
    }
    for key in &body.keys {
        limited_text(key, 4096, "API key")?;
    }
    let _settings = state.settings_lock.lock().await;
    let mut store = state.keys.store();
    let existing = store.services.get(&service).cloned().unwrap_or_default();
    let keys: Vec<String> = body
        .keys
        .iter()
        .enumerate()
        .filter_map(|(i, k)| match k.trim() {
            "" => existing.get(i).cloned(),
            "__remove__" => None,
            k => Some(k.to_string()),
        })
        .collect();
    if keys.is_empty() {
        store.services.remove(&service);
    } else {
        store.services.insert(service, keys);
    }
    store
        .validate()
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    state.keys.replace(store)?;
    Ok(Json(keys_json(&state)))
}

async fn voices(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!(state.services().tts.voices()))
}

#[derive(Deserialize)]
struct PreviewBody {
    voice: String,
    #[serde(default)]
    text: String,
}

async fn voice_preview(
    State(state): State<AppState>,
    Json(body): Json<PreviewBody>,
) -> ApiResult<Response> {
    limited_text(&body.voice, 128, "voice")?;
    limited_text(&body.text, 500, "preview text")?;
    let _slot = state
        .preview_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::conflict("voice previews are busy; try again shortly"))?;
    let text = if body.text.trim().is_empty() {
        "The train leaves at nine. We have time for a cup of coffee."
    } else {
        body.text.trim()
    };
    let out = state
        .paths
        .cache()
        .join(format!("preview-{}.wav", uuid_short()));
    let _cleanup = TempFile(out.clone());
    tokio::time::timeout(
        Duration::from_secs(90),
        state.services().tts.preview(text, &body.voice, &out),
    )
    .await
    .map_err(|_| ApiError::bad_request("voice preview timed out"))??;
    let bytes = tokio::fs::read(&out).await.map_err(anyhow::Error::from)?;
    Ok(([(header::CONTENT_TYPE, "audio/wav")], bytes).into_response())
}

#[derive(Deserialize)]
struct SetupBody {
    what: String,
}

async fn setup(State(state): State<AppState>, Json(body): Json<SetupBody>) -> ApiResult<Response> {
    if !matches!(body.what.as_str(), "yt-dlp" | "kokoro" | "searxng" | "all") {
        return Err(ApiError::bad_request(
            "what must be yt-dlp, kokoro, searxng or all",
        ));
    }
    let slot = state
        .setup_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::conflict("setup is already running"))?;
    tokio::spawn(async move {
        let _slot = slot;
        if let Err(e) = doctor::setup(&state, &body.what).await {
            tracing::warn!("setup {}: {e:#}", body.what);
        }
    });
    Ok((StatusCode::ACCEPTED, Json(json!({ "ok": true }))).into_response())
}

// ---------------------------------------------------------------------------
// Live events

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let updates = BroadcastStream::new(state.events.subscribe()).filter_map(|msg| async move {
        let event = match msg.ok()? {
            ServerEvent::Progress(p) => Event::default().event("progress").json_data(p),
            ServerEvent::Project {
                id,
                updated_at,
                running,
            } => Event::default()
                .event("project")
                .json_data(json!({ "id": id, "updated_at": updated_at, "running": running })),
            ServerEvent::Queue(q) => Event::default().event("queue").json_data(q),
        };
        event.ok().map(Ok)
    });
    let pings = IntervalStream::new(tokio::time::interval(Duration::from_secs(15)))
        .map(|_| Ok(Event::default().event("ping").data(Utc::now().to_rfc3339())));
    Sse::new(futures::stream::select(updates, pings))
}

// ---------------------------------------------------------------------------
// Projects

#[derive(Serialize)]
struct ProjectSummary {
    id: String,
    topic: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    format: Format,
    running: bool,
    stages: Vec<StageState>,
    title: Option<String>,
    duration_s: Option<f32>,
    video_url: Option<String>,
    thumb_url: Option<String>,
    kind: ProjectKind,
    published: Vec<PublishRecord>,
}

fn file_url(state: &AppState, p: &Project, rel: &str, version: i64) -> Option<String> {
    state
        .store
        .dir(&p.id)
        .join(rel)
        .is_file()
        .then(|| format!("/api/projects/{}/files/{rel}?v={version}", p.id))
}

async fn list_projects(State(state): State<AppState>) -> Json<Vec<ProjectSummary>> {
    let out = state
        .store
        .list()
        .into_iter()
        .map(|p| {
            let version = p
                .render
                .as_ref()
                .map(|r| r.rendered_at.timestamp())
                .unwrap_or(0);
            let (video_url, thumb_url) = match &p.render {
                Some(r) => (
                    file_url(&state, &p, &r.path, version),
                    file_url(&state, &p, "render/thumb.png", version)
                        .or_else(|| file_url(&state, &p, "render/thumb.jpg", version)),
                ),
                None => (None, None),
            };
            ProjectSummary {
                running: state.store.is_running(&p.id),
                title: p
                    .script
                    .as_ref()
                    .map(|s| s.title.clone())
                    .filter(|t| !t.is_empty()),
                duration_s: p.render.as_ref().map(|r| r.duration_s),
                video_url,
                thumb_url,
                kind: p.kind,
                published: p.published,
                id: p.id,
                topic: p.topic,
                created_at: p.created_at,
                updated_at: p.updated_at,
                format: p.format,
                stages: p.stages,
            }
        })
        .collect();
    Json(out)
}

#[derive(Deserialize)]
struct CreateBody {
    topic: String,
    #[serde(default)]
    notes: String,
    format: Option<Format>,
    target_seconds: Option<u32>,
    voice: Option<String>,
    autopilot: Option<bool>,
    caption_preset: Option<CaptionPreset>,
    extra_formats: Option<Vec<Format>>,
}

/// Extra formats without the main one or repeats.
pub(crate) fn clean_extra_formats(main: Format, extra: Vec<Format>) -> Vec<Format> {
    let mut out = Vec::new();
    for f in extra {
        if f != main && !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// Changes the format, extra formats and caption style, clearing the render
/// if any of them changed. `Some(None)` captions go back to the configured default.
pub(crate) fn set_video(
    p: &mut Project,
    format: Option<Format>,
    extra_formats: Option<Vec<Format>>,
    caption_preset: Option<Option<CaptionPreset>>,
) {
    if let Some(v) = format
        && p.format != v
    {
        pipeline::reset_from(p, Stage::Render);
        p.format = v;
        p.extra_formats = clean_extra_formats(v, std::mem::take(&mut p.extra_formats));
    }
    if let Some(v) = extra_formats {
        let v = clean_extra_formats(p.format, v);
        if p.extra_formats != v {
            pipeline::reset_from(p, Stage::Render);
            p.extra_formats = v;
        }
    }
    if let Some(v) = caption_preset
        && p.caption_preset != v
    {
        pipeline::reset_from(p, Stage::Render);
        p.caption_preset = v;
    }
}

async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Project>> {
    let topic = body.topic.trim();
    if topic.is_empty() {
        return Err(ApiError::bad_request("topic is required"));
    }
    limited_text(topic, 1000, "topic")?;
    limited_text(&body.notes, 20_000, "notes")?;
    if let Some(voice) = &body.voice {
        limited_text(voice, 128, "voice")?;
    }
    if body
        .target_seconds
        .is_some_and(|s| !(10..=600).contains(&s))
    {
        return Err(ApiError::bad_request(
            "target_seconds must be between 10 and 600",
        ));
    }
    let config = state.config();
    let mut project = Project::new(
        topic,
        body.notes.trim(),
        body.format.unwrap_or(config.video.format),
        body.target_seconds.unwrap_or(config.video.target_seconds),
    );
    project.voice = body.voice.filter(|v| !v.trim().is_empty());
    project.autopilot = body.autopilot.unwrap_or(true);
    project.caption_preset = body.caption_preset;
    project.extra_formats =
        clean_extra_formats(project.format, body.extra_formats.unwrap_or_default());
    let project = state.store.create(project).await?;
    pipeline::start(&state, &project.id, None, false).await?;
    Ok(Json(project))
}

async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Project>> {
    Ok(Json(project_or_404(&state, &id)?))
}

#[derive(Deserialize, Default)]
struct PatchBody {
    topic: Option<String>,
    notes: Option<String>,
    voice: Option<String>,
    autopilot: Option<bool>,
    format: Option<Format>,
    target_seconds: Option<u32>,
    chosen_hook: Option<usize>,
    script_text: Option<String>,
    title: Option<String>,
    description: Option<String>,
    hashtags: Option<Vec<String>>,
    /// `null` goes back to the configured default.
    #[serde(default, deserialize_with = "present")]
    caption_preset: Option<Option<CaptionPreset>>,
    extra_formats: Option<Vec<Format>>,
}

/// Tells a field sent as `null` apart from one left out.
fn present<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

async fn patch_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<Project>> {
    let _operation = state.store.operation(&id).await;
    let current = project_or_404(&state, &id)?;
    ensure_editable(&state, &id)?;
    if body.topic.is_some()
        || body.notes.is_some()
        || body.chosen_hook.is_some()
        || body.script_text.is_some()
    {
        ensure_story(&current, "script")?;
    }
    for (value, max, name) in [
        (body.topic.as_deref(), 1000, "topic"),
        (body.notes.as_deref(), 20_000, "notes"),
        (body.voice.as_deref(), 128, "voice"),
        (body.script_text.as_deref(), 40_000, "script"),
        (body.title.as_deref(), 300, "title"),
        (body.description.as_deref(), 20_000, "description"),
    ] {
        if let Some(value) = value {
            limited_text(value, max, name)?;
        }
    }
    if body.topic.as_ref().is_some_and(|t| t.trim().is_empty()) {
        return Err(ApiError::bad_request("topic cannot be empty"));
    }
    if body
        .target_seconds
        .is_some_and(|s| !(10..=600).contains(&s))
    {
        return Err(ApiError::bad_request(
            "target_seconds must be between 10 and 600",
        ));
    }
    if let Some(tags) = &body.hashtags {
        if tags.len() > 20 {
            return Err(ApiError::bad_request("at most 20 hashtags"));
        }
        for tag in tags {
            limited_text(tag, 100, "hashtag")?;
        }
    }
    let (project, _) = state
        .store
        .update(&id, |p| {
            let topic_changed = body.topic.as_ref().is_some_and(|v| p.topic != v.trim());
            let notes_changed = body.notes.as_ref().is_some_and(|v| p.notes != *v);
            if topic_changed || notes_changed {
                pipeline::reset_from(p, Stage::Research);
            }
            if let Some(v) = body.topic {
                p.topic = v.trim().into();
            }
            if let Some(v) = body.notes {
                p.notes = v;
            }
            if let Some(v) = body.voice {
                let voice = Some(v.trim().to_string()).filter(|v| !v.is_empty());
                if p.voice != voice {
                    pipeline::reset_from(p, Stage::Voice);
                    p.voice = voice;
                }
            }
            if let Some(v) = body.autopilot {
                p.autopilot = v;
            }
            set_video(p, body.format, body.extra_formats, body.caption_preset);
            if let Some(v) = body.target_seconds {
                anyhow::ensure!(
                    p.kind == ProjectKind::Story || p.target_seconds == v,
                    "a clip's length comes from its source moment"
                );
                if p.target_seconds != v {
                    pipeline::reset_from(p, Stage::Script);
                    p.target_seconds = v;
                }
            }
            if let Some(i) = body.chosen_hook {
                anyhow::ensure!(i < p.hooks.len(), "hook {i} does not exist");
                if p.chosen_hook != Some(i) {
                    p.chosen_hook = Some(i);
                    pipeline::reset_from(p, Stage::Script);
                }
            }
            if let Some(text) = body.script_text {
                let text = text.trim().to_string();
                anyhow::ensure!(!text.is_empty(), "the script cannot be empty");
                let script = p.script.get_or_insert_with(Script::default);
                if script.text != text {
                    script.sentences = split_sentences(&text);
                    script.text = text;
                    pipeline::reset_from(p, Stage::Beats);
                    let s = p.stage_mut(Stage::Script);
                    s.status = clipcut_core::Status::Done;
                    s.error = None;
                }
            }
            if body.title.is_some() || body.description.is_some() || body.hashtags.is_some() {
                pipeline::reset_from(p, Stage::Render);
                let script = p.script.get_or_insert_with(Script::default);
                if let Some(v) = body.title {
                    script.title = v;
                }
                if let Some(v) = body.description {
                    script.description = v;
                }
                if let Some(v) = body.hashtags {
                    script.hashtags = v;
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(project))
}

/// Sentence split that keeps the punctuation; joined with spaces it reads like the input.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        current.push(c);
        let prev_terminal = i > 0 && matches!(chars[i - 1], '.' | '!' | '?' | '…');
        let ends = matches!(c, '.' | '!' | '?' | '…')
            || c == '\n'
            || (matches!(c, '"' | '\'' | '”' | ')') && prev_terminal);
        let next_is_space = chars.get(i + 1).is_none_or(|n| n.is_whitespace());
        let next_not_closing = chars
            .get(i + 1)
            .is_none_or(|n| !matches!(n, '.' | '!' | '?' | '"' | '\'' | '”' | ')'));
        if ends && next_is_space && next_not_closing {
            let s = current.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let _operation = state.store.operation(&id).await;
    project_or_404(&state, &id)?;
    if state.project_busy(&id) && !state.store.is_running(&id) {
        return Err(ApiError::conflict(
            "wait for the visual request before deleting",
        ));
    }
    ensure_not_uploading(&state, &id)?;
    state.store.abort_and_wait(&id).await;
    state.store.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct RunBody {
    from: Option<Stage>,
    #[serde(default)]
    only: bool,
}

async fn run_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    project_or_404(&state, &id)?;
    let body: RunBody = if body.iter().all(u8::is_ascii_whitespace) {
        RunBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::bad_request(format!("bad body: {e}")))?
    };
    if state.store.is_running(&id) || state.project_busy(&id) {
        return Err(ApiError::conflict("this project is already running"));
    }
    ensure_not_uploading(&state, &id)?;
    pipeline::start(&state, &id, body.from, body.only)
        .await
        .map_err(|e| {
            if state.store.is_running(&id)
                || state.project_busy(&id)
                || state.publish.is_uploading(&id)
            {
                ApiError::conflict(format!("{e:#}"))
            } else {
                ApiError::bad_request(format!("{e:#}"))
            }
        })?;
    Ok((StatusCode::ACCEPTED, Json(state.store.get(&id)?)).into_response())
}

async fn cancel_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    project_or_404(&state, &id)?;
    let project = pipeline::cancel(&state, &id).await?;
    Ok((StatusCode::ACCEPTED, Json(project)).into_response())
}

async fn duplicate_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Project>> {
    let src = project_or_404(&state, &id)?;
    let mut copy = Project::new(
        src.topic.clone(),
        src.notes.clone(),
        src.format,
        src.target_seconds,
    );
    copy.voice = src.voice.clone();
    copy.autopilot = src.autopilot;
    copy.research = src.research.clone();
    copy.hooks = src.hooks.clone();
    copy.chosen_hook = src.chosen_hook;
    copy.script = src.script.clone();
    copy.caption_preset = src.caption_preset;
    copy.extra_formats = src.extra_formats.clone();
    copy.kind = src.kind;
    copy.clip = src.clip.clone();
    // A clip's beat is its source moment; the copy fetches it again.
    if src.kind == ProjectKind::Clip {
        copy.beats = src
            .beats
            .iter()
            .map(|b| Beat {
                candidates: Vec::new(),
                chosen: None,
                asset: None,
                status: BeatStatus::Pending,
                error: None,
                locked: false,
                start_s: 0.0,
                end_s: 0.0,
                ..b.clone()
            })
            .collect();
    }
    for s in &src.stages {
        let keep = match s.stage {
            Stage::Research | Stage::Hooks | Stage::Script => {
                matches!(s.status, clipcut_core::Status::Done | clipcut_core::Status::Skipped)
            }
            Stage::Beats => src.kind == ProjectKind::Clip,
            _ => false,
        };
        if keep {
            *copy.stage_mut(s.stage) = s.clone();
        }
    }
    Ok(Json(state.store.create(copy).await?))
}

// ---------------------------------------------------------------------------
// Beats

#[derive(Deserialize)]
struct BeatPatch {
    text: Option<String>,
    visual: Option<VisualPlan>,
    locked: Option<bool>,
}

async fn patch_beat(
    State(state): State<AppState>,
    Path((id, beat)): Path<(String, String)>,
    Json(body): Json<BeatPatch>,
) -> ApiResult<Json<Beat>> {
    let _operation = state.store.operation(&id).await;
    ensure_editable(&state, &id)?;
    let project = project_or_404(&state, &id)?;
    beat_or_404(&project, &beat)?;
    if body.text.is_some() || body.visual.is_some() {
        ensure_story(&project, "footage and narration")?;
    }
    if let Some(text) = &body.text {
        limited_text(text, 4000, "beat text")?;
        if text.trim().is_empty() {
            return Err(ApiError::bad_request("beat text cannot be empty"));
        }
    }
    if let Some(visual) = &body.visual {
        validate_visual(visual)?;
    }
    let (_, (updated, old)) = state
        .store
        .update(&id, |p| {
            let b = p
                .beats
                .iter_mut()
                .find(|b| b.id == beat)
                .ok_or_else(|| anyhow::anyhow!("beat {beat} not found"))?;
            let narration_changed = body.text.as_ref().is_some_and(|text| b.text != text.trim());
            let mut old = None;
            if let Some(text) = body.text {
                b.text = text.trim().to_string();
            }
            if let Some(visual) = body.visual {
                let media_changed = b.visual.kind != visual.kind
                    || b.visual.query != visual.query
                    || b.visual.quote != visual.quote
                    || serde_json::to_value(&b.visual.meme)? != serde_json::to_value(&visual.meme)?;
                if media_changed {
                    old = b.asset.take().map(|a| a.path);
                    b.candidates.clear();
                    b.chosen = None;
                    b.error = None;
                    b.status = if visual.kind == VisualKind::Text {
                        BeatStatus::Ready
                    } else {
                        BeatStatus::Pending
                    };
                    b.locked = false;
                }
                b.visual = visual;
            }
            if let Some(locked) = body.locked {
                b.locked = locked;
            }
            if narration_changed {
                let sentences: Vec<String> = p.beats.iter().map(|b| b.text.clone()).collect();
                let script = p.script.get_or_insert_with(Script::default);
                script.text = sentences.join(" ");
                script.sentences = sentences;
                pipeline::reset_from(p, Stage::Voice);
            }
            pipeline::reset_from(p, Stage::Render);
            pipeline::refresh_assets_status(p);
            let updated = p.beats.iter().find(|b| b.id == beat).unwrap().clone();
            Ok((updated, old))
        })
        .await?;
    if let Some(old) = old {
        pipeline::remove_asset_file(&state.store.dir(&id), &old);
    }
    Ok(Json(updated))
}

#[derive(Deserialize, Default)]
struct SearchBody {
    kind: Option<VisualKind>,
    query: Option<String>,
}

async fn search_beat(
    State(state): State<AppState>,
    Path((id, beat_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<Beat>> {
    let body: SearchBody = if body.iter().all(u8::is_ascii_whitespace) {
        SearchBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::bad_request(format!("bad body: {e}")))?
    };
    if let Some(query) = &body.query {
        limited_text(query, 2000, "query")?;
    }
    let (_, beat, _claim) = manual_beat(&state, &id, &beat_id).await?;
    let svc = state.services();
    let progress = state.progress(&id);
    let mut visual = beat.visual.clone();
    if let Some(kind) = body.kind {
        visual.kind = kind;
    }
    if let Some(q) = body
        .query
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty())
    {
        visual.query = q;
    }
    if visual.kind == VisualKind::Text {
        return Err(ApiError::bad_request("text beats have nothing to search"));
    }
    if visual.kind == VisualKind::Meme {
        visual.meme = Some(pipeline::plan_meme(&svc, &beat, &visual).await?);
    }
    progress.beat(
        &beat_id,
        format!("Searching {:?}: {}", visual.kind, visual.query),
    );
    let candidates = svc.sources.candidates(visual.kind, &visual, 12).await?;
    progress.beat(&beat_id, format!("{} result(s)", candidates.len()));
    let (_, (updated, old)) = state
        .store
        .update(&id, |p| {
            let b = p
                .beats
                .iter_mut()
                .find(|b| b.id == beat_id)
                .ok_or_else(|| anyhow::anyhow!("beat {beat_id} was removed"))?;
            let media_changed = b.visual.kind != visual.kind
                || b.visual.query != visual.query
                || serde_json::to_value(&b.visual.meme)? != serde_json::to_value(&visual.meme)?;
            let old = if media_changed {
                b.asset.take().map(|a| a.path)
            } else {
                None
            };
            b.visual = visual;
            b.candidates = candidates;
            b.chosen = None;
            if media_changed {
                b.status = BeatStatus::Pending;
                b.locked = false;
                b.error = None;
            }
            let updated = b.clone();
            if media_changed {
                pipeline::reset_from(p, Stage::Render);
                pipeline::refresh_assets_status(p);
            }
            Ok((updated, old))
        })
        .await?;
    if let Some(old) = old {
        pipeline::remove_asset_file(&state.store.dir(&id), &old);
    }
    Ok(Json(updated))
}

#[derive(Deserialize)]
struct ChooseBody {
    index: usize,
}

async fn choose_beat(
    State(state): State<AppState>,
    Path((id, beat_id)): Path<(String, String)>,
    Json(body): Json<ChooseBody>,
) -> ApiResult<Json<Beat>> {
    let (project, beat, _claim) = manual_beat(&state, &id, &beat_id).await?;
    let candidate = beat
        .candidates
        .get(body.index)
        .cloned()
        .ok_or_else(|| ApiError::bad_request("no candidate at that index"))?;
    let svc = state.services();
    let progress = state.progress(&id);
    let dir = state.store.dir(&id);
    pipeline::set_beat_status(&state, &id, &beat_id, BeatStatus::Searching).await?;
    let mut visual = beat.visual.clone();
    visual.kind = candidate.kind;
    match pipeline::fetch_candidate(&svc, &project, &dir, &beat, &visual, &candidate, &progress)
        .await
    {
        Ok(asset) => {
            let outcome = Outcome {
                visual,
                candidates: beat.candidates.clone(),
                chosen: Some(body.index),
                asset: Some(asset),
                status: BeatStatus::Ready,
                error: None,
            };
            Ok(Json(
                pipeline::save_outcome(&state, &id, &beat_id, outcome, true).await?,
            ))
        }
        Err(e) => {
            let msg = format!("{e:#}");
            restore_beat(&state, &id, &beat_id, &msg).await;
            Err(ApiError(StatusCode::BAD_GATEWAY, msg))
        }
    }
}

/// After a failed choose/upload the old asset is still there; show the error next to it.
async fn restore_beat(state: &AppState, id: &str, beat_id: &str, error: &str) {
    let _ = state
        .store
        .update(id, |p| {
            if let Some(b) = p.beats.iter_mut().find(|b| b.id == beat_id) {
                b.status = if b.asset.is_some() || b.visual.kind == VisualKind::Text {
                    BeatStatus::Ready
                } else {
                    BeatStatus::Failed
                };
                b.error = Some(error.to_string());
            }
            Ok(())
        })
        .await;
}

async fn auto_beat(
    State(state): State<AppState>,
    Path((id, beat_id)): Path<(String, String)>,
) -> ApiResult<Json<Beat>> {
    let (project, beat, _claim) = manual_beat(&state, &id, &beat_id).await?;
    let svc = state.services();
    let progress = state.progress(&id);
    pipeline::set_beat_status(&state, &id, &beat_id, BeatStatus::Searching).await?;
    let outcome =
        pipeline::auto_visual(&svc, &project, &state.store.dir(&id), &beat, &progress).await;
    Ok(Json(
        pipeline::save_outcome(&state, &id, &beat_id, outcome, false).await?,
    ))
}

async fn upload_beat(
    State(state): State<AppState>,
    Path((id, beat_id)): Path<(String, String)>,
    mut multipart: Multipart,
) -> ApiResult<Json<Beat>> {
    let (_, beat, _claim) = manual_beat(&state, &id, &beat_id).await?;
    let dir = state.store.dir(&id);
    let mut saved = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let ext = field
            .file_name()
            .and_then(|n| FsPath::new(n).extension())
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .or_else(|| {
                field
                    .content_type()
                    .and_then(|m| mime_guess::get_mime_extensions_str(m))
                    .and_then(|e| e.first())
                    .map(|e| e.to_string())
            })
            .ok_or_else(|| ApiError::bad_request("could not tell the file type"))?;
        if !UPLOAD_EXTENSIONS.contains(&ext.as_str()) {
            return Err(ApiError::bad_request(
                "upload a video, GIF or raster image; SVG is not accepted",
            ));
        }
        let rel = format!("assets/{beat_id}-upload-{}.{ext}", uuid_short());
        tokio::fs::create_dir_all(ProjectPaths(&dir).assets())
            .await
            .map_err(anyhow::Error::from)?;
        let cleanup = TempFile(dir.join(&rel));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&cleanup.0)
            .await
            .map_err(anyhow::Error::from)?;
        let mut size = 0usize;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?
        {
            size = size
                .checked_add(chunk.len())
                .ok_or_else(|| ApiError::bad_request("file too large"))?;
            if size > MAX_UPLOAD_BYTES {
                return Err(ApiError(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "file exceeds 256 MiB".into(),
                ));
            }
            file.write_all(&chunk).await.map_err(anyhow::Error::from)?;
        }
        if size == 0 {
            return Err(ApiError::bad_request("file is empty"));
        }
        file.flush().await.map_err(anyhow::Error::from)?;
        drop(file);
        saved = Some((rel, cleanup));
        break;
    }
    let (rel, mut cleanup) = saved
        .ok_or_else(|| ApiError::bad_request("send the file in a multipart field named `file`"))?;
    let asset = pipeline::upload_asset(&state.services(), &dir, &rel)
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    let mut visual = beat.visual.clone();
    if visual.kind == VisualKind::Text {
        visual.kind = VisualKind::Image;
    }
    let outcome = Outcome {
        visual,
        candidates: beat.candidates.clone(),
        chosen: None,
        asset: Some(asset),
        status: BeatStatus::Ready,
        error: None,
    };
    let updated = pipeline::save_outcome(&state, &id, &beat_id, outcome, true).await?;
    cleanup.0 = PathBuf::new();
    Ok(Json(updated))
}

/// Owns an unpublished temporary output and cleans up errors/cancellations.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Files

/// Resolve a relative path inside the project dir; refuses `..` and absolute paths.
fn safe_join(base: &FsPath, rel: &str) -> Option<PathBuf> {
    let rel = FsPath::new(rel);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let path = base.join(rel);
    let canonical = path.canonicalize().ok()?;
    canonical
        .starts_with(base.canonicalize().ok()?)
        .then_some(canonical)
}

async fn project_file(
    State(state): State<AppState>,
    Path((id, rel)): Path<(String, String)>,
    req: Request,
) -> ApiResult<Response> {
    if !state.store.exists(&id) {
        return Err(ApiError::not_found("project not found"));
    }
    let path = safe_join(&state.store.dir(&id), &rel)
        .filter(|p| p.is_file())
        .ok_or_else(|| ApiError::not_found("file not found"))?;
    let mut res = ServeFile::new(path)
        .oneshot(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))?
        .map(Body::new);
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    res.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'"),
    );
    Ok(res)
}

#[derive(Deserialize)]
struct StillQuery {
    #[serde(default)]
    t: f32,
}

async fn still(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<StillQuery>,
) -> ApiResult<Response> {
    if !q.t.is_finite() || q.t < 0.0 {
        return Err(ApiError::bad_request(
            "t must be a finite, nonnegative time",
        ));
    }
    let _slot = state
        .still_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::conflict("a frame preview is already rendering"))?;
    let operation = state.store.operation(&id).await;
    ensure_editable(&state, &id)?;
    let project = project_or_404(&state, &id)?;
    let _claim = state
        .claim_beat(&id, "_preview")
        .ok_or_else(|| ApiError::conflict("a frame preview is already rendering"))?;
    drop(operation);
    if project.voice_track.is_none() {
        return Err(ApiError::bad_request(
            "the voiceover is needed before previewing frames",
        ));
    }
    let svc = state.services();
    let out = state
        .paths
        .cache()
        .join(format!("still-{id}-{}.png", uuid_short()));
    let _cleanup = TempFile(out.clone());
    tokio::time::timeout(pipeline::STILL_TIMEOUT, async {
        let timeline = pipeline::build_timeline(&svc, &project, &state.store.dir(&id)).await?;
        let at = q.t.clamp(0.0, timeline.duration_s.max(0.0));
        svc.renderer.still(&timeline, at, &out).await
    })
    .await
    .map_err(|_| anyhow::anyhow!("frame preview timed out"))??;
    let bytes = tokio::fs::read(&out).await.map_err(anyhow::Error::from)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize, Default)]
struct DownloadQuery {
    /// main (default), vertical, horizontal, square, srt, vtt, thumb or credits.
    file: Option<String>,
}

async fn download(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DownloadQuery>,
    req: Request,
) -> ApiResult<Response> {
    let project = project_or_404(&state, &id)?;
    let render = project
        .render
        .as_ref()
        .ok_or_else(|| ApiError::not_found("not rendered yet"))?;
    let which = q.file.as_deref().unwrap_or("main");
    let missing = || ApiError::not_found(format!("this render has no {which} file"));
    let (rel, suffix, ext) = match which {
        "main" => (render.path.as_str(), String::new(), "mp4"),
        "vertical" | "horizontal" | "square" => {
            let format: Format = serde_json::from_value(json!(which))
                .map_err(|_| ApiError::bad_request("unknown format"))?;
            let (rel, _, _) = render.file_for(project.format, format).ok_or_else(missing)?;
            let suffix = if format == project.format {
                String::new()
            } else {
                format!("-{which}")
            };
            (rel, suffix, "mp4")
        }
        "srt" | "vtt" => {
            let subs = render.subtitles.as_ref().ok_or_else(missing)?;
            let rel = if which == "srt" { &subs.srt } else { &subs.vtt };
            (rel.as_str(), String::new(), which)
        }
        "thumb" => (
            render.thumbnail_jpg.as_deref().ok_or_else(missing)?,
            "-thumbnail".to_string(),
            "jpg",
        ),
        "credits" => (render.credits_path.as_str(), "-credits".to_string(), "txt"),
        _ => {
            return Err(ApiError::bad_request(
                "file must be main, vertical, horizontal, square, srt, vtt, thumb or credits",
            ));
        }
    };
    let path = safe_join(&state.store.dir(&id), rel)
        .filter(|p| p.is_file())
        .ok_or_else(missing)?;
    let ext = FsPath::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or(ext)
        .to_ascii_lowercase();
    let name = slug(
        project
            .script
            .as_ref()
            .map(|s| s.title.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or(&project.topic),
    );
    let mut res = ServeFile::new(path)
        .oneshot(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))?
        .map(Body::new);
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}{suffix}.{ext}\"")) {
        res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

// ---------------------------------------------------------------------------
// Publishing

fn upstream(e: anyhow::Error) -> ApiError {
    ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}"))
}

fn ensure_youtube_connected(state: &AppState) -> ApiResult<()> {
    if !state.publish.status().connected {
        return Err(ApiError::bad_request(
            "YouTube isn't connected; connect it in Settings → YouTube",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct PublishBody {
    /// Which render to upload (default: the main one).
    #[serde(default)]
    format: Option<Format>,
    #[serde(flatten)]
    plan: PublishPlan,
}

async fn publish_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut body): Json<PublishBody>,
) -> ApiResult<Json<PublishRecord>> {
    body.plan.default_privacy(&state.config().publish.privacy);
    queue::validate_plan(&body.plan).map_err(ApiError::bad_request)?;
    let project = project_or_404(&state, &id)?;
    if state.store.is_running(&id) {
        return Err(ApiError::conflict(
            "the project is still running; publish it after the render finishes",
        ));
    }
    ensure_not_uploading(&state, &id)?;
    let render = project
        .render
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("render the project before publishing it"))?;
    let format = body.format.unwrap_or(project.format);
    if render.file_for(project.format, format).is_none() {
        return Err(ApiError::bad_request(format!(
            "this project has no {} render; add that format and render again",
            format.slug()
        )));
    }
    ensure_youtube_connected(&state)?;
    // A closed tab must not cut an upload off halfway.
    let task = tokio::spawn(async move {
        let progress = state.progress(&id);
        let result = publish::upload(
            &state,
            &id,
            body.format,
            &body.plan.privacy,
            body.plan.publish_at,
            &progress,
        )
        .await;
        if let Err(e) = &result {
            progress.error(format!("Publishing failed: {e:#}"));
        }
        result
    });
    let record = task
        .await
        .map_err(|e| anyhow::anyhow!("the upload stopped: {e}"))?
        .map_err(|e| {
            let msg = format!("{e:#}");
            if msg.starts_with("this project is already uploading")
                || msg.starts_with("the project is still running")
            {
                ApiError::conflict(msg)
            } else {
                ApiError(StatusCode::BAD_GATEWAY, msg)
            }
        })?;
    Ok(Json(record))
}

async fn youtube_status(State(state): State<AppState>) -> Json<AuthStatus> {
    Json(state.publish.status())
}

async fn put_youtube_client(
    State(state): State<AppState>,
    Json(body): Json<ClientBody>,
) -> ApiResult<Json<AuthStatus>> {
    let (client_id, client_secret) =
        publish::parse_client(&body).map_err(ApiError::bad_request)?;
    state.publish.set_client(client_id, client_secret).await?;
    Ok(Json(state.publish.status()))
}

#[derive(Deserialize, Default)]
struct DisconnectQuery {
    /// Forget the OAuth client too.
    #[serde(default)]
    all: bool,
}

async fn disconnect_youtube(
    State(state): State<AppState>,
    Query(q): Query<DisconnectQuery>,
) -> ApiResult<Json<AuthStatus>> {
    state.publish.disconnect(q.all).await?;
    Ok(Json(state.publish.status()))
}

async fn connect_youtube(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    if !state.publish.status().configured {
        return Err(ApiError::bad_request(
            "add your Google OAuth client in Settings → YouTube first",
        ));
    }
    let url = publish::connect(&state).await?;
    Ok(Json(json!({ "url": url })))
}

#[derive(Deserialize)]
struct CallbackBody {
    url: String,
}

async fn youtube_callback_url(
    State(state): State<AppState>,
    Json(body): Json<CallbackBody>,
) -> ApiResult<Json<AuthStatus>> {
    limited_text(&body.url, 8192, "url")?;
    let status = publish::finish_from_url(&state, &body.url)
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(status))
}

// ---------------------------------------------------------------------------
// Batch queue

async fn get_queue(State(state): State<AppState>) -> Json<QueueState> {
    Json(state.queue.snapshot())
}

fn wants_publish(body: &AddBody) -> bool {
    body.items.iter().any(|i| i.options.publish.is_some())
        || (!body.topics.is_empty() && body.options.publish.is_some())
}

async fn add_queue(
    State(state): State<AppState>,
    Json(mut body): Json<AddBody>,
) -> ApiResult<Json<Vec<QueueItem>>> {
    body.default_privacy(&state.config().publish.privacy);
    queue::validate(&body).map_err(ApiError::bad_request)?;
    if wants_publish(&body) {
        ensure_youtube_connected(&state)?;
    }
    Ok(Json(state.queue.add(body)?))
}

#[derive(Deserialize)]
struct QueuePatch {
    paused: bool,
}

async fn patch_queue(
    State(state): State<AppState>,
    Json(body): Json<QueuePatch>,
) -> ApiResult<Json<QueueState>> {
    state.queue.set_paused(body.paused)?;
    Ok(Json(state.queue.snapshot()))
}

async fn remove_queue_item(
    State(state): State<AppState>,
    Path(item): Path<String>,
) -> ApiResult<Json<QueueState>> {
    if !queue::remove(&state, &item).await? {
        return Err(ApiError::not_found(format!("queue item {item} not found")));
    }
    Ok(Json(state.queue.snapshot()))
}

async fn retry_queue_item(
    State(state): State<AppState>,
    Path(item): Path<String>,
) -> ApiResult<Json<QueueState>> {
    if !state.queue.retry(&item)? {
        return Err(ApiError::not_found(format!("queue item {item} not found")));
    }
    Ok(Json(state.queue.snapshot()))
}

async fn clear_queue(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let removed = state.queue.clear_finished()?;
    Ok(Json(json!({ "removed": removed })))
}

// ---------------------------------------------------------------------------
// Topic ideas and clips

#[derive(Deserialize)]
struct IdeasQuery {
    #[serde(default = "default_idea_source")]
    source: IdeaSource,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    geo: String,
    #[serde(default)]
    limit: Option<usize>,
}

fn default_idea_source() -> IdeaSource {
    IdeaSource::Trends
}

async fn ideas(
    State(state): State<AppState>,
    Query(q): Query<IdeasQuery>,
) -> ApiResult<Json<Vec<Idea>>> {
    if let Some(query) = &q.q {
        limited_text(query, 200, "q")?;
    }
    let geo = q.geo.trim();
    if !geo.is_empty() && !(geo.len() == 2 && geo.bytes().all(|b| b.is_ascii_alphabetic())) {
        return Err(ApiError::bad_request(
            "geo must be a 2-letter country code like US",
        ));
    }
    let limit = q.limit.unwrap_or(12).clamp(1, 25);
    let ideas = state
        .services()
        .sources
        .ideas(q.source, q.q.as_deref(), geo, limit)
        .await
        .map_err(upstream)?;
    Ok(Json(ideas))
}

#[derive(Deserialize)]
struct ClipBody {
    #[serde(flatten)]
    req: ClipRequest,
    #[serde(default)]
    publish: Option<PublishPlan>,
    #[serde(default)]
    publish_every_minutes: Option<u32>,
    /// Client-chosen id that tags this request's progress events.
    #[serde(default)]
    job: Option<String>,
}

fn validate_clip_body(body: &ClipBody) -> Result<(), String> {
    let url = body.req.url.trim();
    if url.is_empty() {
        return Err("url is required".into());
    }
    if url.len() > 2000 {
        return Err("url is too long (maximum 2000 bytes)".into());
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("url must start with http:// or https://".into());
    }
    if !(1..=clips::MAX_CLIPS).contains(&body.req.count) {
        return Err(format!("count must be between 1 and {}", clips::MAX_CLIPS));
    }
    let (min, max) = (body.req.min_s, body.req.max_s);
    let in_range = |s: f32| s.is_finite() && (5.0..=180.0).contains(&s);
    if !in_range(min) || !in_range(max) || min > max {
        return Err("min_s and max_s must be 5–180 seconds, min_s ≤ max_s".into());
    }
    if let Some(plan) = &body.publish {
        if !body.req.start {
            return Err("publish needs start: the clips publish when the queue renders them".into());
        }
        queue::validate_plan(plan)?;
    }
    queue::validate_spacing(body.publish_every_minutes)?;
    if body.job.as_ref().is_some_and(|j| {
        j.is_empty()
            || j.len() > 40
            || !j.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) {
        return Err("job must be 1–40 letters, digits, - or _".into());
    }
    Ok(())
}

/// Find the best moments of a long video and save one project per moment.
/// Progress goes out under `job` (or a new id) while the request waits.
async fn create_clips(
    State(state): State<AppState>,
    Json(mut body): Json<ClipBody>,
) -> ApiResult<Json<Vec<Project>>> {
    if let Some(plan) = &mut body.publish {
        plan.default_privacy(&state.config().publish.privacy);
    }
    validate_clip_body(&body).map_err(ApiError::bad_request)?;
    if body.publish.is_some() {
        ensure_youtube_connected(&state)?;
    }
    body.req.url = body.req.url.trim().to_string();
    let job = body
        .job
        .take()
        .unwrap_or_else(|| format!("clips-{}", &uuid_short()[..10]));
    // Keep working if the browser goes away; the projects show up in the list.
    let task = tokio::spawn(async move {
        let progress = state.progress(&job);
        let projects = clips::create(&state, &body.req, &progress).await?;
        if body.req.start && !projects.is_empty() {
            state
                .queue
                .add_projects(&projects, body.publish, body.publish_every_minutes)?;
        }
        anyhow::Ok(projects)
    });
    let projects = task
        .await
        .map_err(|e| anyhow::anyhow!("the clip finder stopped: {e}"))?
        .map_err(upstream)?;
    Ok(Json(projects))
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 60 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "clipcut".into()
    } else {
        out
    }
}

fn uuid_short() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

// ---------------------------------------------------------------------------
// Embedded UI

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let (file, name) = match Web::get(path) {
        Some(f) => (f, path),
        // Client-side routes (no extension) get the app shell.
        None if !path.rsplit('/').next().unwrap_or("").contains('.') => {
            match Web::get("index.html") {
                Some(f) => (f, "index.html"),
                None => return (StatusCode::NOT_FOUND, "web UI missing").into_response(),
            }
        }
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let mime = mime_guess::from_path(name).first_or_octet_stream();
    let cache = if name == "index.html" {
        "no-cache"
    } else {
        "public, max-age=300"
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestHome, clip_project, finished_project};

    const TEST_HOST: &str = "192.168.100.226:7878";

    fn request(method: &str, path: &str, body: serde_json::Value) -> Request {
        Request::builder()
            .method(method)
            .uri(format!("http://{TEST_HOST}{path}"))
            .header(header::HOST, TEST_HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn call(state: &AppState, method: &str, path: &str, body: serde_json::Value) -> Response {
        router(state.clone())
            .oneshot(request(method, path, body))
            .await
            .unwrap()
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn splits_sentences() {
        let s = split_sentences("Wait. Is that real?! Yes… it is \"real.\" Done");
        assert_eq!(
            s,
            vec!["Wait.", "Is that real?!", "Yes…", "it is \"real.\"", "Done"]
        );
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Why NASA's Moon Rocket?"), "why-nasa-s-moon-rocket");
    }

    #[tokio::test]
    async fn invalid_config_returns_400_without_changing_disk() {
        let home = TestHome::new();
        let state = home.state();
        let before = std::fs::read(state.paths.config_file()).unwrap();
        let mut config = serde_json::to_value(state.config()).unwrap();
        config["video"]["fps"] = json!(0);
        let response = call(&state, "PUT", "/api/config", config).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(std::fs::read(state.paths.config_file()).unwrap(), before);
    }

    #[tokio::test]
    async fn keys_are_normalized_masked_and_validation_keeps_the_old_store() {
        let home = TestHome::new();
        let state = home.state();
        let response = call(
            &state,
            "PUT",
            "/api/keys/giphy",
            json!({
                "keys": ["abc", " abc ", "fixture-secret-1234"]
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let public = response_json(response).await;
        assert!(!public.to_string().contains("fixture-secret"));
        assert!(!public.to_string().contains("abc"));
        assert_eq!(state.keys.store().services["giphy"].len(), 2);
        let before = std::fs::read(state.paths.keys_file()).unwrap();
        for keys in [
            json!(["bad\nkey"]),
            json!(["a", "b", "c", "d", "e", "f"]),
            json!(["x".repeat(4097)]),
        ] {
            let response = call(&state, "PUT", "/api/keys/giphy", json!({"keys": keys})).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(std::fs::read(state.paths.keys_file()).unwrap(), before);
        }
        let response = call(
            &state,
            "PUT",
            "/api/keys/giphy",
            json!({"keys": ["", "__remove__"]}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.keys.store().services["giphy"], ["abc"]);
        let response = call(&state, "PUT", "/api/keys/freesound", json!({"keys": []})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn host_and_browser_origin_checks_reject_drive_by_access() {
        let home = TestHome::new();
        let state = home.state();
        for host in ["evil.example:7878", "192.168.100.226:9999"] {
            let mut req = request("GET", "/api/config", json!(null));
            req.headers_mut()
                .insert(header::HOST, HeaderValue::from_str(host).unwrap());
            let response = router(state.clone()).oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        for (name, value) in [
            (header::ORIGIN, "https://evil.example"),
            (
                header::HeaderName::from_static("sec-fetch-site"),
                "cross-site",
            ),
        ] {
            let mut req = request(
                "PUT",
                "/api/config",
                serde_json::to_value(state.config()).unwrap(),
            );
            req.headers_mut()
                .insert(name, HeaderValue::from_static(value));
            let response = router(state.clone()).oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let mut req = request(
            "PUT",
            "/api/config",
            serde_json::to_value(state.config()).unwrap(),
        );
        req.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("http://192.168.100.226:7878"),
        );
        let response = router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn running_projects_reject_edits_and_manual_visual_requests() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        state
            .store
            .spawn_pipeline(&project.id, futures::future::pending())
            .unwrap();
        for (method, tail, body) in [
            ("PATCH", "", json!({"title": "Cannot change"})),
            ("PATCH", "/beats/b01", json!({"locked": true})),
            ("POST", "/beats/b01/search", json!({})),
            ("POST", "/beats/b01/choose", json!({"index": 0})),
            ("POST", "/beats/b01/auto", json!({})),
            ("POST", "/run", json!({"from": "render"})),
        ] {
            let response = call(
                &state,
                method,
                &format!("/api/projects/{}{tail}", project.id),
                body,
            )
            .await;
            assert_eq!(response.status(), StatusCode::CONFLICT, "{method} {tail}");
        }
        assert_eq!(
            state.store.get(&project.id).unwrap().script.unwrap().title,
            ""
        );
        let response = call(
            &state,
            "POST",
            &format!("/api/projects/{}/cancel", project.id),
            json!({}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(!state.store.is_running(&project.id));
    }

    #[tokio::test]
    async fn manual_claims_block_project_edits_starts_and_deletion() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let claim = state.claim_beat(&project.id, "b01").unwrap();
        for (method, tail, body) in [
            ("PATCH", "", json!({"title": "Busy"})),
            ("POST", "/run", json!({"from": "render"})),
            ("DELETE", "", json!({})),
        ] {
            let response = call(
                &state,
                method,
                &format!("/api/projects/{}{tail}", project.id),
                body,
            )
            .await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
        }
        drop(claim);
        let response = call(
            &state,
            "PATCH",
            &format!("/api/projects/{}", project.id),
            json!({"title": "Ready"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!state.project_busy(&project.id));
    }

    #[tokio::test]
    async fn metadata_edits_preserve_narration_but_script_edits_clear_it() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let path = format!("/api/projects/{}", project.id);
        let response = call(&state, "PATCH", &path, json!({"title": "A revised title"})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let revised = state.store.get(&project.id).unwrap();
        assert!(revised.voice_track.is_some());
        assert_eq!(revised.beats.len(), 1);
        assert!(revised.render.is_none());
        let response = call(
            &state,
            "PATCH",
            &path,
            json!({"script_text": "Another sentence. One more!"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let revised = state.store.get(&project.id).unwrap();
        assert!(revised.voice_track.is_none());
        assert!(revised.beats.is_empty());
        assert_eq!(
            revised.script.unwrap().sentences,
            ["Another sentence.", "One more!"]
        );
    }

    #[tokio::test]
    async fn narration_beat_edits_keep_script_and_voice_dependencies_consistent() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let response = call(
            &state,
            "PATCH",
            &format!("/api/projects/{}/beats/b01", project.id),
            json!({
                "text": "A corrected narration sentence."
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let revised = state.store.get(&project.id).unwrap();
        assert_eq!(
            revised.script.unwrap().text,
            "A corrected narration sentence."
        );
        assert!(revised.voice_track.is_none());
        assert!(revised.render.is_none());
        assert_eq!(revised.beats[0].end_s, 0.0);
    }

    #[tokio::test]
    async fn safe_files_support_ranges_but_not_traversals_or_symlink_escape() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let dir = state.store.dir(&project.id);
        let assets = dir.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(assets.join("fixture.mp4"), b"0123456789").unwrap();
        std::fs::write(home.0.join("private.txt"), b"private").unwrap();
        assert!(safe_join(&dir, "../private.txt").is_none());
        assert!(safe_join(&dir, "/etc/passwd").is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(home.0.join("private.txt"), assets.join("escape.txt"))
                .unwrap();
            let response = call(
                &state,
                "GET",
                &format!("/api/projects/{}/files/assets/escape.txt", project.id),
                json!(null),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        let mut req = request(
            "GET",
            &format!("/api/projects/{}/files/assets/fixture.mp4", project.id),
            json!(null),
        );
        req.headers_mut()
            .insert(header::RANGE, HeaderValue::from_static("bytes=2-5"));
        let response = router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert!(
            response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .starts_with("sandbox")
        );
        let bytes = axum::body::to_bytes(response.into_body(), 20)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"2345");
    }

    #[tokio::test]
    async fn invalid_uploads_leave_no_files_or_busy_claims() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let path = format!("/api/projects/{}/beats/b01/upload", project.id);
        for (name, mime, data) in [
            ("empty.png", "image/png", ""),
            ("unsafe.svg", "image/svg+xml", "<svg/>"),
            ("broken.png", "image/png", "not an image"),
        ] {
            let body = format!(
                "--fixture-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n{data}\r\n--fixture-boundary--\r\n"
            );
            let req = Request::builder()
                .method("POST")
                .uri(format!("http://{TEST_HOST}{path}"))
                .header(header::HOST, TEST_HOST)
                .header(
                    header::CONTENT_TYPE,
                    "multipart/form-data; boundary=fixture-boundary",
                )
                .body(Body::from(body))
                .unwrap();
            let response = router(state.clone()).oneshot(req).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
            assert!(!state.project_busy(&project.id));
            assert_eq!(
                std::fs::read_dir(state.store.dir(&project.id).join("assets"))
                    .map(|entries| entries.count())
                    .unwrap_or(0),
                0
            );
            assert!(state.store.get(&project.id).unwrap().render.is_some());
        }
    }

    #[tokio::test]
    async fn invalid_project_inputs_and_still_times_are_rejected_before_work() {
        let home = TestHome::new();
        let state = home.state();
        for body in [
            json!({"topic": "  "}),
            json!({"topic": "x".repeat(1001)}),
            json!({"topic": "valid", "target_seconds": 9}),
            json!({"topic": "valid", "target_seconds": 601}),
        ] {
            assert_eq!(
                call(&state, "POST", "/api/projects", body).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert!(state.store.list().is_empty());
        let project = state.store.create(finished_project()).await.unwrap();
        for time in ["NaN", "inf", "-1"] {
            let response = call(
                &state,
                "GET",
                &format!("/api/projects/{}/still?t={time}", project.id),
                json!(null),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(!state.project_busy(&project.id));
        }
    }

    async fn error_of(response: Response) -> (StatusCode, String) {
        let status = response.status();
        let body = response_json(response).await;
        (status, body["error"].as_str().unwrap_or_default().to_string())
    }

    #[tokio::test]
    async fn queue_routes_validate_then_add_pause_retry_remove_and_clear() {
        let home = TestHome::new();
        let state = home.state();
        let far = (Utc::now() + chrono::Duration::days(800)).to_rfc3339();
        for body in [
            json!({}),
            json!({"topics": [" ", ""]}),
            json!({"items": [{"topic": "  "}]}),
            json!({"topics": ["x".repeat(1001)]}),
            json!({"topics": ["ok"], "target_seconds": 5}),
            json!({"topics": ["ok"], "voice": "v".repeat(129)}),
            json!({"topics": ["ok"], "publish": {"privacy": "secret"}}),
            json!({"topics": ["ok"], "publish": {"publish_at": far}}),
            json!({"topics": ["ok"], "publish_every_minutes": 43_201}),
            json!({"topics": vec!["ok"; queue::MAX_ADD + 1]}),
        ] {
            let response = call(&state, "POST", "/api/queue", body.clone()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        let response = call(
            &state,
            "POST",
            "/api/queue",
            json!({"topics": ["ok"], "publish": {"privacy": "unlisted"}}),
        )
        .await;
        let (status, error) = error_of(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.contains("isn't connected"), "{error}");
        assert!(state.queue.snapshot().items.is_empty());

        let response = call(
            &state,
            "POST",
            "/api/queue",
            json!({
                "topics": [" First ", "", "Second"],
                "format": "square",
                "caption_preset": "hormozi",
                "extra_formats": ["vertical"]
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let added = response_json(response).await;
        assert_eq!(added.as_array().unwrap().len(), 2, "blank topics are skipped");
        assert_eq!(added[0]["topic"], "First");
        assert_eq!(added[0]["format"], "square");
        assert_eq!(added[0]["caption_preset"], "hormozi");
        assert_eq!(added[0]["status"], "waiting");
        let first = added[0]["id"].as_str().unwrap().to_string();

        let response = call(&state, "PATCH", "/api/queue", json!({"paused": true})).await;
        assert_eq!(response_json(response).await["paused"], true);
        assert!(state.queue.is_idle(), "a paused queue is idle");

        for (method, path) in [("DELETE", "/api/queue/nope"), ("POST", "/api/queue/nope/retry")] {
            let response = call(&state, method, path, json!({})).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        }
        let response = call(&state, "POST", &format!("/api/queue/{first}/retry"), json!({})).await;
        assert_eq!(response.status(), StatusCode::OK, "retrying a waiting item is a no-op");
        let response = call(&state, "DELETE", &format!("/api/queue/{first}"), json!({})).await;
        let left = response_json(response).await;
        assert_eq!(left["items"].as_array().unwrap().len(), 1);
        assert_eq!(left["items"][0]["topic"], "Second");
        let response = call(&state, "POST", "/api/queue/clear", json!({})).await;
        assert_eq!(response_json(response).await["removed"], 0, "waiting items stay");
        assert_eq!(state.queue.snapshot().items.len(), 1);
    }

    #[tokio::test]
    async fn queued_publishing_defaults_to_the_configured_privacy() {
        let home = TestHome::new();
        let state = home.state();
        state.publish.sign_in_for_test();
        let mut config = state.config();
        config.publish.privacy = "unlisted".into();
        state.set_config(config).unwrap();
        state.queue.set_paused(true).unwrap();

        let response = call(
            &state,
            "POST",
            "/api/queue",
            json!({
                "topics": ["Default"],
                "publish": {},
                "items": [{"topic": "Chosen", "publish": {"privacy": "public"}}],
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let added = response_json(response).await;
        assert_eq!(added[0]["topic"], "Chosen");
        assert_eq!(added[0]["publish"]["privacy"], "public");
        assert_eq!(added[1]["topic"], "Default");
        assert_eq!(added[1]["publish"]["privacy"], "unlisted");
    }

    #[tokio::test]
    async fn publishing_needs_a_render_a_connection_and_a_free_upload_slot() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let path = format!("/api/projects/{}/publish", project.id);
        let far = (Utc::now() + chrono::Duration::days(800)).to_rfc3339();
        for body in [json!({"privacy": "secret"}), json!({"publish_at": far})] {
            let response = call(&state, "POST", &path, body.clone()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        let response = call(&state, "POST", "/api/projects/nope/publish", json!({})).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = call(&state, "POST", &path, json!({"format": "square"})).await;
        let (status, error) = error_of(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.contains("no square render"), "{error}");
        let response = call(&state, "POST", &path, json!({"privacy": "unlisted"})).await;
        let (status, error) = error_of(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.contains("isn't connected"), "{error}");

        let mut unrendered = finished_project();
        unrendered.render = None;
        let unrendered = state.store.create(unrendered).await.unwrap();
        let response = call(
            &state,
            "POST",
            &format!("/api/projects/{}/publish", unrendered.id),
            json!({}),
        )
        .await;
        let (status, error) = error_of(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.contains("render the project"), "{error}");

        // Connected, but the project is busy: nothing reaches Google.
        state.publish.sign_in_for_test();
        state.publish.set_uploading_for_test(&project.id, true);
        let project_path = format!("/api/projects/{}", project.id);
        for (method, path, body) in [
            ("POST", path.as_str(), json!({})),
            ("PATCH", project_path.as_str(), json!({"title": "Busy"})),
            ("DELETE", project_path.as_str(), json!({})),
        ] {
            let response = call(&state, method, path, body).await;
            assert_eq!(response.status(), StatusCode::CONFLICT, "{method} {path}");
        }
        state.publish.set_uploading_for_test(&project.id, false);
        state
            .store
            .spawn_pipeline(&project.id, futures::future::pending())
            .unwrap();
        let response = call(&state, "POST", &path, json!({})).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        state.store.abort_and_wait(&project.id).await;
    }

    #[tokio::test]
    async fn youtube_client_settings_validate_and_never_echo_the_secret() {
        let home = TestHome::new();
        let state = home.state();
        let response = call(&state, "GET", "/api/publish/youtube", json!(null)).await;
        let status = response_json(response).await;
        assert_eq!(status["configured"], false);
        assert_eq!(status["connected"], false);
        let response = call(&state, "POST", "/api/publish/youtube/connect", json!({})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "needs a client first");

        for body in [
            json!({}),
            json!({"client_id": "only-an-id"}),
            json!({"client_json": "{not json"}),
            json!({"client_json": r#"{"web":{"client_id":"a","client_secret":"b"}}"#}),
        ] {
            let response = call(&state, "PUT", "/api/publish/youtube", body.clone()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        assert!(!state.publish.status().configured);
        let client = r#"{"installed":{"client_id":"cid.apps.googleusercontent.com","client_secret":"fixture-client-secret"}}"#;
        let response = call(
            &state,
            "PUT",
            "/api/publish/youtube",
            json!({"client_json": client}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let status = response_json(response).await;
        assert!(!status.to_string().contains("fixture-client-secret"));
        assert_eq!(status["configured"], true);
        assert_eq!(status["connected"], false);
        assert_eq!(status["client_id"], "cid.apps.googleusercontent.com");

        for url in ["x".repeat(8193), "not a url".into()] {
            let response = call(
                &state,
                "POST",
                "/api/publish/youtube/callback-url",
                json!({"url": url}),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let response = call(&state, "DELETE", "/api/publish/youtube", json!(null)).await;
        assert_eq!(response_json(response).await["configured"], true, "keeps the client");
        let response = call(&state, "DELETE", "/api/publish/youtube?all=true", json!(null)).await;
        assert_eq!(response_json(response).await["configured"], false);
    }

    #[tokio::test]
    async fn ideas_and_clip_requests_are_checked_before_any_network_call() {
        let home = TestHome::new();
        let state = home.state();
        let long_q = "q".repeat(201);
        for query in ["geo=USA", "geo=1x", "source=nope", &format!("source=news&q={long_q}")] {
            let response = call(&state, "GET", &format!("/api/ideas?{query}"), json!(null)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        }
        let url = "https://www.youtube.com/watch?v=abc";
        for body in [
            json!({"url": " "}),
            json!({"url": "ftp://example.com/video"}),
            json!({"url": format!("https://{}", "x".repeat(2000))}),
            json!({"url": url, "count": 0}),
            json!({"url": url, "count": clips::MAX_CLIPS + 1}),
            json!({"url": url, "min_s": 4}),
            json!({"url": url, "max_s": 181}),
            json!({"url": url, "min_s": 50, "max_s": 40}),
            json!({"url": url, "start": false, "publish": {"privacy": "public"}}),
            json!({"url": url, "publish": {"privacy": "secret"}}),
            json!({"url": url, "publish_every_minutes": 43_201}),
            json!({"url": url, "job": "not a job id"}),
            json!({"url": url, "job": ""}),
        ] {
            let response = call(&state, "POST", "/api/clips", body.clone()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        let response = call(
            &state,
            "POST",
            "/api/clips",
            json!({"url": url, "publish": {"privacy": "public"}}),
        )
        .await;
        let (status, error) = error_of(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.contains("isn't connected"), "{error}");
        assert!(state.store.list().is_empty());
    }

    #[tokio::test]
    async fn clips_keep_their_source_footage_and_narration() {
        let home = TestHome::new();
        let state = home.state();
        let clip = state.store.create(clip_project()).await.unwrap();
        let path = format!("/api/projects/{}", clip.id);
        for (method, tail, body) in [
            ("PATCH", "", json!({"script_text": "New words."})),
            ("PATCH", "", json!({"topic": "Another topic"})),
            ("PATCH", "", json!({"target_seconds": 15})),
            ("PATCH", "/beats/b01", json!({"text": "New words."})),
            ("POST", "/beats/b01/search", json!({})),
            ("POST", "/beats/b01/auto", json!({})),
        ] {
            let response = call(&state, method, &format!("{path}{tail}"), body).await;
            let (status, error) = error_of(response).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {tail}");
            assert!(error.contains("clip"), "{error}");
        }
        let response = call(
            &state,
            "PATCH",
            &path,
            json!({"title": "A better title", "caption_preset": "karaoke"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "metadata and styling stay editable");
        let saved = state.store.get(&clip.id).unwrap();
        assert_eq!(saved.caption_preset, Some(CaptionPreset::Karaoke));
        assert_eq!(saved.script.as_ref().unwrap().title, "A better title");

        let response = call(&state, "POST", &format!("{path}/duplicate"), json!({})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let copy = state
            .store
            .get(response_json(response).await["id"].as_str().unwrap())
            .unwrap();
        assert_eq!(copy.kind, ProjectKind::Clip);
        assert_eq!(copy.clip.as_ref().unwrap().start_s, 100.0);
        assert_eq!(copy.caption_preset, Some(CaptionPreset::Karaoke));
        assert_eq!(copy.beats.len(), 1);
        assert!(copy.beats[0].asset.is_none());
        assert_eq!(pipeline::first_unfinished(&copy), Some(Stage::Assets));

        let response = call(&state, "PATCH", &path, json!({"title": "Kept style"})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved = state.store.get(&clip.id).unwrap();
        assert_eq!(saved.caption_preset, Some(CaptionPreset::Karaoke), "left out keeps it");
        let response = call(&state, "PATCH", &path, json!({"caption_preset": null})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved = state.store.get(&clip.id).unwrap();
        assert_eq!(saved.caption_preset, None, "null goes back to the default");
    }

    #[tokio::test]
    async fn extra_formats_are_cleaned_and_each_render_file_downloads() {
        assert_eq!(
            clean_extra_formats(
                Format::Vertical,
                vec![
                    Format::Vertical,
                    Format::Square,
                    Format::Square,
                    Format::Horizontal
                ],
            ),
            [Format::Square, Format::Horizontal]
        );
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let path = format!("/api/projects/{}", project.id);
        let response = call(
            &state,
            "PATCH",
            &path,
            json!({"extra_formats": ["vertical", "square", "square"]}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved = state.store.get(&project.id).unwrap();
        assert_eq!(saved.extra_formats, [Format::Square]);
        assert!(saved.render.is_none(), "a new format needs a new render");
        let response = call(&state, "PATCH", &path, json!({"format": "square"})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let saved = state.store.get(&project.id).unwrap();
        assert!(saved.extra_formats.is_empty(), "the main format isn't an extra");

        let mut project = finished_project();
        project.extra_formats = vec![Format::Square];
        project.render = serde_json::from_value(json!({
            "path": "render/final.mp4", "credits_path": "render/credits.txt",
            "duration_s": 1.5, "width": 1080, "height": 1920, "rendered_at": Utc::now(),
            "variants": [{"format": "square", "path": "render/final-square.mp4",
                          "width": 1080, "height": 1080}],
            "subtitles": {"srt": "render/captions.srt", "vtt": "render/captions.vtt"},
            "thumbnail_jpg": "render/thumbnail.jpg"
        }))
        .unwrap();
        let project = state.store.create(project).await.unwrap();
        let render = state.store.dir(&project.id).join("render");
        std::fs::create_dir_all(&render).unwrap();
        for file in [
            "final.mp4",
            "final-square.mp4",
            "captions.srt",
            "captions.vtt",
            "thumbnail.jpg",
            "credits.txt",
        ] {
            std::fs::write(render.join(file), file).unwrap();
        }
        let download = format!("/api/projects/{}/download", project.id);
        for (file, name, body) in [
            ("", "a-fixture-topic.mp4", "final.mp4"),
            ("?file=vertical", "a-fixture-topic.mp4", "final.mp4"),
            ("?file=square", "a-fixture-topic-square.mp4", "final-square.mp4"),
            ("?file=srt", "a-fixture-topic.srt", "captions.srt"),
            ("?file=vtt", "a-fixture-topic.vtt", "captions.vtt"),
            ("?file=thumb", "a-fixture-topic-thumbnail.jpg", "thumbnail.jpg"),
            ("?file=credits", "a-fixture-topic-credits.txt", "credits.txt"),
        ] {
            let response = call(&state, "GET", &format!("{download}{file}"), json!(null)).await;
            assert_eq!(response.status(), StatusCode::OK, "{file}");
            let disposition = response.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .to_string();
            assert!(disposition.contains(name), "{file}: {disposition}");
            let bytes = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(&bytes[..], body.as_bytes());
        }
        let response = call(&state, "GET", &format!("{download}?file=horizontal"), json!(null)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = call(&state, "GET", &format!("{download}?file=passwd"), json!(null)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
