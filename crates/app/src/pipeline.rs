//! Stage orchestration: research → hooks → script → beats → assets → voice →
//! render. One task per project; every stage writes its result through the
//! store so the UI can review, edit and re-run any of them.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use chrono::Utc;
use clipcut_core::config::Format;
use clipcut_core::model::{ProjectKind, RenderVariant, Subtitles};
use clipcut_core::paths::ProjectPaths;
use clipcut_core::subtitles;
use clipcut_core::timeline::Timeline;
use clipcut_core::{
    Asset, AssetKind, Beat, BeatStatus, Candidate, Progress, Project, RenderOutput, SourceRef,
    Stage, Status, VisualKind, VisualPlan, align_beats,
};
use clipcut_llm::CandidateBrief;
use clipcut_render::DirectorInputs;
use clipcut_sources::SearchCategory;
use futures::{FutureExt, StreamExt};

use crate::state::{AppState, Services};

/// Beats fetched at once during the assets stage.
const ASSET_CONCURRENCY: usize = 4;
/// Candidates tried per kind before falling back to the next kind.
const FETCH_ATTEMPTS: usize = 3;

/// Start the pipeline for a project in the background.
/// `from`: first stage to run (default: first stage not done). `only`: run just that one.
pub async fn start(
    state: &AppState,
    id: &str,
    from: Option<Stage>,
    only: bool,
) -> anyhow::Result<()> {
    let _operation = state.store.operation(id).await;
    anyhow::ensure!(
        !state.store.is_running(id),
        "this project is already running"
    );
    anyhow::ensure!(
        !state.project_busy(id),
        "a visual request is still running; wait before starting the pipeline"
    );
    anyhow::ensure!(
        !state.publish.is_uploading(id),
        "this project is uploading to YouTube; wait for it to finish"
    );
    let project = state.store.get(id)?;
    let first = match from {
        Some(stage) => stage,
        None => match first_unfinished(&project) {
            Some(stage) => stage,
            None => bail!("every stage is done; pick a stage to re-run"),
        },
    };
    // A clip's script and beat come from its source; there is nothing to write.
    let first = if project.kind == ProjectKind::Clip {
        first.max(Stage::Assets)
    } else {
        first
    };
    validate_start(&project, first)?;
    let stages: Vec<Stage> = if only {
        vec![first]
    } else {
        Stage::ALL.iter().copied().filter(|s| *s >= first).collect()
    };
    state.store.update(id, |p| Ok(reset_from(p, first))).await?;

    let task_state = state.clone();
    let task_id = id.to_string();
    state.store.spawn_pipeline(id, async move {
        match AssertUnwindSafe(run_stages(&task_state, &task_id, &stages))
            .catch_unwind()
            .await
        {
            Ok(Err(e)) => tracing::warn!(project = %task_id, "pipeline stopped: {e:#}"),
            Err(_) => {
                tracing::error!(project = %task_id, "pipeline panicked");
                let _ = task_state
                    .store
                    .update(&task_id, |p| {
                        mark_interrupted(p, "pipeline failed unexpectedly; retry this stage");
                        Ok(())
                    })
                    .await;
            }
            Ok(Ok(())) => {}
        }
    })
}

fn validate_start(p: &Project, first: Stage) -> anyhow::Result<()> {
    if p.kind == ProjectKind::Clip {
        anyhow::ensure!(
            p.clip.is_some() && p.beats.len() == 1,
            "this clip has lost its source moment; make it again from the video"
        );
        if first >= Stage::Voice {
            anyhow::ensure!(p.beats[0].asset.is_some(), "download the clip first");
        }
        if first == Stage::Render {
            anyhow::ensure!(p.voice_track.is_some(), "extract the clip's audio first");
        }
        return Ok(());
    }
    match first {
        Stage::Research => {}
        Stage::Hooks => anyhow::ensure!(p.research.is_some(), "research the topic first"),
        Stage::Script => anyhow::ensure!(
            p.research.is_some() && !p.hooks.is_empty(),
            "research the topic and write hooks first"
        ),
        Stage::Beats | Stage::Voice => anyhow::ensure!(
            p.script
                .as_ref()
                .is_some_and(|s| !s.text.trim().is_empty() && !s.sentences.is_empty()),
            "write the script first"
        ),
        Stage::Assets => anyhow::ensure!(!p.beats.is_empty(), "build the beat sheet first"),
        Stage::Render => {
            anyhow::ensure!(p.voice_track.is_some(), "record the voiceover first");
            anyhow::ensure!(!p.beats.is_empty(), "build the beat sheet first");
            anyhow::ensure!(
                p.beats.iter().all(|b| {
                    matches!(b.status, BeatStatus::Ready | BeatStatus::Fallback)
                        && (b.asset.is_some() || b.visual.kind == VisualKind::Text)
                }),
                "finish choosing visuals before rendering"
            );
        }
    }
    Ok(())
}

/// Abort a running pipeline and mark its in-flight stage as cancelled.
pub async fn cancel(state: &AppState, id: &str) -> anyhow::Result<Project> {
    let _operation = state.store.operation(id).await;
    state.store.abort_and_wait(id).await;
    let (project, _) = state
        .store
        .update(id, |p| {
            mark_interrupted(p, "cancelled");
            Ok(())
        })
        .await?;
    Ok(project)
}

/// After a crash or restart nothing is running: turn stale `running` states into failures.
pub async fn recover_interrupted(state: &AppState) {
    for project in state.store.list() {
        let stale = project.stages.iter().any(|s| s.status == Status::Running)
            || project
                .beats
                .iter()
                .any(|b| b.status == BeatStatus::Searching);
        if stale {
            let _ = state
                .store
                .update(&project.id, |p| {
                    mark_interrupted(p, "interrupted (ClipCut was restarted)");
                    Ok(())
                })
                .await;
        }
    }
}

