//! One-click publishing to YouTube through the Data API v3. Each user brings
//! their own free Google Cloud "Desktop app" OAuth client. Sign-in is the
//! installed-app flow (PKCE + a loopback redirect to `http://127.0.0.1:<port>`),
//! and the refresh token stays in `youtube-oauth.json` (mode 0600). Uploads
//! are resumable, sent in chunks and retried with backoff.

use std::collections::{HashMap, HashSet};
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::Context;
use base64::Engine;
use chrono::{DateTime, SecondsFormat, Utc};
use clipcut_core::config::{Format, PublishConfig};
use clipcut_core::model::write_private;
use clipcut_core::{Progress, Project, PublishRecord};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HeaderMap, LOCATION, RANGE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::AbortHandle;

use crate::queue::validate_privacy;
use crate::state::AppState;

const SCOPES: &str = "https://www.googleapis.com/auth/youtube.upload \
                      https://www.googleapis.com/auth/youtube.readonly";
/// How long a sign-in started with Connect stays valid.
const SIGN_IN_TTL: Duration = Duration::from_secs(600);
/// Failed chunks in a row before the upload gives up.
const MAX_RETRIES: u32 = 5;
/// YouTube treats vertical and square videos up to 3 minutes as Shorts.
const SHORTS_MAX_S: f32 = 180.0;

/// Google's endpoints; tests point them at a mock server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub revoke: String,
    pub api: String,
    pub upload: String,
    /// Upload chunk size, a multiple of 256 KiB.
    pub chunk: usize,
    /// First retry delay; it doubles per failure, up to 32×.
    pub backoff: Duration,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            api: "https://www.googleapis.com/youtube/v3".into(),
            upload: "https://www.googleapis.com/upload/youtube/v3".into(),
            chunk: 8 * 1024 * 1024,
            backoff: Duration::from_secs(1),
        }
    }
}

/// `youtube-oauth.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Auth {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub connected_at: Option<DateTime<Utc>>,
}

/// What the Settings page shows. Never includes the secret or tokens.
#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    /// An OAuth client is saved.
    pub configured: bool,
    pub connected: bool,
    pub client_id: Option<String>,
    pub channel: Option<String>,
    pub channel_id: Option<String>,
    pub connected_at: Option<DateTime<Utc>>,
    /// A sign-in is waiting for Google's redirect.
    pub connecting: bool,
}

/// `PUT /api/publish/youtube`: the two values, or the downloaded client JSON.
#[derive(Debug, Default, Deserialize)]
pub struct ClientBody {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// The `client_secret_….json` Google Cloud downloads for a Desktop app client.
    #[serde(default)]
    pub client_json: Option<String>,
}

struct Pending {
    state: String,
    verifier: String,
    redirect_uri: String,
    created: Instant,
}

pub struct Publisher {
    file: PathBuf,
    pub endpoints: RwLock<Endpoints>,
    http: reqwest::Client,
    pending: Mutex<Option<Pending>>,
    /// The loopback listener waiting for Google's redirect.
    listener: Mutex<Option<AbortHandle>>,
    /// Serializes reads-modify-writes of the auth file (token refreshes).
    file_lock: tokio::sync::Mutex<()>,
    uploading: Mutex<HashSet<String>>,
}

/// Releases a project's upload slot even if the upload future is dropped.
struct Uploading<'a> {
    publisher: &'a Publisher,
    id: String,
}

impl Drop for Uploading<'_> {
    fn drop(&mut self) {
        self.publisher.uploading.lock().unwrap().remove(&self.id);
    }
}

#[derive(Debug, Deserialize)]
struct Tokens {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// An error from Google's token endpoint, e.g. `invalid_grant`.
#[derive(Debug)]
struct OAuthError {
    code: String,
    description: String,
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.description.is_empty() {
            write!(f, "Google said {}", self.code)
        } else {
            write!(f, "Google said {}: {}", self.code, self.description)
        }
    }
}

impl std::error::Error for OAuthError {}

