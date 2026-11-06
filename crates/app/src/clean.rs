//! What takes up space in ClipCut's home, and removing the parts that can be
//! made again: `clipcut clean`, `GET /api/storage` and `POST /api/storage/clean`.
//! keys.json, youtube-oauth.json, config.toml, queue.json, the voice models,
//! music and sound effects are never touched.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use chrono::Utc;
use clipcut_core::paths::ProjectPaths;
use clipcut_core::{Project, Stage, Status};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::{api, disk_usage, human_size, running_app};
use crate::state::AppState;

/// Renders older than this many days are offered unless asked otherwise.
pub const DEFAULT_RENDER_DAYS: u32 = 30;
/// A leftover younger than this may still belong to a download or a save.
const TEMP_MIN_AGE: Duration = Duration::from_secs(3600);
/// The app window's Chromium profile, inside the cache folder.
const APP_WINDOW: &str = "app-window";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    /// Caption, icon, meme template and topic idea downloads.
    Cache,
    /// The `render/` folders of projects rendered a while ago.
    Renders,
    /// `*.tmp` and `*.part` files and caption attempts left by a crash or a
    /// cancelled download.
    Temp,
    /// The Chromium profile of the app window.
    Browser,
}

impl Part {
    pub const ALL: [Part; 4] = [Part::Cache, Part::Renders, Part::Temp, Part::Browser];

    pub fn label(self) -> &'static str {
        match self {
            Part::Cache => "Download cache",
            Part::Renders => "Old renders",
            Part::Temp => "Leftover temporary files",
            Part::Browser => "App window browser profile",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Part::Cache => "cache",
            Part::Renders => "renders",
            Part::Temp => "temp",
            Part::Browser => "browser",
        }
    }
}

/// One file or folder that cleaning a part removes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub part: Part,
    /// Relative to the ClipCut home, with `/` between folders.
    pub path: String,
    pub bytes: u64,
    /// The project an old render belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub part: Part,
    pub bytes: u64,
    pub count: usize,
    /// Why some of this part is kept for now.
    #[serde(default)]
    pub kept: Vec<String>,
}

/// What each part holds (from a scan), or what was removed (from a clean).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub home: PathBuf,
    pub older_than_days: u32,
    pub rows: Vec<Row>,
    pub items: Vec<Item>,
}

impl Report {
    fn new(
        state: &AppState,
        older_than_days: u32,
        items: Vec<Item>,
        kept: &[(Part, String)],
    ) -> Self {
        let rows = Part::ALL
            .iter()
            .map(|&part| {
                let mine = items.iter().filter(|i| i.part == part);
                Row {
                    part,
                    bytes: mine.clone().map(|i| i.bytes).sum(),
                    count: mine.count(),
                    kept: kept
                        .iter()
                        .filter(|(p, _)| *p == part)
                        .map(|(_, why)| why.clone())
                        .collect(),
                }
            })
            .collect();
        Self {
            home: state.paths.home.clone(),
            older_than_days,
            rows,
            items,
        }
    }

    /// The items cleaning `parts` removes, leaving out those inside another
    /// one (a caption attempt inside the cache), so nothing counts twice.
    pub fn selected(&self, parts: &[Part]) -> Vec<&Item> {
        let chosen: Vec<&Item> = self
            .items
            .iter()
            .filter(|i| parts.contains(&i.part))
            .collect();
        chosen
            .iter()
            .copied()
            .filter(|i| {
                !chosen.iter().any(|o| {
                    i.path
                        .strip_prefix(&o.path)
                        .is_some_and(|rest| rest.starts_with('/'))
                })
            })
            .collect()
    }

    pub fn bytes(&self, parts: &[Part]) -> u64 {
        self.selected(parts).iter().map(|i| i.bytes).sum()
    }
}

/// Projects to leave alone: marked running (here, in the app or in another
/// `clipcut` command), or busy with a visual request or an upload in this app.
fn busy_projects(state: &AppState, projects: &[Project]) -> Vec<String> {
    projects
        .iter()
        .filter(|p| {
            p.stages.iter().any(|s| s.status == Status::Running)
                || state.store.is_running(&p.id)
                || state.project_busy(&p.id)
                || state.publish.is_uploading(&p.id)
        })
        .map(|p| p.id.clone())
        .collect()
}

