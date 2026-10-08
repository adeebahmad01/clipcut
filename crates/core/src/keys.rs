use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Up to this many keys are kept per service.
pub const MAX_KEYS_PER_SERVICE: usize = 5;
/// API keys are small opaque tokens, not arbitrary request bodies (bytes).
pub const MAX_KEY_LENGTH: usize = 4096;

/// Services that accept keys. All have free tiers; none are required.
pub const KNOWN_SERVICES: &[(&str, &str)] = &[
    ("giphy", "GIPHY — GIF search (developers.giphy.com)"),
    ("tenor", "Tenor — GIF search fallback (Google Cloud API key)"),
    ("pexels", "Pexels — stock video/photos (pexels.com/api)"),
    ("pixabay", "Pixabay — stock video fallback (pixabay.com/api/docs)"),
    ("unsplash", "Unsplash — photos (optional)"),
    ("tavily", "Tavily — web search fallback (optional)"),
    ("brave", "Brave Search — web/image search fallback (optional)"),
];

/// Error a provider adapter returns for a non-success HTTP status so the pool
/// can decide whether to rotate keys.
#[derive(Debug, thiserror::Error)]
#[error("{service} returned HTTP {status}: {message}")]
pub struct ProviderError {
    pub service: String,
    pub status: u16,
    pub message: String,
}

/// `keys.json`: `{ "giphy": ["k1", "k2"], "pexels": ["k1"] }`.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct KeyStore {
    #[serde(flatten)]
    pub services: BTreeMap<String, Vec<String>>,
}

impl KeyStore {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        // Repair permissive legacy stores before reading any secret contents.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let clean = serde_json::from_str::<Self>(&text)?.normalized();
        clean.validate()?;
        Ok(clean)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let clean = self.clone().normalized();
        clean.validate()?;
        crate::atomic::write(path, serde_json::to_string_pretty(&clean)?.as_bytes(), true)
    }

    /// Check token size and header-safety without including secrets in errors.
    /// Save/load additionally trim, deduplicate, and cap each service at five.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (service, keys) in &self.services {
            for (slot, key) in keys.iter().enumerate() {
                let key = key.trim();
                anyhow::ensure!(
                    key.len() <= MAX_KEY_LENGTH,
                    "key for {service} in slot {slot} exceeds {MAX_KEY_LENGTH} bytes"
                );
                anyhow::ensure!(
                    !key.chars().any(char::is_control),
                    "key for {service} in slot {slot} contains control characters"
                );
            }
        }
        Ok(())
    }

    fn normalized(self) -> Self {
        let mut clean = Self::default();
        for (service, keys) in self.services {
            let service = service.trim().to_ascii_lowercase();
            if service.is_empty() {
                continue;
            }
            let entry = clean.services.entry(service).or_default();
            for key in keys {
                let key = key.trim();
                if !key.is_empty() && entry.len() < MAX_KEYS_PER_SERVICE && !entry.iter().any(|k| k == key) {
                    entry.push(key.to_string());
                }
            }
        }
        clean.services.retain(|_, keys| !keys.is_empty());
        clean
    }
}

impl std::fmt::Debug for KeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.services.iter().map(|(service, keys)| (service, keys.len()))).finish()
    }
}

#[derive(Debug, Default)]
struct SlotState {
    cooldown_until: Option<Instant>,
    invalid: bool,
}

#[derive(Debug, Default)]
struct PoolState {
    store: KeyStore,
    /// Ignore results from requests using a store that has since been replaced.
    generation: u64,
    next: HashMap<String, usize>,
    slots: HashMap<(String, usize), SlotState>,
}

/// Round-robin key rotation with cooldowns. 401/403 disables a key for the
/// session; 408/429/5xx cools it down; any other error is returned as-is.
#[derive(Debug, Clone)]
pub struct KeyPool {
    path: PathBuf,
    state: Arc<Mutex<PoolState>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyStatus {
    pub service: String,
    pub slot: usize,
    /// Last 4 characters only; tokens of 4 characters or fewer are fully masked.
    pub hint: String,
    pub state: &'static str,
}

impl KeyPool {
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let store = KeyStore::load(&path)?;
        Ok(Self { path, state: Arc::new(Mutex::new(PoolState { store, ..Default::default() })) })
    }

    pub fn store(&self) -> KeyStore {
        self.state.lock().unwrap().store.clone()
    }

    pub fn replace(&self, store: KeyStore) -> anyhow::Result<()> {
        let store = store.normalized();
        store.validate()?;
        // Serialize replacements so the in-memory store agrees with the last
        // atomic disk write, and do not clear state when saving fails.
        let mut state = self.state.lock().unwrap();
        store.save(&self.path)?;
        *state = PoolState { store, generation: state.generation.wrapping_add(1), ..Default::default() };
        Ok(())
    }

