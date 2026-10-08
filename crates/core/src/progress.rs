use std::sync::Arc;

use serde::Serialize;

use crate::model::Stage;

#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub project_id: String,
    pub stage: Option<Stage>,
    /// Beat this event concerns, when any.
    pub beat_id: Option<String>,
    /// 0.0 ..= 1.0 within the stage, when known.
    pub progress: Option<f32>,
    pub message: String,
    pub level: Level,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Cheap, cloneable progress callback shared by every stage.
#[derive(Clone)]
pub struct Progress {
    project_id: String,
    stage: Option<Stage>,
    sink: Arc<dyn Fn(ProgressEvent) + Send + Sync>,
}

impl Progress {
    pub fn new(project_id: impl Into<String>, sink: Arc<dyn Fn(ProgressEvent) + Send + Sync>) -> Self {
        Self { project_id: project_id.into(), stage: None, sink }
    }

    /// A sink that only logs through `tracing`.
    pub fn log_only(project_id: impl Into<String>) -> Self {
        Self::new(
            project_id,
            Arc::new(|e: ProgressEvent| tracing::info!(stage = ?e.stage, beat = ?e.beat_id, "{}", e.message)),
        )
    }

    pub fn for_stage(&self, stage: Stage) -> Self {
        Self { stage: Some(stage), ..self.clone() }
    }

    fn emit(&self, beat_id: Option<&str>, progress: Option<f32>, level: Level, message: String) {
        (self.sink)(ProgressEvent {
            project_id: self.project_id.clone(),
            stage: self.stage,
            beat_id: beat_id.map(str::to_string),
            progress,
            message,
            level,
        });
    }

    pub fn info(&self, message: impl Into<String>) {
        self.emit(None, None, Level::Info, message.into());
    }
    pub fn warn(&self, message: impl Into<String>) {
        self.emit(None, None, Level::Warn, message.into());
    }
    pub fn error(&self, message: impl Into<String>) {
        self.emit(None, None, Level::Error, message.into());
    }
    /// Stage fraction 0..=1 with a status line.
    pub fn fraction(&self, progress: f32, message: impl Into<String>) {
        self.emit(None, Some(progress.clamp(0.0, 1.0)), Level::Info, message.into());
    }
    pub fn beat(&self, beat_id: &str, message: impl Into<String>) {
        self.emit(Some(beat_id), None, Level::Info, message.into());
    }
    pub fn beat_warn(&self, beat_id: &str, message: impl Into<String>) {
        self.emit(Some(beat_id), None, Level::Warn, message.into());
    }
}