/// The last six characters of an ID, which `clipcut` commands accept.
fn short(id: &str) -> String {
    let skip = id.chars().count().saturating_sub(6);
    id.chars().skip(skip).collect()
}

/// `*.tmp` and `*.part` files under `dir`, and caption `attempt-*` folders
/// under `captions`, last changed before `before`. Symlinks are not followed.
fn leftovers(
    dir: &Path,
    captions: &Path,
    before: SystemTime,
    skip: &dyn Fn(&Path) -> bool,
    out: &mut Vec<PathBuf>,
) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if skip(&path) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let old = meta.modified().is_ok_and(|t| t < before);
        if meta.is_dir() {
            if name.starts_with("attempt-") && path.starts_with(captions) {
                if old {
                    out.push(path);
                }
            } else {
                leftovers(&path, captions, before, skip, out);
            }
        } else if meta.is_file() && old && (name.ends_with(".tmp") || name.ends_with(".part")) {
            out.push(path);
        }
    }
}

/// What each part holds now. `in_app` is set when the app is running, whose
/// window may be using its browser profile.
pub fn scan(state: &AppState, older_than_days: u32, in_app: bool) -> Report {
    let home = &state.paths.home;
    let rel = |path: &Path| {
        let rel = path.strip_prefix(home).unwrap_or(path);
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    };
    let item = |part: Part, path: &Path, project: Option<&str>| Item {
        part,
        path: rel(path),
        bytes: disk_usage(path),
        project: project.map(String::from),
    };
    let projects = state.store.list();
    let busy = busy_projects(state, &projects);
    let cache = state.paths.cache();
    let profile = cache.join(APP_WINDOW);
    let mut items = Vec::new();
    let mut kept = Vec::new();

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&cache)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| *p != profile)
        .collect();
    entries.sort();
    if let Some(id) = busy.first() {
        let bytes = entries.iter().map(|p| disk_usage(p)).sum();
        let why = format!("{} kept while {} is busy", human_size(bytes), short(id));
        kept.push((Part::Cache, why));
    } else {
        // Empty folders free nothing; they are made again at once anyway.
        items.extend(
            entries
                .iter()
                .map(|p| item(Part::Cache, p, None))
                .filter(|i| i.bytes > 0),
        );
    }

    let cutoff = Utc::now().checked_sub_signed(chrono::Duration::days(older_than_days.into()));
    for p in projects.iter().rev() {
        let Some(render) = &p.render else { continue };
        if cutoff.is_none_or(|cutoff| render.rendered_at > cutoff) {
            continue;
        }
        if busy.contains(&p.id) {
            kept.push((Part::Renders, format!("{} is busy", short(&p.id))));
            continue;
        }
        let dir = ProjectPaths(&state.store.dir(&p.id)).render();
        if dir.is_dir() {
            items.push(item(Part::Renders, &dir, Some(&p.id)));
        }
    }

    let before = SystemTime::now() - TEMP_MIN_AGE;
    let captions = cache.join("captions");
    let busy_dirs: Vec<PathBuf> = busy.iter().map(|id| state.store.dir(id)).collect();
    let skip = |path: &Path| path == profile || busy_dirs.iter().any(|d| d == path);
    let mut temp = Vec::new();
    for dir in [state.paths.projects(), cache.clone(), state.paths.bin()] {
        leftovers(&dir, &captions, before, &skip, &mut temp);
    }
    // Only files at the top of the home: the rest of it is kept as it is.
    let top = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path());
    temp.extend(top.filter(|p| {
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        std::fs::symlink_metadata(p)
            .is_ok_and(|m| m.is_file() && m.modified().is_ok_and(|t| t < before))
            && (name.ends_with(".tmp") || name.ends_with(".part"))
    }));
    temp.sort();
    items.extend(temp.iter().map(|p| item(Part::Temp, p, None)));

    if profile.is_dir() {
        if in_app {
            let why = format!(
                "{} kept while ClipCut is running",
                human_size(disk_usage(&profile))
            );
            kept.push((Part::Browser, why));
        } else {
            items.push(item(Part::Browser, &profile, None));
        }
    }
    Report::new(state, older_than_days, items, &kept)
}

