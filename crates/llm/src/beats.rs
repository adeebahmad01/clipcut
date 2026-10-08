//! Beat-sheet validation and deterministic repair.
//!
//! The model is asked to copy the script verbatim into beats. When it slips
//! (fixes a typo, drops a word, merges punctuation), we re-cut the exact script
//! text at the model's beat boundaries using a word alignment; when the
//! alignment is too poor we re-chunk the script into ~6-word beats and carry
//! the planned visuals over by best word overlap.

use clipcut_core::{MemePlan, VisualKind, VisualPlan};

use crate::BeatPlan;
use crate::text::{norm_word, normalize_ws, split_sentences, truncate_words};

/// How the beat texts were reconciled with the script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeatRepair {
    /// Beats reproduced the script exactly (after whitespace normalization).
    Exact,
    /// Small differences; beat boundaries were mapped onto the exact script words.
    Realigned,
    /// The model's beats diverged too far; the script was re-chunked.
    Rechunked,
}

/// Longest beat (in words) before it gets split in two.
const MAX_BEAT_WORDS: usize = 12;
/// Below this fraction of aligned script words, re-chunk instead of realigning.
const MIN_COVERAGE: f32 = 0.6;

/// Do the beats, joined with single spaces, reproduce `script` exactly?
pub fn beats_match_script(script: &str, beats: &[BeatPlan]) -> bool {
    let joined = beats
        .iter()
        .map(|b| normalize_ws(&b.text))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    joined == normalize_ws(script)
}

/// Make the beats reproduce `script` exactly. Visual plans are preserved.
pub fn reconcile_beats(script: &str, beats: Vec<BeatPlan>) -> (Vec<BeatPlan>, BeatRepair) {
    let script = normalize_ws(script);
    let beats: Vec<BeatPlan> = beats
        .into_iter()
        .map(|mut b| {
            b.text = normalize_ws(&b.text);
            b
        })
        .filter(|b| !b.text.is_empty())
        .collect();
    if script.is_empty() {
        return (Vec::new(), BeatRepair::Exact);
    }
    if beats.is_empty() {
        return (rechunk(&script, &[]), BeatRepair::Rechunked);
    }
    if beats_match_script(&script, &beats) {
        return (split_long(beats), BeatRepair::Exact);
    }
    match realign(&script, &beats) {
        Some(realigned) => (split_long(realigned), BeatRepair::Realigned),
        None => (rechunk(&script, &beats), BeatRepair::Rechunked),
    }
}