fn mark_interrupted(p: &mut Project, reason: &str) {
    for s in p.stages.iter_mut().filter(|s| s.status == Status::Running) {
        s.status = Status::Failed;
        s.error = Some(reason.to_string());
        s.finished_at = Some(Utc::now());
    }
    for b in p
        .beats
        .iter_mut()
        .filter(|b| b.status == BeatStatus::Searching)
    {
        b.status = if b.asset.is_some() {
            BeatStatus::Ready
        } else {
            BeatStatus::Pending
        };
    }
}

pub fn first_unfinished(p: &Project) -> Option<Stage> {
    Stage::ALL.iter().copied().find(|&stage| {
        !p.stages
            .iter()
            .any(|s| s.stage == stage && matches!(s.status, Status::Done | Status::Skipped))
    })
}

/// Invalidate outputs that depend on this stage. Asset swaps do not invalidate
/// narration, and a visuals rerun preserves user-locked assets.
pub fn reset_from(p: &mut Project, stage: Stage) {
    // A clip's script and single beat are its source; never clear them.
    let stage = if p.kind == ProjectKind::Clip {
        stage.max(Stage::Assets)
    } else {
        stage
    };
    if stage <= Stage::Research {
        p.research = None;
    }
    if stage <= Stage::Hooks {
        p.hooks.clear();
        p.chosen_hook = None;
    }
    if stage <= Stage::Script {
        p.script = None;
    }
    if stage <= Stage::Beats {
        p.beats.clear();
    }
    if stage <= Stage::Voice && stage != Stage::Assets {
        p.voice_track = None;
        for beat in &mut p.beats {
            beat.start_s = 0.0;
            beat.end_s = 0.0;
        }
    }
    p.render = None;
    for s in p
        .stages
        .iter_mut()
        .filter(|s| s.stage >= stage && !(stage == Stage::Assets && s.stage == Stage::Voice))
    {
        s.status = Status::Pending;
        s.error = None;
        s.progress = 0.0;
        s.message.clear();
        s.started_at = None;
        s.finished_at = None;
    }
}

async fn run_stages(state: &AppState, id: &str, stages: &[Stage]) -> anyhow::Result<()> {
    let root = state.progress(id);
    for (i, &stage) in stages.iter().enumerate() {
        let progress = root.for_stage(stage);
        state
            .store
            .update(id, |p| {
                let s = p.stage_mut(stage);
                s.status = Status::Running;
                s.started_at = Some(Utc::now());
                s.finished_at = None;
                s.error = None;
                s.progress = 0.0;
                s.message = format!("{}…", stage.label());
                Ok(())
            })
            .await?;
        progress.fraction(0.0, format!("{}…", stage.label()));

        let result = run_stage(state, id, stage, &progress).await;
        let failed = result.as_ref().err().map(|e| format!("{e:#}"));
        let (project, _) = state
            .store
            .update(id, |p| {
                let s = p.stage_mut(stage);
                s.finished_at = Some(Utc::now());
                match &failed {
                    None => {
                        s.status = Status::Done;
                        s.progress = 1.0;
                        s.message = "Done".into();
                    }
                    Some(e) => {
                        s.status = Status::Failed;
                        s.error = Some(e.clone());
                        s.message = "Failed".into();
                    }
                }
                Ok(())
            })
            .await?;
        if let Some(e) = failed {
            progress.error(format!("{} failed: {e}", stage.label()));
            bail!("{} failed: {e}", stage.label());
        }
        progress.fraction(1.0, format!("{} done", stage.label()));

        let more = i + 1 < stages.len();
        // A clip's only visual is its own footage; there is nothing to review.
        let review = match stage {
            Stage::Hooks | Stage::Script => true,
            Stage::Assets => project.kind != ProjectKind::Clip,
            _ => false,
        };
        if more && !project.autopilot && review {
            progress.info(format!("Paused after {} for review", stage.label()));
            break;
        }
    }
    Ok(())
}

