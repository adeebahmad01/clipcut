//! Health checks for the status page / `clipcut doctor`, and one-click setup
//! of the free pieces ClipCut can install itself (yt-dlp, Kokoro, SearXNG).

use clipcut_core::Progress;
use clipcut_core::keys::KNOWN_SERVICES;
use clipcut_core::tools::install_hint;
use serde::Serialize;

use crate::state::AppState;

pub const SETUP_ID: &str = "_setup";

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// Command or hint; `setup:<target>` names a `/api/setup` target.
    pub fix: Option<String>,
    /// ClipCut cannot make a video without it.
    pub required: bool,
}

fn check(
    name: &str,
    ok: bool,
    detail: impl Into<String>,
    fix: Option<&str>,
    required: bool,
) -> Check {
    Check {
        name: name.into(),
        ok,
        detail: detail.into(),
        fix: if ok { None } else { fix.map(str::to_string) },
        required,
    }
}

fn tool(name: &str, path: &Option<std::path::PathBuf>, fix: &str, required: bool) -> Check {
    match path {
        Some(p) => check(name, true, p.display().to_string(), None, required),
        None => check(name, false, "not found", Some(fix), required),
    }
}

pub async fn checks(state: &AppState) -> Vec<Check> {
    let svc = state.services();
    let t = &svc.tools;
    let mut out = vec![
        tool("ffmpeg", &t.ffmpeg, &install_hint("ffmpeg"), true),
        tool("ffprobe", &t.ffprobe, "installed with ffmpeg", true),
        tool("yt-dlp", &t.yt_dlp, "setup:yt-dlp", false),
    ];
    let claude_help = match &t.claude {
        Some(program) => clipcut_core::tools::run(
            program,
            &["--help"],
            None,
            std::time::Duration::from_secs(5),
        )
        .await
        .map(|output| format!("{}\n{}", output.stdout, output.stderr)),
        None => Err(anyhow::anyhow!("Claude Code is not installed")),
    };
    out.push(llm_check(
        t.claude.as_deref(),
        claude_help.as_deref().map_err(|error| error.to_string()),
        t.codex.is_some(),
        &svc.config.llm.provider,
    ));
    out.push(tool(
        "JS runtime for yt-dlp (node/deno)",
        if t.deno.is_some() { &t.deno } else { &t.node },
        "install node or deno; YouTube downloads need one",
        false,
    ));
    out.push(tool(
        "Chromium (article screenshots)",
        &t.chromium,
        &if cfg!(target_os = "macos") {
            "brew install --cask google-chrome".to_string()
        } else {
            install_hint("chromium")
        },
        false,
    ));
    out.push(tool(
        "espeak-ng (voice phonemes)",
        &t.espeak_ng,
        &install_hint("espeak-ng"),
        svc.config.tts.engine != "command",
    ));

    let tts = svc.tts.health();
    out.push(check(
        &format!("Voice ({})", tts.engine),
        tts.ready,
        tts.detail,
        Some("setup:kokoro"),
        true,
    ));

    let container = t.podman.is_some() || t.docker.is_some();
    out.push(check(
        "Container runtime (for SearXNG)",
        container,
        if container {
            "podman/docker found"
        } else {
            "not found"
        },
        Some(&format!(
            "{} (optional: DuckDuckGo is used without SearXNG)",
            install_hint("podman")
        )),
        false,
    ));
    // Source health repeats ffmpeg, yt-dlp and the key rows. Keep the tool
    // check above, and keep one key row that names the keyless fallback.
    let covered = ["yt-dlp", "ffmpeg", "chromium", "yt-dlp js runtime"];
    for h in svc.sources.health().await {
        let name = h.name.to_ascii_lowercase();
        if covered.iter().any(|c| name == *c)
            || out.iter().any(|c| c.name.eq_ignore_ascii_case(&h.name))
        {
            continue;
        }
        let fix = if name.contains("searxng") {
            Some("setup:searxng")
        } else {
            None
        };
        out.push(check(&h.name, h.ok, h.detail, fix, false));
    }
    for (service, label) in KNOWN_SERVICES {
        let name = format!("{service} key");
        if out.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let has = state.keys.has(service);
        let detail = if has {
            "key saved".to_string()
        } else {
            format!("optional: free key from {label}")
        };
        let fix = format!("add a key in Settings → Keys, or run `clipcut keys add {service}`");
        out.push(check(&name, has, detail, Some(&fix), false));
    }
    out
}

