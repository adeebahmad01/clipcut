//! End-to-end renderer demo with generated media.
//!
//!     cargo run -p clipcut-render --example demo -- [--format horizontal] [--fps 12] [--stills-only]
//!
//! Writes everything to /tmp/clipcut-render-smoke/: generated media, the
//! timeline JSON, demo.mp4 and a few stills.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use clipcut_core::config::Format;
use clipcut_core::timeline::Timeline;
use clipcut_core::{
    Asset, AssetKind, Beat, BeatStatus, Config, MemePlan, Paths, Progress, ProgressEvent, Project,
    RectF, Tools, VisualKind, VisualPlan, VoiceTrack, WordTiming,
};
use clipcut_render::{DirectorInputs, Renderer};

const OUT: &str = "/tmp/clipcut-render-smoke";

fn ffmpeg(args: &[&str]) {
    let st = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-threads",
            "2",
            "-filter_threads",
            "1",
        ])
        .args(args)
        .status()
        .expect("ffmpeg");
    assert!(st.success(), "ffmpeg failed: {args:?}");
}

fn exists(p: &Path) -> bool {
    p.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

/// Generate demo media (skipped when already present).
fn make_media(dir: &Path) {
    let a = dir.join("assets");
    std::fs::create_dir_all(&a).unwrap();
    let p = |n: &str| a.join(n).to_string_lossy().into_owned();
    if !exists(&a.join("clip.mp4")) {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1280x720:r=30:d=8",
            "-f",
            "lavfi",
            "-i",
            "sine=f=330:d=8",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            &p("clip.mp4"),
        ]);
    }
    if !exists(&a.join("gif.mp4")) {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "life=s=320x240:mold=10:r=15:ratio=0.12:death_color=#401020:life_color=#ffd23c,scale=480:360:flags=neighbor",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
            &p("gif.mp4"),
        ]);
    }
    if !exists(&a.join("reaction.gif")) {
        ffmpeg(&[
            "-i",
            &p("gif.mp4"),
            "-vf",
            "fps=6,scale=240:180",
            "-loop",
            "0",
            &p("reaction.gif"),
        ]);
    }
    if !exists(&a.join("stock.mp4")) {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "mandelbrot=s=960x540:r=30:start_scale=2:end_scale=0.05",
            "-t",
            "6",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
            &p("stock.mp4"),
        ]);
    }
    if !exists(&a.join("photo.jpg")) {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "gradients=s=1600x1067:c0=0x1d3557:c1=0xe63946:c2=0xf1c453:n=3:seed=7",
            "-frames:v",
            "1",
            &p("photo.jpg"),
        ]);
    }
    if !exists(&a.join("article.png")) {
        article_image(&a.join("article.png"));
    }
    if !exists(&a.join("meme.png")) {
        meme_image(&a.join("meme.png"));
    }
    std::fs::write(
        a.join("bolt.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="currentColor" d="M11 21h-1l1-7H7.5c-.58 0-.57-.32-.38-.66.19-.34.05-.08.07-.12C8.48 10.94 10.42 7.54 13 3h1l-1 7h3.5c.49 0 .56.33.47.51l-.07.15C12.96 17.55 11 21 11 21z"/></svg>"##,
    )
    .unwrap();
    std::fs::write(
        a.join("chip.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect x="14" y="14" width="36" height="36" rx="6" fill="#3a86ff"/><rect x="22" y="22" width="20" height="20" rx="3" fill="#ffbe0b"/><g fill="#8ecae6"><rect x="18" y="4" width="4" height="10"/><rect x="30" y="4" width="4" height="10"/><rect x="42" y="4" width="4" height="10"/><rect x="18" y="50" width="4" height="10"/><rect x="30" y="50" width="4" height="10"/><rect x="42" y="50" width="4" height="10"/><rect x="4" y="18" width="10" height="4"/><rect x="4" y="30" width="10" height="4"/><rect x="4" y="42" width="10" height="4"/><rect x="50" y="18" width="10" height="4"/><rect x="50" y="30" width="10" height="4"/><rect x="50" y="42" width="10" height="4"/></g></svg>"##,
    )
    .unwrap();
    if !exists(&a.join("music.wav")) {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='0.18*sin(2*PI*110*t)*(0.6+0.4*sin(2*PI*2*t))+0.12*sin(2*PI*164.8*t)+0.1*sin(2*PI*220*t)*gt(mod(t,0.5),0.25)|0.18*sin(2*PI*110*t)*(0.6+0.4*sin(2*PI*2*t))+0.12*sin(2*PI*196*t)+0.1*sin(2*PI*220*t)*gt(mod(t,0.5),0.25)':s=48000:d=20",
            &p("music.wav"),
        ]);
    }
}

