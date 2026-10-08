//! ClipCut: topic in, short edited video out. `clipcut` starts the web app;
//! the other subcommands run the same pipeline headless.

mod doctor;
mod net;
mod pipeline;
mod server;
mod state;
mod store;
#[cfg(test)]
mod test_support;

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use clipcut_core::config::Format;
use clipcut_core::{Paths, Project, Stage, Status};
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

#[derive(Parser)]
#[command(
    name = "clipcut",
    version,
    about = "Short videos from YouTube clips, articles, memes and GIFs"
)]
struct Cli {
    /// Data directory (default: $CLIPCUT_HOME or ~/.local/share/clipcut).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the web app (default).
    Serve {
        /// Don't open the app window.
        #[arg(long)]
        no_window: bool,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        host: Option<String>,
    },
    /// Make a video from the command line, start to finish.
    New {
        topic: String,
        #[arg(long, default_value = "")]
        notes: String,
        /// vertical (9:16) or horizontal (16:9).
        #[arg(long)]
        horizontal: bool,
        #[arg(long)]
        seconds: Option<u32>,
        #[arg(long)]
        voice: Option<String>,
    },
    /// Continue or re-run a project: `clipcut run <id> --from assets`.
    Run {
        id: String,
        #[arg(long, value_parser = parse_stage)]
        from: Option<Stage>,
        #[arg(long)]
        only: bool,
    },
    /// Re-render an existing project.
    Render { id: String },
    /// Check tools, models, search and keys.
    Doctor,
    /// Install free components: yt-dlp, kokoro, searxng or all.
    Setup {
        #[arg(default_value = "all")]
        what: String,
    },
}

fn parse_stage(s: &str) -> Result<Stage, String> {
    serde_json::from_value(serde_json::Value::String(s.to_ascii_lowercase()))
        .map_err(|_| "one of research, hooks, script, beats, assets, voice, render".to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn,hyper=warn")),
        )
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let state = AppState::load(Paths::resolve(cli.home))?;
    match cli.command.unwrap_or(Command::Serve {
        no_window: false,
        port: None,
        host: None,
    }) {
        Command::Serve {
            no_window,
            port,
            host,
        } => serve(state, no_window, port, host).await,
        Command::New {
            topic,
            notes,
            horizontal,
            seconds,
            voice,
        } => {
            let config = state.config();
            let seconds = seconds.unwrap_or(config.video.target_seconds);
            anyhow::ensure!(
                (10..=600).contains(&seconds),
                "seconds must be between 10 and 600"
            );
            anyhow::ensure!(
                !topic.trim().is_empty() && topic.len() <= 1000,
                "topic must be 1–1000 bytes"
            );
            anyhow::ensure!(notes.len() <= 20_000, "notes must be at most 20000 bytes");
            anyhow::ensure!(
                voice.as_ref().is_none_or(|v| v.len() <= 128),
                "voice must be at most 128 bytes"
            );
            let format = if horizontal {
                Format::Horizontal
            } else {
                config.video.format
            };
            let mut project = Project::new(topic, notes, format, seconds);
            project.voice = voice;
            let project = state.store.create(project).await?;
            println!("Project {}", project.id);
            run_headless(&state, &project.id, None, false).await
        }
        Command::Run { id, from, only } => run_headless(&state, &id, from, only).await,
        Command::Render { id } => run_headless(&state, &id, Some(Stage::Render), true).await,
        Command::Doctor => {
            let checks = doctor::checks(&state).await;
            for c in &checks {
                let mark = if c.ok {
                    "ok  "
                } else if c.required {
                    "FAIL"
                } else {
                    "warn"
                };
                println!("[{mark}] {:<36} {}", c.name, c.detail);
                if let Some(fix) = &c.fix {
                    let fix = fix
                        .strip_prefix("setup:")
                        .map(|t| format!("clipcut setup {t}"))
                        .unwrap_or_else(|| fix.clone());
                    println!("       fix: {fix}");
                }
            }
            println!("\nHome: {}", state.paths.home.display());
            if doctor::ready(&checks) {
                println!("Ready to make videos.");
                Ok(())
            } else {
                anyhow::bail!("required pieces are missing")
            }
        }
        Command::Setup { what } => doctor::setup(&state, &what).await,
    }
}

async fn run_headless(
    state: &AppState,
    id: &str,
    from: Option<Stage>,
    only: bool,
) -> anyhow::Result<()> {
    // Headless runs never pause for review.
    state.store.update(id, |p| Ok(p.autopilot = true)).await?;
    pipeline::start(state, id, from, only).await?;
    while state.store.is_running(id) {
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let project = state.store.get(id)?;
    if let Some(failed) = project.stages.iter().find(|s| s.status == Status::Failed) {
        anyhow::bail!(
            "{} failed: {}",
            failed.stage.label(),
            failed.error.clone().unwrap_or_default()
        );
    }
    match &project.render {
        Some(r) => println!("Video: {}", state.store.dir(id).join(&r.path).display()),
        None => println!(
            "Stopped before render; project folder: {}",
            state.store.dir(id).display()
        ),
    }
    Ok(())
}

async fn serve(
    state: AppState,
    no_window: bool,
    port: Option<u16>,
    host: Option<String>,
) -> anyhow::Result<()> {
    let config = state.config();
    let host = host.unwrap_or(config.server.host.clone());
    let port = port.unwrap_or(config.server.port);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|e| {
            anyhow::anyhow!("cannot listen on {host}:{port}: {e} (is ClipCut already running?)")
        })?;
    let port = listener.local_addr()?.port();
    state.set_bound_port(port);

    pipeline::recover_interrupted(&state).await;
    let setup_state = state.clone();
    let setup_handle = tokio::spawn(async move {
        let _slot = setup_state.setup_slots.clone().acquire_owned().await;
        doctor::auto_setup(&setup_state).await
    });

    let local = format!("http://localhost:{port}");
    println!("\n  ClipCut {} is running\n", env!("CARGO_PKG_VERSION"));
    println!("  Local:   {local}");
    match net::network_url(port) {
        Some(url) if host != "127.0.0.1" && host != "localhost" => println!("  Network: {url}"),
        _ => println!("  Network: (not exposed; listening on {host})"),
    }
    println!("  Data:    {}\n", state.paths.home.display());

    if config.server.open_window && !no_window {
        if let Some(chromium) = state.services().tools.chromium {
            let profile = state.paths.cache().join("app-window");
            let url = local.clone();
            // Give the listener a moment so the window never shows an error page.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                net::open_window(&chromium, &url, &profile);
            });
        }
    }

    let shutdown_state = state.clone();
    let app = server::router(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            setup_handle.abort();
            let _ = setup_handle.await;
            for id in shutdown_state.store.running_ids() {
                let _ = pipeline::cancel(&shutdown_state, &id).await;
            }
            clipcut_sources::shutdown_browser().await;
        })
        .await?;
    Ok(())
}