impl Publisher {
    pub fn new(file: PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ClipCut/", env!("CARGO_PKG_VERSION")))
            // A 308 means "resume the upload here", not a redirect to follow.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self {
            file,
            endpoints: RwLock::new(Endpoints::default()),
            http,
            pending: Mutex::new(None),
            listener: Mutex::new(None),
            file_lock: tokio::sync::Mutex::new(()),
            uploading: Mutex::new(HashSet::new()),
        }
    }

    pub fn endpoints(&self) -> Endpoints {
        self.endpoints.read().unwrap().clone()
    }

    fn read(&self) -> Auth {
        match std::fs::read(&self.file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("ignoring unreadable {}: {e}", self.file.display());
                Auth::default()
            }),
            Err(_) => Auth::default(),
        }
    }

    fn write(&self, auth: &Auth) -> anyhow::Result<()> {
        write_private(&self.file, &serde_json::to_vec_pretty(auth)?)
    }

    pub fn status(&self) -> AuthStatus {
        let auth = self.read();
        let configured = !auth.client_id.is_empty() && !auth.client_secret.is_empty();
        let connecting = self
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|p| p.created.elapsed() < SIGN_IN_TTL);
        AuthStatus {
            configured,
            connected: configured && auth.refresh_token.is_some(),
            client_id: (!auth.client_id.is_empty()).then_some(auth.client_id),
            channel: auth.channel,
            channel_id: auth.channel_id,
            connected_at: auth.connected_at,
            connecting,
        }
    }

    /// Save the OAuth client. A different client drops the old sign-in.
    pub async fn set_client(&self, client_id: String, client_secret: String) -> anyhow::Result<()> {
        let _lock = self.file_lock.lock().await;
        let mut auth = self.read();
        if auth.client_id != client_id {
            self.cancel_sign_in();
            auth = Auth::default();
        }
        auth.client_id = client_id;
        auth.client_secret = client_secret;
        self.write(&auth)
    }

    /// Forget the sign-in (revoking it at Google, best effort). With
    /// `forget_client` the OAuth client goes too.
    pub async fn disconnect(&self, forget_client: bool) -> anyhow::Result<()> {
        self.cancel_sign_in();
        let _lock = self.file_lock.lock().await;
        let auth = self.read();
        if let Some(token) = auth.refresh_token.as_ref().or(auth.access_token.as_ref()) {
            let revoked = self
                .http
                .post(self.endpoints().revoke)
                .form(&[("token", token.as_str())])
                .timeout(Duration::from_secs(15))
                .send()
                .await;
            if let Err(e) = revoked {
                tracing::warn!("could not revoke the YouTube sign-in at Google: {e}");
            }
        }
        if forget_client {
            return match std::fs::remove_file(&self.file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        self.write(&Auth {
            client_id: auth.client_id,
            client_secret: auth.client_secret,
            ..Default::default()
        })
    }

    fn cancel_sign_in(&self) {
        *self.pending.lock().unwrap() = None;
        if let Some(listener) = self.listener.lock().unwrap().take() {
            listener.abort();
        }
    }

    pub fn is_uploading(&self, project_id: &str) -> bool {
        self.uploading.lock().unwrap().contains(project_id)
    }

    fn claim(&self, project_id: &str) -> Option<Uploading<'_>> {
        self.uploading
            .lock()
            .unwrap()
            .insert(project_id.to_string())
            .then(|| Uploading {
                publisher: self,
                id: project_id.to_string(),
            })
    }

    /// Pretend a sign-in finished, for route tests.
    #[cfg(test)]
    pub(crate) fn sign_in_for_test(&self) {
        self.write(&Auth {
            client_id: "cid".into(),
            client_secret: "secret".into(),
            refresh_token: Some("refresh".into()),
            ..Default::default()
        })
        .unwrap();
    }

    /// Hold a project's upload slot, for route tests.
    #[cfg(test)]
    pub(crate) fn set_uploading_for_test(&self, project_id: &str, uploading: bool) {
        let mut set = self.uploading.lock().unwrap();
        if uploading {
            set.insert(project_id.to_string());
        } else {
            set.remove(project_id);
        }
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> anyhow::Result<Tokens> {
        let response = self
            .http
            .post(self.endpoints().token)
            .form(form)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("could not reach Google")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let v: Value = serde_json::from_str(&body).unwrap_or_default();
            return Err(OAuthError {
                code: v["error"].as_str().unwrap_or(status.as_str()).to_string(),
                description: v["error_description"].as_str().unwrap_or_default().to_string(),
            }
            .into());
        }
        serde_json::from_str(&body).context("Google's token response was not understood")
    }

    /// A valid access token, refreshed when it expires within a minute (or
    /// always, with `force`, after YouTube answered 401).
    async fn access_token(&self, force: bool) -> anyhow::Result<String> {
        let _lock = self.file_lock.lock().await;
        let mut auth = self.read();
        let refresh = match &auth.refresh_token {
            Some(token) if !auth.client_id.is_empty() => token.clone(),
            _ => anyhow::bail!("YouTube isn't connected; connect it in Settings → YouTube"),
        };
        if !force
            && let (Some(token), Some(expires)) = (&auth.access_token, auth.expires_at)
            && expires > Utc::now() + chrono::Duration::seconds(60)
        {
            return Ok(token.clone());
        }
        let result = self
            .token_request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh),
                ("client_id", &auth.client_id),
                ("client_secret", &auth.client_secret),
            ])
            .await;
        match result {
            Ok(tokens) => {
                auth.access_token = Some(tokens.access_token.clone());
                auth.expires_at = Some(expiry(tokens.expires_in));
                if tokens.refresh_token.is_some() {
                    auth.refresh_token = tokens.refresh_token;
                }
                self.write(&auth)?;
                Ok(tokens.access_token)
            }
            Err(e) if e.downcast_ref::<OAuthError>().is_some_and(|o| o.code == "invalid_grant") => {
                auth.refresh_token = None;
                auth.access_token = None;
                auth.expires_at = None;
                self.write(&auth)?;
                anyhow::bail!(
                    "YouTube access was revoked or has expired; reconnect in Settings → YouTube. \
                     (While a Google Cloud app is in Testing mode, Google ends its sign-ins after \
                     7 days; publish the OAuth consent screen to keep them.)"
                )
            }
            Err(e) => Err(e.context("could not refresh the YouTube sign-in")),
        }
    }

    /// The signed-in account's channel id and title.
    async fn channel(&self, token: &str) -> anyhow::Result<Option<(String, String)>> {
        let response = self
            .http
            .get(format!("{}/channels", self.endpoints().api))
            .query(&[("part", "snippet"), ("mine", "true")])
            .bearer_auth(token)
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(api_error(status, &body));
        }
        let v: Value = serde_json::from_str(&body)?;
        Ok(v["items"].get(0).map(|c| {
            (
                c["id"].as_str().unwrap_or_default().to_string(),
                c["snippet"]["title"].as_str().unwrap_or_default().to_string(),
            )
        }))
    }
}

fn expiry(expires_in: Option<i64>) -> DateTime<Utc> {
    Utc::now() + chrono::Duration::seconds(expires_in.unwrap_or(3600).clamp(60, 86_400))
}