/// A fake news page: masthead, headline, grey text lines, one highlighted quote.
fn article_image(out: &Path) {
    let (w, h) = (1200u32, 2200u32);
    let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([250, 250, 247, 255]));
    let mut rect = |x: u32, y: u32, rw: u32, rh: u32, c: [u8; 4]| {
        for yy in y..(y + rh).min(h) {
            for xx in x..(x + rw).min(w) {
                img.put_pixel(xx, yy, image::Rgba(c));
            }
        }
    };
    rect(0, 0, w, 120, [180, 20, 30, 255]);
    rect(60, 40, 300, 40, [255, 255, 255, 255]);
    rect(60, 200, 1000, 56, [20, 20, 24, 255]);
    rect(60, 276, 760, 56, [20, 20, 24, 255]);
    rect(60, 380, 1080, 520, [90, 110, 140, 255]);
    let mut y = 960;
    let mut i = 0u32;
    while y < h - 80 {
        let quote = (1260..1420).contains(&y);
        if quote {
            rect(50, y - 12, 1100, 46, [255, 226, 90, 255]);
        }
        let len = 900 + (i * 137 % 180);
        rect(
            60,
            y,
            len,
            22,
            if quote {
                [30, 30, 30, 255]
            } else {
                [150, 150, 155, 255]
            },
        );
        y += 52;
        i += 1;
        if i % 6 == 0 {
            y += 40;
        }
    }
    img.save(out).unwrap();
}

/// A two-panel "reaction" template.
fn meme_image(out: &Path) {
    let (w, h) = (900u32, 900u32);
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let top = y < h / 2;
        let (cx, cy) = (
            if top { 640.0 } else { 640.0 },
            if top { 225.0 } else { 675.0 },
        );
        let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
        if x < 450 {
            let g = (y % 450) as u8 / 3;
            if top {
                image::Rgba([200, 90 + g, 40, 255])
            } else {
                image::Rgba([60, 140 + g / 2, 90, 255])
            }
        } else if d < 150.0 {
            if top {
                image::Rgba([230, 60, 60, 255])
            } else {
                image::Rgba([60, 200, 90, 255])
            }
        } else {
            image::Rgba([235, 235, 230, 255])
        }
    });
    img.save(out).unwrap();
}

