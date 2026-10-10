//! Process-wide state shared by the server, the pipeline and the CLI.

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use clipcut_core::{Config, KeyPool, Paths, Progress, ProgressEvent, Tools};
use clipcut_llm::Llm;
use clipcut_render::Renderer;
use clipcut_sources::Sources;
use clipcut_tts::Tts;
use serde::Serialize;
use tokio::sync::{Semaphore, broadcast};

use crate::store::ProjectStore;

/// Everything the UI hears about over SSE.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ServerEvent {
    Progress(ProgressEvent),
    Project {
        id: String,
        updated_at: chrono::DateTime<chrono::Utc>,
        running: bool,
    },
    Queue(crate::queue::QueueState),
}

/// The four engines, built from one config snapshot.
#[derive(Clone)]
pub struct Services {
    pub config: Config,
    pub tools: Tools,
    pub llm: Llm,
    pub sources: Sources,
    pub tts: Tts,
    pub renderer: Renderer,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub paths: Paths,
    pub keys: KeyPool,
    pub store: ProjectStore,
    pub events: broadcast::Sender<ServerEvent>,
    pub queue: crate::queue::Queue,
    pub publish: crate::publish::Publisher,
    config: RwLock<Config>,
    services: RwLock<Services>,
    /// Beats with a search/choose/auto request in flight, as `project/beat`.
    busy_beats: Mutex<std::collections::HashSet<String>>,
    pub settings_lock: tokio::sync::Mutex<()>,
    pub setup_slots: Arc<Semaphore>,
    pub preview_slots: Arc<Semaphore>,
    pub still_slots: Arc<Semaphore>,
    bound_port: AtomicU16,
}

/// Release claims even if a request or pipeline future is cancelled.
pub struct BeatClaim {
    state: AppState,
    key: String,
}

impl Drop for BeatClaim {
    fn drop(&mut self) {
        self.state.busy_beats.lock().unwrap().remove(&self.key);
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn load(paths: Paths) -> anyhow::Result<Self> {
        paths.ensure()?;
        let config = Config::load(&paths.config_file())?;
        config.validate()?;
        if !paths.config_file().exists() {
            config.save(&paths.config_file())?;
        }
        let keys = KeyPool::load(paths.keys_file())?;
        let (events, _) = broadcast::channel(1024);
        let services = build_services(&config, &paths, &keys);
        let store = ProjectStore::new(paths.clone(), events.clone());
        let queue = crate::queue::Queue::load(paths.queue_file(), events.clone());
        let publish = crate::publish::Publisher::new(paths.youtube_auth_file());
        Ok(Self(Arc::new(Inner {
            paths,
            keys,
            store,
            events,
            queue,
            publish,
            bound_port: AtomicU16::new(config.server.port),
            config: RwLock::new(config),
            services: RwLock::new(services),
            busy_beats: Mutex::new(Default::default()),
            settings_lock: tokio::sync::Mutex::new(()),
            setup_slots: Arc::new(Semaphore::new(1)),
            preview_slots: Arc::new(Semaphore::new(2)),
            still_slots: Arc::new(Semaphore::new(1)),
        })))
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    pub fn services(&self) -> Services {
        self.services.read().unwrap().clone()
    }

    /// Save a new config and rebuild the engines (tool paths may have changed).
    pub fn set_config(&self, config: Config) -> anyhow::Result<()> {
        config.validate()?;
        config.save(&self.paths.config_file())?;
        *self.services.write().unwrap() = build_services(&config, &self.paths, &self.keys);
        *self.config.write().unwrap() = config;
        Ok(())
    }

    /// Re-detect tools after setup installed something.
    pub fn refresh_tools(&self) {
        let config = self.config();
        *self.services.write().unwrap() = build_services(&config, &self.paths, &self.keys);
    }

    /// Progress sink that fans out to SSE and the log.
    pub fn progress(&self, project_id: &str) -> Progress {
        let events = self.events.clone();
        Progress::new(
            project_id,
            Arc::new(move |e: ProgressEvent| {
                match e.level {
                    clipcut_core::progress::Level::Error => {
                        tracing::error!(project = %e.project_id, stage = ?e.stage, "{}", e.message)
                    }
                    clipcut_core::progress::Level::Warn => {
                        tracing::warn!(project = %e.project_id, stage = ?e.stage, "{}", e.message)
                    }
                    clipcut_core::progress::Level::Info => {
                        tracing::info!(project = %e.project_id, stage = ?e.stage, "{}", e.message)
                    }
                }
                let _ = events.send(ServerEvent::Progress(e));
            }),
        )
    }

    /// This is acquired while holding the project's operation lock, so starts
    /// and interactive mutations cannot race with a new manual request.
    pub fn claim_beat(&self, project: &str, beat: &str) -> Option<BeatClaim> {
        let key = format!("{project}/{beat}");
        self.busy_beats
            .lock()
            .unwrap()
            .insert(key.clone())
            .then(|| BeatClaim {
                state: self.clone(),
                key,
            })
    }

    pub fn project_busy(&self, project: &str) -> bool {
        let prefix = format!("{project}/");
        self.busy_beats
            .lock()
            .unwrap()
            .iter()
            .any(|key| key.starts_with(&prefix))
    }

    pub fn beat_busy(&self, project: &str, beat: &str) -> bool {
        self.busy_beats
            .lock()
            .unwrap()
            .contains(&format!("{project}/{beat}"))
    }

    pub fn set_bound_port(&self, port: u16) {
        self.bound_port.store(port, Ordering::Relaxed);
    }

    pub fn bound_port(&self) -> u16 {
        self.bound_port.load(Ordering::Relaxed)
    }
}

pub fn build_services(config: &Config, paths: &Paths, keys: &KeyPool) -> Services {
    let tools = Tools::detect(config, paths);
    Services {
        config: config.clone(),
        tools: tools.clone(),
        llm: Llm::new(config.clone(), tools.clone()),
        sources: Sources::new(config.clone(), keys.clone(), paths.clone(), tools.clone()),
        tts: Tts::new(config.clone(), paths.clone(), tools.clone()),
        renderer: Renderer::new(config.clone(), paths.clone(), tools.clone()),
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::TestHome;

    #[tokio::test]
    async fn beat_claims_are_exclusive_and_released_when_a_future_is_aborted() {
        let home = TestHome::new();
        let state = home.state();
        let claim = state.claim_beat("project-a", "b01").unwrap();
        assert!(state.claim_beat("project-a", "b01").is_none());
        assert!(state.project_busy("project-a"));
        assert!(!state.project_busy("project-ab"));
        let task = tokio::spawn(async move {
            let _claim = claim;
            futures::future::pending::<()>().await;
        });
        task.abort();
        let _ = task.await;
        assert!(!state.project_busy("project-a"));
        assert!(state.claim_beat("project-a", "b01").is_some());
    }

    #[test]
    fn invalid_settings_do_not_change_memory_or_disk() {
        let home = TestHome::new();
        let state = home.state();
        let old_file = std::fs::read(state.paths.config_file()).unwrap();
        let mut invalid = state.config();
        invalid.video.fps = 0;
        assert!(state.set_config(invalid).is_err());
        assert_eq!(state.config().video.fps, 30);
        assert_eq!(std::fs::read(state.paths.config_file()).unwrap(), old_file);
    }
}
