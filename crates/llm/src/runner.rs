//! Output-only CLI boundary. Claude runs in a fresh isolated directory with
//! safe mode, plan permissions, no tools/skills/hooks/MCP, and stdin prompts.
//! Unsupported flags cause failure, never a retry with weaker isolation.
//!
//! Codex's read-only exec sandbox does NOT disable its tools. Until its CLI
//! exposes a verified deny-all tool policy, generation fails closed and the
//! caller may fall back to Claude. `codex_args` is retained for compatibility,
//! not as an authorization to execute that command.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use clipcut_core::Progress;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::prompts;

/// Which CLI ran a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude Code",
            Provider::Codex => "Codex",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmErrorKind {
    /// Neither CLI is installed.
    NotInstalled,
    /// Login missing or expired.
    Auth,
    /// Rate limit / usage cap.
    RateLimit,
    Timeout,
    /// No or malformed structured output (after the retry).
    InvalidOutput,
    /// Well-formed but useless output (e.g. web search returned nothing).
    Unusable,
    /// Anything else (crash, non-zero exit).
    Failed,
}

/// A failed agent call with a user-facing message.
#[derive(Debug, Clone)]
pub struct LlmError {
    pub provider: Option<Provider>,
    pub kind: LlmErrorKind,
    pub message: String,
}

