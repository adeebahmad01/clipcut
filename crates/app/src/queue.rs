//! Batch mode: a persistent queue of topics (and clip projects) that become
//! videos one at a time, each optionally published to YouTube when it renders.
//! The queue lives in `queue.json`; one worker drains it in the background.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use clipcut_core::config::Format;
use clipcut_core::model::write_atomic;
use clipcut_core::timeline::CaptionPreset;
use clipcut_core::{Project, Status};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast};
use tokio::task::AbortHandle;

use crate::pipeline;
use crate::state::{AppState, ServerEvent};

/// Most items accepted in one request.
pub const MAX_ADD: usize = 200;
const PRIVACY: [&str; 3] = ["public", "unlisted", "private"];
/// Furthest a release can be scheduled.
const MAX_SCHEDULE_DAYS: i64 = 730;
const MAX_SPACING_MINUTES: u32 = 30 * 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Waiting,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Publish a queued video once it renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishPlan {
    /// public, unlisted or private. Left out of a request, it is filled in
    /// from `[publish].privacy` with [`PublishPlan::default_privacy`].
    #[serde(default)]
    pub privacy: String,
    /// Release time. While in the future the upload stays private until then.
    #[serde(default)]
    pub publish_at: Option<DateTime<Utc>>,
}