/// RFC 7636 S256 challenge for a PKCE verifier.
fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Validate the OAuth client from the Settings form.
pub fn parse_client(body: &ClientBody) -> Result<(String, String), String> {
    let json = body
        .client_json
        .as_deref()
        .map(str::trim)
        .filter(|j| !j.is_empty());
    let (id, secret) = match json {
        Some(json) => {
            if json.len() > 16_384 {
                return Err("the client JSON is too long".into());
            }
            let v: Value = serde_json::from_str(json).map_err(|_| {
                "that isn't the JSON file Google Cloud downloads for an OAuth client".to_string()
            })?;
            if v.get("installed").is_none() && v.get("web").is_some() {
                return Err(
                    "this is a Web application client; create a Desktop app client instead".into(),
                );
            }
            let c = &v["installed"];
            (
                c["client_id"].as_str().unwrap_or_default().trim().to_string(),
                c["client_secret"].as_str().unwrap_or_default().trim().to_string(),
            )
        }
        None => (
            body.client_id.trim().to_string(),
            body.client_secret.trim().to_string(),
        ),
    };
    for (name, value) in [("client_id", &id), ("client_secret", &secret)] {
        if value.is_empty() {
            return Err(format!("{name} is missing"));
        }
        if value.len() > 512 || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!("{name} is not valid"));
        }
    }
    Ok((id, secret))
}

/// Start a sign-in: the Google URL to open and the loopback listener that
/// will receive the redirect.
pub async fn begin_connect(state: &AppState) -> anyhow::Result<(String, TcpListener)> {
    let p = &state.publish;
    let auth = p.read();
    anyhow::ensure!(
        !auth.client_id.is_empty() && !auth.client_secret.is_empty(),
        "add your Google OAuth client in Settings → YouTube first"
    );
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("could not open a local port for Google's sign-in redirect")?;
    let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = pkce_challenge(&verifier);
    let csrf = uuid::Uuid::new_v4().simple().to_string();
    let url = url::Url::parse_with_params(
        &p.endpoints().auth,
        &[
            ("client_id", auth.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", SCOPES),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", csrf.as_str()),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("include_granted_scopes", "true"),
        ],
    )?;
    *p.pending.lock().unwrap() = Some(Pending {
        state: csrf,
        verifier,
        redirect_uri,
        created: Instant::now(),
    });
    Ok((url.into(), listener))
}

/// Start a sign-in and wait for the redirect in the background.
pub async fn connect(state: &AppState) -> anyhow::Result<String> {
    let (url, listener) = begin_connect(state).await?;
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        if let Err(e) = serve_callback(&task_state, listener).await {
            tracing::warn!("YouTube sign-in: {e:#}");
        }
    });
    if let Some(old) = state
        .publish
        .listener
        .lock()
        .unwrap()
        .replace(task.abort_handle())
    {
        old.abort();
    }
    Ok(url)
}

/// Answer Google's redirect on the loopback listener and finish the sign-in.
pub async fn serve_callback(state: &AppState, listener: TcpListener) -> anyhow::Result<AuthStatus> {
    let deadline = tokio::time::Instant::now() + SIGN_IN_TTL;
    loop {
        let (mut socket, _) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| anyhow::anyhow!("Google sign-in timed out; click Connect again"))??;
        let target =
            match tokio::time::timeout(Duration::from_secs(10), request_target(&mut socket)).await {
                Ok(Ok(target)) => target,
                _ => continue,
            };
        let params = query_params(&target);
        if !params.contains_key("code") && !params.contains_key("error") {
            let _ = respond(&mut socket, "404 Not Found", "Not found", "").await;
            continue;
        }
        let result = finish_connect(state, &params).await;
        let _ = match &result {
            Ok(status) => {
                let who = status
                    .channel
                    .as_deref()
                    .map(|c| format!(" as {c}"))
                    .unwrap_or_default();
                let detail = format!("Signed in{who}. Close this tab and go back to ClipCut.");
                respond(&mut socket, "200 OK", "YouTube connected", &detail).await
            }
            Err(e) => {
                let detail = format!("{e:#}");
                respond(&mut socket, "400 Bad Request", "YouTube sign-in failed", &detail).await
            }
        };
        let still_waiting = state.publish.pending.lock().unwrap().is_some();
        match result {
            // A stale tab; keep waiting for the current sign-in.
            Err(_) if still_waiting => continue,
            other => return other,
        }
    }
}

