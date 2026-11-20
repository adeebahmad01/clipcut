//! Bug report generation and secret redaction.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Local;
use clipcut_core::keys::KNOWN_SERVICES;
use regex::Regex;

use crate::doctor;
use crate::logging::read_recent_lines;
use crate::state::AppState;

/// Redact sensitive information (tokens, keys, secrets, cookies, emails) from text.
#[allow(dead_code)]
pub fn redact(text: &str) -> String {
    redact_with_secrets(text, &[])
}

/// Redact sensitive information including an explicit slice of known secret strings.
pub fn redact_with_secrets(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();

    // 1. Explicitly replace known secret values first
    for secret in secrets {
        let trimmed = secret.trim();
        if trimmed.len() >= 4 {
            out = out.replace(trimmed, "[REDACTED]");
        }
    }

    // 2. Email addresses
    let email_re = Regex::new(r"[a-zA-Z0-9_.+-]+@[a-zA-Z0-9-]+\.[a-zA-Z0-9-.]+").unwrap();
    out = email_re.replace_all(&out, "[REDACTED EMAIL]").to_string();

    // 3. Bearer authentication tokens
    let bearer_re = Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9_\-\.~+/]+=*").unwrap();
    out = bearer_re.replace_all(&out, "Bearer [REDACTED]").to_string();

    // 4. Authorization and Cookie headers
    let auth_hdr_re = Regex::new(r"(?i)(authorization\s*:\s*)[^\r\n]+").unwrap();
    out = auth_hdr_re.replace_all(&out, "$1[REDACTED]").to_string();

    let cookie_hdr_re = Regex::new(r"(?i)(cookie\s*:\s*)[^\r\n]+").unwrap();
    out = cookie_hdr_re
        .replace_all(&out, "$1[REDACTED COOKIE]")
        .to_string();

    // 5. Google OAuth access and refresh tokens
    let ya29_re = Regex::new(r"ya29\.[A-Za-z0-9_\-]+").unwrap();
    out = ya29_re.replace_all(&out, "[REDACTED TOKEN]").to_string();

    let google_refresh_re = Regex::new(r"1//[A-Za-z0-9_\-]+").unwrap();
    out = google_refresh_re
        .replace_all(&out, "[REDACTED TOKEN]")
        .to_string();

    // 6. JSON fields containing secret values
    let json_secret_re = Regex::new(
        r#"(?i)("(?:client_secret|refresh_token|access_token|api_key|secret|token|password|auth)"\s*:\s*")[^"]+(")"#,
    )
    .unwrap();
    out = json_secret_re
        .replace_all(&out, "${1}[REDACTED]${2}")
        .to_string();

    // 7. TOML fields containing secret values
    let toml_secret_re =
        Regex::new(r#"(?i)((?:secret|token|password|api_key|client_secret)\s*=\s*")[^"]+(")"#)
            .unwrap();
    out = toml_secret_re
        .replace_all(&out, "${1}[REDACTED]${2}")
        .to_string();

    // 8. URL query parameters containing keys/tokens
    let url_param_re = Regex::new(
        r"(?i)([?&](?:key|token|api_key|access_token|refresh_token|client_secret)=)[^&\s]+",
    )
    .unwrap();
    out = url_param_re.replace_all(&out, "${1}[REDACTED]").to_string();

    // 9. Well-known API key patterns
    let ant_re = Regex::new(r"sk-ant-[A-Za-z0-9_\-]{20,}").unwrap();
    out = ant_re.replace_all(&out, "[REDACTED KEY]").to_string();

    let oai_re = Regex::new(r"\bsk-[A-Za-z0-9]{20,}\b").unwrap();
    out = oai_re.replace_all(&out, "[REDACTED KEY]").to_string();

    let gh_re = Regex::new(r"\bgh[pousr]_[A-Za-z0-9_]{30,}\b").unwrap();
    out = gh_re.replace_all(&out, "[REDACTED TOKEN]").to_string();

    out
}

/// Replace user's home directory and data home path with generic placeholders.
pub fn mask_home(text: &str, home: &Path) -> String {
    let mut out = text.to_string();
    let home_str = home.to_string_lossy();
    if !home_str.is_empty() {
        out = out.replace(home_str.as_ref(), "<clipcut-home>");
    }
    if let Some(user_home) = dirs::home_dir() {
        let user_home_str = user_home.to_string_lossy();
        if !user_home_str.is_empty() {
            out = out.replace(user_home_str.as_ref(), "~");
        }
    }
    out
}