async fn run_stage(
    state: &AppState,
    id: &str,
    stage: Stage,
    progress: &Progress,
) -> anyhow::Result<()> {
    let svc = state.services();
    let project = state.store.get(id)?;
    let dir = state.store.dir(id);
    match stage {
        Stage::Research => {
            progress.info("Fetching web evidence before writing");
            let sources = research_sources(&svc, &project.topic).await?;
            let research = svc
                .llm
                .research_with_context(&project.topic, &project.notes, &sources, progress)
                .await
                .context("could not synthesize the supplied search evidence")?;
            state
                .store
                .update(id, |p| Ok(p.research = Some(research)))
                .await?;
        }
        Stage::Hooks => {
            let research = project.research.clone().unwrap_or_default();
            let hooks = svc
                .llm
                .hooks(&project.topic, &project.notes, &research, progress)
                .await?;
            if hooks.is_empty() {
                bail!("the model returned no hooks");
            }
            state
                .store
                .update(id, |p| {
                    p.hooks = hooks;
                    p.chosen_hook = Some(0);
                    Ok(())
                })
                .await?;
        }
        Stage::Script => {
            let research = project.research.clone().unwrap_or_default();
            let hook = project
                .chosen_hook_text()
                .or_else(|| project.hooks.first().map(|h| h.text.as_str()))
                .unwrap_or(&project.topic)
                .to_string();
            let script = svc
                .llm
                .script(
                    &project.topic,
                    &project.notes,
                    &research,
                    &hook,
                    project.target_seconds,
                    progress,
                )
                .await?;
            if script.sentences.is_empty() {
                bail!("the script came back empty");
            }
            state
                .store
                .update(id, |p| Ok(p.script = Some(script)))
                .await?;
        }
        Stage::Beats => {
            let script = project.script.clone().context("write the script first")?;
            let research = project.research.clone().unwrap_or_default();
            let plans = svc
                .llm
                .beats(&project.topic, &script, &research, progress)
                .await?;
            if plans.is_empty() {
                bail!("the beat sheet came back empty");
            }
            let beats: Vec<Beat> = plans
                .into_iter()
                .enumerate()
                .map(|(i, plan)| Beat {
                    id: format!("b{:02}", i + 1),
                    text: plan.text,
                    visual: plan.visual,
                    candidates: Vec::new(),
                    chosen: None,
                    asset: None,
                    status: BeatStatus::Pending,
                    error: None,
                    locked: false,
                    start_s: 0.0,
                    end_s: 0.0,
                })
                .collect();
            state
                .store
                .update(id, |p| {
                    p.beats = beats;
                    // Old timings no longer match the new beats.
                    if let Some(track) = &p.voice_track {
                        let (words, total) = (track.words.clone(), track.duration_s);
                        align_beats(&mut p.beats, &words, total);
                    }
                    Ok(())
                })
                .await?;
        }
        Stage::Assets if project.kind == ProjectKind::Clip => {
            crate::clips::download(state, &svc, &project, &dir, progress).await?
        }
        Stage::Voice if project.kind == ProjectKind::Clip => {
            crate::clips::extract_voice(state, &svc, &project, &dir, progress).await?
        }
        Stage::Assets => assets_stage(state, &svc, &project, &dir, progress).await?,
        Stage::Voice => {
            let script = project.script.clone().context("write the script first")?;
            let out = ProjectPaths(&dir).voice().join("voice.wav");
            tokio::fs::create_dir_all(ProjectPaths(&dir).voice()).await?;
            let track = svc
                .tts
                .synthesize(
                    &script.sentences,
                    project.voice.as_deref(),
                    &dir,
                    &out,
                    progress,
                )
                .await?;
            state
                .store
                .update(id, |p| {
                    align_beats(&mut p.beats, &track.words, track.duration_s);
                    p.voice_track = Some(track);
                    Ok(())
                })
                .await?;
        }
        Stage::Render => {
            if project.voice_track.is_none() {
                bail!("record the voiceover first");
            }
            if project.beats.is_empty() {
                bail!("build the beat sheet first");
            }
            let render_dir = ProjectPaths(&dir).render();
            tokio::fs::create_dir_all(&render_dir).await?;
            let formats = project.formats();
            for stale in [render_dir.join("thumb.jpg")].into_iter().chain(
                Format::ALL
                    .iter()
                    .map(|f| render_dir.join(variant_file(*f))),
            ) {
                let _ = std::fs::remove_file(stale);
            }
            // Each format gets an equal share of the progress bar.
            let share = 1.0 / formats.len() as f32;
            let timeline = build_timeline(&svc, &project, &dir).await?;
            write_json(&render_dir.join("timeline.json"), &timeline)?;
            let out = render_dir.join("final.mp4");
            svc.renderer
                .render(&timeline, &out, &progress.span(0.0, share))
                .await?;
            std::fs::write(render_dir.join("credits.txt"), credits(&project))?;
            let subtitles = write_subtitles(&project, &render_dir).unwrap_or_else(|e| {
                progress.warn(format!("Caption files failed: {e:#}"));
                None
            });
            let thumbnail_jpg = match svc
                .renderer
                .thumbnail(
                    &project,
                    &dir,
                    &timeline,
                    &render_dir.join("thumb.png"),
                    &render_dir.join("thumb.jpg"),
                )
                .await
            {
                Ok(()) => Some("render/thumb.jpg".to_string()),
                Err(e) => {
                    progress.warn(format!("Designed thumbnail failed, using a frame: {e:#}"));
                    let thumb_at = (timeline.duration_s * 0.1).clamp(0.0, 1.2);
                    if let Err(e) = svc
                        .renderer
                        .still(&timeline, thumb_at, &render_dir.join("thumb.png"))
                        .await
                    {
                        progress.warn(format!("Thumbnail failed: {e:#}"));
                    }
                    None
                }
            };
            // The same edit in the other aspect ratios. A failed variant
            // keeps the main video.
            let mut variants = Vec::new();
            for (i, &format) in formats.iter().enumerate().skip(1) {
                progress.info(format!("Rendering the {} version", format.ratio()));
                let mut alt = project.clone();
                alt.format = format;
                let file = variant_file(format);
                let result = async {
                    let timeline = build_timeline(&svc, &alt, &dir).await?;
                    let span = progress.span(i as f32 * share, share);
                    svc.renderer
                        .render(&timeline, &render_dir.join(&file), &span)
                        .await?;
                    anyhow::Ok((timeline.width, timeline.height))
                }
                .await;
                match result {
                    Ok((width, height)) => variants.push(RenderVariant {
                        format,
                        path: format!("render/{file}"),
                        width,
                        height,
                    }),
                    Err(e) => progress.warn(format!(
                        "The {} version failed: {e:#}",
                        format.ratio()
                    )),
                }
            }
            let output = RenderOutput {
                path: "render/final.mp4".into(),
                credits_path: "render/credits.txt".into(),
                duration_s: timeline.duration_s,
                width: timeline.width,
                height: timeline.height,
                rendered_at: Utc::now(),
                variants,
                subtitles,
                thumbnail_jpg,
            };
            state
                .store
                .update(id, |p| Ok(p.render = Some(output)))
                .await?;
        }
    }
    Ok(())
}