    pub fn has(&self, service: &str) -> bool {
        self.state.lock().unwrap().store.services.get(service).is_some_and(|k| k.iter().any(|k| !k.trim().is_empty()))
    }

    pub fn status(&self) -> Vec<KeyStatus> {
        let state = self.state.lock().unwrap();
        let now = Instant::now();
        let mut out = Vec::new();
        for (service, keys) in &state.store.services {
            for (slot, key) in keys.iter().enumerate() {
                let s = state.slots.get(&(service.clone(), slot));
                let label = match s {
                    Some(s) if s.invalid => "invalid",
                    Some(SlotState { cooldown_until: Some(t), .. }) if *t > now => "cooling_down",
                    _ => "ready",
                };
                let hint = if key.chars().count() <= 4 {
                    "****".to_string()
                } else {
                    key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect()
                };
                out.push(KeyStatus { service: service.clone(), slot, hint, state: label });
            }
        }
        out
    }

    fn next_key(&self, service: &str) -> Option<(usize, String, u64)> {
        let mut state = self.state.lock().unwrap();
        let keys: Vec<String> = state.store.services.get(service)?.clone();
        if keys.is_empty() {
            return None;
        }
        let start = *state.next.get(service).unwrap_or(&0);
        let now = Instant::now();
        for offset in 0..keys.len() {
            let slot = (start + offset) % keys.len();
            if keys[slot].trim().is_empty() {
                continue;
            }
            let usable = match state.slots.get(&(service.to_string(), slot)) {
                Some(s) if s.invalid => false,
                Some(SlotState { cooldown_until: Some(t), .. }) => *t <= now,
                _ => true,
            };
            if usable {
                state.next.insert(service.to_string(), (slot + 1) % keys.len());
                return Some((slot, keys[slot].trim().to_string(), state.generation));
            }
        }
        None
    }

    fn mark(&self, service: &str, slot: usize, generation: u64, invalid: bool, cooldown: Option<Duration>) {
        let mut state = self.state.lock().unwrap();
        if state.generation != generation {
            return;
        }
        let entry = state.slots.entry((service.to_string(), slot)).or_default();
        if invalid {
            entry.invalid = true;
        }
        if let Some(duration) = cooldown {
            let until = Instant::now() + duration;
            // An older successful or shorter-cooldown response must not
            // re-enable a key another in-flight request just cooled down.
            entry.cooldown_until = Some(entry.cooldown_until.map_or(until, |old| old.max(until)));
        }
    }

