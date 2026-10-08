//! Heuristic sensitivity check: compare full-sentence word starts with the
//! duration of independently synthesized prefixes. Prefix prosody changes, so
//! these differences are NOT forced-alignment accuracy or ground truth.
//!
//! cargo run -p clipcut-tts --example align_check -- --engine kokoro --expect-engine kokoro
//! Supports the same engine/voice/speed/output options as `say`.

mod common;

use anyhow::ensure;
use clipcut_core::Progress;
use clipcut_tts::{normalize::normalize_words, split_sentences};
use common::{ALIGNMENT, Options, verify_and_save};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = Options::parse("align-check.wav")?;
    let tts = options.tts();
    let progress = Progress::log_only("align-check");
    let sentences = split_sentences(&options.text);
    println!("alignment: {ALIGNMENT}");
    println!(
        "Prefix comparison is only a sensitivity check; standalone prefix prosody is not timing ground truth."
    );
    let mut differences = Vec::new();
    let mut comparisons = Vec::new();
    let stem = options
        .out
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let path = |name: &str| options.directory().join(format!("{stem}-{name}.wav"));
    for (index, sentence) in sentences.iter().enumerate() {
        let words = normalize_words(sentence);
        ensure!(
            words.len() <= 64,
            "prefix smoke is limited to 64 words per sentence"
        );
        let full_path = path(&format!("sentence-{index}"));
        let full = tts
            .synthesize(
                &[sentence.clone()],
                options.voice.as_deref(),
                options.directory(),
                &full_path,
                &progress,
            )
            .await?;
        verify_and_save(
            &full,
            &[sentence.clone()],
            &full_path,
            options.expected_engine.as_deref(),
        )?;
        let base = full.sentences[0].start_s;
        for k in 1..words.len() {
            let prefix = words[..k]
                .iter()
                .map(|w| w.original.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let prefix_path = path(&format!("sentence-{index}-prefix-{k}"));
            let prefix_sentences = [prefix];
            let track = tts
                .synthesize(
                    &prefix_sentences,
                    options.voice.as_deref(),
                    options.directory(),
                    &prefix_path,
                    &progress,
                )
                .await?;
            verify_and_save(
                &track,
                &prefix_sentences,
                &prefix_path,
                options.expected_engine.as_deref(),
            )?;
            ensure!(
                track.engine == full.engine,
                "engine changed during prefix comparison"
            );
            let prefix_len = track.sentences[0].end_s - track.sentences[0].start_s;
            let ours = full.words[k].start_s - base;
            let difference = ours - prefix_len;
            differences.push(difference.abs());
            comparisons.push(serde_json::json!({
                "sentence": index, "next_word": words[k].original,
                "estimated_start_s": ours, "standalone_prefix_s": prefix_len,
                "difference_s": difference,
            }));
            println!(
                "{:>12}  estimate {:5.3}  standalone-prefix {:5.3}  diff {:+.3}",
                words[k].original, ours, prefix_len, difference
            );
        }
    }
    ensure!(
        !differences.is_empty(),
        "prefix comparison needs at least two speakable words"
    );
    differences.sort_by(f32::total_cmp);
    let summary = serde_json::json!({
        "alignment_method": ALIGNMENT,
        "ground_truth": false,
        "comparison_count": differences.len(),
        "median_difference_s": differences[differences.len() / 2],
        "p90_difference_s": differences[differences.len() * 9 / 10],
        "max_difference_s": differences.last().unwrap(),
        "comparisons": comparisons,
    });
    std::fs::write(
        options.out.with_extension("json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("Heuristic sensitivity summary (NOT accuracy): {summary}");
    Ok(())
}
