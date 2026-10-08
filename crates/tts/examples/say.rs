//! Real-engine smoke: synthesize narration, validate waveform/timings, save JSON.
//!
//! cargo run -p clipcut-tts --example say -- --engine kokoro --expect-engine kokoro
//! cargo run -p clipcut-tts --example say -- --engine espeak --voice en-us+m3
//! Set CLIPCUT_KOKORO_VARIANT=fp16|int8|fp32 to pick the installed model file.
//! Default artifacts: /tmp/clipcut-tts-smoke/say.{wav,json}.

mod common;

use std::time::Instant;

use clipcut_core::Progress;
use clipcut_tts::split_sentences;
use common::{ALIGNMENT, Options, verify_and_save};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    let options = Options::parse("say.wav")?;
    let tts = options.tts();
    let sentences = split_sentences(&options.text);
    let progress = Progress::log_only("say");
    println!("health: {:?}", tts.health());
    println!("alignment: {ALIGNMENT}");

    // No warm-up WAV: an empty/failed request must not leave behind a
    // successful-looking "Ready." artifact. This measures the actual first
    // call, including model loading.
    let start = Instant::now();
    let track = tts
        .synthesize(
            &sentences,
            options.voice.as_deref(),
            options.directory(),
            &options.out,
            &progress,
        )
        .await?;
    let elapsed = start.elapsed().as_secs_f32();
    let validation = verify_and_save(
        &track,
        &sentences,
        &options.out,
        options.expected_engine.as_deref(),
    )?;
    println!(
        "\nengine {} voice {} -> {} ({:.3}s @ {} Hz)",
        track.engine,
        track.voice,
        options.out.display(),
        track.duration_s,
        track.sample_rate
    );
    for sentence in &track.sentences {
        println!(
            "  sentence {:7.3}-{:7.3}  {}",
            sentence.start_s, sentence.end_s, sentence.text
        );
    }
    for word in &track.words {
        println!("  {:7.3}-{:7.3}  {}", word.start_s, word.end_s, word.text);
    }
    println!("\nvalidated: {validation:?}");
    println!(
        "synthesis {:.3}s, first-call RTF {:.3}; evidence {}",
        elapsed,
        elapsed / track.duration_s,
        options.out.with_extension("json").display()
    );
    Ok(())
}