/// Installation alone is not generation readiness. A read-only Codex sandbox
/// still exposes tools, so it cannot satisfy this app's output-only contract.
/// Inspect help without sending a prompt, reading credentials, or spending
/// provider usage. Actual authentication and model access are tested on a run.
fn llm_check(
    claude: Option<&std::path::Path>,
    help: Result<&str, String>,
    codex_installed: bool,
    configured_provider: &str,
) -> Check {
    const REQUIRED_FLAGS: &[&str] = &[
        "--safe-mode",
        "--tools",
        "--permission-mode",
        "--permission-prompts",
        "--strict-mcp-config",
        "--mcp-config",
        "--setting-sources",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
        "--json-schema",
        "--system-prompt",
        "--settings",
    ];
    let fix = "install/update Claude Code, then run `claude` once to sign in; Codex generation is disabled until a deny-all tool policy is supported";
    let Some(program) = claude else {
        return check(
            "LLM (tool-free Claude Code)",
            false,
            if codex_installed {
                "Codex is installed, but generation is disabled: its read-only sandbox does not disable tools"
            } else {
                "Claude Code is not installed"
            },
            Some(fix),
            true,
        );
    };
    let help = match help {
        Ok(help) => help,
        Err(error) => {
            return check(
                "LLM (tool-free Claude Code)",
                false,
                format!("could not verify safe CLI flags: {error}"),
                Some(fix),
                true,
            );
        }
    };
    let missing: Vec<_> = REQUIRED_FLAGS
        .iter()
        .filter(|flag| !help.contains(**flag))
        .copied()
        .collect();
    if !missing.is_empty() {
        return check(
            "LLM (tool-free Claude Code)",
            false,
            format!("CLI lacks required isolation flags: {}", missing.join(", ")),
            Some(fix),
            true,
        );
    }
    let fallback = if configured_provider.eq_ignore_ascii_case("codex") {
        "; configured Codex is disabled, so Claude is the safe fallback"
    } else {
        ""
    };
    check(
        "LLM (tool-free Claude Code)",
        true,
        format!(
            "{}; safe flags available, authentication/model access not checked{fallback}",
            program.display()
        ),
        None,
        true,
    )
}

pub fn ready(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.ok || !c.required)
}

/// Install/refresh one setup target (`yt-dlp`, `kokoro`, `searxng` or `all`).
pub async fn setup(state: &AppState, what: &str) -> anyhow::Result<()> {
    let progress = state.progress(SETUP_ID);
    let targets: Vec<&str> = match what {
        "all" => vec!["yt-dlp", "kokoro", "searxng"],
        "yt-dlp" | "kokoro" | "searxng" => vec![what],
        other => {
            anyhow::bail!("unknown setup target {other:?}; use yt-dlp, kokoro, searxng or all")
        }
    };
    let mut errors = Vec::new();
    for target in targets {
        if let Err(e) = setup_one(state, target, &progress).await {
            progress.error(format!("{target}: {e:#}"));
            errors.push(format!("{target}: {e:#}"));
        } else {
            progress.info(format!("{target} ready"));
        }
        state.refresh_tools();
    }
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(errors.join("; "))
    }
}

async fn setup_one(state: &AppState, target: &str, progress: &Progress) -> anyhow::Result<()> {
    let svc = state.services();
    match target {
        "yt-dlp" => {
            if svc.tools.yt_dlp.is_some() {
                clipcut_sources::setup::update_yt_dlp(&state.paths, &svc.sources.http, progress)
                    .await?;
            } else {
                clipcut_sources::setup::ensure_yt_dlp(&state.paths, &svc.sources.http, progress)
                    .await?;
            }
        }
        "kokoro" => svc.tts.ensure_models(progress).await?,
        "searxng" => {
            clipcut_sources::setup::ensure_searxng(&svc.config, &state.paths, &svc.tools, progress)
                .await?
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// On startup, quietly install whatever free pieces are missing.
pub async fn auto_setup(state: &AppState) {
    let svc = state.services();
    let progress = state.progress(SETUP_ID);
    if svc.tools.yt_dlp.is_none()
        && let Err(e) = setup_one(state, "yt-dlp", &progress).await
    {
        tracing::warn!("yt-dlp setup failed: {e:#}");
    }
    if svc.config.tts.engine == "kokoro"
        && !svc.tts.health().ready
        && let Err(e) = setup_one(state, "kokoro", &progress).await
    {
        tracing::warn!("Kokoro setup failed: {e:#}");
    }
    if svc.config.search.manage_searxng
        && (svc.tools.podman.is_some() || svc.tools.docker.is_some())
        && let Err(e) = setup_one(state, "searxng", &progress).await
    {
        tracing::warn!("SearXNG setup failed: {e:#} (DuckDuckGo will be used)");
    }
    state.refresh_tools();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_only_is_not_generation_ready() {
        let result = llm_check(None, Err("missing".into()), true, "codex");
        assert!(!result.ok);
        assert!(result.required);
        assert!(!ready(&[result]));
    }

    #[test]
    fn claude_installation_without_safe_flags_is_not_ready() {
        let result = llm_check(
            Some(std::path::Path::new("/fixture/claude")),
            Ok("--tools --json-schema"),
            true,
            "claude",
        );
        assert!(!result.ok);
        assert!(result.detail.contains("--safe-mode"));
    }

    #[test]
    fn safe_claude_can_fall_back_from_blocked_codex() {
        let help = "--safe-mode --tools --permission-mode --permission-prompts \
            --strict-mcp-config --mcp-config --setting-sources --disable-slash-commands \
            --no-chrome --no-session-persistence --json-schema --system-prompt --settings";
        let result = llm_check(
            Some(std::path::Path::new("/fixture/claude")),
            Ok(help),
            true,
            "codex",
        );
        assert!(ready(std::slice::from_ref(&result)));
        assert!(
            result
                .detail
                .contains("authentication/model access not checked")
        );
        assert!(result.detail.contains("safe fallback"));
    }
}
