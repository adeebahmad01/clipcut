//! `project.json` persistence. All writes go through `update`, which holds a
//! per-project lock for the read-modify-write so the pipeline and UI edits
//! never clobber each other. Long work happens outside the lock.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clipcut_core::{Paths, Project};
use tokio::sync::{OwnedMutexGuard, broadcast, watch};
use tokio::task::AbortHandle;

use crate::state::ServerEvent;

#[derive(Clone)]
pub struct ProjectStore {
    paths: Paths,
    events: broadcast::Sender<ServerEvent>,
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// Serializes starts/cancellations with interactive edits. This is distinct
    /// from the short persistence lock: long work never holds a JSON lock.
    operations: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    running: Arc<Mutex<HashMap<String, RunningTask>>>,
}

struct RunningTask {
    token: String,
    abort: AbortHandle,
    done: watch::Receiver<bool>,
}

/// Lives in the task future, including before its first poll. Cleanup therefore
/// also happens on immediate cancellation, panic, or an early return.
struct RunGuard {
    store: ProjectStore,
    id: String,
    token: String,
    done: watch::Sender<bool>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        {
            let mut running = self.store.running.lock().unwrap();
            if running.get(&self.id).is_some_and(|r| r.token == self.token) {
                running.remove(&self.id);
            }
        }
        if let Ok(project) = self.store.get(&self.id) {
            self.store.notify(&self.id, project.updated_at);
        }
        let _ = self.done.send(true);
    }
}

impl ProjectStore {
    pub fn new(paths: Paths, events: broadcast::Sender<ServerEvent>) -> Self {
        Self {
            paths,
            events,
            locks: Default::default(),
            operations: Default::default(),
            running: Default::default(),
        }
    }

    pub fn dir(&self, id: &str) -> PathBuf {
        self.paths.project(id)
    }

    fn lock_for(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    pub fn exists(&self, id: &str) -> bool {
        valid_id(id) && self.dir(id).join("project.json").is_file()
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Project> {
        anyhow::ensure!(valid_id(id), "bad project id");
        Project::load(&self.dir(id)).with_context(|| format!("project {id} not found"))
    }

    pub async fn create(&self, mut project: Project) -> anyhow::Result<Project> {
        anyhow::ensure!(valid_id(&project.id), "bad project id");
        let lock = self.lock_for(&project.id);
        let _guard = lock.lock().await;
        anyhow::ensure!(!self.exists(&project.id), "project already exists");
        project.save(&self.dir(&project.id))?;
        self.emit(&project);
        Ok(project)
    }

    /// Load, mutate, save, notify. The closure's error aborts without saving.
    pub async fn update<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut Project) -> anyhow::Result<T>,
    ) -> anyhow::Result<(Project, T)> {
        let lock = self.lock_for(id);
        let _guard = lock.lock().await;
        let mut project = self.get(id)?;
        let out = f(&mut project)?;
        project.save(&self.dir(id))?;
        self.emit(&project);
        Ok((project, out))
    }

    pub async fn delete(&self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(valid_id(id), "bad project id");
        let lock = self.lock_for(id);
        let _guard = lock.lock().await;
        tokio::fs::remove_dir_all(self.dir(id)).await?;
        Ok(())
    }

