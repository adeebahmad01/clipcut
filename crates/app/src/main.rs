//! ClipCut: topic in, short edited video out. `clipcut` starts the web app;
//! the other subcommands run the same pipeline headless.

mod cli;
mod clips;
mod doctor;
mod net;
mod pipeline;
mod publish;
mod queue;
mod server;
mod state;
mod store;
#[cfg(test)]
mod test_support;

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::{CommandFactory, Parser, Subcommand, ValueHint};
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
    #[arg(long, global = true, value_hint = ValueHint::DirPath)]
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
        #[command(flatten)]
        video: cli::VideoOpts,
        /// Target length in seconds (10–600).
        #[arg(long)]
        seconds: Option<u32>,
        #[arg(long)]
        voice: Option<String>,
    },
    /// Continue or re-run a project: `clipcut run <id> --from assets`.
    Run {
        /// Project ID, its last few characters, or `latest`.
        id: String,
        #[arg(long, value_parser = STAGES)]
        from: Option<Stage>,
        #[arg(long)]
        only: bool,
    },
    /// Re-render a project, optionally in another format or caption style.
    Render {
        /// Project ID, its last few characters, or `latest`.
        id: String,
        #[command(flatten)]
        video: cli::VideoOpts,
        /// Stop exporting extra formats.
        #[arg(long, conflicts_with = "also")]
        no_also: bool,
    },
    /// List projects, newest first, with their IDs and where each one stands.
    List {
        /// Show only the newest N.
        #[arg(long, short = 'n', value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
        limit: Option<usize>,
    },
    /// Show one project: how each stage went, its files and where it was published.
    Show {
        /// Project ID, its last few characters, or `latest`.
        id: String,
        /// Print only the video's path, for scripts.
        #[arg(long, conflicts_with = "json")]
        path: bool,
        /// Print the whole project as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete projects and their files: `clipcut delete 3fa9c1 b07e44`.
    Delete {
        /// Project IDs, their last few characters, or `latest`.
        #[arg(required = true)]
        ids: Vec<String>,
        /// Don't ask first.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Also delete projects marked as running.
        #[arg(long)]
        force: bool,
    },
    /// Copy a project to try another take, keeping its research, hook and script.
    Duplicate {
        /// Project ID, its last few characters, or `latest`.
        id: String,
    },
    /// Make one video per line of a text file (blank and # lines are skipped).
    Batch {
        #[arg(value_hint = ValueHint::FilePath)]
        file: PathBuf,
        /// Publish each video when it renders: public, unlisted or private.
        #[arg(long, value_parser = cli::PRIVACY)]
        publish: Option<String>,
        /// Space the releases: 90m, 2h, 1d…
        #[arg(long, value_parser = cli::parse_every)]
        every: Option<u32>,
        /// First release: "2026-10-12 15:00" (local) or RFC 3339.
        #[arg(long, value_parser = cli::parse_time)]
        start: Option<DateTime<Utc>>,
        #[command(flatten)]
        video: cli::VideoOpts,
        /// Target length in seconds (10–600).
        #[arg(long)]
        seconds: Option<u32>,
    },
    /// Show the batch queue, or pause, resume, retry, remove and clear its items.
    #[command(args_conflicts_with_subcommands = true)]
    Queue {
        #[command(subcommand)]
        action: Option<cli::QueueAction>,
        /// Print the queue as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cut the best moments of a long YouTube video into shorts.
    Clip {
        #[arg(value_hint = ValueHint::Url)]
        url: String,
        /// How many clips (1–5).
        #[arg(long, default_value_t = 3)]
        count: usize,
        /// Shortest clip, in seconds.
        #[arg(long, default_value_t = 30.0)]
        min: f32,
        /// Longest clip, in seconds.
        #[arg(long, default_value_t = 60.0)]
        max: f32,
        #[command(flatten)]
        video: cli::VideoOpts,
        /// Publish each clip when it renders: public, unlisted or private.
        #[arg(long, value_parser = cli::PRIVACY)]
        publish: Option<String>,
        /// Space the releases: 90m, 2h, 1d…
        #[arg(long, value_parser = cli::parse_every)]
        every: Option<u32>,
        /// First release: "2026-10-12 15:00" (local) or RFC 3339.
        #[arg(long, value_parser = cli::parse_time)]
        start: Option<DateTime<Utc>>,
    },
    /// Upload a rendered project to YouTube; `--connect` signs in first.
    Publish {
        /// Project ID, its last few characters, or `latest`.
        id: Option<String>,
        /// public, unlisted or private; default: `privacy` under [publish] in config.toml.
        #[arg(long, value_parser = cli::PRIVACY)]
        privacy: Option<String>,
        /// Release time; the video stays private until then.
        #[arg(long, value_parser = cli::parse_time)]
        at: Option<DateTime<Utc>>,
        /// Which render to upload: vertical, horizontal or square.
        #[arg(long, value_parser = cli::FORMATS)]
        format: Option<Format>,
        /// Sign in to YouTube.
        #[arg(long)]
        connect: bool,
        /// Save the OAuth client JSON downloaded from Google Cloud.
        #[arg(long, value_name = "client_secret.json", value_hint = ValueHint::FilePath)]
        client: Option<PathBuf>,
        /// Forget the YouTube sign-in.
        #[arg(long, conflicts_with_all = ["connect", "id"])]
        disconnect: bool,
    },
    /// Topic ideas from Google Trends, or news headlines.
    Ideas {
        /// News headlines instead of trending searches.
        #[arg(long)]
        news: bool,
        /// Search the news for this.
        #[arg(long)]
        q: Option<String>,
        /// Country for Google Trends.
        #[arg(long, default_value = "US")]
        geo: String,
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Show or change settings: `clipcut config set video.format horizontal`.
    #[command(args_conflicts_with_subcommands = true)]
    Config {
        #[command(subcommand)]
        action: Option<cli::ConfigAction>,
        /// Print only the path of config.toml.
        #[arg(long, conflicts_with = "json")]
        path: bool,
        /// Print the settings as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check tools, models, search and keys.
    Doctor,
    /// Install free components: yt-dlp, kokoro, searxng or all.
    Setup {
        #[arg(default_value = "all", value_parser = SETUP_TARGETS)]
        what: String,
    },
    /// Print a shell completion script: `clipcut completions bash`.
    Completions {
        /// bash, zsh, fish, elvish or powershell.
        shell: clap_complete::Shell,
    },
}

const STAGES: cli::OneOf<Stage> = cli::OneOf::new(
    &[
        "research", "hooks", "script", "beats", "assets", "voice", "render",
    ],
    parse_stage,
);
const SETUP_TARGETS: cli::OneOf<String> = cli::OneOf::hint(&["all", "yt-dlp", "kokoro", "searxng"]);

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
    let command = match cli.command {
        // Completions don't need (or create) the data directory.
        Some(Command::Completions { shell }) => {
            print_completions(shell, &mut std::io::stdout());
            return Ok(());
        }
        // Config only needs config.toml, which may be the reason the app won't load.
        Some(Command::Config { action, path, json }) => {
            let paths = Paths::resolve(cli.home);
            if path {
                println!("{}", paths.config_file().display());
                return Ok(());
            }
            return cli::config(&paths, action, json).await;
        }
        command => command,
    };
    let state = AppState::load(Paths::resolve(cli.home))?;
    match command.unwrap_or(Command::Serve {
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
            video,
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
            let format = video.format().unwrap_or(config.video.format);
            let mut project = Project::new(topic, notes, format, seconds);
            project.voice = voice;
            project.caption_preset = video.captions;
            project.extra_formats = server::clean_extra_formats(format, video.also);
            let project = state.store.create(project).await?;
            println!("Project {}", project.id);
            run_headless(&state, &project.id, None, false).await
        }
        Command::Run { id, from, only } => {
            let id = cli::resolve_id(&state, &id)?;
            run_headless(&state, &id, from, only).await
        }
        Command::Render { id, video, no_also } => {
            let id = cli::resolve_id(&state, &id)?;
            cli::change_video(&state, &id, cli::VideoChanges::new(video, no_also)).await?;
            run_headless(&state, &id, Some(Stage::Render), true).await
        }
        Command::List { limit } => cli::list(&state, limit),
        Command::Show { id, path, json } => cli::show(&state, &id, path, json),
        Command::Delete { ids, yes, force } => {
            cli::delete(&state, cli::DeleteArgs { ids, yes, force }).await
        }
        Command::Duplicate { id } => cli::duplicate(&state, &id).await,
        Command::Batch {
            file,
            publish,
            every,
            start,
            video,
            seconds,
        } => {
            let args = cli::BatchArgs {
                file,
                publish,
                every,
                start,
                video,
                seconds,
            };
            cli::batch(state, args).await
        }
        Command::Queue { action, json } => cli::queue(&state, action, json).await,
        Command::Clip {
            url,
            count,
            min,
            max,
            video,
            publish,
            every,
            start,
        } => {
            let args = cli::ClipArgs {
                url,
                count,
                min,
                max,
                video,
                publish,
                every,
                start,
            };
            cli::clip(state, args).await
        }
        Command::Publish {
            id,
            privacy,
            at,
            format,
            connect,
            client,
            disconnect,
        } => {
            let args = cli::PublishArgs {
                id,
                privacy,
                at,
                format,
                connect,
                client,
                disconnect,
            };
            cli::publish(state, args).await
        }
        Command::Ideas {
            news,
            q,
            geo,
            limit,
        } => cli::ideas(&state, news, q, &geo, limit).await,
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
        Command::Completions { .. } | Command::Config { .. } => {
            unreachable!("handled before loading state")
        }
    }
}

fn print_completions(shell: clap_complete::Shell, out: &mut dyn std::io::Write) {
    clap_complete::generate(shell, &mut Cli::command(), "clipcut", out);
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
    let queue_handle = tokio::spawn(queue::worker(state.clone(), false));
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
            queue_handle.abort();
            let _ = queue_handle.await;
            for id in shutdown_state.store.running_ids() {
                let _ = pipeline::cancel(&shutdown_state, &id).await;
            }
            clipcut_sources::shutdown_browser().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn completions_cover_commands_and_values() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
            clap_complete::Shell::Elvish,
            clap_complete::Shell::PowerShell,
        ] {
            let mut out = Vec::new();
            print_completions(shell, &mut out);
            let script = String::from_utf8(out).unwrap();
            for word in [
                "render",
                "queue",
                "duplicate",
                "completions",
                "config",
                "hormozi",
                "unlisted",
                "searxng",
            ] {
                assert!(script.contains(word), "{shell} script lacks {word}");
            }
            // clap_complete only completes positional values in bash and zsh.
            if matches!(
                shell,
                clap_complete::Shell::Bash | clap_complete::Shell::Zsh
            ) {
                assert!(
                    script.contains("video.caption_preset"),
                    "{shell} lacks settings"
                );
            }
        }
    }

    /// Every listed word parses, and together they name every variant.
    fn lists_all<T: Clone + Send + Sync + PartialEq + std::fmt::Debug + 'static>(
        parser: &cli::OneOf<T>,
        all: &[T],
    ) {
        use clap::builder::TypedValueParser;
        let cmd = clap::Command::new("t");
        let parsed: Vec<T> = parser
            .possible_values()
            .unwrap()
            .map(|v| {
                parser
                    .parse_ref(&cmd, None, std::ffi::OsStr::new(v.get_name()))
                    .unwrap()
            })
            .collect();
        assert_eq!(parsed, all);
    }

    #[test]
    fn completion_words_match_the_types() {
        lists_all(&STAGES, &Stage::ALL);
        lists_all(&cli::FORMATS, &Format::ALL);
        lists_all(&cli::CAPTIONS, &clipcut_core::timeline::CaptionPreset::ALL);
    }

    #[test]
    fn choice_flags_still_parse_leniently() {
        let cli = Cli::try_parse_from([
            "clipcut",
            "new",
            "t",
            "--format",
            " Square ",
            "--captions",
            "HORMOZI",
            "--also",
            "vertical,Horizontal",
        ])
        .unwrap();
        let Some(Command::New { video, .. }) = cli.command else {
            panic!("not new")
        };
        assert_eq!(video.format, Some(Format::Square));
        assert!(video.captions.is_some());
        assert_eq!(video.also, [Format::Vertical, Format::Horizontal]);

        let cli = Cli::try_parse_from(["clipcut", "run", "latest", "--from", "Assets"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Run {
                from: Some(Stage::Assets),
                ..
            })
        ));
        let err = Cli::try_parse_from(["clipcut", "run", "latest", "--from", "nope"])
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("one of research, hooks"), "{err}");

        let cli = Cli::try_parse_from([
            "clipcut", "render", "latest", "--format", "square", "--also", "vertical",
        ])
        .unwrap();
        let Some(Command::Render { video, no_also, .. }) = cli.command else {
            panic!("not render")
        };
        assert_eq!(video.format, Some(Format::Square));
        assert_eq!(video.also, [Format::Vertical]);
        assert!(!no_also);
        assert!(
            Cli::try_parse_from([
                "clipcut",
                "render",
                "latest",
                "--no-also",
                "--also",
                "square"
            ])
            .is_err()
        );

        // Privacy is checked by the command, so its message stays the same.
        let cli =
            Cli::try_parse_from(["clipcut", "publish", "latest", "--privacy", "Unlisted"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Publish { privacy: Some(p), .. }) if p == "Unlisted"
        ));
    }
}