impl PublishPlan {
    /// Use `privacy` (the configured default) when none was given.
    pub fn default_privacy(&mut self, privacy: &str) {
        if self.privacy.trim().is_empty() {
            self.privacy = privacy.to_string();
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemOptions {
    #[serde(default)]
    pub format: Option<Format>,
    #[serde(default)]
    pub target_seconds: Option<u32>,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub caption_preset: Option<CaptionPreset>,
    #[serde(default)]
    pub extra_formats: Vec<Format>,
    #[serde(default)]
    pub publish: Option<PublishPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub topic: String,
    #[serde(default)]
    pub notes: String,
    #[serde(flatten)]
    pub options: ItemOptions,
    /// Set once the item has a project (clips are queued with theirs).
    #[serde(default)]
    pub project_id: Option<String>,
    pub status: ItemStatus,
    #[serde(default)]
    pub error: Option<String>,
    pub added_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    /// Where the video was published, when it was.
    #[serde(default)]
    pub video_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueState {
    pub items: Vec<QueueItem>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_minutes: Option<u32>,
}

/// `POST /api/queue`: explicit items, or plain topics sharing one set of options.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddBody {
    #[serde(default)]
    pub items: Vec<NewItem>,
    #[serde(default)]
    pub topics: Vec<String>,
    /// Options for `topics`.
    #[serde(flatten)]
    pub options: ItemOptions,
    /// Space scheduled releases: item N publishes N × this many minutes
    /// after the first (which publishes at `publish.publish_at`, or on render).
    #[serde(default)]
    pub publish_every_minutes: Option<u32>,
}

impl AddBody {
    /// Fill every publish plan that has no privacy with `privacy`.
    pub fn default_privacy(&mut self, privacy: &str) {
        let plans = self
            .items
            .iter_mut()
            .map(|i| &mut i.options)
            .chain([&mut self.options])
            .filter_map(|o| o.publish.as_mut());
        for plan in plans {
            plan.default_privacy(privacy);
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewItem {
    pub topic: String,
    #[serde(default)]
    pub notes: String,
    #[serde(flatten)]
    pub options: ItemOptions,
}

pub struct Queue {
    file: PathBuf,
    state: Mutex<QueueState>,
    events: broadcast::Sender<ServerEvent>,
    wake: Notify,
    /// The item the worker is on and the task running it.
    current: Mutex<Option<(String, AbortHandle)>>,
}

impl Queue {
    /// Items that were running when the app stopped go back to waiting.
    pub fn load(file: PathBuf, events: broadcast::Sender<ServerEvent>) -> Self {
        let mut state: QueueState = std::fs::read(&file)
            .ok()
            .and_then(|bytes| match serde_json::from_slice(&bytes) {
                Ok(state) => Some(state),
                Err(e) => {
                    tracing::warn!("ignoring unreadable {}: {e}", file.display());
                    None
                }
            })
            .unwrap_or_default();
        for item in &mut state.items {
            if item.status == ItemStatus::Running {
                item.status = ItemStatus::Waiting;
                item.started_at = None;
            }
        }
        Self {
            file,
            state: Mutex::new(state),
            events,
            wake: Notify::new(),
            current: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> QueueState {
        self.state.lock().unwrap().clone()
    }

    /// Apply a change, persist it and tell the UI.
    fn change<T>(&self, f: impl FnOnce(&mut QueueState) -> T) -> anyhow::Result<T> {
        let (out, snapshot) = {
            let mut state = self.state.lock().unwrap();
            let mut next = state.clone();
            let out = f(&mut next);
            let bytes = serde_json::to_vec_pretty(&next)?;
            write_atomic(&self.file, &bytes)
                .with_context(|| format!("could not save {}", self.file.display()))?;
            *state = next;
            (out, state.clone())
        };
        let _ = self.events.send(ServerEvent::Queue(snapshot));
        self.wake.notify_one();
        Ok(out)
    }

    pub fn add(&self, body: AddBody) -> anyhow::Result<Vec<QueueItem>> {
        let now = Utc::now();
        let mut new: Vec<QueueItem> = body
            .items
            .into_iter()
            .chain(body.topics.into_iter().map(|topic| NewItem {
                topic,
                notes: String::new(),
                options: body.options.clone(),
            }))
            .filter(|item| !item.topic.trim().is_empty())
            .map(|item| QueueItem {
                id: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
                topic: item.topic.trim().to_string(),
                notes: item.notes.trim().to_string(),
                options: item.options,
                project_id: None,
                status: ItemStatus::Waiting,
                error: None,
                added_at: now,
                started_at: None,
                finished_at: None,
                video_url: None,
            })
            .collect();
        space_releases(&mut new, body.publish_every_minutes);
        self.change(|s| s.items.extend(new.iter().cloned()))?;
        Ok(new)
    }

    /// Queue existing projects (clips) for rendering.
    pub fn add_projects(
        &self,
        projects: &[Project],
        publish: Option<PublishPlan>,
        every_minutes: Option<u32>,
    ) -> anyhow::Result<Vec<QueueItem>> {
        let now = Utc::now();
        let mut new: Vec<QueueItem> = projects
            .iter()
            .map(|p| QueueItem {
                id: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(),
                topic: p.topic.clone(),
                notes: String::new(),
                options: ItemOptions {
                    format: Some(p.format),
                    target_seconds: Some(p.target_seconds),
                    voice: None,
                    caption_preset: p.caption_preset,
                    extra_formats: p.extra_formats.clone(),
                    publish: publish.clone(),
                },
                project_id: Some(p.id.clone()),
                status: ItemStatus::Waiting,
                error: None,
                added_at: now,
                started_at: None,
                finished_at: None,
                video_url: None,
            })
            .collect();
        space_releases(&mut new, every_minutes);
        self.change(|s| s.items.extend(new.iter().cloned()))?;
        Ok(new)
    }

    pub fn set_paused(&self, paused: bool) -> anyhow::Result<()> {
        self.change(|s| {
            s.paused = paused;
            if !paused {
                s.pause_reason = None;
                s.resume_at = None;
                s.backoff_minutes = None;
            }
        })
    }

    pub fn pause_for_limit(
        &self,
        resume_at: DateTime<Utc>,
        reason: impl Into<String>,
        backoff_minutes: u32,
    ) -> anyhow::Result<()> {
        self.change(|s| {
            s.paused = true;
            s.pause_reason = Some(reason.into());
            s.resume_at = Some(resume_at);
            s.backoff_minutes = Some(backoff_minutes);
        })
    }

    pub fn requeue_for_limit(&self, id: &str) -> anyhow::Result<bool> {
        self.change(|s| {
            let Some(item) = s.items.iter_mut().find(|i| i.id == id) else {
                return false;
            };
            if item.status != ItemStatus::Cancelled {
                item.status = ItemStatus::Waiting;
                item.error = None;
                item.started_at = None;
                item.finished_at = None;
            }
            true
        })
    }

    pub fn auto_resume(&self) -> anyhow::Result<bool> {
        self.change(|s| {
            if s.paused && s.resume_at.is_some() {
                s.paused = false;
                s.pause_reason = None;
                s.resume_at = None;
                true
            } else {
                false
            }
        })
    }

    /// Put a failed or cancelled item back in line. It keeps its project, so
    /// the pipeline resumes from the stage that stopped.
    pub fn retry(&self, id: &str) -> anyhow::Result<bool> {
        self.change(|s| {
            let Some(item) = s.items.iter_mut().find(|i| i.id == id) else {
                return false;
            };
            if matches!(item.status, ItemStatus::Failed | ItemStatus::Cancelled) {
                item.status = ItemStatus::Waiting;
                item.error = None;
                item.started_at = None;
                item.finished_at = None;
            }
            true
        })
    }

    /// Drop finished, failed and cancelled items.
    pub fn clear_finished(&self) -> anyhow::Result<usize> {
        self.change(|s| {
            let before = s.items.len();
            s.items
                .retain(|i| matches!(i.status, ItemStatus::Waiting | ItemStatus::Running));
            before - s.items.len()
        })
    }

    /// Next waiting item, marked running. None while paused.
    fn take_next(&self) -> anyhow::Result<Option<QueueItem>> {
        {
            let s = self.state.lock().unwrap();
            if s.paused {
                if let Some(at) = s.resume_at {
                    if Utc::now() < at {
                        return Ok(None);
                    }
                } else {
                    return Ok(None);
                }
            }
            if !s.items.iter().any(|i| i.status == ItemStatus::Waiting) {
                return Ok(None);
            }
        }
        self.change(|s| {
            if s.paused {
                if s.resume_at.is_some_and(|at| Utc::now() >= at) {
                    s.paused = false;
                    s.pause_reason = None;
                    s.resume_at = None;
                } else {
                    return None;
                }
            }
            let item = s
                .items
                .iter_mut()
                .find(|i| i.status == ItemStatus::Waiting)?;
            item.status = ItemStatus::Running;
            item.started_at = Some(Utc::now());
            item.finished_at = None;
            item.error = None;
            Some(item.clone())
        })
    }

    fn update_item(&self, id: &str, f: impl FnOnce(&mut QueueItem)) -> anyhow::Result<()> {
        self.change(|s| {
            if let Some(item) = s.items.iter_mut().find(|i| i.id == id) {
                f(item)
            }
        })
    }

    /// Record how a run ended. A cancellation made meanwhile wins.
    fn finish(&self, id: &str, result: anyhow::Result<Option<String>>) -> anyhow::Result<()> {
        self.change(|s| {
            if result.is_ok() {
                s.backoff_minutes = None;
            }
            if let Some(item) = s.items.iter_mut().find(|i| i.id == id) {
                if item.status == ItemStatus::Cancelled {
                    return;
                }
                item.finished_at = Some(Utc::now());
                match result {
                    Ok(url) => {
                        item.status = ItemStatus::Done;
                        item.video_url = url;
                    }
                    Err(e) => {
                        item.status = ItemStatus::Failed;
                        item.error = Some(format!("{e:#}"));
                    }
                }
            }
        })
    }

    pub fn is_idle(&self) -> bool {
        let s = self.state.lock().unwrap();
        if s.paused && s.resume_at.is_none() {
            return true;
        }
        !s.items
            .iter()
            .any(|i| matches!(i.status, ItemStatus::Waiting | ItemStatus::Running))
    }
}

/// With an interval, item N releases N intervals after the first. Without a
/// first release time the first item publishes as soon as it renders.
fn space_releases(items: &mut [QueueItem], every_minutes: Option<u32>) {
    let Some(every) = every_minutes.filter(|m| *m > 0) else {
        return;
    };
    let first = items
        .iter()
        .find_map(|i| i.options.publish.as_ref().and_then(|p| p.publish_at));
    let base = first.unwrap_or_else(Utc::now);
    for (n, item) in items.iter_mut().enumerate() {
        if let Some(plan) = &mut item.options.publish {
            plan.publish_at = (first.is_some() || n > 0)
                .then(|| base + chrono::Duration::minutes(i64::from(every) * n as i64));
        }
    }
}

/// Reject what would fail later, before anything is queued.
pub fn validate(body: &AddBody) -> Result<(), String> {
    let count = body.items.len() + body.topics.len();
    let blank = |t: &str| t.trim().is_empty();
    if body.items.iter().all(|i| blank(&i.topic)) && body.topics.iter().all(|t| blank(t)) {
        return Err("add at least one topic".into());
    }
    if count > MAX_ADD {
        return Err(format!("at most {MAX_ADD} topics at once"));
    }
    validate_spacing(body.publish_every_minutes)?;
    let topics = body
        .items
        .iter()
        .map(|i| (i.topic.as_str(), i.notes.as_str(), &i.options))
        .chain(body.topics.iter().map(|t| (t.as_str(), "", &body.options)));
    for (topic, notes, options) in topics {
        if topic.len() > 1000 {
            return Err("topic is too long (maximum 1000 bytes)".into());
        }
        if notes.len() > 20_000 {
            return Err("notes are too long (maximum 20000 bytes)".into());
        }
        validate_options(options)?;
    }
    Ok(())
}

pub fn validate_options(options: &ItemOptions) -> Result<(), String> {
    if options
        .target_seconds
        .is_some_and(|s| !(10..=600).contains(&s))
    {
        return Err("target_seconds must be between 10 and 600".into());
    }
    if options.voice.as_ref().is_some_and(|v| v.len() > 128) {
        return Err("voice is too long (maximum 128 bytes)".into());
    }
    if let Some(plan) = &options.publish {
        validate_plan(plan)?;
    }
    Ok(())
}

pub fn validate_plan(plan: &PublishPlan) -> Result<(), String> {
    validate_privacy(&plan.privacy)?;
    if plan
        .publish_at
        .is_some_and(|at| at > Utc::now() + chrono::Duration::days(MAX_SCHEDULE_DAYS))
    {
        return Err(format!("publish_at must be within {MAX_SCHEDULE_DAYS} days"));
    }
    Ok(())
}

pub fn validate_spacing(every_minutes: Option<u32>) -> Result<(), String> {
    if every_minutes.is_some_and(|m| m > MAX_SPACING_MINUTES) {
        return Err("publish_every_minutes must be at most 43200 (30 days)".into());
    }
    Ok(())
}

pub fn validate_privacy(privacy: &str) -> Result<(), String> {
    if PRIVACY.contains(&privacy) {
        Ok(())
    } else {
        Err("privacy must be public, unlisted or private".into())
    }
}

/// Cancel a running item or remove a waiting/finished one.
pub async fn remove(state: &AppState, id: &str) -> anyhow::Result<bool> {
    let Some(item) = state
        .queue
        .snapshot()
        .items
        .into_iter()
        .find(|i| i.id == id)
    else {
        return Ok(false);
    };
    if item.status != ItemStatus::Running {
        state.queue.change(|s| s.items.retain(|i| i.id != id))?;
        return Ok(true);
    }
    state.queue.update_item(id, |i| {
        i.status = ItemStatus::Cancelled;
        i.error = Some("cancelled".into());
        i.finished_at = Some(Utc::now());
    })?;
    let current = state.queue.current.lock().unwrap().take();
    match current {
        Some((current, abort)) if current == id => abort.abort(),
        other => *state.queue.current.lock().unwrap() = other,
    }
    let project_id = state
        .queue
        .snapshot()
        .items
        .into_iter()
        .find(|i| i.id == id)
        .and_then(|i| i.project_id);
    if let Some(pid) = project_id
        && state.store.is_running(&pid)
    {
        pipeline::cancel(state, &pid).await?;
    }
    Ok(true)
}

#[derive(Debug, thiserror::Error)]
pub enum QueueRunError {
    #[error("Claude usage limit: {message}")]
    Limit {
        stage: clipcut_core::Stage,
        message: String,
        resume_at: Option<DateTime<Utc>>,
    },
}

/// Drain the queue one item at a time. With `until_idle` (the CLI) it returns
/// once nothing is waiting; the server's worker runs forever.
pub async fn worker(state: AppState, until_idle: bool) {
    worker_with(state, until_idle, |s, item| {
        let s = s.clone();
        let item = item.clone();
        Box::pin(async move { run_item(&s, &item).await })
    })
    .await;
}

pub async fn worker_with<F>(state: AppState, until_idle: bool, runner: F)
where
    F: Fn(
            &AppState,
            &QueueItem,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<Option<String>>> + Send>,
        > + Send
        + Sync
        + 'static,
{
    loop {
        let item = match state.queue.take_next() {
            Ok(item) => item,
            Err(e) => {
                tracing::error!("queue: {e:#}");
                None
            }
        };
        let Some(item) = item else {
            if until_idle && state.queue.is_idle() {
                return;
            }
            let (has_resume, sleep_duration) = {
                let s = state.queue.snapshot();
                match s.resume_at {
                    Some(at) => {
                        let now = Utc::now();
                        let dur = if at > now {
                            (at - now).to_std().unwrap_or(Duration::from_millis(100))
                        } else {
                            Duration::ZERO
                        };
                        (true, dur)
                    }
                    None => (false, Duration::from_secs(5)),
                }
            };
            tokio::select! {
                _ = state.queue.wake.notified() => {}
                _ = tokio::time::sleep(sleep_duration) => {
                    if has_resume {
                        let _ = state.queue.auto_resume();
                    }
                }
            }
            continue;
        };
        tracing::info!(item = %item.id, "queue: making \"{}\"", item.topic);
        let task_state = state.clone();
        let task_item = item.clone();
        let task = tokio::spawn(runner(&task_state, &task_item));
        *state.queue.current.lock().unwrap() = Some((item.id.clone(), task.abort_handle()));
        let result = match task.await {
            Ok(result) => result,
            Err(e) if e.is_cancelled() => Err(anyhow::anyhow!("cancelled")),
            Err(_) => Err(anyhow::anyhow!("the queue worker failed unexpectedly")),
        };
        {
            let mut current = state.queue.current.lock().unwrap();
            if current.as_ref().is_some_and(|(id, _)| id == &item.id) {
                *current = None;
            }
        }

        let limit_info = result.as_ref().err().and_then(|err| {
            if let Some(QueueRunError::Limit {
                message, resume_at, ..
            }) = err.downcast_ref::<QueueRunError>()
            {
                return Some((message.clone(), *resume_at));
            }
            let text = format!("{err:#}");
            if clipcut_llm::classify(&text) == clipcut_llm::LlmErrorKind::RateLimit {
                let at = clipcut_llm::parse_reset_time(&text);
                return Some((text, at));
            }
            None
        });

        if let Some((err_msg, parsed_at)) = limit_info {
            tracing::warn!(item = %item.id, "queue: hit limit ({err_msg}), pausing queue");
            let _ = state.queue.requeue_for_limit(&item.id);
            let (resume_at, wait_mins) = match parsed_at {
                Some(at) => (at, 15),
                None => {
                    let prev = state.queue.snapshot().backoff_minutes;
                    let mins = match prev {
                        None => 15,
                        Some(m) => (m * 2).min(120),
                    };
                    (Utc::now() + chrono::Duration::minutes(mins as i64), mins)
                }
            };
            if let Err(e) = state.queue.pause_for_limit(resume_at, "Claude usage limit", wait_mins) {
                tracing::error!("queue pause_for_limit: {e:#}");
            }
            continue;
        }

        if let Err(e) = &result {
            tracing::warn!(item = %item.id, "queue: {e:#}");
        }
        if let Err(e) = state.queue.finish(&item.id, result) {
            tracing::error!("queue: {e:#}");
        }
    }
}

/// Make one video: create (or resume) its project, run the pipeline on
/// autopilot, then publish it. Returns the published URL.
async fn run_item(state: &AppState, item: &QueueItem) -> anyhow::Result<Option<String>> {
    let id = match &item.project_id {
        Some(id) => {
            anyhow::ensure!(state.store.exists(id), "its project was deleted");
            id.clone()
        }
        None => {
            let project = state.store.create(new_project(state, item)).await?;
            let pid = project.id.clone();
            state
                .queue
                .update_item(&item.id, |i| i.project_id = Some(pid))?;
            project.id
        }
    };
    state
        .store
        .update(&id, |p| {
            p.autopilot = true;
            Ok(())
        })
        .await?;
    let project = state.store.get(&id)?;
    let rendered = project.render.is_some() && pipeline::first_unfinished(&project).is_none();
    if !rendered && !state.store.is_running(&id) {
        pipeline::start(state, &id, None, false).await?;
    }
    while state.store.is_running(&id) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let project = state.store.get(&id)?;
    if let Some(s) = project.stages.iter().find(|s| s.status == Status::Failed) {
        let err_msg = s.error.clone().unwrap_or_else(|| "unknown error".into());
        if s.error_kind == Some(clipcut_core::model::StageErrorKind::RateLimit)
            || clipcut_llm::classify(&err_msg) == clipcut_llm::LlmErrorKind::RateLimit
        {
            let resume_at = clipcut_llm::parse_reset_time(&err_msg);
            return Err(QueueRunError::Limit {
                stage: s.stage,
                message: err_msg,
                resume_at,
            }
            .into());
        }
        anyhow::bail!(
            "{} failed: {err_msg}",
            s.stage.label()
        );
    }
    anyhow::ensure!(project.render.is_some(), "the pipeline stopped before rendering");
    let Some(plan) = &item.options.publish else {
        return Ok(None);
    };
    let progress = state.progress(&id);
    let record = crate::publish::upload(
        state,
        &id,
        None,
        &plan.privacy,
        plan.publish_at,
        &progress,
    )
    .await
    .context("rendered, but publishing failed")?;
    Ok(Some(record.url))
}

fn new_project(state: &AppState, item: &QueueItem) -> Project {
    let config = state.config();
    let o = &item.options;
    let mut project = Project::new(
        item.topic.as_str(),
        item.notes.as_str(),
        o.format.unwrap_or(config.video.format),
        o.target_seconds.unwrap_or(config.video.target_seconds),
    );
    project.voice = o.voice.clone().filter(|v| !v.trim().is_empty());
    project.autopilot = true;
    project.caption_preset = o.caption_preset;
    project.extra_formats =
        crate::server::clean_extra_formats(project.format, o.extra_formats.clone());
    project
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestHome;

    fn body(topics: &[&str]) -> AddBody {
        AddBody {
            topics: topics.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn queue_persists_and_requeues_interrupted_items() {
        let home = TestHome::new();
        let state = home.state();
        let added = state.queue.add(body(&["first", "  ", "second"])).unwrap();
        assert_eq!(added.len(), 2, "blank topics are skipped");
        let taken = state.queue.take_next().unwrap().unwrap();
        assert_eq!(taken.topic, "first");
        assert_eq!(state.queue.snapshot().items[0].status, ItemStatus::Running);

        let reloaded = Queue::load(state.paths.queue_file(), state.events.clone());
        let items = reloaded.snapshot().items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].status, ItemStatus::Waiting, "running → waiting on restart");
        assert_eq!(items[1].topic, "second");
    }

    #[test]
    fn paused_queue_hands_out_nothing() {
        let home = TestHome::new();
        let state = home.state();
        state.queue.add(body(&["one"])).unwrap();
        state.queue.set_paused(true).unwrap();
        assert!(state.queue.take_next().unwrap().is_none());
        assert!(state.queue.is_idle());
        state.queue.set_paused(false).unwrap();
        assert!(state.queue.take_next().unwrap().is_some());
    }

    #[test]
    fn finish_keeps_cancellation_and_retry_requeues() {
        let home = TestHome::new();
        let state = home.state();
        let item = state.queue.add(body(&["one"])).unwrap().remove(0);
        state.queue.take_next().unwrap();
        state
            .queue
            .update_item(&item.id, |i| i.status = ItemStatus::Cancelled)
            .unwrap();
        state.queue.finish(&item.id, Ok(None)).unwrap();
        assert_eq!(state.queue.snapshot().items[0].status, ItemStatus::Cancelled);
        assert!(state.queue.retry(&item.id).unwrap());
        assert_eq!(state.queue.snapshot().items[0].status, ItemStatus::Waiting);
        state.queue.take_next().unwrap();
        state
            .queue
            .finish(&item.id, Err(anyhow::anyhow!("Render failed: boom")))
            .unwrap();
        let s = state.queue.snapshot();
        assert_eq!(s.items[0].status, ItemStatus::Failed);
        assert_eq!(s.items[0].error.as_deref(), Some("Render failed: boom"));
        assert_eq!(state.queue.clear_finished().unwrap(), 1);
        assert!(state.queue.snapshot().items.is_empty());
    }

    #[test]
    fn releases_are_spaced_from_the_first_time() {
        let home = TestHome::new();
        let state = home.state();
        let at = Utc::now() + chrono::Duration::days(1);
        let mut b = body(&["a", "b", "c"]);
        b.options.publish = Some(PublishPlan {
            privacy: "public".into(),
            publish_at: Some(at),
        });
        b.publish_every_minutes = Some(90);
        let items = state.queue.add(b).unwrap();
        let times: Vec<_> = items
            .iter()
            .map(|i| i.options.publish.as_ref().unwrap().publish_at.unwrap())
            .collect();
        assert_eq!(times[0], at);
        assert_eq!(times[2] - times[0], chrono::Duration::minutes(180));
    }

    #[test]
    fn validation_rejects_bad_options() {
        let mut b = body(&["a"]);
        assert!(validate(&b).is_ok());
        b.options.target_seconds = Some(5);
        assert!(validate(&b).is_err());
        b.options.target_seconds = None;
        b.options.publish = Some(PublishPlan {
            privacy: "everyone".into(),
            publish_at: None,
        });
        assert!(validate(&b).is_err());
        assert!(validate(&AddBody::default()).is_err());
    }

    #[test]
    fn omitted_privacy_takes_the_configured_default() {
        let mut b: AddBody = serde_json::from_value(serde_json::json!({
            "topics": ["a"],
            "publish": {},
            "items": [
                { "topic": "b", "publish": { "privacy": "public" } },
                { "topic": "c", "publish": { "publish_at": null } },
                { "topic": "d" },
            ],
        }))
        .unwrap();
        assert!(validate(&b).is_err(), "an unfilled privacy is rejected");
        b.default_privacy("unlisted");
        assert!(validate(&b).is_ok());
        let privacy = |o: &ItemOptions| o.publish.as_ref().map(|p| p.privacy.clone());
        assert_eq!(privacy(&b.options).as_deref(), Some("unlisted"));
        assert_eq!(privacy(&b.items[0].options).as_deref(), Some("public"));
        assert_eq!(privacy(&b.items[1].options).as_deref(), Some("unlisted"));
        assert_eq!(privacy(&b.items[2].options), None, "no plan stays no plan");
    }

    #[tokio::test]
    async fn five_items_with_second_hitting_limit_all_succeed_after_wait() {
        tokio::time::pause();
        let home = TestHome::new();
        let state = home.state();

        let added = state
            .queue
            .add(body(&["one", "two", "three", "four", "five"]))
            .unwrap();
        assert_eq!(added.len(), 5);

        let second_attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts = second_attempts.clone();

        let worker_state = state.clone();
        let worker_handle = tokio::spawn(async move {
            worker_with(worker_state, true, move |_s, item| {
                let item_topic = item.topic.clone();
                let att = attempts.clone();
                Box::pin(async move {
                    if item_topic == "two" {
                        let prev = att.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if prev == 0 {
                            return Err(QueueRunError::Limit {
                                stage: clipcut_core::Stage::Research,
                                message: "Claude usage limit reached".into(),
                                resume_at: None,
                            }
                            .into());
                        }
                    }
                    Ok(Some(format!("https://youtu.be/{item_topic}")))
                })
            })
            .await;
        });

        // Yield to allow worker to run item 1 and hit limit on item 2
        while !state.queue.snapshot().paused {
            tokio::task::yield_now().await;
        }

        let snap = state.queue.snapshot();
        assert!(snap.paused);
        assert_eq!(snap.pause_reason.as_deref(), Some("Claude usage limit"));
        assert!(snap.resume_at.is_some());
        assert_eq!(snap.backoff_minutes, Some(15));
        // Item 2 was put back in line (Waiting), not Failed!
        assert_eq!(snap.items[1].status, ItemStatus::Waiting);
        assert_eq!(snap.items[1].error, None);

        // Advance past the 15-minute wait
        tokio::time::advance(Duration::from_secs(16 * 60)).await;

        worker_handle.await.unwrap();

        let final_snap = state.queue.snapshot();
        assert_eq!(final_snap.items.len(), 5);
        for item in &final_snap.items {
            assert_eq!(item.status, ItemStatus::Done);
            assert!(item.error.is_none());
            assert!(item.video_url.is_some());
        }
        assert!(!final_snap.paused);
    }

    #[tokio::test]
    async fn consecutive_limits_double_backoff_up_to_two_hours() {
        tokio::time::pause();
        let home = TestHome::new();
        let state = home.state();

        state.queue.add(body(&["topic1"])).unwrap();
        let fail_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fails = fail_count.clone();

        let worker_state = state.clone();
        let worker_handle = tokio::spawn(async move {
            worker_with(worker_state, true, move |_s, _item| {
                let count = fails.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    if count < 5 {
                        return Err(QueueRunError::Limit {
                            stage: clipcut_core::Stage::Research,
                            message: "rate limit".into(),
                            resume_at: None,
                        }
                        .into());
                    }
                    Ok(None)
                })
            })
            .await;
        });

        // 1st failure: 15m
        while state.queue.snapshot().backoff_minutes != Some(15) {
            tokio::task::yield_now().await;
        }
        assert_eq!(state.queue.snapshot().backoff_minutes, Some(15));
        tokio::time::advance(Duration::from_secs(16 * 60)).await;

        // 2nd failure: 30m
        while state.queue.snapshot().backoff_minutes != Some(30) {
            tokio::task::yield_now().await;
        }
        assert_eq!(state.queue.snapshot().backoff_minutes, Some(30));
        tokio::time::advance(Duration::from_secs(31 * 60)).await;

        // 3rd failure: 60m
        while state.queue.snapshot().backoff_minutes != Some(60) {
            tokio::task::yield_now().await;
        }
        assert_eq!(state.queue.snapshot().backoff_minutes, Some(60));
        tokio::time::advance(Duration::from_secs(61 * 60)).await;

        // 4th failure: 120m (capped at 2 hours)
        while state.queue.snapshot().backoff_minutes != Some(120) {
            tokio::task::yield_now().await;
        }
        assert_eq!(state.queue.snapshot().backoff_minutes, Some(120));
        tokio::time::advance(Duration::from_secs(121 * 60)).await;

        // 5th failure: still 120m
        while fail_count.load(std::sync::atomic::Ordering::SeqCst) < 5 {
            tokio::task::yield_now().await;
        }
        assert_eq!(state.queue.snapshot().backoff_minutes, Some(120));
        tokio::time::advance(Duration::from_secs(121 * 60)).await;

        worker_handle.await.unwrap();
        assert_eq!(state.queue.snapshot().items[0].status, ItemStatus::Done);
    }

    #[tokio::test]
    async fn limit_with_parsed_reset_time_uses_that_time() {
        tokio::time::pause();
        let home = TestHome::new();
        let state = home.state();

        state.queue.add(body(&["topic1"])).unwrap();
        let attempted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let att = attempted.clone();

        let target_time = Utc::now() + chrono::Duration::minutes(42);

        let worker_state = state.clone();
        let worker_handle = tokio::spawn(async move {
            worker_with(worker_state, true, move |_s, _item| {
                let was_attempted = att.swap(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    if !was_attempted {
                        return Err(QueueRunError::Limit {
                            stage: clipcut_core::Stage::Hooks,
                            message: "Your limit will reset at ...".into(),
                            resume_at: Some(target_time),
                        }
                        .into());
                    }
                    Ok(None)
                })
            })
            .await;
        });

        while !state.queue.snapshot().paused {
            tokio::task::yield_now().await;
        }
        let s = state.queue.snapshot();
        assert!(s.paused);
        assert_eq!(s.resume_at, Some(target_time));

        tokio::time::advance(Duration::from_secs(43 * 60)).await;
        worker_handle.await.unwrap();
        assert_eq!(state.queue.snapshot().items[0].status, ItemStatus::Done);
    }
}