/// The path and query of an HTTP request's first line.
async fn request_target(socket: &mut TcpStream) -> anyhow::Result<String> {
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    while !head.windows(2).any(|w| w == b"\r\n") && head.len() < 8192 {
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    anyhow::ensure!(parts.next() == Some("GET"), "not a GET request");
    Ok(parts.next().unwrap_or("/").to_string())
}

async fn respond(socket: &mut TcpStream, status: &str, title: &str, detail: &str) -> std::io::Result<()> {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>ClipCut</title>\
         <body style=\"font:16px system-ui,sans-serif;max-width:40em;margin:4em auto;padding:0 1em\">\
         <h2>{}</h2><p>{}</p>",
        escape_html(title),
        escape_html(detail)
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Query parameters of a request target or a full URL.
fn query_params(target: &str) -> HashMap<String, String> {
    url::Url::parse("http://127.0.0.1/")
        .and_then(|base| base.join(target))
        .map(|u| u.query_pairs().into_owned().collect())
        .unwrap_or_default()
}

/// Finish a sign-in from the address the browser landed on. Used when the
/// browser runs on another device, where `127.0.0.1` isn't this computer.
pub async fn finish_from_url(state: &AppState, pasted: &str) -> anyhow::Result<AuthStatus> {
    let params = query_params(pasted.trim());
    anyhow::ensure!(
        params.contains_key("state")
            && (params.contains_key("code") || params.contains_key("error")),
        "paste the whole address of the tab Google sent you to \
         (it starts with http://127.0.0.1 and contains code= and state=)"
    );
    let status = finish_connect(state, &params).await?;
    if let Some(listener) = state.publish.listener.lock().unwrap().take() {
        listener.abort();
    }
    Ok(status)
}

async fn finish_connect(
    state: &AppState,
    params: &HashMap<String, String>,
) -> anyhow::Result<AuthStatus> {
    let p = &state.publish;
    let pending = {
        let mut slot = p.pending.lock().unwrap();
        let Some(pending) = slot.as_ref() else {
            anyhow::bail!("no sign-in is waiting; click Connect again");
        };
        let expired = pending.created.elapsed() > SIGN_IN_TTL;
        let matches = params.get("state") == Some(&pending.state);
        if expired {
            *slot = None;
            anyhow::bail!("the sign-in expired; click Connect again");
        }
        // Leave the current sign-in waiting: this is an old tab's redirect.
        anyhow::ensure!(matches, "this sign-in is stale; click Connect again");
        slot.take().unwrap()
    };
    if let Some(error) = params.get("error") {
        if error == "access_denied" {
            anyhow::bail!("the Google sign-in was cancelled");
        }
        anyhow::bail!("the Google sign-in failed: {error}");
    }
    let code = params.get("code").context("Google's redirect had no code")?;
    let client = p.read();
    let tokens = p
        .token_request(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &client.client_id),
            ("client_secret", &client.client_secret),
            ("redirect_uri", &pending.redirect_uri),
            ("code_verifier", &pending.verifier),
        ])
        .await
        .context("could not finish the Google sign-in")?;
    let channel = p.channel(&tokens.access_token).await;
    let _lock = p.file_lock.lock().await;
    let mut auth = p.read();
    anyhow::ensure!(
        auth.client_id == client.client_id,
        "the OAuth client changed during sign-in; click Connect again"
    );
    auth.refresh_token = tokens.refresh_token.or(auth.refresh_token);
    anyhow::ensure!(
        auth.refresh_token.is_some(),
        "Google sent no refresh token; remove ClipCut at myaccount.google.com/permissions \
         and connect again"
    );
    auth.access_token = Some(tokens.access_token);
    auth.expires_at = Some(expiry(tokens.expires_in));
    match channel {
        Ok(Some((id, title))) => {
            auth.channel_id = Some(id);
            auth.channel = Some(title);
        }
        Ok(None) => {
            tracing::warn!("this Google account has no YouTube channel yet");
            auth.channel_id = None;
            auth.channel = None;
        }
        Err(e) => tracing::warn!("could not read the YouTube channel: {e:#}"),
    }
    auth.connected_at = Some(Utc::now());
    p.write(&auth)?;
    drop(_lock);
    Ok(p.status())
}

/// Turn a YouTube API error into advice.
fn api_error(status: StatusCode, body: &str) -> anyhow::Error {
    let v: Value = serde_json::from_str(body).unwrap_or_default();
    let reason = v["error"]["errors"][0]["reason"].as_str().unwrap_or_default();
    let message = v["error"]["message"].as_str().unwrap_or_default();
    let text = match reason {
        "quotaExceeded" | "dailyLimitExceeded" => "YouTube's daily API quota is used up \
            (each upload costs 1,600 of the free 10,000 daily units, about 6 uploads a day). \
            It resets at midnight Pacific time."
            .to_string(),
        "uploadLimitExceeded" => {
            "this channel reached YouTube's upload limit for today; try again tomorrow".into()
        }
        "youtubeSignupRequired" => {
            "this Google account has no YouTube channel yet; create one on youtube.com".into()
        }
        "accessNotConfigured" => "the YouTube Data API v3 isn't enabled in your Google Cloud \
            project; enable it in the API Library and try again"
            .into(),
        "insufficientPermissions" | "forbidden" => "ClipCut isn't allowed to upload to this \
            channel; reconnect YouTube and tick every permission"
            .into(),
        _ if !message.is_empty() => format!("YouTube refused the request ({status}): {message}"),
        _ => format!("YouTube refused the request ({status})"),
    };
    anyhow::anyhow!(text)
}

/// Upload metadata derived from a project.
#[derive(Debug)]
struct Meta {
    title: String,
    body: Value,
    /// The privacy the video ends up with.
    privacy: String,
    publish_at: Option<DateTime<Utc>>,
    warnings: Vec<String>,
}

fn metadata(
    project: &Project,
    defaults: &PublishConfig,
    privacy: &str,
    publish_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Meta {
    let script = project.script.as_ref();
    let raw_title = script
        .map(|s| s.title.as_str())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(&project.topic);
    let mut title: String = strip_brackets(raw_title.trim()).chars().take(100).collect();
    if title.trim().is_empty() {
        title = "ClipCut video".into();
    }

    let mut hashtags: Vec<String> = Vec::new();
    for tag in script.map(|s| s.hashtags.as_slice()).unwrap_or_default() {
        let tag = strip_brackets(tag.trim().trim_start_matches('#')).replace(',', "");
        if !tag.is_empty() && !hashtags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            hashtags.push(tag);
        }
    }
    let mut description = strip_brackets(script.map(|s| s.description.trim()).unwrap_or_default());
    if !hashtags.is_empty() {
        let line: Vec<String> = hashtags
            .iter()
            .map(|t| format!("#{}", t.split_whitespace().collect::<String>()))
            .collect();
        description.push_str(&format!("\n\n{}", line.join(" ")));
    }
    let credits = credits(project, &description);
    if !credits.is_empty() {
        description.push_str(&format!("\n\n{credits}"));
    }
    let description = truncate_bytes(description.trim(), 5000).to_string();

    // YouTube allows 500 characters of tags, counting separators and quotes.
    let mut tags = Vec::new();
    let mut used = 0;
    for tag in &hashtags {
        let cost = tag.chars().count() + if tag.contains(' ') { 3 } else { 1 };
        if used + cost > 400 {
            break;
        }
        used += cost;
        tags.push(tag.clone());
    }

    let mut warnings = Vec::new();
    let (requested, privacy, publish_at) = match publish_at {
        Some(_) if privacy == "private" => {
            warnings.push("Private videos aren't scheduled; ignoring the release time.".into());
            ("private", "private", None)
        }
        Some(at) if at > now + chrono::Duration::seconds(60) => {
            if privacy == "unlisted" {
                warnings.push(
                    "YouTube can't schedule unlisted videos; this one goes public at its release time."
                        .into(),
                );
            }
            ("private", "public", Some(at))
        }
        Some(_) => {
            warnings.push("The release time has passed; publishing now.".into());
            (privacy, privacy, None)
        }
        None => (privacy, privacy, None),
    };
    let mut status = json!({
        "privacyStatus": requested,
        "selfDeclaredMadeForKids": defaults.made_for_kids,
    });
    if let Some(at) = publish_at {
        status["publishAt"] = json!(at.to_rfc3339_opts(SecondsFormat::Secs, true));
    }
    let body = json!({
        "snippet": {
            "title": title,
            "description": description,
            "tags": tags,
            "categoryId": defaults.category_id,
        },
        "status": status,
    });
    Meta {
        title,
        body,
        privacy: privacy.to_string(),
        publish_at,
        warnings,
    }
}