impl LlmError {
    pub(crate) fn new(
        provider: Option<Provider>,
        kind: LlmErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.provider {
            Some(p) => write!(f, "{p}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for LlmError {}

/// Legacy stage hints. ALL variants now run without tools.
#[derive(Debug, Clone)]
pub enum Tooling {
    /// Pure text-in, JSON-out.
    None,
    /// Legacy research hint; evidence must be supplied in the prompt.
    Web,
    /// Legacy hint; skill text must be loaded by the app, not the agent.
    Unslop { skill_dir: PathBuf },
}

/// One structured agent call.
#[derive(Debug, Clone)]
pub struct Call {
    /// Heartbeat label, e.g. "Researching".
    pub label: String,
    pub prompt: String,
    /// Replacement prompt for Codex (defaults to `prompt`).
    pub codex_prompt: Option<String>,
    pub schema: Value,
    /// Model alias from config (Claude names like "opus"/"sonnet" are mapped per CLI).
    pub model: String,
    /// Small pick: Claude falls back to "sonnet", Codex runs at low effort.
    pub fast: bool,
    pub tooling: Tooling,
    pub system: Option<&'static str>,
    pub max_turns: u32,
    pub timeout: Duration,
    /// Legacy file staging is rejected. Embed context in `prompt` instead.
    pub files: Vec<(String, String)>,
}

/// Why a parsed answer was rejected.
#[derive(Debug, Clone)]
pub enum Rejection {
    /// Fixable by asking again ("your previous output was invalid: ...").
    Invalid(String),
    /// Asking the same agent again won't help (e.g. its web search is broken).
    Unusable(String),
}

/// Claude Code aliases and ids; anything else is assumed to be a Codex model.
pub fn is_claude_model(model: &str) -> bool {
    let m = model.trim().to_lowercase();
    [
        "opus", "sonnet", "haiku", "fable", "default", "opusplan", "best",
    ]
    .contains(&m.as_str())
        || m.starts_with("claude")
        || ["opus", "sonnet", "haiku", "fable"]
            .iter()
            .any(|k| m.contains(k))
}

pub fn claude_model(call: &Call) -> String {
    if is_claude_model(&call.model) {
        call.model.clone()
    } else if call.fast {
        "sonnet".into()
    } else {
        "opus".into()
    }
}

pub fn codex_model(call: &Call) -> Option<String> {
    let m = call.model.trim();
    (!m.is_empty() && !is_claude_model(m)).then(|| m.to_string())
}

pub const EMPTY_MCP: &str = r#"{"mcpServers":{}}"#;

pub fn claude_args(call: &Call, _prompt: &str) -> Vec<String> {
    let system = match call.system {
        Some(role) => format!("{}\n\n{role}", prompts::SYSTEM_SAFE),
        None => prompts::SYSTEM_SAFE.to_string(),
    };
    vec![
        "-p".into(),
        "--safe-mode".into(),
        "--model".into(),
        claude_model(call),
        "--output-format".into(),
        "json".into(),
        "--json-schema".into(),
        call.schema.to_string(),
        "--no-session-persistence".into(),
        "--permission-mode".into(),
        "plan".into(),
        "--permission-prompts".into(),
        "none".into(),
        "--tools".into(),
        String::new(),
        "--disallowedTools".into(),
        "mcp__*".into(),
        "--disable-slash-commands".into(),
        "--no-chrome".into(),
        "--settings".into(),
        r#"{"disableAllHooks":true,"autoMemoryEnabled":false}"#.into(),
        // Keep transport/auth environment settings, while safe-mode disables
        // customizations and explicit flags disable tools and MCP.
        "--setting-sources".into(),
        "user".into(),
        "--strict-mcp-config".into(),
        "--mcp-config".into(),
        EMPTY_MCP.into(),
        "--max-turns".into(),
        call.max_turns.clamp(1, 6).to_string(),
        "--system-prompt".into(),
        system,
    ]
}

pub fn codex_args(call: &Call, workdir: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "exec".into(),
        "--sandbox".into(),
        "read-only".into(),
        "--ephemeral".into(),
        "--ignore-user-config".into(),
        "--ignore-rules".into(),
        "--color".into(),
        "never".into(),
        "--skip-git-repo-check".into(),
        "-C".into(),
        workdir.display().to_string(),
        "--output-schema".into(),
        workdir.join("schema.json").display().to_string(),
        "-o".into(),
        workdir.join("last-message.txt").display().to_string(),
    ];
    if let Some(model) = codex_model(call) {
        args.extend(["-m".into(), model]);
    }
    args.extend(["-c".into(), "web_search=\"disabled\"".into()]);
    if call.fast {
        args.extend(["-c".into(), "model_reasoning_effort=\"low\"".into()]);
    }
    args.push("-".into());
    args
}

/// Accept one complete JSON object, or one complete fenced JSON object. Never
/// fish objects out of logs, prose, multiple answers, or malformed nesting.
pub fn extract_json(text: &str) -> Option<Value> {
    let mut body = text.trim();
    if body.starts_with("```") {
        let (header, rest) = body.split_once('\n')?;
        if !matches!(header.trim(), "```" | "```json") {
            return None;
        }
        body = rest.strip_suffix("```")?.trim();
    }
    serde_json::from_str::<Value>(body)
        .ok()
        .filter(Value::is_object)
}

/// Map an error text from either CLI to a kind.
pub fn classify(text: &str) -> LlmErrorKind {
    let t = text.to_lowercase();
    let any = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if any(&[
        "not logged in",
        "please run /login",
        "/login",
        "invalid api key",
        "authentication_error",
        "authentication failed",
        "unauthorized",
        "oauth token has expired",
        "token has expired",
        "token expired",
        "codex login",
        "401",
        "credit balance is too low",
    ]) {
        LlmErrorKind::Auth
    } else if any(&[
        "rate limit",
        "rate_limit",
        "429",
        "usage limit",
        "too many requests",
        "quota",
        "limit reached",
        "hit your limit",
        "limit will reset",
    ]) {
        LlmErrorKind::RateLimit
    } else {
        LlmErrorKind::Failed
    }
}

fn friendly(provider: Provider, kind: LlmErrorKind, detail: &str) -> String {
    let detail = tail(detail, 400);
    match (kind, provider) {
        (LlmErrorKind::Auth, Provider::Claude) => {
            format!(
                "not signed in or the login expired. Run `claude` in a terminal and use /login. ({detail})"
            )
        }
        (LlmErrorKind::Auth, Provider::Codex) => {
            format!(
                "not signed in or the login expired. Run `codex login` in a terminal. ({detail})"
            )
        }
        (LlmErrorKind::RateLimit, _) => {
            format!("rate limit or usage cap reached; try again later. ({detail})")
        }
        _ => detail,
    }
}

fn tail(text: &str, max: usize) -> String {
    let text = text.trim();
    let n = text.chars().count();
    if n <= max {
        text.to_string()
    } else {
        format!("…{}", text.chars().skip(n - max).collect::<String>())
    }
}

/// Parse Claude Code's `--output-format json` envelope.
pub fn parse_claude_envelope(stdout: &str) -> Result<Value, LlmError> {
    let p = Some(Provider::Claude);
    let envelope = serde_json::from_str::<Value>(stdout.trim())
        .ok()
        .filter(|v| v.get("type").is_some_and(|t| t == "result"))
        .or_else(|| {
            // CLI warnings may precede a JSONL final result. Never skip a later
            // malformed/log/error record to accept an earlier stale success.
            let final_json_line = stdout
                .lines()
                .rev()
                .find(|l| l.trim_start().starts_with('{'))?;
            serde_json::from_str::<Value>(final_json_line)
                .ok()
                .filter(|v| v.get("type").is_some_and(|t| t == "result"))
        });
    let Some(env) = envelope else {
        let kind = match classify(stdout) {
            LlmErrorKind::Failed => LlmErrorKind::InvalidOutput,
            k => k,
        };
        return Err(LlmError::new(
            p,
            kind,
            format!("unexpected output: {}", tail(stdout, 300)),
        ));
    };
    let subtype = env.get("subtype").and_then(Value::as_str).unwrap_or("");
    let result = env.get("result").and_then(Value::as_str).unwrap_or("");
    let is_error = env
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_error || subtype.starts_with("error") {
        let detail = format!(
            "{subtype}: {result} {}",
            env.get("errors").unwrap_or(&Value::Null)
        );
        let kind = match subtype {
            "error_max_structured_output_retries" | "error_max_turns" => {
                LlmErrorKind::InvalidOutput
            }
            _ => classify(&format!(
                "{detail} {}",
                env.get("api_error_status")
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            )),
        };
        let message = match subtype {
            "error_max_turns" => {
                "ran out of turns before returning the structured answer".to_string()
            }
            "error_max_structured_output_retries" => {
                "could not produce output matching the schema".to_string()
            }
            _ => friendly(Provider::Claude, kind, &detail),
        };
        return Err(LlmError::new(p, kind, message));
    }
    if subtype != "success" || env.get("is_error") != Some(&Value::Bool(false)) {
        return Err(LlmError::new(
            p,
            LlmErrorKind::InvalidOutput,
            "missing successful final result envelope",
        ));
    }
    if let Some(structured) = env.get("structured_output").filter(|v| !v.is_null()) {
        return if structured.is_object() {
            Ok(structured.clone())
        } else {
            Err(LlmError::new(
                p,
                LlmErrorKind::InvalidOutput,
                "structured output is not an object",
            ))
        };
    }
    extract_json(result).ok_or_else(|| {
        LlmError::new(
            p,
            LlmErrorKind::InvalidOutput,
            format!("no structured output; reply was: {}", tail(result, 300)),
        )
    })
}

/// Temp working dir, removed on drop.
pub(crate) struct WorkDir(pub(crate) PathBuf);

impl WorkDir {
    pub(crate) fn create() -> std::io::Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("clipcut-llm-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Env vars of an enclosing Claude Code session that must not leak into ours.
const SESSION_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_PID",
    "AI_AGENT",
];

pub(crate) struct Finished {
    pub(crate) success: bool,
    pub(crate) exit_code: Option<i32>,
    code: String,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 1024 * 1024;

async fn capture(mut pipe: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    (&mut pipe)
        .take((MAX_CAPTURE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_CAPTURE_BYTES {
        return Err(std::io::Error::other(
            "process output exceeded the capture limit",
        ));
    }
    String::from_utf8(bytes).map_err(|_| std::io::Error::other("process output was not UTF-8"))
}

/// Each spawned process gets its own Unix process group. Kill descendants too
/// on timeout, cancellation, early I/O failure, or successful parent exit.
struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.filter(|p| *p > 0 && *p <= i32::MAX as u32) {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // SAFETY: process_group(0) gives this child a new group with its PID;
            // a strictly positive checked PID cannot address our own group.
            unsafe {
                kill(-(pid as i32), 9);
            }
        }
    }
}

/// Covers stdin, exit, and BOTH output streams with one deadline. No detached
/// I/O tasks survive an error or cancellation; capture sizes are bounded.
pub(crate) async fn spawn(
    provider: Provider,
    mut cmd: Command,
    stdin: Option<String>,
    timeout: Duration,
    label: &str,
    progress: Option<&Progress>,
) -> Result<Finished, LlmError> {
    let p = Some(provider);
    if stdin.as_ref().is_some_and(|s| s.len() > MAX_PROMPT_BYTES) {
        return Err(LlmError::new(
            p,
            LlmErrorKind::Failed,
            "input exceeded the prompt limit",
        ));
    }
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| LlmError::new(p, LlmErrorKind::Failed, format!("failed to start: {e}")))?;
    let _group = ProcessGroup(child.id());
    let input = child.stdin.take();
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let feed = async move {
        if let (Some(data), Some(mut pipe)) = (stdin, input) {
            pipe.write_all(data.as_bytes()).await?;
            pipe.shutdown().await?;
        }
        Ok::<_, std::io::Error>(())
    };
    let started = Instant::now();
    let result = {
        let operation = async {
            let (_, status, stdout, stderr) =
                tokio::try_join!(feed, child.wait(), capture(out), capture(err))?;
            Ok::<_, std::io::Error>((status, stdout, stderr))
        };
        tokio::pin!(operation);
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut beat = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(15),
            Duration::from_secs(15),
        );
        loop {
            tokio::select! {
                result = &mut operation => break Some(result),
                _ = &mut deadline => break None,
                _ = beat.tick() => if let Some(progress) = progress {
                    progress.info(format!("{label}… {} s ({provider})", started.elapsed().as_secs()));
                },
            }
        }
    };
    let Some(result) = result else {
        let _ = child.kill().await;
        return Err(LlmError::new(
            p,
            LlmErrorKind::Timeout,
            format!("timed out after {:.1} s", timeout.as_secs_f32()),
        ));
    };
    let (status, stdout, stderr) = match result {
        Ok(result) => result,
        Err(e) => {
            let _ = child.kill().await;
            return Err(LlmError::new(
                p,
                LlmErrorKind::Failed,
                format!("process I/O failed: {e}"),
            ));
        }
    };
    tracing::debug!(%provider, label, secs = started.elapsed().as_secs_f32(), "process finished");
    Ok(Finished {
        success: status.success(),
        exit_code: status.code(),
        code: status.to_string(),
        stdout,
        stderr,
    })
}

/// Run one call once. Safety controls are never removed on a retry.
pub async fn invoke(
    provider: Provider,
    program: &Path,
    call: &Call,
    prompt: &str,
    progress: Option<&Progress>,
) -> Result<Value, LlmError> {
    let p = Some(provider);
    if provider == Provider::Codex {
        return Err(LlmError::new(
            p,
            LlmErrorKind::Unusable,
            "generation disabled: Codex exec read-only still exposes tools, and no supported CLI deny-all tool policy is available; use Claude Code for output-only generation",
        ));
    }
    if !call.files.is_empty() {
        return Err(LlmError::new(
            p,
            LlmErrorKind::Unusable,
            "file staging is disabled; supply all evidence and skill text in the prompt",
        ));
    }
    let workdir = WorkDir::create()
        .map_err(|e| LlmError::new(p, LlmErrorKind::Failed, format!("temp dir: {e}")))?;
    let mut cmd = Command::new(program);
    cmd.current_dir(&workdir.0).env("NO_COLOR", "1");
    for key in SESSION_ENV {
        cmd.env_remove(key);
    }
    cmd.args(claude_args(call, prompt))
        .env("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    let done = spawn(
        provider,
        cmd,
        Some(prompt.to_string()),
        call.timeout,
        &call.label,
        progress,
    )
    .await?;
    let parsed = parse_claude_envelope(&done.stdout);
    if !done.success {
        let detail = format!("{}\n{}", done.stderr, done.stdout);
        let mut kind = classify(&detail);
        if let Err(error) = &parsed
            && kind == LlmErrorKind::Failed
            && error.kind != LlmErrorKind::InvalidOutput
        {
            kind = error.kind;
        }
        return Err(LlmError::new(
            p,
            kind,
            friendly(
                provider,
                kind,
                &format!("exited with {}: {}", done.code, tail(&detail, 600)),
            ),
        ));
    }
    match parsed {
        Err(error) if error.kind == LlmErrorKind::InvalidOutput => {
            let kind = classify(&done.stderr);
            if matches!(kind, LlmErrorKind::Auth | LlmErrorKind::RateLimit) {
                Err(LlmError::new(
                    p,
                    kind,
                    friendly(provider, kind, &done.stderr),
                ))
            } else {
                Err(error)
            }
        }
        result => result,
    }
}

pub fn nudge(prompt: &str, reason: &str) -> String {
    format!(
        "{prompt}{}",
        prompts::render(
            prompts::RETRY_NUDGE,
            &[(
                "reason",
                &serde_json::to_string(&tail(reason, 1200)).expect("string JSON")
            )]
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(tooling: Tooling, model: &str, fast: bool) -> Call {
        Call {
            label: "Testing".into(),
            prompt: "hello".into(),
            codex_prompt: None,
            schema: json!({"type": "object"}),
            model: model.into(),
            fast,
            tooling,
            system: Some("SYS"),
            max_turns: 5,
            timeout: Duration::from_secs(10),
            files: vec![],
        }
    }

    fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    }

    #[test]
    fn every_legacy_stage_is_tool_free() {
        for tooling in [
            Tooling::None,
            Tooling::Web,
            Tooling::Unslop {
                skill_dir: "/s/unslop".into(),
            },
        ] {
            let mut c = call(tooling, "opus", false);
            c.system = None;
            let args = claude_args(&c, "PROMPT");
            assert_eq!(args[0], "-p");
            assert!(!args.iter().any(|arg| arg == "PROMPT"));
            assert_eq!(flag(&args, "--tools"), Some(""));
            assert_eq!(flag(&args, "--disallowedTools"), Some("mcp__*"));
            assert_eq!(flag(&args, "--permission-mode"), Some("plan"));
            assert_eq!(flag(&args, "--mcp-config"), Some(EMPTY_MCP));
            assert!(
                flag(&args, "--system-prompt")
                    .unwrap()
                    .contains(prompts::SYSTEM_SAFE)
            );
            for flag in [
                "--safe-mode",
                "--disable-slash-commands",
                "--strict-mcp-config",
                "--no-session-persistence",
                "--no-chrome",
            ] {
                assert!(args.iter().any(|arg| arg == flag));
            }
            assert!(flag(&args, "--allowedTools").is_none());
            assert!(flag(&args, "--add-dir").is_none());
        }
    }

    #[test]
    fn model_mapping_per_cli() {
        let c = call(Tooling::None, "gpt-5.5", true);
        assert_eq!(claude_model(&c), "sonnet");
        assert_eq!(codex_model(&c).as_deref(), Some("gpt-5.5"));
        let c = call(Tooling::None, "opus", false);
        assert_eq!(claude_model(&c), "opus");
        assert_eq!(codex_model(&c), None);
        assert!(is_claude_model("claude-sonnet-4-6"));
        assert!(!is_claude_model("gpt-5.6-sol"));
    }

    #[test]
    fn codex_args_shape() {
        let args = codex_args(&call(Tooling::Web, "gpt-5.5", false), Path::new("/tmp/w"));
        assert_eq!(args[0], "exec");
        assert_eq!(flag(&args, "--sandbox"), Some("read-only"));
        assert_eq!(flag(&args, "-C"), Some("/tmp/w"));
        assert_eq!(flag(&args, "--output-schema"), Some("/tmp/w/schema.json"));
        assert_eq!(flag(&args, "-o"), Some("/tmp/w/last-message.txt"));
        assert_eq!(flag(&args, "-m"), Some("gpt-5.5"));
        assert!(args.contains(&"web_search=\"disabled\"".to_string()));
        assert!(args.contains(&"--skip-git-repo-check".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("-"));
        let args = codex_args(&call(Tooling::None, "opus", true), Path::new("/tmp/w"));
        assert!(flag(&args, "-m").is_none());
        assert!(args.contains(&"web_search=\"disabled\"".to_string()));
        assert!(args.contains(&"model_reasoning_effort=\"low\"".to_string()));
    }

    #[test]
    fn parses_success_envelope() {
        let stdout = r#"{"type":"result","subtype":"success","is_error":false,"num_turns":2,"result":"{\"text\":\"hi\"}","structured_output":{"text":"hi"},"duration_ms":1200}"#;
        assert_eq!(
            parse_claude_envelope(stdout).unwrap(),
            json!({"text": "hi"})
        );
    }

    #[test]
    fn falls_back_to_json_in_result_text() {
        let stdout = r#"{"type":"result","subtype":"success","is_error":false,"result":"```json\n{\"text\": \"a {b} c\"}\n```"}"#;
        assert_eq!(
            parse_claude_envelope(stdout).unwrap(),
            json!({"text": "a {b} c"})
        );
        let stdout =
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Sorry, I can't."}"#;
        assert_eq!(
            parse_claude_envelope(stdout).unwrap_err().kind,
            LlmErrorKind::InvalidOutput
        );
    }

    #[test]
    fn classifies_envelope_errors() {
        let auth = r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}"#;
        assert_eq!(
            parse_claude_envelope(auth).unwrap_err().kind,
            LlmErrorKind::Auth
        );
        let rate = r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached|1759999999"}"#;
        assert_eq!(
            parse_claude_envelope(rate).unwrap_err().kind,
            LlmErrorKind::RateLimit
        );
        let turns =
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":6}"#;
        assert_eq!(
            parse_claude_envelope(turns).unwrap_err().kind,
            LlmErrorKind::InvalidOutput
        );
        let other = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"boom"}"#;
        assert_eq!(
            parse_claude_envelope(other).unwrap_err().kind,
            LlmErrorKind::Failed
        );
    }

    #[test]
    fn finds_result_line_in_noisy_stdout() {
        let stdout = "warning: something\n{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"structured_output\":{\"index\":2}}\n";
        assert_eq!(parse_claude_envelope(stdout).unwrap(), json!({"index": 2}));
        assert_eq!(
            parse_claude_envelope("").unwrap_err().kind,
            LlmErrorKind::InvalidOutput
        );
        for trailing in [
            r#"{"type":"result","subtype":"error"#,
            r#"{"type":"assistant","content":"not a final result"}"#,
        ] {
            assert_eq!(
                parse_claude_envelope(&format!("{stdout}{trailing}\n"))
                    .unwrap_err()
                    .kind,
                LlmErrorKind::InvalidOutput
            );
        }
    }

    #[test]
    fn extracts_json_from_text() {
        assert_eq!(extract_json("{\"a\": \"}\"}"), Some(json!({"a": "}"})));
        assert_eq!(extract_json("noise {\"a\": \"}\"} tail"), None);
        assert_eq!(extract_json("{bad} then {\"ok\": 1}"), None);
        assert_eq!(extract_json("none"), None);
    }
}
