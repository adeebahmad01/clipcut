//! End-to-end smoke run of the text pipeline against the real CLIs:
//! research → hooks → script (+ unslop) → beats, then one of each pick.
//!
//!   cargo run -p clipcut-llm --example llm_smoke -- "Why octopuses have three hearts"
//!
//! Env overrides: CLIPCUT_LLM_PROVIDER, CLIPCUT_LLM_MODEL, CLIPCUT_LLM_FAST_MODEL,
//! CLIPCUT_SKIP_UNSLOP=1, CLIPCUT_SECONDS (default 45), CLIPCUT_SKIP_PICKS=1.
//! CLIPCUT_RESEARCH_CONTEXT must name a JSON file of app-fetched SourceRef
//! records. This example never enables browsing or continues without evidence.

use std::sync::Arc;
use std::time::Instant;

use clipcut_core::{CaptionCue, Config, Paths, Progress, ProgressEvent, SourceRef, Tools};
use clipcut_llm::{CandidateBrief, Llm, beats::beats_match_script, kind_mix};

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let topic = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let topic = if topic.trim().is_empty() {
        "Why octopuses have three hearts".to_string()
    } else {
        topic
    };
    let paths = Paths::resolve(None);
    let mut config = Config::load(&paths.config_file()).unwrap_or_default();
    if let Some(v) = env("CLIPCUT_LLM_PROVIDER") {
        config.llm.provider = v;
    }
    if let Some(v) = env("CLIPCUT_LLM_MODEL") {
        config.llm.model = v;
    }
    if let Some(v) = env("CLIPCUT_LLM_FAST_MODEL") {
        config.llm.fast_model = v;
    }
    if env("CLIPCUT_SKIP_UNSLOP").is_some() {
        config.llm.skip_unslop = true;
    }
    let seconds: u32 = env("CLIPCUT_SECONDS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(45);
    let tools = Tools::detect(&config, &paths);
    println!(
        "topic: {topic}\nprovider={} model={} fast_model={} unslop={} claude={:?} codex={:?}\n",
        config.llm.provider,
        config.llm.model,
        config.llm.fast_model,
        if config.llm.skip_unslop {
            "off".to_string()
        } else {
            config.llm.unslop_preset.clone()
        },
        tools.claude,
        tools.codex
    );

    let start = Instant::now();
    let progress = Progress::new(
        "smoke",
        Arc::new(move |e: ProgressEvent| {
            println!(
                "  [{:>6.1}s] {:?}: {}",
                start.elapsed().as_secs_f32(),
                e.level,
                e.message
            )
        }),
    );
    let llm = Llm::new(config, tools);
    let mut timings: Vec<(&str, f32)> = Vec::new();

    let t = Instant::now();
    let context_file = env("CLIPCUT_RESEARCH_CONTEXT").ok_or_else(|| {
        anyhow::anyhow!(
            "set CLIPCUT_RESEARCH_CONTEXT to app-fetched SourceRef JSON before this live smoke"
        )
    })?;
    let sources: Vec<SourceRef> = serde_json::from_slice(&std::fs::read(context_file)?)?;
    let research = llm
        .research_with_context(&topic, "", &sources, &progress)
        .await?;
    timings.push(("research", t.elapsed().as_secs_f32()));
    println!("\n== RESEARCH ==\n{}\n", research.summary);
    for f in &research.facts {
        println!(
            "- {}\n    {}",
            f.text,
            f.source_url.as_deref().unwrap_or("(no url)")
        );
    }
    println!(
        "angles: {:#?}\nkeywords: {}\n",
        research.angles,
        research.keywords.join(", ")
    );

    let t = Instant::now();
    let hooks = llm.hooks(&topic, "", &research, &progress).await?;
    timings.push(("hooks", t.elapsed().as_secs_f32()));
    println!("\n== HOOKS ==");
    for h in &hooks {
        println!(
            "[{:.1}] {}  ({} words)\n      angle: {}\n      why: {}",
            h.score,
            h.text,
            clipcut_llm::text::word_count(&h.text),
            h.angle,
            h.why
        );
    }

    let t = Instant::now();
    let script = llm
        .script(&topic, "", &research, &hooks[0].text, seconds, &progress)
        .await?;
    timings.push(("script+unslop", t.elapsed().as_secs_f32()));
    let wc = clipcut_llm::text::word_count;
    println!(
        "\n== SCRIPT ({} words, target {}) ==\n{}\n",
        wc(&script.text),
        clipcut_llm::word_budget(seconds).0,
        script.text
    );
    if script.draft != script.text {
        println!(
            "-- draft ({} words) --\n{}\n",
            wc(&script.draft),
            script.draft
        );
    }
    println!(
        "title: {} ({} chars)\ndescription: {}\nhashtags: {:?}",
        script.title,
        script.title.chars().count(),
        script.description,
        script.hashtags
    );
    println!("sentences ({}):", script.sentences.len());
    for s in &script.sentences {
        println!("  | {s}");
    }
    assert_eq!(
        script.sentences.join(" "),
        script.text,
        "sentences must join to the text"
    );

    let t = Instant::now();
    let beats = llm.beats(&topic, &script, &research, &progress).await?;
    timings.push(("beats", t.elapsed().as_secs_f32()));
    println!("\n== BEATS ({}: {}) ==", beats.len(), kind_mix(&beats));
    for (i, b) in beats.iter().enumerate() {
        let v = &b.visual;
        println!(
            "{:>2}. {}\n      {:?} \"{}\" alt={:?} text={:?} icon={:?} quote={:?} sfx={:?}{}",
            i + 1,
            b.text,
            v.kind,
            v.query,
            v.alt_queries,
            v.on_screen_text,
            v.icon,
            v.quote,
            v.sfx,
            v.meme
                .as_ref()
                .map(|m| format!(" meme=[{} | {}]", m.template, m.caption))
                .unwrap_or_default()
        );
    }
    println!(
        "beats join == script: {}",
        beats_match_script(&script.text, &beats)
    );

    if env("CLIPCUT_SKIP_PICKS").is_none() {
        let beat = &beats[0].text;
        let cues: Vec<CaptionCue> = [
            "hey everyone welcome back to the channel",
            "today we're diving deep with an octopus",
            "this octopus has three hearts",
            "two hearts pump blood through the gills",
            "the third heart pumps it around the body",
            "and when it swims that heart stops beating",
            "don't forget to subscribe",
        ]
        .iter()
        .enumerate()
        .map(|(i, t)| CaptionCue {
            start_s: i as f32 * 4.0,
            end_s: i as f32 * 4.0 + 4.0,
            text: t.to_string(),
        })
        .collect();
        let t = Instant::now();
        let moment = llm
            .pick_moment(
                beat,
                Some("three hearts"),
                "Octopus anatomy explained",
                &cues,
                3.0,
            )
            .await;
        timings.push(("pick_moment", t.elapsed().as_secs_f32()));
        println!("\npick_moment({beat:?}) -> {moment:?}");

        let templates: Vec<String> = [
            "Drake Hotline Bling",
            "Two Buttons",
            "Distracted Boyfriend",
            "Surprised Pikachu",
            "This Is Fine",
        ]
        .map(String::from)
        .to_vec();
        let t = Instant::now();
        let meme = llm
            .pick_meme(
                "Swimming literally stops its heart, so it would rather crawl.",
                None,
                &templates,
            )
            .await;
        timings.push(("pick_meme", t.elapsed().as_secs_f32()));
        println!("pick_meme -> {meme:?}");

        let candidates = vec![
            CandidateBrief {
                index: 0,
                title: "TOP 10 SCARIEST SEA MONSTERS (reaction)".into(),
                provider: "youtube".into(),
                detail: "compilation".into(),
            },
            CandidateBrief {
                index: 1,
                title: "Octopus heart anatomy | Monterey Bay Aquarium".into(),
                provider: "youtube".into(),
                detail: "4:12, official channel".into(),
            },
            CandidateBrief {
                index: 2,
                title: "Heart-shaped cookies recipe".into(),
                provider: "youtube".into(),
                detail: String::new(),
            },
        ];
        let t = Instant::now();
        let pick = llm
            .pick_candidate(
                "An octopus has three hearts.",
                "octopus three hearts anatomy",
                &candidates,
            )
            .await;
        timings.push(("pick_candidate", t.elapsed().as_secs_f32()));
        println!("pick_candidate -> {pick:?}");
    }

    println!("\n== TIMINGS ==");
    for (stage, secs) in &timings {
        println!("{stage:>15}: {secs:>6.1} s");
    }
    println!("{:>15}: {:>6.1} s", "total", start.elapsed().as_secs_f32());
    Ok(())
}