/// Collect any known stored secrets from keys.json, youtube-oauth.json, and token files.
fn collect_known_secrets(state: &AppState) -> Vec<String> {
    let mut secrets = Vec::new();

    // Keys from keys.json (both via KeyPool and raw file if present)
    for keys in state.keys.store().services.values() {
        for key in keys {
            if !key.trim().is_empty() {
                secrets.push(key.clone());
            }
        }
    }
    if let Ok(content) = std::fs::read_to_string(state.paths.keys_file())
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(obj) = val.as_object()
    {
        for v in obj.values() {
            if let Some(arr) = v.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str()
                        && !s.trim().is_empty()
                    {
                        secrets.push(s.to_string());
                    }
                }
            }
        }
    }

    // Secrets from youtube-oauth.json
    if let Ok(content) = std::fs::read_to_string(state.paths.youtube_auth_file())
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&content)
        && let Some(obj) = val.as_object()
    {
        for key in ["client_secret", "refresh_token", "access_token"] {
            if let Some(s) = obj.get(key).and_then(|v| v.as_str())
                && !s.trim().is_empty()
            {
                secrets.push(s.to_string());
            }
        }
    }

    // Token files in data directory if present
    for name in ["token", "web-token", "auth-token"] {
        let token_path = state.paths.home.join(name);
        if let Ok(content) = std::fs::read_to_string(token_path) {
            let trimmed = content.trim().to_string();
            if !trimmed.is_empty() {
                secrets.push(trimmed);
            }
        }
    }

    secrets
}

/// Query tool versions with a short timeout.
async fn check_tool_versions(state: &AppState) -> Vec<(String, String)> {
    let tools = &state.services().tools;
    let mut out = Vec::new();

    let lookup: [(&str, &Option<PathBuf>); 11] = [
        ("ffmpeg", &tools.ffmpeg),
        ("ffprobe", &tools.ffprobe),
        ("yt-dlp", &tools.yt_dlp),
        ("chromium", &tools.chromium),
        ("claude", &tools.claude),
        ("codex", &tools.codex),
        ("espeak-ng", &tools.espeak_ng),
        ("podman", &tools.podman),
        ("docker", &tools.docker),
        ("deno", &tools.deno),
        ("node", &tools.node),
    ];

    for (name, path_opt) in lookup {
        let path = match path_opt {
            Some(p) => p,
            None => {
                out.push((name.to_string(), "not installed".to_string()));
                continue;
            }
        };

        let flag = if name == "ffmpeg" || name == "ffprobe" {
            "-version"
        } else {
            "--version"
        };

        let ver = match clipcut_core::tools::run(path, &[flag], None, Duration::from_secs(2)).await
        {
            Ok(output) => {
                let text = if !output.stdout.trim().is_empty() {
                    output.stdout
                } else {
                    output.stderr
                };
                text.lines()
                    .next()
                    .unwrap_or("empty output")
                    .trim()
                    .to_string()
            }
            Err(_) => "error querying version".to_string(),
        };

        out.push((name.to_string(), format!("{} ({})", ver, path.display())));
    }

    out
}