/// Visual and research credits for the description, skipping links it
/// already has.
fn credits(project: &Project, description: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    let mut visuals = Vec::new();
    for asset in project.beats.iter().filter_map(|b| b.asset.as_ref()) {
        let url = asset.source_url.trim();
        if url.is_empty() || description.contains(url) || seen.contains(&url) {
            continue;
        }
        seen.push(url);
        let who = if asset.credit.trim().is_empty() {
            asset.provider.as_str()
        } else {
            asset.credit.trim()
        };
        visuals.push(format!("- {}: {url}", strip_brackets(who)));
    }
    let mut sources = Vec::new();
    for s in project.research.iter().flat_map(|r| &r.sources) {
        let url = s.url.trim();
        if url.is_empty() || description.contains(url) || seen.contains(&url) {
            continue;
        }
        seen.push(url);
        sources.push(format!("- {}: {url}", strip_brackets(s.title.trim())));
    }
    let mut out = String::new();
    if !visuals.is_empty() {
        out.push_str(&format!("Credits\n{}", visuals.join("\n")));
    }
    if !sources.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("Sources\n{}", sources.join("\n")));
    }
    out
}

/// YouTube rejects `<` and `>` in titles and descriptions.
fn strip_brackets(s: &str) -> String {
    s.replace(['<', '>'], "")
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Where to resume after a 308: one past the last byte YouTube has.
fn next_offset(headers: &HeaderMap) -> u64 {
    headers
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.rsplit('-').next())
        .and_then(|n| n.trim().parse::<u64>().ok())
        .map_or(0, |last| last + 1)
}

/// Publish a rendered project to YouTube. `format` picks one of its renders
/// (default: the main one). A future `publish_at` uploads it private and
/// YouTube makes it public then.
pub async fn upload(
    state: &AppState,
    project_id: &str,
    format: Option<Format>,
    privacy: &str,
    publish_at: Option<DateTime<Utc>>,
    progress: &Progress,
) -> anyhow::Result<PublishRecord> {
    validate_privacy(privacy).map_err(anyhow::Error::msg)?;
    let p = &state.publish;
    let _claim = p
        .claim(project_id)
        .context("this project is already uploading")?;
    anyhow::ensure!(
        !state.store.is_running(project_id),
        "the project is still running; publish it after the render finishes"
    );
    let project = state.store.get(project_id)?;
    let render = project
        .render
        .as_ref()
        .context("render the project before publishing it")?;
    let format = format.unwrap_or(project.format);
    let (file, _, _) = render.file_for(project.format, format).with_context(|| {
        format!(
            "this project has no {} render; add that format and render again",
            format.slug()
        )
    })?;
    let dir = state.store.dir(project_id);
    let path = dir.join(file);
    let mut file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("could not open {}", path.display()))?;
    let total = file.metadata().await?.len();
    anyhow::ensure!(total > 0, "{} is empty; render again", path.display());
    let is_short = format != Format::Horizontal && render.duration_s <= SHORTS_MAX_S;

    let defaults = state.config().publish;
    let meta = metadata(&project, &defaults, privacy, publish_at, Utc::now());
    for warning in &meta.warnings {
        progress.warn(warning.clone());
    }
    let endpoints = p.endpoints();
    let mut token = p.access_token(false).await?;
    progress.info(format!(
        "Uploading \"{}\" to YouTube ({:.1} MB)",
        meta.title,
        total as f64 / 1_048_576.0
    ));

    let mut session = None;
    for attempt in 0..2 {
        let response = p
            .http
            .post(format!("{}/videos", endpoints.upload))
            .query(&[("uploadType", "resumable"), ("part", "snippet,status")])
            .bearer_auth(&token)
            .header(CONTENT_TYPE, "application/json; charset=UTF-8")
            .header("X-Upload-Content-Length", total)
            .header("X-Upload-Content-Type", "video/mp4")
            .body(serde_json::to_vec(&meta.body)?)
            .send()
            .await
            .context("could not reach YouTube")?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED && attempt == 0 {
            token = p.access_token(true).await?;
            continue;
        }
        if !status.is_success() {
            return Err(api_error(status, &response.text().await.unwrap_or_default()));
        }
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .context("YouTube didn't return an upload address")?;
        session = Some(location.to_string());
        break;
    }
    let session = session.context("YouTube didn't accept the sign-in; reconnect it")?;

    let video = send_file(p, &endpoints, &session, &mut file, total, &mut token, progress).await?;
    let video_id = video["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("YouTube didn't return the video id")?
        .to_string();
    let requested = meta.body["status"]["privacyStatus"].as_str();
    if let Some(actual) = video["status"]["privacyStatus"].as_str()
        && Some(actual) != requested
        && actual == "private"
    {
        progress.warn(
            "YouTube made this upload private. Uploads from Google Cloud projects that haven't \
             passed YouTube's API compliance audit are locked private; see Troubleshooting → \
             Publishing.",
        );
    }
    let url = if is_short {
        format!("https://www.youtube.com/shorts/{video_id}")
    } else {
        format!("https://www.youtube.com/watch?v={video_id}")
    };

    // Shorts show a frame YouTube picks; the custom thumbnail is for long videos.
    if !is_short
        && let Some(thumb) = &render.thumbnail_jpg
        && let Ok(bytes) = tokio::fs::read(dir.join(thumb)).await
    {
        match set_thumbnail(p, &endpoints, &token, &video_id, bytes).await {
            Ok(()) => progress.info("Thumbnail set"),
            Err(e) => progress.warn(format!("Uploaded, but the thumbnail wasn't set: {e:#}")),
        }
    }

    let record = PublishRecord {
        platform: "youtube".into(),
        format,
        video_id,
        url,
        privacy: meta.privacy,
        publish_at: meta.publish_at,
        uploaded_at: Utc::now(),
    };
    state
        .store
        .update(project_id, |project| {
            project.published.push(record.clone());
            Ok(())
        })
        .await
        .with_context(|| format!("uploaded to {}, but the project wasn't updated", record.url))?;
    match record.publish_at {
        Some(at) => progress.info(format!(
            "Uploaded {}; it goes public {}",
            record.url,
            at.with_timezone(&chrono::Local).format("%a %-d %b %H:%M")
        )),
        None => progress.info(format!("Published {}", record.url)),
    }
    Ok(record)
}