struct BeatSpec {
    kind: VisualKind,
    text: &'static str,
    on_screen: Option<&'static str>,
    icon: Option<&'static str>,
    asset: Option<(&'static str, AssetKind)>,
    meme: Option<(&'static str, &'static str, &'static str)>,
    focus: Option<RectF>,
}

fn beats() -> Vec<BeatSpec> {
    use VisualKind::*;
    let b = |kind, text, on_screen, icon, asset| BeatSpec {
        kind,
        text,
        on_screen,
        icon,
        asset,
        meme: None,
        focus: None,
    };
    vec![
        b(
            Youtube,
            "This tiny chip just made every laptop you own look ancient.",
            Some("Every laptop is ancient"),
            None,
            Some(("clip.mp4", AssetKind::Video)),
        ),
        BeatSpec {
            focus: Some(RectF {
                x: 0.04,
                y: 0.56,
                w: 0.92,
                h: 0.1,
            }),
            ..b(
                Article,
                "Engineers say it runs forty times faster while sipping half the power.",
                Some("40x faster"),
                None,
                Some(("article.png", AssetKind::Image)),
            )
        },
        BeatSpec {
            meme: Some(("Reaction", "Me checking if my old laptop still works", "")),
            ..b(
                Meme,
                "Naturally, everyone rushed to check their own machines.",
                None,
                None,
                Some(("meme.png", AssetKind::Image)),
            )
        },
        // Intentionally imported as Image: real GIFs must remain animated.
        b(
            Gif,
            "And the results were honestly painful to watch.",
            Some("Painful"),
            None,
            Some(("reaction.gif", AssetKind::Image)),
        ),
        b(
            Image,
            "The secret is a new kind of transistor stacked like a skyscraper.",
            Some("Stacked transistors"),
            Some("chip.svg"),
            Some(("photo.jpg", AssetKind::Image)),
        ),
        b(
            Stock,
            "Billions of them fit on a sliver smaller than your fingernail.",
            Some("Smaller than a fingernail"),
            None,
            Some(("stock.mp4", AssetKind::Video)),
        ),
        b(
            Text,
            "The first devices ship in 2027, and prices start at 499 dollars.",
            Some("Shipping 2027"),
            Some("bolt.svg"),
            None,
        ),
        BeatSpec {
            meme: Some(("Two panel", "", "my laptop yesterday")),
            ..b(
                Meme,
                "So yes, your laptop is officially a museum piece.",
                None,
                None,
                Some(("gif.mp4", AssetKind::Video)),
            )
        },
    ]
}

/// Synthesize the narration with espeak-ng (sine fallback) and fake word timings.
fn make_voice(dir: &Path, text: &str) -> VoiceTrack {
    let wav = dir.join("voice.wav");
    let ok = Command::new("espeak-ng")
        .args(["-v", "en-us", "-s", "165", "-w"])
        .arg(&wav)
        .arg(text)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "sine=f=220:d=38",
            "-ac",
            "1",
            wav.to_str().unwrap(),
        ]);
    }
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&wav)
        .output()
        .unwrap();
    let dur: f32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(38.0);
    // Weight words by length plus punctuation pauses, spread over the speech.
    let toks: Vec<&str> = text.split_whitespace().collect();
    let weight = |t: &str| t.len() as f32 + 2.5;
    let pause = |t: &str| match t.chars().last() {
        Some('.') | Some('!') | Some('?') => 6.0,
        Some(',') => 3.0,
        _ => 0.0,
    };
    let total: f32 = toks.iter().map(|t| weight(t) + pause(t)).sum();
    let (lead, tail) = (0.12, 0.25);
    let k = (dur - lead - tail) / total;
    let mut t = lead;
    let words: Vec<WordTiming> = toks
        .iter()
        .map(|w| {
            let s = t;
            t += weight(w) * k;
            let e = t;
            t += pause(w) * k;
            WordTiming {
                text: w.to_string(),
                start_s: s,
                end_s: e,
            }
        })
        .collect();
    VoiceTrack {
        path: "voice.wav".into(),
        duration_s: dur,
        sample_rate: 22050,
        engine: "espeak-ng".into(),
        voice: "en-us".into(),
        words,
        sentences: vec![],
    }
}

fn project(dir: &Path, format: Format) -> Project {
    let specs = beats();
    let script: Vec<&str> = specs.iter().map(|b| b.text).collect();
    let voice = make_voice(dir, &script.join(" "));
    let mut wi = 0usize;
    let mut out_beats = Vec::new();
    for (i, s) in specs.iter().enumerate() {
        let n = s.text.split_whitespace().count();
        let first = &voice.words[wi];
        let last = &voice.words[wi + n - 1];
        let next_start = voice
            .words
            .get(wi + n)
            .map(|w| w.start_s)
            .unwrap_or(voice.duration_s);
        let start_s = if i == 0 { 0.0 } else { first.start_s };
        let _ = last;
        wi += n;
        let asset = s.asset.map(|(path, kind)| {
            let full = dir.join("assets").join(path);
            Asset {
                kind,
                path: format!("assets/{path}"),
                provider: "demo".into(),
                source_url: String::new(),
                credit: "generated".into(),
                width: 0,
                height: 0,
                duration_s: None,
                trim_start_s: if path == "clip.mp4" { 1.0 } else { 0.0 },
                focus: s.focus,
            }
            .tap(|_| assert!(full.exists(), "missing {}", full.display()))
        });
        out_beats.push(Beat {
            id: format!("b{:02}", i + 1),
            text: s.text.into(),
            visual: VisualPlan {
                kind: s.kind,
                query: String::new(),
                alt_queries: vec![],
                on_screen_text: s.on_screen.map(str::to_string),
                icon: s.icon.map(str::to_string),
                meme: s.meme.map(|(t, c, bottom)| MemePlan {
                    template: t.into(),
                    caption: c.into(),
                    top_text: if c.is_empty() {
                        "me today".into()
                    } else {
                        String::new()
                    },
                    bottom_text: bottom.into(),
                }),
                quote: None,
                sfx: None,
            },
            candidates: vec![],
            chosen: None,
            asset,
            status: BeatStatus::Ready,
            error: None,
            locked: false,
            start_s,
            end_s: next_start,
        });
    }
    if let Some(b) = out_beats.last_mut() {
        b.end_s = voice.duration_s;
    }
    let now = chrono::Utc::now();
    Project {
        id: "demo-project-0001".into(),
        created_at: now,
        updated_at: now,
        topic: "A new chip".into(),
        notes: String::new(),
        format,
        target_seconds: 40,
        voice: None,
        autopilot: true,
        stages: vec![],
        research: None,
        hooks: vec![],
        chosen_hook: None,
        script: None,
        beats: out_beats,
        voice_track: Some(voice),
        render: None,
        caption_preset: None,
        extra_formats: vec![],
        published: vec![],
        kind: Default::default(),
        clip: None,
    }
}

trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}
impl<T> Tap for T {}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let horizontal = args.iter().any(|a| a == "horizontal");
    let stills_only = args.iter().any(|a| a == "--stills-only");
    let format = if horizontal {
        Format::Horizontal
    } else {
        Format::Vertical
    };
    let tag = if horizontal { "h" } else { "v" };

    let dir = PathBuf::from(OUT);
    std::fs::create_dir_all(&dir)?;
    make_media(&dir);
    let project = project(&dir, format);

    let mut config = Config::default();
    config.video.format = format;
    if let Some(i) = args.iter().position(|a| a == "--fps") {
        config.video.fps = args
            .get(i + 1)
            .ok_or_else(|| anyhow::anyhow!("--fps needs a value"))?
            .parse()?;
    }
    let paths = Paths::resolve(Some(dir.join("home")));
    let tools = Tools::detect(&config, &paths);
    let renderer = Renderer::new(config, paths, tools);
    let inputs = DirectorInputs {
        icons: project
            .beats
            .iter()
            .filter_map(|beat| {
                beat.visual
                    .icon
                    .as_ref()
                    .map(|icon| (beat.id.clone(), dir.join("assets").join(icon)))
            })
            .collect::<HashMap<_, _>>(),
        music: Some(dir.join("assets/music.wav")),
    };

    let t0 = std::time::Instant::now();
    let tl: Timeline = renderer.direct(&project, &dir, &inputs)?;
    println!(
        "directed {} layers, {} sfx, {} caption words in {:.0} ms; duration {:.2}s",
        tl.layers.len(),
        tl.audio.sfx.len(),
        tl.captions.len(),
        t0.elapsed().as_secs_f32() * 1000.0,
        tl.duration_s
    );
    std::fs::write(
        dir.join(format!("timeline-{tag}.json")),
        serde_json::to_string_pretty(&tl)?,
    )?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        // Stills: inside each beat plus a few transition moments.
        let mut times: Vec<f32> = project
            .beats
            .iter()
            .map(|b| b.start_s + (b.end_s - b.start_s) * 0.45)
            .collect();
        times.extend([0.35, 1.0]);
        for t in times {
            let png = dir.join(format!("still-{tag}-{:05.2}.png", t));
            let s = std::time::Instant::now();
            renderer.still(&tl, t, &png).await?;
            println!(
                "still {} ({:.0} ms)",
                png.display(),
                s.elapsed().as_secs_f32() * 1000.0
            );
        }
        if stills_only {
            return anyhow::Ok(());
        }
        let last = Arc::new(std::sync::Mutex::new(0.0f32));
        let l2 = last.clone();
        let progress = Progress::new(
            project.id.clone(),
            Arc::new(move |e: ProgressEvent| {
                let p = e.progress.unwrap_or(-1.0);
                let mut last = l2.lock().unwrap();
                if p < 0.0 || p - *last >= 0.1 || p >= 1.0 {
                    *last = p.max(*last);
                    println!("[{:?}] {:>5.1}% {}", e.level, p.max(0.0) * 100.0, e.message);
                }
            }),
        );
        let out = dir.join(format!("demo{}.mp4", if horizontal { "-h" } else { "" }));
        let s = std::time::Instant::now();
        renderer.render(&tl, &out, &progress).await?;
        let secs = s.elapsed().as_secs_f32();
        println!(
            "rendered {} in {:.1}s for {:.1}s of video = {:.2}x real time",
            out.display(),
            secs,
            tl.duration_s,
            secs / tl.duration_s
        );
        anyhow::Ok(())
    })?;
    Ok(())
}