/// Generate a complete bug report as a plain text string with all sensitive data redacted.
pub async fn generate(state: &AppState) -> anyhow::Result<String> {
    let mut report = String::new();
    let now = Local::now();

    report.push_str(
        "================================================================================\n",
    );
    report.push_str("ClipCut Bug Report\n");
    report.push_str(&format!(
        "Generated: {}\n",
        now.format("%Y-%m-%d %H:%M:%S %z")
    ));
    report.push_str(&format!("Version:   {}\n", env!("CARGO_PKG_VERSION")));
    report.push_str(&format!(
        "Platform:  {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    report.push_str(&format!("Data dir:  {}\n", state.paths.home.display()));
    report.push_str(
        "================================================================================\n\n",
    );

    // Doctor checks
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    report.push_str("Doctor checks\n");
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    let checks = doctor::checks(state).await;
    for c in &checks {
        let mark = if c.ok {
            "ok  "
        } else if c.required {
            "FAIL"
        } else {
            "warn"
        };
        report.push_str(&format!("[{mark}] {:<36} {}\n", c.name, c.detail));
        if let Some(fix) = &c.fix {
            report.push_str(&format!("       fix: {fix}\n"));
        }
    }
    report.push('\n');

    // Tool versions
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    report.push_str("Tool versions\n");
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    for (name, ver) in check_tool_versions(state).await {
        report.push_str(&format!("{:<12} {}\n", name, ver));
    }
    report.push('\n');

    // Config (with secrets and paths masked)
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    report.push_str("Configuration (masked)\n");
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    let mut config = state.config().clone();
    if config.youtube.cookies_from_browser.is_some() {
        config.youtube.cookies_from_browser = Some("[MASKED]".to_string());
    }
    let config_toml = toml::to_string_pretty(&config).unwrap_or_else(|_| "invalid config".into());
    report.push_str(&mask_home(&config_toml, &state.paths.home));
    report.push_str("\n\n");

    // Key states (without key text)
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    report.push_str("API Key states (no secrets shown)\n");
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    for (service, label) in KNOWN_SERVICES {
        let count = state
            .keys
            .store()
            .services
            .get(*service)
            .map(|v| v.len())
            .unwrap_or(0);
        let status = if count > 0 {
            format!("saved ({count} key{})", if count == 1 { "" } else { "s" })
        } else {
            format!("not set ({label})")
        };
        report.push_str(&format!("{:<12} {}\n", service, status));
    }

    let yt = state.publish.status();
    let yt_status = if yt.connected {
        format!(
            "connected (channel: {}, id: {})",
            yt.channel.as_deref().unwrap_or("unknown"),
            yt.channel_id.as_deref().unwrap_or("unknown")
        )
    } else if yt.configured {
        "configured (not connected)".to_string()
    } else {
        "not connected".to_string()
    };
    report.push_str(&format!("{:<12} {}\n\n", "youtube", yt_status));

    // Project list (topics and scripts omitted)
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    report.push_str("Projects (topics omitted for privacy)\n");
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    let projects = state.store.list();
    if projects.is_empty() {
        report.push_str("(no projects)\n");
    } else {
        for p in &projects {
            report.push_str(&format!(
                "- ID: {} | Format: {} | Target: {}s | Status: {} | Created: {}\n",
                p.id,
                p.format.slug(),
                p.target_seconds,
                crate::cli::project_status(p),
                p.created_at.format("%Y-%m-%d %H:%M")
            ));
        }
    }
    report.push('\n');

    // Recent logs
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    report.push_str("Recent logs (last 500 lines)\n");
    report.push_str(
        "--------------------------------------------------------------------------------\n",
    );
    let logs = read_recent_lines(&state.paths.logs(), 500).unwrap_or_default();
    if logs.is_empty() {
        report.push_str("(no logs found)\n");
    } else {
        for line in &logs {
            report.push_str(line);
            report.push('\n');
        }
    }
    report.push('\n');

    // Final redaction pass over the complete report with known planted secrets
    let known_secrets = collect_known_secrets(state);
    let secret_refs: Vec<&str> = known_secrets.iter().map(String::as_str).collect();
    let sanitized = redact_with_secrets(&report, &secret_refs);

    Ok(mask_home(&sanitized, &state.paths.home))
}

/// Write the bug report to a file, returning its path.
pub async fn write_to_file(state: &AppState, dest: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let content = generate(state).await?;
    let path = match dest {
        Some(p) => p,
        None => {
            let date = Local::now().format("%Y-%m-%d");
            std::env::current_dir()?.join(format!("clipcut-report-{date}.txt"))
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestHome;

    #[test]
    fn test_redaction_cases() {
        // Emails
        assert_eq!(
            redact("Contact me at user.name+tag@example.com for info"),
            "Contact me at [REDACTED EMAIL] for info"
        );

        // Bearer token
        assert_eq!(
            redact("token is Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.xyz"),
            "token is Bearer [REDACTED]"
        );

        // Authorization header
        assert_eq!(
            redact("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.xyz"),
            "Authorization: [REDACTED]"
        );

        // Cookies
        assert_eq!(
            redact("Cookie: session_id=abc123xyz; user=foo"),
            "Cookie: [REDACTED COOKIE]"
        );

        // Google tokens
        assert_eq!(
            redact("Using ya29.a0AfH6SMD token"),
            "Using [REDACTED TOKEN] token"
        );
        assert_eq!(
            redact("Refresh token 1//04xyz123"),
            "Refresh token [REDACTED TOKEN]"
        );

        // API keys
        assert_eq!(
            redact("Anthropic key sk-ant-api03-abcdefghijklmnop123456789"),
            "Anthropic key [REDACTED KEY]"
        );
        assert_eq!(
            redact("OpenAI key sk-1234567890abcdefghijklmnopqrstuv"),
            "OpenAI key [REDACTED KEY]"
        );

        // URL parameter
        assert_eq!(
            redact("https://api.example.com/search?q=test&key=my_secret_key_123&other=1"),
            "https://api.example.com/search?q=test&key=[REDACTED]&other=1"
        );

        // JSON secrets
        assert_eq!(
            redact(r#"{"client_secret": "sensitive-oauth-secret", "port": 7878}"#),
            r#"{"client_secret": "[REDACTED]", "port": 7878}"#
        );
    }

    #[tokio::test]
    async fn test_report_never_contains_planted_secrets() {
        let home = TestHome::new();

        // 1. Plant keys in keys.json
        let planted_giphy_key = "planted-giphy-secret-key-987654";
        let planted_unsplash_key = "planted-unsplash-secret-key-123456";
        let keys_file = home.0.join("keys.json");
        std::fs::write(
            &keys_file,
            serde_json::to_string(&serde_json::json!({
                "giphy": [planted_giphy_key],
                "unsplash": [planted_unsplash_key],
            }))
            .unwrap(),
        )
        .unwrap();

        // 2. Plant OAuth secrets in youtube-oauth.json
        let planted_oauth_secret = "planted-client-secret-xyz890";
        let planted_refresh_token = "1//planted-refresh-token-val";
        let planted_access_token = "ya29.planted-access-token-val";
        let oauth_file = home.0.join("youtube-oauth.json");
        std::fs::write(
            &oauth_file,
            serde_json::to_string(&serde_json::json!({
                "client_id": "planted-client-id.apps.googleusercontent.com",
                "client_secret": planted_oauth_secret,
                "refresh_token": planted_refresh_token,
                "access_token": planted_access_token,
                "channel": "My Channel",
                "channel_id": "UC_test_123"
            }))
            .unwrap(),
        )
        .unwrap();

        // 3. Plant web token in token file
        let planted_web_token = "planted-web-token-secret-abc-def";
        std::fs::write(home.0.join("token"), planted_web_token).unwrap();

        // 4. Plant an email and a secret in log file
        let planted_email = "secret-engineer@example.org";
        let log_dir = home.0.join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let mut log_writer = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("clipcut.log"))
            .unwrap();
        use std::io::Write;
        writeln!(
            log_writer,
            "INFO: user logged in as {planted_email} with key {planted_giphy_key}"
        )
        .unwrap();
        writeln!(
            log_writer,
            "DEBUG: oauth exchange returned access_token {planted_access_token} and secret {planted_oauth_secret}"
        )
        .unwrap();

        let state = home.state();

        // 5. Generate report
        let report = generate(&state).await.unwrap();

        // Assert planted secrets are NEVER present in the report
        assert!(
            !report.contains(planted_giphy_key),
            "Giphy key must be redacted"
        );
        assert!(
            !report.contains(planted_unsplash_key),
            "Unsplash key must be redacted"
        );
        assert!(
            !report.contains(planted_oauth_secret),
            "OAuth secret must be redacted"
        );
        assert!(
            !report.contains(planted_refresh_token),
            "Refresh token must be redacted"
        );
        assert!(
            !report.contains(planted_access_token),
            "Access token must be redacted"
        );
        assert!(
            !report.contains(planted_web_token),
            "Web token must be redacted"
        );
        assert!(!report.contains(planted_email), "Email must be redacted");

        // Assert standard report sections exist
        assert!(report.contains("ClipCut Bug Report"));
        assert!(report.contains("Doctor checks"));
        assert!(report.contains("Tool versions"));
        assert!(report.contains("Configuration (masked)"));
        assert!(report.contains("API Key states"));
        assert!(report.contains("Projects (topics omitted for privacy)"));
        assert!(report.contains("Recent logs"));
    }
}