/// [`scan`] off the async runtime: it reads every file's size.
pub async fn report(
    state: &AppState,
    older_than_days: u32,
    in_app: bool,
) -> anyhow::Result<Report> {
    let state = state.clone();
    Ok(tokio::task::spawn_blocking(move || scan(&state, older_than_days, in_app)).await?)
}

fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Removes a project's render folder and marks it not rendered, keeping its
/// beats and voiceover, so `clipcut render` makes the video again.
async fn forget_render(state: &AppState, id: &str, dir: &Path) -> anyhow::Result<()> {
    let _operation = state.store.operation(id).await;
    anyhow::ensure!(
        !(state.store.is_running(id) || state.project_busy(id) || state.publish.is_uploading(id)),
        "{} is busy",
        short(id)
    );
    state
        .store
        .update(id, |p| {
            anyhow::ensure!(
                !p.stages.iter().any(|s| s.status == Status::Running),
                "{} is running",
                short(id)
            );
            p.render = None;
            if let Some(s) = p.stages.iter_mut().find(|s| s.stage == Stage::Render) {
                s.status = Status::Pending;
                s.progress = 0.0;
                s.message.clear();
                s.error = None;
                s.started_at = None;
                s.finished_at = None;
            }
            Ok(())
        })
        .await?;
    Ok(remove(dir)?)
}

/// Removes what [`scan`] lists for `parts` and reports what went.
pub async fn clean(
    state: &AppState,
    parts: &[Part],
    older_than_days: u32,
    in_app: bool,
) -> anyhow::Result<Report> {
    let found = report(state, older_than_days, in_app).await?;
    let mut removed = Vec::new();
    let mut kept: Vec<(Part, String)> = found
        .rows
        .iter()
        .filter(|r| parts.contains(&r.part))
        .flat_map(|r| r.kept.iter().map(|why| (r.part, why.clone())))
        .collect();
    for item in found.selected(parts) {
        let path = state.paths.home.join(&item.path);
        let done = match &item.project {
            Some(id) => forget_render(state, id, &path).await,
            None => remove(&path).map_err(anyhow::Error::from),
        };
        match done {
            Ok(()) => removed.push(item.clone()),
            Err(e) => kept.push((item.part, format!("couldn't remove {}: {e:#}", item.path))),
        }
    }
    Ok(Report::new(state, older_than_days, removed, &kept))
}

// ---------------------------------------------------------------------------
// clipcut clean

/// `30d`, `2w` or plain days.
pub fn parse_days(s: &str) -> Result<u32, String> {
    let s = s.trim().to_ascii_lowercase();
    let (n, weeks) = match s.strip_suffix('w') {
        Some(n) => (n, true),
        None => (s.strip_suffix('d').unwrap_or(&s), false),
    };
    let n: u32 = n
        .trim()
        .parse()
        .map_err(|_| "use 30d, 2w or a number of days".to_string())?;
    if weeks {
        n.checked_mul(7).ok_or_else(|| "too many weeks".into())
    } else {
        Ok(n)
    }
}

pub struct CleanArgs {
    pub parts: Vec<Part>,
    pub older_than_days: u32,
    pub dry_run: bool,
    pub yes: bool,
}

fn row_name(row: &Row, older_than_days: u32) -> String {
    match row.part {
        Part::Renders => format!("{} (over {older_than_days} days)", row.part.label()),
        _ => row.part.label().to_string(),
    }
}