    /// Run `request` with the next healthy key, rotating on auth/quota errors.
    /// Fails with `NoKey` text when the service has no usable key, so callers
    /// can fall through to their next provider.
    pub async fn with_key<T, F, Fut>(&self, service: &str, mut request: F) -> anyhow::Result<T>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = anyhow::Result<T>>,
    {
        let mut last: Option<anyhow::Error> = None;
        for _ in 0..MAX_KEYS_PER_SERVICE {
            let Some((slot, key, generation)) = self.next_key(service) else { break };
            match request(key).await {
                Ok(v) => {
                    self.mark(service, slot, generation, false, None);
                    return Ok(v);
                }
                Err(err) => {
                    let status = err.downcast_ref::<ProviderError>().map(|e| e.status);
                    match status {
                        Some(401 | 403) => self.mark(service, slot, generation, true, None),
                        Some(408 | 429) => {
                            self.mark(service, slot, generation, false, Some(Duration::from_secs(15 * 60)))
                        }
                        Some(500..=599) => self.mark(service, slot, generation, false, Some(Duration::from_secs(60))),
                        _ => return Err(err),
                    }
                    last = Some(err);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("NoKey: no usable {service} key configured")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    fn store(keys: &[&str]) -> KeyStore {
        KeyStore { services: BTreeMap::from([("giphy".into(), keys.iter().map(|k| k.to_string()).collect())]) }
    }

    fn pool(keys: &[&str]) -> (TestDir, KeyPool) {
        let dir = TestDir::new();
        let path = dir.path().join("keys.json");
        store(keys).save(&path).unwrap();
        let pool = KeyPool::load(path).unwrap();
        (dir, pool)
    }

    fn error(status: u16) -> anyhow::Error {
        ProviderError { service: "giphy".into(), status, message: "test error".into() }.into()
    }

    #[test]
    fn missing_store_loads_empty_without_creating_file() {
        let dir = TestDir::new();
        let path = dir.path().join("keys.json");
        assert!(KeyStore::load(&path).unwrap().services.is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn loaded_stores_are_normalized_deduplicated_and_capped() {
        let dir = TestDir::new();
        let path = dir.path().join("keys.json");
        std::fs::write(
            &path,
            r#"{" GIPHY ": [" one ", "", "one", "two", "three", "four", "five", "six"], "giphy": ["seven"], " ": ["secret"], "tenor": [" "]}"#,
        )
        .unwrap();
        let store = KeyStore::load(&path).unwrap();
        assert_eq!(store.services.len(), 1);
        assert_eq!(store.services["giphy"], ["one", "two", "three", "four", "five"]);
        assert_eq!(KeyPool::load(path).unwrap().status().len(), MAX_KEYS_PER_SERVICE);
    }

    #[test]
    fn saved_store_uses_the_same_normalization_as_loaded_store() {
        let dir = TestDir::new();
        let path = dir.path().join("keys.json");
        let original = store(&["  one  ", "one", "", " two ", "three", "four", "five", "six"]);
        original.save(&path).unwrap();
        let persisted: KeyStore = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.services["giphy"], ["one", "two", "three", "four", "five"]);
        assert_eq!(original.services["giphy"].len(), 8);
    }

    #[test]
    fn validates_token_size_and_control_characters_without_leaking_keys() {
        store(&[&"a".repeat(MAX_KEY_LENGTH)]).validate().unwrap();
        for secret in ["a".repeat(MAX_KEY_LENGTH + 1), "private\nsecret".into(), "private\0secret".into()] {
            let message = store(&[&secret]).validate().unwrap_err().to_string();
            assert!(!message.contains(&secret));
            assert!(message.contains("giphy"));
            assert!(message.contains("slot 0"));
        }
    }

    #[test]
    fn invalid_save_preserves_previous_store_and_has_no_temps() {
        let dir = TestDir::new();
        let path = dir.path().join("keys.json");
        store(&["original"]).save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(store(&[&"a".repeat(MAX_KEY_LENGTH + 1)]).save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(dir.entries(), vec![path]);
    }

    #[test]
    fn hints_fully_mask_short_tokens_and_keep_only_long_token_suffixes() {
        let (_dir, pool) = pool(&["a", "ab", "abc", "abcd", "secret-suffix"]);
        let status = pool.status();
        for item in &status[..4] {
            assert_eq!(item.hint, "****");
        }
        assert_eq!(status[4].hint, "ffix");
    }

    #[test]
    fn short_unicode_tokens_are_also_fully_masked() {
        let (_dir, pool) = pool(&["é", "é日", "é日🔒", "é日🔒ß", "prefix-é日🔒ß"]);
        let status = pool.status();
        assert!(status[..4].iter().all(|s| s.hint == "****"));
        assert_eq!(status[4].hint, "é日🔒ß");
    }

    #[test]
    fn debug_output_does_not_contain_tokens() {
        let (_dir, pool) = pool(&["super-private-token"]);
        assert!(!format!("{pool:?}").contains("super-private-token"));
        assert!(!format!("{:?}", pool.store()).contains("super-private-token"));
    }

    #[test]
    fn known_services_only_advertise_real_adapters() {
        for service in ["giphy", "tenor", "pexels", "pixabay", "unsplash", "brave", "tavily"] {
            assert!(KNOWN_SERVICES.iter().any(|(name, _)| *name == service));
        }
        assert!(!KNOWN_SERVICES.iter().any(|(name, _)| *name == "freesound"));
    }

    #[tokio::test]
    async fn rotates_healthy_keys_round_robin() {
        let (_dir, pool) = pool(&["first", "second", "third"]);
        for expected in ["first", "second", "third", "first", "second"] {
            let value = pool.with_key("giphy", |key| async move { Ok(key) }).await.unwrap();
            assert_eq!(value, expected);
        }
        assert!(pool.has("giphy"));
        assert!(!pool.has("missing"));
        assert!(pool.status().iter().all(|s| s.state == "ready"));
    }

    #[tokio::test]
    async fn auth_failures_disable_only_failed_keys_and_rotate() {
        let (_dir, pool) = pool(&["first", "second", "third"]);
        let mut attempted = Vec::new();
        let result = pool
            .with_key("giphy", |key| {
                attempted.push(key.clone());
                async move {
                    match key.as_str() {
                        "first" => Err(error(401)),
                        "second" => Err(error(403)),
                        _ => Ok(key),
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(result, "third");
        assert_eq!(attempted, ["first", "second", "third"]);
        assert_eq!(pool.status().iter().map(|s| s.state).collect::<Vec<_>>(), ["invalid", "invalid", "ready"]);
        assert_eq!(pool.with_key("giphy", |key| async move { Ok(key) }).await.unwrap(), "third");
    }

    #[tokio::test]
    async fn quota_and_server_failures_cool_down_then_recover() {
        for status in [408, 429, 500, 503, 599] {
            let (_dir, pool) = pool(&["first", "second"]);
            let result = pool
                .with_key("giphy", |key| async move { if key == "first" { Err(error(status)) } else { Ok(key) } })
                .await
                .unwrap();
            assert_eq!(result, "second");
            assert_eq!(pool.status()[0].state, "cooling_down");
            assert_eq!(pool.with_key("giphy", |key| async move { Ok(key) }).await.unwrap(), "second");
            pool.state.lock().unwrap().slots.get_mut(&("giphy".into(), 0)).unwrap().cooldown_until =
                Some(Instant::now() - Duration::from_secs(1));
            assert_eq!(pool.status()[0].state, "ready");
            assert_eq!(pool.with_key("giphy", |key| async move { Ok(key) }).await.unwrap(), "first");
        }
    }

    #[tokio::test]
    async fn ordinary_errors_do_not_trigger_retry_or_invalidation() {
        for status in [400, 404, 418, 600] {
            let (_dir, pool) = pool(&["first", "second"]);
            let mut calls = 0;
            let err = pool
                .with_key::<(), _, _>("giphy", |_| {
                    calls += 1;
                    std::future::ready(Err(error(status)))
                })
                .await
                .unwrap_err();
            assert_eq!(err.downcast_ref::<ProviderError>().unwrap().status, status);
            assert_eq!(calls, 1);
            assert!(pool.status().iter().all(|s| s.state == "ready"));
        }
        let (_dir, pool) = pool(&["first", "second"]);
        let mut calls = 0;
        assert!(
            pool.with_key::<(), _, _>("giphy", |_| {
                calls += 1;
                std::future::ready(Err(anyhow::anyhow!("network failure")))
            })
            .await
            .is_err()
        );
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn exhausted_pool_never_retries_disabled_keys() {
        let (_dir, pool) = pool(&["first", "second"]);
        let mut calls = 0;
        let result = pool
            .with_key::<(), _, _>("giphy", |_| {
                calls += 1;
                std::future::ready(Err(error(401)))
            })
            .await;
        assert!(result.is_err());
        assert_eq!(calls, 2);
        let result = pool
            .with_key::<(), _, _>("giphy", |_| {
                calls += 1;
                std::future::ready(Ok(()))
            })
            .await;
        assert!(result.unwrap_err().to_string().starts_with("NoKey:"));
        assert_eq!(calls, 2);
    }

    #[test]
    fn replacement_resets_rotation_and_ignores_old_inflight_results() {
        let (_dir, pool) = pool(&["old-first", "old-second"]);
        let (slot, _, generation) = pool.next_key("giphy").unwrap();
        pool.mark("giphy", slot, generation, true, None);
        pool.replace(store(&["new-first", "new-second"])).unwrap();
        pool.mark("giphy", slot, generation, true, Some(Duration::from_secs(900)));
        assert!(pool.status().iter().all(|s| s.state == "ready"));
        assert_eq!(pool.next_key("giphy").unwrap().1, "new-first");
        assert_eq!(KeyStore::load(&pool.path).unwrap().services, pool.store().services);
    }

    #[test]
    fn late_success_or_shorter_cooldown_cannot_clear_cooldown() {
        let (_dir, pool) = pool(&["first"]);
        let (slot, _, generation) = pool.next_key("giphy").unwrap();
        pool.mark("giphy", slot, generation, false, Some(Duration::from_secs(900)));
        let until = pool.state.lock().unwrap().slots[&("giphy".into(), slot)].cooldown_until;
        pool.mark("giphy", slot, generation, false, None);
        pool.mark("giphy", slot, generation, false, Some(Duration::from_secs(60)));
        assert_eq!(pool.state.lock().unwrap().slots[&("giphy".into(), slot)].cooldown_until, until);
        assert!(pool.next_key("giphy").is_none());
    }

    #[test]
    fn failed_replacement_preserves_pool_and_disk() {
        let (_dir, pool) = pool(&["first"]);
        let original = pool.store().services;
        assert!(pool.replace(store(&[&"a".repeat(MAX_KEY_LENGTH + 1)])).is_err());
        assert_eq!(pool.store().services, original);
        assert_eq!(KeyStore::load(&pool.path).unwrap().services, original);
    }

    #[test]
    fn simultaneous_replacements_keep_memory_and_disk_in_sync() {
        let (_dir, pool) = pool(&["first"]);
        std::thread::scope(|scope| {
            for i in 0..8 {
                let pool = &pool;
                scope.spawn(move || pool.replace(store(&[&format!("replacement-{i}")])).unwrap());
            }
        });
        assert_eq!(KeyStore::load(&pool.path).unwrap().services, pool.store().services);
    }

    #[cfg(unix)]
    #[test]
    fn save_and_load_enforce_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let path = dir.path().join("keys.json");
        store(&["secret"]).save(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        KeyStore::load(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        store(&["replacement"]).save(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(dir.entries(), vec![path]);
    }
}