/// Map each script word to a beat through an LCS alignment of normalized words.
fn realign(script: &str, beats: &[BeatPlan]) -> Option<Vec<BeatPlan>> {
    let script_tokens: Vec<&str> = script.split(' ').collect();
    let s: Vec<String> = script_tokens.iter().map(|t| norm_word(t)).collect();
    let mut b: Vec<String> = Vec::new();
    let mut owner: Vec<usize> = Vec::new();
    for (i, beat) in beats.iter().enumerate() {
        for tok in beat.text.split(' ') {
            let w = norm_word(tok);
            if !w.is_empty() {
                b.push(w);
                owner.push(i);
            }
        }
    }
    let spoken = s.iter().filter(|w| !w.is_empty()).count();
    if spoken == 0 || b.is_empty() {
        return None;
    }

    // LCS table over (script words, beat words); empty script tokens never match.
    let (n, m) = (s.len(), b.len());
    if n.checked_add(1)?.checked_mul(m.checked_add(1)?)? > 2_000_000 {
        return None;
    }
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if !s[i].is_empty() && s[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let matched = dp[0][0] as usize;
    if (matched as f32) < MIN_COVERAGE * spoken as f32 {
        return None;
    }
    let mut assign: Vec<Option<usize>> = vec![None; n];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if !s[i].is_empty() && s[i] == b[j] {
            assign[i] = Some(owner[j]);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    // Unmatched words join the previous beat (leading ones join the first).
    let first = assign.iter().flatten().next().copied().unwrap_or(0);
    let mut last = first;
    let mut groups: Vec<(usize, Vec<&str>)> = Vec::new();
    for (k, tok) in script_tokens.iter().enumerate() {
        let beat = assign[k].unwrap_or(last);
        last = beat;
        match groups.last_mut() {
            Some((idx, words)) if *idx == beat => words.push(tok),
            _ => groups.push((beat, vec![tok])),
        }
    }
    Some(
        groups
            .into_iter()
            .map(|(idx, words)| BeatPlan {
                text: words.join(" "),
                visual: beats[idx].visual.clone(),
            })
            .collect(),
    )
}

/// Split beats longer than `MAX_BEAT_WORDS`; later parts reuse the plan with
/// the next alternative query so the same visual isn't fetched twice.
fn split_long(beats: Vec<BeatPlan>) -> Vec<BeatPlan> {
    let mut out = Vec::with_capacity(beats.len());
    for beat in beats {
        let tokens: Vec<&str> = beat.text.split(' ').collect();
        if tokens.len() <= MAX_BEAT_WORDS {
            out.push(beat);
            continue;
        }
        let parts = tokens.len().div_ceil(7);
        for (p, chunk) in chunk_tokens(&tokens, parts).into_iter().enumerate() {
            let mut visual = beat.visual.clone();
            if p > 0 {
                rotate_query(&mut visual, p);
            }
            out.push(BeatPlan {
                text: chunk,
                visual,
            });
        }
    }
    out
}

fn rotate_query(visual: &mut VisualPlan, part: usize) {
    if let Some(alt) = visual
        .alt_queries
        .get((part - 1) % visual.alt_queries.len().max(1))
        .cloned()
    {
        let old = std::mem::replace(&mut visual.query, alt.clone());
        visual.alt_queries.retain(|q| q != &alt);
        visual.alt_queries.push(old);
    }
    // A second copy of the same overlay/punchline would look like a glitch.
    visual.on_screen_text = None;
}

/// Split tokens into `parts` nearly equal chunks, nudging each cut to just
/// after a comma/semicolon/dash when one is within two words.
fn chunk_tokens(tokens: &[&str], parts: usize) -> Vec<String> {
    let parts = parts.clamp(1, tokens.len().max(1));
    let mut out = Vec::with_capacity(parts);
    let mut start = 0;
    for p in 1..=parts {
        let ideal = (tokens.len() * p).div_ceil(parts).max(start + 1);
        let mut cut = ideal.min(tokens.len());
        if p < parts {
            let soft = |k: usize| {
                tokens
                    .get(k - 1)
                    .is_some_and(|t| t.ends_with([',', ';', ':', '—', '-']))
            };
            let candidates = [
                ideal,
                ideal + 1,
                ideal.saturating_sub(1),
                ideal + 2,
                ideal.saturating_sub(2),
            ];
            if let Some(&k) = candidates
                .iter()
                .find(|&&k| k > start + 1 && k < tokens.len() && soft(k))
            {
                cut = k;
            }
        }
        if cut <= start {
            continue;
        }
        out.push(tokens[start..cut].join(" "));
        start = cut;
    }
    if start < tokens.len() {
        match out.last_mut() {
            Some(last) => {
                last.push(' ');
                last.push_str(&tokens[start..].join(" "));
            }
            None => out.push(tokens.join(" ")),
        }
    }
    out
}

/// Re-chunk the script into ~6-word beats inside sentence boundaries and carry
/// over visual plans from `planned` by best word overlap (ties: nearest position).
fn rechunk(script: &str, planned: &[BeatPlan]) -> Vec<BeatPlan> {
    let mut chunks: Vec<String> = Vec::new();
    for sentence in split_sentences(script) {
        let tokens: Vec<&str> = sentence.split(' ').collect();
        let parts = if tokens.len() <= 8 {
            1
        } else {
            tokens.len().div_ceil(6)
        };
        chunks.extend(chunk_tokens(&tokens, parts));
    }
    // Merge 1-2 word fragments into the previous chunk when it stays short.
    let mut merged: Vec<String> = Vec::new();
    for chunk in chunks {
        let words = chunk.split(' ').count();
        match merged.last_mut() {
            Some(prev) if words <= 2 && prev.split(' ').count() + words <= 8 => {
                prev.push(' ');
                prev.push_str(&chunk);
            }
            _ => merged.push(chunk),
        }
    }

    let planned_words: Vec<Vec<String>> = planned
        .iter()
        .map(|b| {
            b.text
                .split_whitespace()
                .map(norm_word)
                .filter(|w| w.len() > 2)
                .collect()
        })
        .collect();
    let total = merged.len().max(1);
    let mut used_last: Option<usize> = None;
    merged
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let visual = if planned.is_empty() {
                text_plan(&text)
            } else {
                let words: Vec<String> = text
                    .split_whitespace()
                    .map(norm_word)
                    .filter(|w| w.len() > 2)
                    .collect();
                let rel = i as f32 / total as f32;
                let best = (0..planned.len())
                    .map(|k| {
                        let overlap = words
                            .iter()
                            .filter(|w| planned_words[k].contains(w))
                            .count() as f32;
                        let pos = k as f32 / planned.len() as f32;
                        let reuse_penalty = if used_last == Some(k) { 0.5 } else { 0.0 };
                        (k, overlap - reuse_penalty - (pos - rel).abs())
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(k, _)| k)
                    .unwrap_or(0);
                let mut visual = planned[best].visual.clone();
                if used_last == Some(best) {
                    rotate_query(&mut visual, 1);
                }
                used_last = Some(best);
                visual
            };
            BeatPlan { text, visual }
        })
        .collect()
}

/// Kinetic-text plan for a beat that has nothing better.
pub fn text_plan(text: &str) -> VisualPlan {
    VisualPlan {
        kind: VisualKind::Text,
        query: truncate_words(text, 6),
        alt_queries: Vec::new(),
        on_screen_text: Some(truncate_words(text, 5)),
        icon: None,
        meme: None,
        quote: None,
        sfx: None,
    }
}

/// Tidy a model-produced plan: trim strings, cap lengths, fill missing queries.
pub fn sanitize_plan(beat_text: &str, mut visual: VisualPlan) -> VisualPlan {
    let clean = |s: String| {
        Some(normalize_ws(&s)).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null"))
    };
    visual.query = normalize_ws(&visual.query).trim_matches('"').to_string();
    if visual.query.is_empty() {
        visual.query = truncate_words(beat_text, 6);
    }
    visual.alt_queries = visual
        .alt_queries
        .into_iter()
        .filter_map(clean)
        .filter(|q| !q.eq_ignore_ascii_case(&visual.query))
        .take(2)
        .collect();
    visual.on_screen_text = visual
        .on_screen_text
        .and_then(clean)
        .map(|t| truncate_words(&t, 5));
    visual.icon = visual.icon.and_then(clean);
    visual.quote = visual.quote.and_then(clean);
    visual.meme = visual
        .meme
        .filter(|m| !m.template.trim().is_empty() || !m.caption.trim().is_empty());
    if visual.kind == VisualKind::Meme && visual.meme.is_none() {
        visual.meme = Some(MemePlan {
            template: visual.query.clone(),
            caption: truncate_words(beat_text, 12),
            ..Default::default()
        });
    }
    if visual.kind != VisualKind::Meme {
        visual.meme = None;
    }
    if visual.kind == VisualKind::Text && visual.on_screen_text.is_none() {
        visual.on_screen_text = Some(truncate_words(beat_text, 5));
    }
    visual
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(kind: VisualKind, query: &str) -> VisualPlan {
        VisualPlan {
            kind,
            query: query.into(),
            alt_queries: vec![format!("{query} alt")],
            on_screen_text: None,
            icon: None,
            meme: None,
            quote: None,
            sfx: None,
        }
    }

    fn beat(text: &str, kind: VisualKind, query: &str) -> BeatPlan {
        BeatPlan {
            text: text.into(),
            visual: plan(kind, query),
        }
    }

    const SCRIPT: &str = "Octopuses have three hearts. Two pump blood through the gills. The third one stops when they swim. So they'd rather crawl.";

    fn joined(beats: &[BeatPlan]) -> String {
        beats
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn exact_beats_pass_through() {
        let beats = vec![
            beat(
                "Octopuses have three hearts.",
                VisualKind::Youtube,
                "octopus",
            ),
            beat(
                "Two pump blood  through the gills.",
                VisualKind::Text,
                "gills",
            ),
            beat(
                "The third one stops when they swim.",
                VisualKind::Gif,
                "stop",
            ),
            beat(
                "So they'd rather crawl.",
                VisualKind::Youtube,
                "octopus crawling",
            ),
        ];
        let (out, how) = reconcile_beats(SCRIPT, beats);
        assert_eq!(how, BeatRepair::Exact);
        assert_eq!(joined(&out), SCRIPT);
        assert_eq!(out[1].text, "Two pump blood through the gills.");
    }

    #[test]
    fn small_drift_is_realigned_onto_exact_script() {
        // Model changed punctuation, dropped "one" and added "really".
        let beats = vec![
            beat(
                "Octopuses have three hearts!",
                VisualKind::Youtube,
                "octopus",
            ),
            beat(
                "Two pump blood through the gills",
                VisualKind::Text,
                "gills",
            ),
            beat(
                "The third stops when they really swim.",
                VisualKind::Gif,
                "stop",
            ),
            beat(
                "So they would rather crawl.",
                VisualKind::Youtube,
                "octopus crawling",
            ),
        ];
        let (out, how) = reconcile_beats(SCRIPT, beats);
        assert_eq!(how, BeatRepair::Realigned);
        assert_eq!(joined(&out), SCRIPT);
        assert_eq!(out.len(), 4);
        assert_eq!(out[2].text, "The third one stops when they swim.");
        assert_eq!(out[2].visual.query, "stop");
        assert_eq!(out[3].visual.query, "octopus crawling");
    }

    #[test]
    fn garbage_is_rechunked_with_visuals_carried_over() {
        let beats = vec![
            beat(
                "Hearts: three of them.",
                VisualKind::Image,
                "octopus hearts diagram",
            ),
            beat(
                "Crawling beats swimming for octopuses.",
                VisualKind::Youtube,
                "octopus crawling",
            ),
        ];
        let (out, how) = reconcile_beats(SCRIPT, beats);
        assert_eq!(how, BeatRepair::Rechunked);
        assert_eq!(joined(&out), SCRIPT);
        assert!(out.iter().all(|b| b.text.split(' ').count() <= 8));
        assert_eq!(out[0].visual.query, "octopus hearts diagram");
        assert!(out.last().unwrap().visual.query.contains("crawling"));
    }

    #[test]
    fn empty_beats_become_text_beats() {
        let (out, how) = reconcile_beats(SCRIPT, vec![]);
        assert_eq!(how, BeatRepair::Rechunked);
        assert_eq!(joined(&out), SCRIPT);
        assert!(out.iter().all(|b| b.visual.kind == VisualKind::Text));
    }

    #[test]
    fn long_beats_are_split() {
        let long =
            "One two three four five six seven eight nine ten eleven twelve thirteen fourteen.";
        let (out, how) = reconcile_beats(long, vec![beat(long, VisualKind::Youtube, "numbers")]);
        assert_eq!(how, BeatRepair::Exact);
        assert_eq!(out.len(), 2);
        assert_eq!(joined(&out), long);
        assert_eq!(out[0].visual.query, "numbers");
        assert_eq!(out[1].visual.query, "numbers alt");
    }

    #[test]
    fn sanitizes_plans() {
        let mut p = plan(VisualKind::Meme, "Two Buttons");
        p.on_screen_text = Some("way too many words for an overlay".into());
        p.icon = Some("  ".into());
        let p = sanitize_plan("Swim or crawl, tough choice.", p);
        assert_eq!(p.on_screen_text.as_deref(), Some("way too many words for"));
        assert!(p.icon.is_none());
        assert_eq!(p.meme.as_ref().unwrap().template, "Two Buttons");
    }
}