    /// Every project, newest first. Unreadable folders are skipped.
    pub fn list(&self) -> Vec<Project> {
        let mut out: Vec<Project> = std::fs::read_dir(self.paths.projects())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Project::load(&e.path()).ok())
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out
    }

    fn emit(&self, project: &Project) {
        self.notify(&project.id, project.updated_at);
    }

    pub fn notify(&self, id: &str, updated_at: chrono::DateTime<chrono::Utc>) {
        let _ = self.events.send(ServerEvent::Project {
            id: id.to_string(),
            updated_at,
            running: self.is_running(id),
        });
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.running.lock().unwrap().contains_key(id)
    }

    pub fn running_ids(&self) -> Vec<String> {
        self.running.lock().unwrap().keys().cloned().collect()
    }

    /// Hold across a start/cancel/delete or an interactive operation. Pipeline
    /// stage writes deliberately do not take this lock.
    pub async fn operation(&self, id: &str) -> OwnedMutexGuard<()> {
        let lock = self
            .operations
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone();
        lock.lock_owned().await
    }

    /// Atomically reserve and launch one task. Its guard owns removal from the
    /// registry, so a finished old task cannot clear a newer run's handle.
    pub fn spawn_pipeline(
        &self,
        id: &str,
        future: impl Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        let mut running = self.running.lock().unwrap();
        anyhow::ensure!(!running.contains_key(id), "this project is already running");
        let token = uuid::Uuid::new_v4().to_string();
        let (done_tx, done_rx) = watch::channel(false);
        let guard = RunGuard {
            store: self.clone(),
            id: id.to_string(),
            token: token.clone(),
            done: done_tx,
        };
        let handle = tokio::spawn(async move {
            let _guard = guard;
            future.await;
        });
        running.insert(
            id.to_string(),
            RunningTask {
                token,
                abort: handle.abort_handle(),
                done: done_rx,
            },
        );
        drop(running);
        self.notify(id, chrono::Utc::now());
        Ok(())
    }

    /// Wait for all task-owned futures/guards to be dropped before an edit or
    /// restart. Keep the registry entry until that happens.
    pub async fn abort_and_wait(&self, id: &str) {
        let task = self
            .running
            .lock()
            .unwrap()
            .get(id)
            .map(|r| (r.abort.clone(), r.done.clone()));
        if let Some((abort, mut done)) = task {
            abort.abort();
            while !*done.borrow_and_update() {
                if done.changed().await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Project ids are generated as `YYYYMMDD-HHMMSS-xxxxxx`; refuse anything
/// that could escape the projects directory.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestHome, finished_project};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    async fn wait_finished(store: &ProjectStore, id: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while store.is_running(id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("task registry did not clear");
    }

    #[test]
    fn ids_cannot_escape_the_project_root() {
        for id in [
            "",
            "../escape",
            ".",
            "/tmp/escape",
            "a/b",
            "a\\b",
            "a.b",
            "a b",
        ] {
            assert!(!valid_id(id), "{id:?}");
        }
        assert!(valid_id("20261008-101500-abcdef"));
    }

    #[tokio::test]
    async fn immediate_completion_does_not_leave_a_running_entry() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        state.store.spawn_pipeline(&project.id, async {}).unwrap();
        wait_finished(&state.store, &project.id).await;
        assert!(state.store.running_ids().is_empty());
    }

    #[tokio::test]
    async fn abort_before_first_poll_cleans_the_registry_and_owned_guards() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let claim = state.claim_beat(&project.id, "b01").unwrap();
        state
            .store
            .spawn_pipeline(&project.id, async move {
                let _claim = claim;
                futures::future::pending::<()>().await;
            })
            .unwrap();
        // A current-thread test has not yielded since spawning.
        state.store.abort_and_wait(&project.id).await;
        assert!(!state.store.is_running(&project.id));
        assert!(!state.project_busy(&project.id));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simultaneous_reservations_start_exactly_one_task() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let wins = Arc::new(AtomicUsize::new(0));
        let mut attempts = Vec::new();
        for _ in 0..8 {
            let (store, id, barrier, wins) = (
                state.store.clone(),
                project.id.clone(),
                barrier.clone(),
                wins.clone(),
            );
            attempts.push(tokio::spawn(async move {
                barrier.wait().await;
                if store
                    .spawn_pipeline(&id, futures::future::pending())
                    .is_ok()
                {
                    wins.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for task in attempts {
            task.await.unwrap();
        }
        assert_eq!(wins.load(Ordering::SeqCst), 1);
        state.store.abort_and_wait(&project.id).await;
        assert!(state.store.running_ids().is_empty());
    }

    #[tokio::test]
    async fn cancelling_an_old_run_does_not_remove_a_new_run() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        state
            .store
            .spawn_pipeline(&project.id, futures::future::pending())
            .unwrap();
        state.store.abort_and_wait(&project.id).await;
        state
            .store
            .spawn_pipeline(&project.id, futures::future::pending())
            .unwrap();
        tokio::task::yield_now().await;
        assert!(state.store.is_running(&project.id));
        state.store.abort_and_wait(&project.id).await;
    }

    #[tokio::test]
    async fn panic_also_cleans_the_registry() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        state
            .store
            .spawn_pipeline(&project.id, async { panic!("fixture panic") })
            .unwrap();
        wait_finished(&state.store, &project.id).await;
    }

    #[tokio::test]
    async fn failed_update_keeps_the_persisted_project_and_duplicate_is_rejected() {
        let home = TestHome::new();
        let state = home.state();
        let project = state.store.create(finished_project()).await.unwrap();
        let result: anyhow::Result<(Project, ())> = state
            .store
            .update(&project.id, |p| {
                p.topic = "should not persist".into();
                anyhow::bail!("fixture rejection")
            })
            .await;
        assert!(result.is_err());
        assert_eq!(state.store.get(&project.id).unwrap().topic, project.topic);
        assert!(state.store.create(project).await.is_err());
    }

    #[tokio::test]
    async fn operation_lock_serializes_short_mutations() {
        let home = TestHome::new();
        let state = home.state();
        let entered = Arc::new(AtomicBool::new(false));
        let first = state.store.operation("fixture").await;
        let (store, flag) = (state.store.clone(), entered.clone());
        let task = tokio::spawn(async move {
            let _second = store.operation("fixture").await;
            flag.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert!(!entered.load(Ordering::SeqCst));
        drop(first);
        task.await.unwrap();
        assert!(entered.load(Ordering::SeqCst));
    }
}