/// The app fetches evidence; coding-agent subprocesses never get browsing tools.
/// Search excerpts are evidence to assess, not facts automatically certified by us.
async fn research_sources(svc: &Services, topic: &str) -> anyhow::Result<Vec<SourceRef>> {
    let search = async {
        let (news, general) = tokio::join!(
            svc.sources.web_search(topic, SearchCategory::News, 8),
            svc.sources.web_search(topic, SearchCategory::General, 8),
        );
        let mut errors = Vec::new();
        let mut hits = Vec::new();
        for result in [news, general] {
            match result {
                Ok(found) => hits.extend(found),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        let sources = usable_evidence(hits);
        anyhow::ensure!(
            !sources.is_empty(),
            "no usable web evidence for {topic:?}; check search setup or retry. {}",
            errors.join("; ")
        );
        Ok(sources)
    };
    tokio::time::timeout(Duration::from_secs(45), search)
        .await
        .context("web evidence search exceeded 45 seconds")?
}

fn usable_evidence(hits: Vec<clipcut_sources::SearchHit>) -> Vec<SourceRef> {
    let mut seen = HashSet::new();
    hits.into_iter()
        .filter_map(|hit| {
            let mut url = reqwest::Url::parse(hit.url.trim()).ok()?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || hit.snippet.trim().is_empty()
            {
                return None;
            }
            url.set_fragment(None);
            let url = url.to_string();
            if !seen.insert(url.clone()) {
                return None;
            }
            Some(SourceRef {
                title: hit.title.trim().chars().take(300).collect(),
                url,
                snippet: hit.snippet.trim().chars().take(4000).collect(),
            })
        })
        .take(12)
        .collect()
}

// ---------------------------------------------------------------------------
// Visuals

async fn assets_stage(
    state: &AppState,
    svc: &Services,
    project: &Project,
    dir: &Path,
    progress: &Progress,
) -> anyhow::Result<()> {
    if project.beats.is_empty() {
        bail!("build the beat sheet first");
    }
    let todo: Vec<Beat> = project
        .beats
        .iter()
        .filter(|b| !(b.locked && b.asset.is_some()))
        .cloned()
        .collect();
    let total = todo.len().max(1);
    let mut done = 0usize;
    let mut fallbacks = 0usize;
    // Futures are built up front (not in a lazy `map`) so the task stays `Send`.
    let jobs: Vec<_> = todo
        .into_iter()
        .map(|beat| {
            let (state, svc, project, dir, progress) = (
                state.clone(),
                svc.clone(),
                project.clone(),
                dir.to_path_buf(),
                progress.clone(),
            );
            Box::pin(async move {
                let _claim = state
                    .claim_beat(&project.id, &beat.id)
                    .context("a beat is already busy")?;
                set_beat_status(&state, &project.id, &beat.id, BeatStatus::Searching).await?;
                let outcome = auto_visual(&svc, &project, &dir, &beat, &progress).await;
                save_outcome(&state, &project.id, &beat.id, outcome, false)
                    .await
                    .map(|b| b.status)
            })
        })
        .collect();
    let mut results = futures::stream::iter(jobs).buffer_unordered(ASSET_CONCURRENCY);
    while let Some(result) = results.next().await {
        done += 1;
        if result? == BeatStatus::Fallback {
            fallbacks += 1;
        }
        progress.fraction(
            done as f32 / total as f32,
            format!("Visuals {done}/{total}"),
        );
    }
    if fallbacks > 0 {
        progress.info(format!("{fallbacks} beat(s) used a fallback visual"));
    }
    Ok(())
}

pub async fn set_beat_status(
    state: &AppState,
    project_id: &str,
    beat_id: &str,
    status: BeatStatus,
) -> anyhow::Result<()> {
    state
        .store
        .update(project_id, |p| {
            let b = p
                .beats
                .iter_mut()
                .find(|b| b.id == beat_id)
                .context("beat no longer exists")?;
            b.status = status;
            b.error = None;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Result of finding a visual for one beat.
pub struct Outcome {
    pub visual: VisualPlan,
    pub candidates: Vec<Candidate>,
    pub chosen: Option<usize>,
    pub asset: Option<Asset>,
    pub status: BeatStatus,
    pub error: Option<String>,
}

/// Write an outcome onto the beat (if it still exists) and drop the replaced asset file.
pub async fn save_outcome(
    state: &AppState,
    project_id: &str,
    beat_id: &str,
    outcome: Outcome,
    locked: bool,
) -> anyhow::Result<Beat> {
    let dir = state.store.dir(project_id);
    let (_, (beat, old)) = state
        .store
        .update(project_id, |p| {
            let b = p
                .beats
                .iter_mut()
                .find(|b| b.id == beat_id)
                .context("beat no longer exists")?;
            let old = b.asset.as_ref().map(|a| a.path.clone());
            b.visual = outcome.visual;
            b.candidates = outcome.candidates;
            b.chosen = outcome.chosen;
            b.status = outcome.status;
            b.error = outcome.error;
            b.locked = locked;
            b.asset = outcome.asset;
            let old = old.filter(|o| b.asset.as_ref().is_none_or(|a| &a.path != o));
            let beat = b.clone();
            reset_from(p, Stage::Render);
            refresh_assets_status(p);
            Ok((beat, old))
        })
        .await?;
    if let Some(old) = old {
        remove_asset_file(&dir, &old);
    }
    Ok(beat)
}

/// Manual visual changes can finish the assets stage without another pipeline
/// run. Keep a running stage's own progress intact until its worker finishes.
pub fn refresh_assets_status(p: &mut Project) {
    let complete = !p.beats.is_empty()
        && p.beats.iter().all(|b| {
            matches!(b.status, BeatStatus::Ready | BeatStatus::Fallback)
                && (b.asset.is_some() || b.visual.kind == VisualKind::Text)
        });
    let s = p.stage_mut(Stage::Assets);
    if s.status == Status::Running {
        return;
    }
    s.status = if complete {
        Status::Done
    } else {
        Status::Pending
    };
    s.error = None;
    s.progress = if complete { 1.0 } else { 0.0 };
    s.message = if complete {
        "Done".into()
    } else {
        String::new()
    };
    s.finished_at = complete.then(Utc::now);
}

pub fn remove_asset_file(project_dir: &Path, rel: &str) {
    let relative = Path::new(rel);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return;
    }
    let path = project_dir.join(rel);
    if let (Ok(path), Ok(assets)) = (
        path.canonicalize(),
        ProjectPaths(project_dir).assets().canonicalize(),
    ) && path.starts_with(&assets)
        && path.is_file()
    {
        let _ = std::fs::remove_file(&path);
        let sidecar = path.with_extension("source.json");
        if let Ok(sidecar) = sidecar.canonicalize()
            && sidecar.starts_with(assets)
        {
            let _ = std::fs::remove_file(sidecar);
        }
    }
}

/// Planned kind first, then its fallbacks; `Text` always succeeds.
pub async fn auto_visual(
    svc: &Services,
    project: &Project,
    dir: &Path,
    beat: &Beat,
    progress: &Progress,
) -> Outcome {
    let planned = beat.visual.kind;
    let mut kinds = vec![planned];
    kinds.extend(
        planned
            .fallbacks()
            .iter()
            .copied()
            .filter(|k| *k != planned),
    );
    if !kinds.contains(&VisualKind::Text) {
        kinds.push(VisualKind::Text);
    }
    let mut last_error = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    for kind in kinds {
        let status = if kind == planned {
            BeatStatus::Ready
        } else {
            BeatStatus::Fallback
        };
        if kind == VisualKind::Text {
            let mut visual = beat.visual.clone();
            visual.kind = VisualKind::Text;
            if visual.on_screen_text.is_none() {
                visual.on_screen_text = Some(short_text(&beat.text));
            }
            return Outcome {
                visual,
                candidates: Vec::new(),
                chosen: None,
                asset: None,
                status,
                error: last_error,
            };
        }
        if kind != planned {
            progress.beat(&beat.id, format!("Trying {kind:?} instead"));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let attempt = tokio::time::timeout(
            remaining.min(Duration::from_secs(60)),
            try_kind(svc, project, dir, beat, kind, progress),
        )
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("visual search/download budget exhausted")));
        match attempt {
            Ok(Some((visual, candidates, chosen, asset))) => {
                return Outcome {
                    visual,
                    candidates,
                    chosen: Some(chosen),
                    asset: Some(asset),
                    status,
                    error: None,
                };
            }
            Ok(None) => progress.beat(&beat.id, format!("No usable {kind:?} found")),
            Err(e) => {
                progress.beat_warn(&beat.id, format!("{kind:?} failed: {e:#}"));
                last_error = Some(format!("{kind:?}: {e:#}"));
            }
        }
    }
    unreachable!("text kind always returns")
}

type Found = (VisualPlan, Vec<Candidate>, usize, Asset);

async fn try_kind(
    svc: &Services,
    project: &Project,
    dir: &Path,
    beat: &Beat,
    kind: VisualKind,
    progress: &Progress,
) -> anyhow::Result<Option<Found>> {
    let mut visual = beat.visual.clone();
    visual.kind = kind;
    if kind == VisualKind::Meme {
        visual.meme = Some(plan_meme(svc, beat, &visual).await?);
    }
    progress.beat(&beat.id, format!("Searching {kind:?}: {}", visual.query));
    let mut candidates = svc.sources.candidates(kind, &visual, 8).await?;
    for alt in visual.alt_queries.clone() {
        if !candidates.is_empty() {
            break;
        }
        let mut alt_plan = visual.clone();
        alt_plan.query = alt;
        candidates = svc
            .sources
            .candidates(kind, &alt_plan, 8)
            .await
            .unwrap_or_default();
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    let order = rank_candidates(svc, beat, &visual, &candidates).await;
    let mut last_error = None;
    for &index in order.iter().take(FETCH_ATTEMPTS) {
        match fetch_candidate(
            svc,
            project,
            dir,
            beat,
            &visual,
            &candidates[index],
            progress,
        )
        .await
        {
            Ok(asset) => return Ok(Some((visual, candidates, index, asset))),
            Err(e) => {
                progress.beat_warn(
                    &beat.id,
                    format!("Skipping {:?}: {e:#}", candidates[index].title),
                );
                last_error = Some(e);
            }
        }
    }
    match last_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

/// Keep the planner's template when Imgflip knows it, else let the model pick one.
pub async fn plan_meme(
    svc: &Services,
    beat: &Beat,
    visual: &VisualPlan,
) -> anyhow::Result<clipcut_core::MemePlan> {
    let templates = svc.sources.meme_templates().await?;
    if let Some(plan) = &visual.meme
        && let Some(t) = templates
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(plan.template.trim()))
    {
        return Ok(clipcut_core::MemePlan {
            template: t.name.clone(),
            ..plan.clone()
        });
    }
    let names: Vec<String> = templates.iter().map(|t| t.name.clone()).collect();
    svc.llm
        .pick_meme(&beat.text, visual.meme.as_ref(), &names)
        .await
}

/// Candidate indices, best first. The model only weighs in where titles matter.
async fn rank_candidates(
    svc: &Services,
    beat: &Beat,
    visual: &VisualPlan,
    candidates: &[Candidate],
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    if candidates.len() < 2
        || !matches!(
            visual.kind,
            VisualKind::Youtube | VisualKind::Article | VisualKind::Image
        )
    {
        return order;
    }
    let briefs: Vec<CandidateBrief> = candidates
        .iter()
        .enumerate()
        .take(8)
        .map(|(index, c)| CandidateBrief {
            index,
            title: c.title.clone(),
            provider: c.provider.clone(),
            detail: [
                c.duration_s.map(|d| format!("{d:.0}s")),
                c.width.zip(c.height).map(|(w, h)| format!("{w}x{h}")),
                Some(c.page_url.clone()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · "),
        })
        .collect();
    if let Ok(Some(best)) = svc
        .llm
        .pick_candidate(&beat.text, &visual.query, &briefs)
        .await
        && best < order.len()
    {
        order.retain(|&i| i != best);
        order.insert(0, best);
    }
    order
}

/// Download one candidate. YouTube cuts only the moment that matches the beat.
pub async fn fetch_candidate(
    svc: &Services,
    project: &Project,
    dir: &Path,
    beat: &Beat,
    visual: &VisualPlan,
    candidate: &Candidate,
    progress: &Progress,
) -> anyhow::Result<Asset> {
    let assets = ProjectPaths(dir).assets();
    tokio::fs::create_dir_all(&assets).await?;
    // A fresh name per fetch so the browser never shows a cached old clip.
    let stem = format!("{}-{}", beat.id, uuid::Uuid::new_v4().simple());
    let window = if candidate.kind == VisualKind::Youtube {
        Some(youtube_window(svc, project, beat, visual, candidate, progress).await)
    } else {
        None
    };
    progress.beat(&beat.id, format!("Downloading {}", candidate.provider));
    let mut asset = svc
        .sources
        .fetch(candidate, &assets, &stem, window, visual.quote.as_deref())
        .await?;
    // Sources return paths relative to the project dir; normalize absolute ones.
    if let Ok(rel) = Path::new(&asset.path).strip_prefix(dir) {
        asset.path = rel.to_string_lossy().into_owned();
    } else if !dir.join(&asset.path).exists() && assets.join(&asset.path).exists() {
        asset.path = format!("assets/{}", asset.path);
    }
    Ok(asset)
}

/// Downloads are bounded too, not just the final timeline's cut lengths.
fn clip_seconds(svc: &Services, beat: &Beat) -> f32 {
    let spoken = if beat.end_s > beat.start_s {
        beat.end_s - beat.start_s
    } else {
        beat.text.split_whitespace().count() as f32 / 2.6
    };
    spoken.clamp(
        svc.config.video.min_clip_seconds,
        svc.config.video.max_clip_seconds,
    )
}

async fn youtube_window(
    svc: &Services,
    project: &Project,
    beat: &Beat,
    visual: &VisualPlan,
    c: &Candidate,
    progress: &Progress,
) -> (f32, f32) {
    let _ = project;
    let len = clip_seconds(svc, beat);
    let max_clip = svc.config.video.max_clip_seconds;
    let cues = match svc.sources.youtube_captions(&c.page_url).await {
        Ok(cues) => cues,
        Err(e) => {
            progress.beat_warn(
                &beat.id,
                format!("No captions ({e:#}); using a default moment"),
            );
            Vec::new()
        }
    };
    let mut moment = None;
    if !cues.is_empty() {
        moment = match svc
            .llm
            .pick_moment(
                &beat.text,
                visual.quote.as_deref(),
                &c.title,
                &cues,
                max_clip,
            )
            .await
        {
            Ok(m) => m,
            Err(e) => {
                progress.beat_warn(
                    &beat.id,
                    format!("Moment pick failed ({e:#}); using keyword match"),
                );
                None
            }
        };
        if moment.is_none() {
            moment = svc.sources.best_moment_heuristic(
                &cues,
                &beat.text,
                visual.quote.as_deref(),
                max_clip,
            );
        }
    }
    let mut start = match moment {
        Some((s, _)) => (s - 0.2).max(0.0),
        None => c
            .start_s
            .unwrap_or_else(|| c.duration_s.map(|d| (d * 0.25).min(60.0)).unwrap_or(10.0)),
    };
    if !start.is_finite() || start < 0.0 {
        start = 0.0;
    }
    let duration = c.duration_s.filter(|d| d.is_finite() && *d > 0.0);
    if let Some(d) = duration {
        start = start.min((d - len).max(0.0));
    }
    (
        start,
        duration.map_or(start + len, |d| (start + len).min(d)),
    )
}

fn short_text(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().take(5).collect();
    words
        .join(" ")
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

// ---------------------------------------------------------------------------
// Render helpers

/// Resolve icons and music, then let the director lay out the edit.
pub async fn build_timeline(
    svc: &Services,
    project: &Project,
    dir: &Path,
) -> anyhow::Result<Timeline> {
    let lookups: Vec<_> = project
        .beats
        .iter()
        .filter_map(|b| b.visual.icon.clone().map(|icon| (b.id.clone(), icon)))
        .map(|(beat, icon)| {
            let sources = svc.sources.clone();
            Box::pin(async move {
                match sources.icon_svg(&icon).await {
                    Ok(Some(path)) => Some((beat, path)),
                    _ => None,
                }
            })
        })
        .collect();
    let found: Vec<Option<(String, PathBuf)>> = futures::stream::iter(lookups)
        .buffer_unordered(6)
        .collect()
        .await;
    let icons: HashMap<String, PathBuf> = found.into_iter().flatten().collect();
    let music = if svc.config.audio.use_music {
        pick_music(&svc.renderer.paths.music(), &project.id)
    } else {
        None
    };
    let inputs = DirectorInputs { icons, music };
    let renderer = svc.renderer.clone();
    let (project, dir) = (project.clone(), dir.to_path_buf());
    tokio::task::spawn_blocking(move || renderer.direct(&project, &dir, &inputs)).await?
}

/// Stable pick from the music folder so re-renders keep the same track.
fn pick_music(folder: &Path, seed: &str) -> Option<PathBuf> {
    let mut tracks: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "mp3" | "wav" | "ogg" | "m4a" | "flac" | "opus"
                )
            })
        })
        .collect();
    if tracks.is_empty() {
        return None;
    }
    tracks.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut h);
    Some(tracks.swap_remove(h.finish() as usize % tracks.len()))
}

fn credits(project: &Project) -> String {
    let mut out = format!(
        "{}\n\n",
        project
            .script
            .as_ref()
            .map(|s| s.title.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or(&project.topic)
    );
    if let Some(c) = &project.clip {
        out.push_str(&format!(
            "Clip source\n- {} — {} {} ({}–{})\n\n",
            c.title,
            c.channel,
            c.url,
            crate::clips::clock(c.start_s),
            crate::clips::clock(c.end_s)
        ));
    }
    out.push_str("Visuals\n");
    for b in &project.beats {
        if let Some(a) = &b.asset {
            let credit = if a.credit.is_empty() {
                a.provider.as_str()
            } else {
                a.credit.as_str()
            };
            out.push_str(&format!(
                "- {} ({}): {} {}\n",
                b.id, a.provider, credit, a.source_url
            ));
        }
    }
    out.push_str("\nRights review\nAttribution is not permission to reuse. YouTube footage, memes, GIFs, article captures and search images may be copyrighted. Check each asset's .source.json provenance, licensing and permissions before publishing. Uploaded media and local music remain your responsibility.\n");
    if let Some(r) = &project.research
        && !r.sources.is_empty()
    {
        out.push_str("\nSources\n");
        for s in &r.sources {
            out.push_str(&format!("- {} {}\n", s.title, s.url));
        }
    }
    out
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    clipcut_core::write_atomic(path, serde_json::to_string_pretty(value)?.as_bytes())
}

/// File name of a format's extra render, next to `final.mp4`.
fn variant_file(format: Format) -> String {
    format!("final-{}.mp4", format.slug())
}

/// `render/captions.srt` and `.vtt` from the voice track's word timings.
fn write_subtitles(project: &Project, render_dir: &Path) -> anyhow::Result<Option<Subtitles>> {
    let Some(track) = &project.voice_track else {
        return Ok(None);
    };
    let cues = subtitles::cues_from_words(&track.words);
    if cues.is_empty() {
        return Ok(None);
    }
    let srt = render_dir.join("captions.srt");
    let vtt = render_dir.join("captions.vtt");
    clipcut_core::write_atomic(&srt, subtitles::to_srt(&cues).as_bytes())?;
    clipcut_core::write_atomic(&vtt, subtitles::to_vtt(&cues).as_bytes())?;
    Ok(Some(Subtitles {
        srt: "render/captions.srt".into(),
        vtt: "render/captions.vtt".into(),
    }))
}

/// Classify a user upload by extension and probe its size.
pub async fn upload_asset(svc: &Services, project_dir: &Path, rel: &str) -> anyhow::Result<Asset> {
    let path = project_dir.join(rel);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let kind = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "bmp" => AssetKind::Image,
        "mp4" | "mov" | "webm" | "mkv" | "gif" | "m4v" | "avi" => AssetKind::Video,
        _ => bail!("unsupported file type .{ext}; upload a video, GIF or raster image"),
    };
    let ffprobe = clipcut_core::Tools::require(&svc.tools.ffprobe, "ffprobe")?;
    let probe = clipcut_core::media::probe(ffprobe, &path)
        .await
        .context("could not read that file")?;
    anyhow::ensure!(
        probe.has_video && probe.width > 0 && probe.height > 0,
        "the file has no usable visual stream"
    );
    anyhow::ensure!(
        probe.width <= 8192
            && probe.height <= 8192
            && u64::from(probe.width) * u64::from(probe.height) <= 32_000_000,
        "image/video dimensions are too large"
    );
    let duration = if kind == AssetKind::Video {
        anyhow::ensure!(
            probe.duration_s.is_finite() && probe.duration_s > 0.0 && probe.duration_s <= 1800.0,
            "video must be readable and between 0 and 1800 seconds"
        );
        Some(probe.duration_s)
    } else {
        None
    };
    Ok(Asset {
        kind,
        path: rel.to_string(),
        provider: "upload".into(),
        source_url: String::new(),
        credit: "Uploaded by you".into(),
        width: probe.width,
        height: probe.height,
        duration_s: duration,
        trim_start_s: 0.0,
        focus: None,
    })
}

pub const STILL_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestHome, finished_project};

    fn stage_status(project: &Project, stage: Stage) -> Status {
        project
            .stages
            .iter()
            .find(|s| s.stage == stage)
            .unwrap()
            .status
    }

    #[test]
    fn visual_resets_preserve_narration_and_locked_assets() {
        let mut project = finished_project();
        project.beats[0].locked = true;
        reset_from(&mut project, Stage::Assets);
        assert!(project.voice_track.is_some());
        assert_eq!(project.beats[0].end_s, 1.5);
        assert!(project.beats[0].locked);
        assert!(project.render.is_none());
        assert_eq!(stage_status(&project, Stage::Assets), Status::Pending);
        assert_eq!(stage_status(&project, Stage::Voice), Status::Done);
        assert_eq!(stage_status(&project, Stage::Render), Status::Pending);
    }

    #[test]
    fn script_resets_remove_all_dependent_outputs_but_not_research_or_hooks() {
        let mut project = finished_project();
        reset_from(&mut project, Stage::Script);
        assert!(project.research.is_some());
        assert!(!project.hooks.is_empty());
        assert!(project.script.is_none());
        assert!(project.beats.is_empty());
        assert!(project.voice_track.is_none());
        assert!(project.render.is_none());
        assert_eq!(first_unfinished(&project), Some(Stage::Script));
    }

    #[test]
    fn voice_reset_clears_timing_without_losing_the_beat_plan() {
        let mut project = finished_project();
        reset_from(&mut project, Stage::Voice);
        assert!(project.voice_track.is_none());
        assert!(project.render.is_none());
        assert_eq!(project.beats.len(), 1);
        assert_eq!(project.beats[0].start_s, 0.0);
        assert_eq!(project.beats[0].end_s, 0.0);
        assert_eq!(stage_status(&project, Stage::Assets), Status::Done);
    }

    #[test]
    fn assets_status_matches_manual_choices_but_keeps_pipeline_progress() {
        let mut project = finished_project();
        project.stage_mut(Stage::Assets).status = Status::Pending;
        refresh_assets_status(&mut project);
        assert_eq!(stage_status(&project, Stage::Assets), Status::Done);
        project.beats[0].status = BeatStatus::Pending;
        refresh_assets_status(&mut project);
        assert_eq!(stage_status(&project, Stage::Assets), Status::Pending);
        project.stage_mut(Stage::Assets).status = Status::Running;
        project.stage_mut(Stage::Assets).progress = 0.5;
        refresh_assets_status(&mut project);
        assert_eq!(stage_status(&project, Stage::Assets), Status::Running);
        assert_eq!(project.stage_mut(Stage::Assets).progress, 0.5);
        project.beats.clear();
        project.stage_mut(Stage::Assets).status = Status::Done;
        refresh_assets_status(&mut project);
        assert_eq!(stage_status(&project, Stage::Assets), Status::Pending);
    }

    #[test]
    fn interruption_restores_beats_and_marks_only_running_stages_failed() {
        let mut project = finished_project();
        project.stage_mut(Stage::Assets).status = Status::Running;
        project.beats[0].status = BeatStatus::Searching;
        mark_interrupted(&mut project, "cancelled");
        assert_eq!(stage_status(&project, Stage::Assets), Status::Failed);
        assert_eq!(stage_status(&project, Stage::Voice), Status::Done);
        assert_eq!(project.beats[0].status, BeatStatus::Pending);
        assert_eq!(
            project.stage_mut(Stage::Assets).error.as_deref(),
            Some("cancelled")
        );
    }

    #[tokio::test]
    async fn starts_with_missing_prerequisites_or_busy_visuals_do_not_reset_outputs() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let _claim = state.claim_beat(&project.id, "b01").unwrap();
        assert!(
            start(&state, &project.id, Some(Stage::Render), true)
                .await
                .is_err()
        );
        assert!(state.store.get(&project.id).unwrap().render.is_some());
        drop(_claim);
        state
            .store
            .update(&project.id, |p| Ok(p.voice_track = None))
            .await
            .unwrap();
        assert!(
            start(&state, &project.id, Some(Stage::Render), true)
                .await
                .is_err()
        );
        assert!(state.store.get(&project.id).unwrap().render.is_some());
        assert!(!state.store.is_running(&project.id));
    }

    #[tokio::test]
    async fn recovery_changes_stale_running_flags_without_starting_any_tasks() {
        let home = TestHome::new();
        let state = home.state();
        let mut project = finished_project();
        project.stage_mut(Stage::Voice).status = Status::Running;
        project.beats[0].status = BeatStatus::Searching;
        let project = state.store.create(project).await.unwrap();
        recover_interrupted(&state).await;
        let recovered = state.store.get(&project.id).unwrap();
        assert_eq!(stage_status(&recovered, Stage::Voice), Status::Failed);
        assert_eq!(recovered.beats[0].status, BeatStatus::Pending);
        assert!(!state.store.is_running(&project.id));
    }

    fn hit(url: &str, snippet: &str) -> clipcut_sources::SearchHit {
        clipcut_sources::SearchHit {
            title: "An evidence title".into(),
            url: url.into(),
            snippet: snippet.into(),
            image_url: None,
            thumbnail_url: None,
            published: None,
            engine: "fixture".into(),
        }
    }

    #[test]
    fn evidence_rejects_unsafe_or_empty_sources_and_deduplicates_fragments() {
        let evidence = usable_evidence(vec![
            hit("https://example.org/article#one", "A supplied excerpt"),
            hit("https://example.org/article#two", "Duplicate"),
            hit("file:///etc/passwd", "Unsafe"),
            hit("javascript:alert(1)", "Unsafe"),
            hit("https://user:pass@example.org/private", "Unsafe"),
            hit("https://example.org/empty", "  "),
        ]);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].url, "https://example.org/article");
        assert_eq!(evidence[0].snippet, "A supplied excerpt");
    }

    #[test]
    fn evidence_count_and_text_are_bounded() {
        let mut hits = Vec::new();
        for i in 0..30 {
            let mut item = hit(&format!("https://example.org/{i}"), &"a".repeat(6000));
            item.title = "b".repeat(1000);
            hits.push(item);
        }
        let evidence = usable_evidence(hits);
        assert_eq!(evidence.len(), 12);
        assert!(
            evidence
                .iter()
                .all(|s| s.title.len() == 300 && s.snippet.len() == 4000)
        );
    }

    #[test]
    fn youtube_download_windows_stay_between_one_and_three_seconds() {
        let home = TestHome::new();
        let state = home.state();
        let svc = state.services();
        let mut beat = finished_project().beats.remove(0);
        beat.text = "a".into();
        beat.start_s = 0.0;
        beat.end_s = 0.2;
        assert_eq!(clip_seconds(&svc, &beat), 1.0);
        beat.end_s = 25.0;
        assert_eq!(clip_seconds(&svc, &beat), 3.0);
        beat.start_s = 0.0;
        beat.end_s = 0.0;
        beat.text = "a very long sentence ".repeat(100);
        assert_eq!(clip_seconds(&svc, &beat), 3.0);
    }

    #[test]
    fn asset_cleanup_cannot_remove_traversals_or_symlink_targets() {
        let home = TestHome::new();
        let root = home.0.join("project");
        let assets = root.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let outside = home.0.join("outside.png");
        std::fs::write(&outside, b"outside").unwrap();
        remove_asset_file(&root, "../outside.png");
        assert!(outside.exists());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, assets.join("escape.png")).unwrap();
            remove_asset_file(&root, "assets/escape.png");
            assert!(outside.exists());
        }
        let inside = assets.join("fixture.png");
        let sidecar = assets.join("fixture.source.json");
        std::fs::write(&inside, b"inside").unwrap();
        std::fs::write(&sidecar, b"{}").unwrap();
        remove_asset_file(&root, "assets/fixture.png");
        assert!(!inside.exists());
        assert!(!sidecar.exists());
    }
}