/// Send the file to a resumable session in chunks, resuming where YouTube
/// says it stopped. Returns the created video resource.
async fn send_file(
    p: &Publisher,
    endpoints: &Endpoints,
    session: &str,
    file: &mut tokio::fs::File,
    total: u64,
    token: &mut String,
    progress: &Progress,
) -> anyhow::Result<Value> {
    let mut offset = 0u64;
    let mut failures = 0u32;
    loop {
        let result = if offset < total {
            let end = (offset + endpoints.chunk as u64).min(total);
            let mut data = vec![0u8; (end - offset) as usize];
            file.seek(SeekFrom::Start(offset)).await?;
            file.read_exact(&mut data).await?;
            p.http
                .put(session)
                .bearer_auth(&*token)
                .header(CONTENT_RANGE, format!("bytes {offset}-{}/{total}", end - 1))
                .body(data)
                .send()
                .await
        } else {
            upload_status(p, session, token, total).await
        };
        match result {
            Ok(r) if r.status() == StatusCode::PERMANENT_REDIRECT => {
                offset = next_offset(r.headers());
                failures = 0;
                progress.fraction(
                    offset as f32 / total as f32,
                    format!("Uploading to YouTube: {}%", offset * 100 / total),
                );
                continue;
            }
            Ok(r) if r.status().is_success() => {
                progress.fraction(1.0, "Uploaded to YouTube");
                return r.json().await.context("YouTube's upload reply wasn't understood");
            }
            Ok(r) if r.status() == StatusCode::UNAUTHORIZED => {
                failures += 1;
                anyhow::ensure!(failures <= MAX_RETRIES, "YouTube kept refusing the sign-in");
                *token = p.access_token(true).await?;
                continue;
            }
            Ok(r) if matches!(r.status(), StatusCode::NOT_FOUND | StatusCode::GONE) => {
                anyhow::bail!("the YouTube upload session expired; publish again")
            }
            Ok(r) if r.status().is_server_error() || r.status() == StatusCode::TOO_MANY_REQUESTS => {
                tracing::warn!("YouTube upload: {} at byte {offset}", r.status());
            }
            Ok(r) => {
                let status = r.status();
                return Err(api_error(status, &r.text().await.unwrap_or_default()));
            }
            Err(e) => tracing::warn!("YouTube upload: {e}"),
        }
        failures += 1;
        anyhow::ensure!(
            failures <= MAX_RETRIES,
            "the YouTube upload kept failing; check the connection and publish again"
        );
        let delay = endpoints.backoff * 2u32.pow(failures.min(5));
        progress.warn(format!(
            "YouTube upload interrupted; retrying in {}s",
            delay.as_secs().max(1)
        ));
        tokio::time::sleep(delay).await;
        // Ask how much arrived before resuming.
        match upload_status(p, session, token, total).await {
            Ok(r) if r.status() == StatusCode::PERMANENT_REDIRECT => offset = next_offset(r.headers()),
            Ok(r) if r.status().is_success() => {
                progress.fraction(1.0, "Uploaded to YouTube");
                return r.json().await.context("YouTube's upload reply wasn't understood");
            }
            Ok(r) if matches!(r.status(), StatusCode::NOT_FOUND | StatusCode::GONE) => {
                anyhow::bail!("the YouTube upload session expired; publish again")
            }
            // Resend from the last confirmed offset.
            _ => {}
        }
    }
}

/// The resumable-upload status query.
async fn upload_status(
    p: &Publisher,
    session: &str,
    token: &str,
    total: u64,
) -> reqwest::Result<reqwest::Response> {
    p.http
        .put(session)
        .bearer_auth(token)
        .header(CONTENT_RANGE, format!("bytes */{total}"))
        .header(CONTENT_LENGTH, 0)
        .timeout(Duration::from_secs(60))
        .send()
        .await
}