/// The `clipcut clean` table: each part with its size and why some is kept.
pub fn storage_table(report: &Report, parts: &[Part]) -> String {
    let rows: Vec<[String; 4]> = report
        .rows
        .iter()
        .map(|r| {
            let mark = if parts.contains(&r.part) { "*" } else { " " };
            let mut note = match (r.part, r.count) {
                (_, 0) => "nothing to remove".to_string(),
                (Part::Renders, n) => format!("{n} project{}", if n == 1 { "" } else { "s" }),
                (Part::Temp, n) => format!("{n} item{}", if n == 1 { "" } else { "s" }),
                _ => String::new(),
            };
            for why in &r.kept {
                if !note.is_empty() {
                    note.push_str("; ");
                }
                note.push_str(why);
            }
            [
                format!("{mark}{}", r.part.slug()),
                row_name(r, report.older_than_days),
                human_size(r.bytes),
                note,
            ]
        })
        .collect();
    let header = [" PART", "WHAT", "SIZE", ""].map(String::from);
    let width = |col: usize| {
        std::iter::once(&header)
            .chain(&rows)
            .map(|r| r[col].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (part_w, what_w, size_w) = (width(0), width(1), width(2));
    std::iter::once(&header)
        .chain(&rows)
        .map(|[part, what, size, note]| {
            format!("{part:<part_w$}  {what:<what_w$}  {size:>size_w$}  {note}")
                .trim_end()
                .to_string()
                + "\n"
        })
        .collect()
}

/// `clipcut clean`: shows what uses space and, for the parts asked for,
/// removes it. With the app running on this home it goes through the app,
/// which knows what it is busy with.
pub async fn run(state: &AppState, args: CleanArgs) -> anyhow::Result<()> {
    let days = args.older_than_days;
    let app = running_app(state).await;
    let mut via = None;
    if let Some(base) = &app {
        let url = format!("{base}/storage?older_than_days={days}");
        if let Ok(value) = api(reqwest::Method::GET, &url, None).await
            && let Ok(found) = serde_json::from_value::<Report>(value)
            && same_dir(&found.home, &state.paths.home)
        {
            via = Some((base.clone(), found));
        }
    }
    let found = match &via {
        Some((_, found)) => found.clone(),
        None => report(state, days, app.is_some()).await?,
    };
    print!("{}", storage_table(&found, &args.parts));
    if args.parts.is_empty() {
        println!(
            "\nPass --cache, --renders, --temp, --browser or --all to remove some of it \
             (--dry-run shows what would go)."
        );
        println!("ClipCut's data is in {}", state.paths.home.display());
        return Ok(());
    }
    let chosen = found.selected(&args.parts);
    if chosen.is_empty() {
        println!("\nNothing to remove.");
        return Ok(());
    }
    let size = found.bytes(&args.parts);
    if args.dry_run {
        println!(
            "\nWould remove {} ({}):",
            count(chosen.len()),
            human_size(size)
        );
        for item in &chosen {
            println!(
                "  {}  ({})",
                found.home.join(&item.path).display(),
                human_size(item.bytes)
            );
        }
        return Ok(());
    }
    if !args.yes {
        use std::io::{IsTerminal, Write};
        anyhow::ensure!(
            std::io::stdin().is_terminal(),
            "pass --yes to remove without being asked"
        );
        print!(
            "\nRemove {} and free {}? [y/N] ",
            count(chosen.len()),
            human_size(size)
        );
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("Nothing removed.");
            return Ok(());
        }
    }
    let done = match &via {
        Some((base, _)) => {
            let body = serde_json::json!({ "parts": args.parts, "older_than_days": days });
            let value: Value = api(
                reqwest::Method::POST,
                &format!("{base}/storage/clean"),
                Some(body),
            )
            .await
            .context("could not clean up through the running app")?;
            serde_json::from_value::<Report>(value)?
        }
        None => clean(state, &args.parts, days, app.is_some()).await?,
    };
    for row in done.rows.iter().filter(|r| args.parts.contains(&r.part)) {
        for why in &row.kept {
            eprintln!("{}: {why}", row.part.label());
        }
    }
    let renders = done.items.iter().filter(|i| i.project.is_some()).count();
    println!(
        "Removed {}, freeing {}.",
        count(done.items.len()),
        human_size(done.bytes(&Part::ALL))
    );
    if renders > 0 {
        println!("`clipcut render <id>` makes a cleaned project's video again.");
    }
    Ok(())
}

fn count(n: usize) -> String {
    format!("{n} item{}", if n == 1 { "" } else { "s" })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestHome, finished_project};

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    /// Backdate `path` so it counts as a leftover.
    fn age(path: &Path) {
        let then = SystemTime::now() - Duration::from_secs(2 * 3600);
        std::fs::File::options()
            .write(true)
            .open(path)
            .or_else(|_| std::fs::File::open(path))
            .unwrap()
            .set_modified(then)
            .unwrap();
    }

    async fn rendered(state: &AppState, id: &str, days_ago: i64) {
        let mut p = finished_project();
        p.id = id.into();
        p.render.as_mut().unwrap().rendered_at = Utc::now() - chrono::Duration::days(days_ago);
        state.store.create(p).await.unwrap();
        let dir = state.store.dir(id);
        write(&dir.join("render/final.mp4"), 4000);
        write(&dir.join("render/credits.txt"), 100);
        write(&dir.join("voice/voice.wav"), 2000);
    }

    fn paths(items: &[&Item]) -> Vec<String> {
        items.iter().map(|i| i.path.clone()).collect()
    }

    #[test]
    fn days_parse_with_units() {
        assert_eq!(parse_days("30d"), Ok(30));
        assert_eq!(parse_days("2W"), Ok(14));
        assert_eq!(parse_days(" 7 "), Ok(7));
        assert_eq!(parse_days("0d"), Ok(0));
        assert!(parse_days("soon").is_err());
        assert!(parse_days("999999999w").is_err());
    }

    #[tokio::test]
    async fn scan_lists_each_part_and_never_the_settings() {
        let home = TestHome::new();
        let state = home.state();
        let cache = state.paths.cache();
        write(&cache.join("icons/mdi/fire.svg"), 300);
        write(&cache.join("imgflip.json"), 200);
        write(&cache.join("captions/abc/cues.json"), 100);
        let attempt = cache.join("captions/abc/attempt-1f2e");
        write(&attempt.join("abc.en.vtt"), 50);
        age(&attempt);
        write(&cache.join(APP_WINDOW).join("Default/Cookies"), 5000);
        rendered(&state, "20261001-090000-0a1d00", 40).await;
        rendered(&state, "20261101-090000-e5a1e0", 2).await;
        let stale = state
            .store
            .dir("20261001-090000-0a1d00")
            .join("assets/b01.mp4.1234.part");
        write(&stale, 700);
        age(&stale);
        let fresh = state
            .store
            .dir("20261001-090000-0a1d00")
            .join("assets/b02.mp4.5678.part");
        write(&fresh, 700);
        let saved = state.paths.home.join(".clipcut-77.tmp");
        write(&saved, 10);
        age(&saved);
        let model = state.paths.models().join("kokoro.onnx.part");
        write(&model, 900);
        age(&model);
        for keep in ["keys.json", "youtube-oauth.json", "queue.json"] {
            write(&state.paths.home.join(keep), 10);
        }

        let r = scan(&state, 30, false);
        let of = |part: Part| paths(&r.selected(&[part]));
        assert_eq!(
            of(Part::Cache),
            ["cache/captions", "cache/icons", "cache/imgflip.json"]
        );
        assert_eq!(
            of(Part::Renders),
            ["projects/20261001-090000-0a1d00/render"]
        );
        assert_eq!(
            of(Part::Temp),
            [
                ".clipcut-77.tmp",
                "cache/captions/abc/attempt-1f2e",
                "projects/20261001-090000-0a1d00/assets/b01.mp4.1234.part",
            ]
        );
        assert_eq!(of(Part::Browser), ["cache/app-window"]);
        let row = |part: Part| r.rows.iter().find(|row| row.part == part).unwrap().clone();
        assert_eq!(row(Part::Renders).bytes, 4100);
        assert_eq!(row(Part::Browser).bytes, 5000);
        // The attempt inside the cache counts once when both are chosen.
        let both = r.selected(&[Part::Cache, Part::Temp]);
        assert!(!paths(&both).contains(&"cache/captions/abc/attempt-1f2e".to_string()));
        assert_eq!(
            r.bytes(&[Part::Cache, Part::Temp]),
            300 + 200 + 150 + 10 + 700
        );
        // A shorter age picks up the newer render too.
        assert_eq!(scan(&state, 0, false).selected(&[Part::Renders]).len(), 2);
        // With the app running its window may be open.
        let r = scan(&state, 30, true);
        assert!(r.selected(&[Part::Browser]).is_empty());
        assert_eq!(
            row_kept(&r, Part::Browser),
            ["5.0 KB kept while ClipCut is running"]
        );
    }

    fn row_kept(r: &Report, part: Part) -> Vec<String> {
        r.rows
            .iter()
            .find(|row| row.part == part)
            .unwrap()
            .kept
            .clone()
    }

    #[tokio::test]
    async fn busy_projects_keep_their_render_and_the_cache() {
        let home = TestHome::new();
        let state = home.state();
        write(&state.paths.cache().join("imgflip.json"), 200);
        rendered(&state, "20261001-090000-0a1d00", 40).await;
        state
            .store
            .update("20261001-090000-0a1d00", |p| {
                p.stages
                    .iter_mut()
                    .find(|s| s.stage == Stage::Voice)
                    .unwrap()
                    .status = Status::Running;
                Ok(())
            })
            .await
            .unwrap();
        let r = scan(&state, 30, false);
        assert!(r.selected(&[Part::Cache, Part::Renders]).is_empty());
        assert_eq!(
            row_kept(&r, Part::Cache),
            ["200 B kept while 0a1d00 is busy"]
        );
        assert_eq!(row_kept(&r, Part::Renders), ["0a1d00 is busy"]);
        let done = clean(&state, &[Part::Cache, Part::Renders], 30, false)
            .await
            .unwrap();
        assert!(done.items.is_empty());
        assert!(
            state
                .store
                .dir("20261001-090000-0a1d00")
                .join("render/final.mp4")
                .exists()
        );
    }

    #[tokio::test]
    async fn a_cleaned_render_can_be_made_again() {
        let home = TestHome::new();
        let state = home.state();
        let id = "20261001-090000-0a1d00";
        rendered(&state, id, 40).await;
        write(&state.paths.cache().join("imgflip.json"), 200);

        let dry = scan(&state, 30, false);
        let done = clean(&state, &[Part::Renders], 30, false).await.unwrap();
        assert_eq!(
            paths(&done.items.iter().collect::<Vec<_>>()),
            paths(&dry.selected(&[Part::Renders]))
        );
        assert_eq!(done.bytes(&Part::ALL), 4100);

        let dir = state.store.dir(id);
        assert!(!dir.join("render").exists());
        assert!(dir.join("voice/voice.wav").exists());
        assert!(state.paths.cache().join("imgflip.json").exists());
        let p = state.store.get(id).unwrap();
        assert!(p.render.is_none() && !p.beats.is_empty() && p.voice_track.is_some());
        assert_eq!(crate::pipeline::first_unfinished(&p), Some(Stage::Render));
        assert_eq!(crate::cli::project_status(&p), "stopped before render");
    }

    #[test]
    fn the_table_marks_the_chosen_parts() {
        let home = TestHome::new();
        let state = home.state();
        let items = vec![Item {
            part: Part::Temp,
            path: "x.tmp".into(),
            bytes: 2048,
            project: None,
        }];
        let r = Report::new(
            &state,
            30,
            items,
            &[(Part::Browser, "kept while ClipCut is running".into())],
        );
        let table = storage_table(&r, &[Part::Temp]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(
            lines[0].trim_end(),
            " PART     WHAT                          SIZE"
        );
        assert!(
            lines[2].starts_with(" renders  Old renders (over 30 days)"),
            "{table}"
        );
        assert!(lines[3].starts_with("*temp"), "{table}");
        assert!(lines[3].ends_with("2.0 KB  1 item"), "{table}");
        assert!(
            lines[4].ends_with("kept while ClipCut is running"),
            "{table}"
        );
    }
}