async fn set_thumbnail(
    p: &Publisher,
    endpoints: &Endpoints,
    token: &str,
    video_id: &str,
    jpeg: Vec<u8>,
) -> anyhow::Result<()> {
    let response = p
        .http
        .post(format!("{}/thumbnails/set", endpoints.upload))
        .query(&[("videoId", video_id)])
        .bearer_auth(token)
        .header(CONTENT_TYPE, "image/jpeg")
        .body(jpeg)
        .send()
        .await?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    if status == StatusCode::FORBIDDEN {
        anyhow::bail!("custom thumbnails need a verified channel (youtube.com/verify)");
    }
    Err(api_error(status, &response.text().await.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::Arc;

    use axum::Json;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post, put};

    use super::*;
    use crate::test_support::{TestHome, finished_project};

    #[derive(Default)]
    struct Mock {
        base: String,
        metadata: Option<Value>,
        authorization: Option<String>,
        total: usize,
        received: Vec<u8>,
        chunks: usize,
        failed_once: bool,
        status_queries: usize,
        thumbnails: usize,
    }

    type Shared = Arc<Mutex<Mock>>;

    fn received(len: usize) -> Response {
        if len == 0 {
            StatusCode::PERMANENT_REDIRECT.into_response()
        } else {
            let range = format!("bytes=0-{}", len - 1);
            (StatusCode::PERMANENT_REDIRECT, [(header::RANGE, range)]).into_response()
        }
    }

    async fn start(State(m): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
        let mut m = m.lock().unwrap();
        m.authorization = headers
            .get(header::AUTHORIZATION)
            .map(|v| v.to_str().unwrap().to_string());
        m.total = headers["x-upload-content-length"].to_str().unwrap().parse().unwrap();
        m.metadata = serde_json::from_slice(&body).ok();
        let location = format!("{}/session", m.base);
        (StatusCode::OK, [(header::LOCATION, location)]).into_response()
    }

    async fn chunk(State(m): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
        let mut m = m.lock().unwrap();
        let range = headers[header::CONTENT_RANGE].to_str().unwrap().to_string();
        if range.starts_with("bytes */") {
            m.status_queries += 1;
            return received(m.received.len());
        }
        m.chunks += 1;
        if m.chunks == 2 && !m.failed_once {
            m.failed_once = true;
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let start: usize = range["bytes ".len()..]
            .split('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        if start != m.received.len() {
            return StatusCode::BAD_REQUEST.into_response();
        }
        m.received.extend_from_slice(&body);
        if m.received.len() == m.total {
            Json(json!({"id": "vid123", "status": {"privacyStatus": "unlisted"}})).into_response()
        } else {
            received(m.received.len())
        }
    }

    async fn thumbnail(State(m): State<Shared>) -> StatusCode {
        m.lock().unwrap().thumbnails += 1;
        StatusCode::OK
    }

    async fn mock_google() -> (Endpoints, Shared) {
        let ip = crate::net::lan_ip().unwrap_or(IpAddr::from([127, 0, 0, 1]));
        let listener = tokio::net::TcpListener::bind((ip, 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mock: Shared = Arc::new(Mutex::new(Mock {
            base: base.clone(),
            ..Default::default()
        }));
        let app = axum::Router::new()
            .route(
                "/token",
                post(|| async {
                    Json(json!({"access_token": "tok", "refresh_token": "r1", "expires_in": 3600}))
                }),
            )
            .route(
                "/api/channels",
                get(|| async {
                    Json(json!({"items": [{"id": "UC1", "snippet": {"title": "Mock channel"}}]}))
                }),
            )
            .route("/upload/videos", post(start))
            .route("/session", put(chunk))
            .route("/upload/thumbnails/set", post(thumbnail))
            .with_state(mock.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoints = Endpoints {
            auth: format!("{base}/auth"),
            token: format!("{base}/token"),
            revoke: format!("{base}/revoke"),
            api: format!("{base}/api"),
            upload: format!("{base}/upload"),
            chunk: 256 * 1024,
            backoff: Duration::from_millis(1),
        };
        (endpoints, mock)
    }

    #[tokio::test]
    async fn uploads_resumably_and_records_the_short() {
        let home = TestHome::new();
        let state = home.state();
        let (endpoints, mock) = mock_google().await;
        *state.publish.endpoints.write().unwrap() = endpoints;
        state
            .publish
            .write(&Auth {
                client_id: "cid".into(),
                client_secret: "secret".into(),
                refresh_token: Some("refresh".into()),
                ..Default::default()
            })
            .unwrap();
        let mut config = state.config();
        config.publish.category_id = "27".into();
        config.publish.made_for_kids = true;
        state.set_config(config).unwrap();
        let mut project = finished_project();
        project.script.as_mut().unwrap().hashtags =
            vec!["#shorts".into(), "#Fixture".into(), "fixture".into()];
        let project = state.store.create(project).await.unwrap();
        let video: Vec<u8> = (0..600 * 1024).map(|i| (i % 251) as u8).collect();
        let render = state.store.dir(&project.id).join("render");
        std::fs::create_dir_all(&render).unwrap();
        std::fs::write(render.join("final.mp4"), &video).unwrap();

        let progress = Progress::log_only(project.id.as_str());
        let record = upload(&state, &project.id, None, "unlisted", None, &progress)
            .await
            .unwrap();
        assert_eq!(record.url, "https://www.youtube.com/shorts/vid123");
        assert_eq!(record.privacy, "unlisted");
        {
            let m = mock.lock().unwrap();
            assert_eq!(m.received, video, "every byte arrives once, in order");
            assert!(m.failed_once && m.status_queries >= 1, "the 503 was resumed");
            assert_eq!(m.thumbnails, 0, "Shorts keep YouTube's own thumbnail");
            assert_eq!(m.authorization.as_deref(), Some("Bearer tok"));
            let meta = m.metadata.as_ref().unwrap();
            assert_eq!(meta["snippet"]["title"], "A fixture topic");
            assert_eq!(meta["snippet"]["tags"], json!(["shorts", "Fixture"]));
            assert_eq!(meta["status"]["privacyStatus"], "unlisted");
            assert!(meta["status"].get("publishAt").is_none());
            assert_eq!(meta["snippet"]["categoryId"], "27", "from [publish]");
            assert_eq!(meta["status"]["selfDeclaredMadeForKids"], true);
        }
        let saved = state.store.get(&project.id).unwrap();
        assert_eq!(saved.published.len(), 1);
        assert_eq!(saved.published[0].video_id, "vid123");
        assert!(state.publish.read().access_token.is_some(), "the refreshed token is kept");
        assert!(!state.publish.is_uploading(&project.id));

        let missing = upload(&state, &project.id, Some(Format::Square), "public", None, &progress)
            .await
            .unwrap_err();
        assert!(format!("{missing:#}").contains("no square render"));
    }

    #[tokio::test]
    async fn sign_in_through_the_loopback_redirect() {
        let home = TestHome::new();
        let state = home.state();
        let (endpoints, _mock) = mock_google().await;
        *state.publish.endpoints.write().unwrap() = endpoints;
        assert!(begin_connect(&state).await.is_err(), "needs a client first");
        let client = parse_client(&ClientBody {
            client_json: Some(
                r#"{"installed":{"client_id":"cid.apps.googleusercontent.com","client_secret":"sec"}}"#
                    .into(),
            ),
            ..Default::default()
        })
        .unwrap();
        state.publish.set_client(client.0, client.1).await.unwrap();

        let (url, listener) = begin_connect(&state).await.unwrap();
        let params: HashMap<String, String> =
            url::Url::parse(&url).unwrap().query_pairs().into_owned().collect();
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["access_type"], "offline");
        assert_eq!(params["client_id"], "cid.apps.googleusercontent.com");
        let redirect = params["redirect_uri"].clone();
        assert!(redirect.starts_with("http://127.0.0.1:"));
        assert!(state.publish.status().connecting);

        let stale = finish_from_url(&state, "http://127.0.0.1:9/?state=old&code=abc").await;
        assert!(format!("{:#}", stale.unwrap_err()).contains("stale"));
        assert!(state.publish.status().connecting, "a stale redirect leaves the sign-in waiting");
        assert!(finish_from_url(&state, "not a url").await.is_err());

        let server_state = state.clone();
        let server = tokio::spawn(async move { serve_callback(&server_state, listener).await });
        let http = reqwest::Client::new();
        let favicon = http.get(format!("{redirect}/favicon.ico")).send().await.unwrap();
        assert_eq!(favicon.status(), StatusCode::NOT_FOUND);
        let page = http
            .get(format!("{redirect}/?state={}&code=good", params["state"]))
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        assert!(page.text().await.unwrap().contains("Mock channel"));
        let status = server.await.unwrap().unwrap();
        assert!(status.connected && !status.connecting);
        assert_eq!(status.channel.as_deref(), Some("Mock channel"));
        assert_eq!(state.publish.read().refresh_token.as_deref(), Some("r1"));
    }

    #[test]
    fn pkce_matches_the_rfc_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn client_json_must_be_a_desktop_client() {
        let web = ClientBody {
            client_json: Some(r#"{"web":{"client_id":"a","client_secret":"b"}}"#.into()),
            ..Default::default()
        };
        assert!(parse_client(&web).unwrap_err().contains("Desktop app"));
        let typed = ClientBody {
            client_id: " id ".into(),
            client_secret: "secret".into(),
            client_json: None,
        };
        assert_eq!(parse_client(&typed).unwrap(), ("id".into(), "secret".into()));
        assert!(parse_client(&ClientBody::default()).is_err());
    }

    #[test]
    fn scheduling_uploads_private_until_release() {
        let mut project = finished_project();
        project.script.as_mut().unwrap().title = "A <b>title</b>".into();
        let now = Utc::now();
        let later = now + chrono::Duration::hours(3);
        let defaults = PublishConfig::default();

        let meta = metadata(&project, &defaults, "unlisted", Some(later), now);
        assert_eq!(meta.body["snippet"]["title"], "A btitle/b");
        assert_eq!(meta.body["snippet"]["categoryId"], "22");
        assert_eq!(meta.body["status"]["selfDeclaredMadeForKids"], false);
        assert_eq!(meta.body["status"]["privacyStatus"], "private");
        assert_eq!(
            meta.body["status"]["publishAt"],
            later.to_rfc3339_opts(SecondsFormat::Secs, true)
        );
        assert_eq!(meta.privacy, "public");
        assert_eq!(meta.publish_at, Some(later));
        assert_eq!(meta.warnings.len(), 1);

        let hour_ago = now - chrono::Duration::hours(1);
        let past = metadata(&project, &defaults, "public", Some(hour_ago), now);
        assert_eq!(past.body["status"]["privacyStatus"], "public");
        assert!(past.publish_at.is_none() && past.body["status"].get("publishAt").is_none());

        let private = metadata(&project, &defaults, "private", Some(later), now);
        assert_eq!(private.body["status"]["privacyStatus"], "private");
        assert!(private.publish_at.is_none());
    }

    #[test]
    fn resume_offset_comes_from_the_range_header() {
        let mut headers = HeaderMap::new();
        assert_eq!(next_offset(&headers), 0);
        headers.insert(RANGE, "bytes=0-524287".parse().unwrap());
        assert_eq!(next_offset(&headers), 524_288);
    }

    #[test]
    fn descriptions_stay_within_youtube_limits() {
        let mut project = finished_project();
        project.script.as_mut().unwrap().description = "é".repeat(4000);
        let defaults = PublishConfig::default();
        let meta = metadata(&project, &defaults, "public", None, Utc::now());
        let description = meta.body["snippet"]["description"].as_str().unwrap();
        assert!(description.len() <= 5000);
    }
}
