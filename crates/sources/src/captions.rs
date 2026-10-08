//! WebVTT parsing (incl. YouTube's rolling auto-captions) and the keyword
//! moment heuristic.

use std::sync::LazyLock;

use clipcut_core::CaptionCue;
use regex::Regex;

use crate::util::{content_words, words};

static TS_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(\d{1,2}:)?\d{2}:\d{2}[.,]\d{3}>").unwrap());
static ANY_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());

fn parse_ts(s: &str) -> Option<f32> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    if parts.iter().any(|p| p.is_empty() || p.chars().any(|c| !c.is_ascii_digit() && c != '.')) {
        return None;
    }
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (h.parse::<u32>().ok()? as f32, m.parse::<u32>().ok()? as f32, s.parse::<f32>().ok()?),
        [m, s] => (0.0, m.parse::<u32>().ok()? as f32, s.parse::<f32>().ok()?),
        _ => return None,
    };
    let result = h * 3600.0 + m * 60.0 + sec;
    (m < 60.0 && (0.0..60.0).contains(&sec) && result.is_finite() && result <= 86_400.0).then_some(result)
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn clean_line(line: &str) -> String {
    let no_tags = ANY_TAG.replace_all(line, "");
    decode_entities(&no_tags).split_whitespace().collect::<Vec<_>>().join(" ")
}

struct RawCue {
    start: f32,
    end: f32,
    lines: Vec<String>,
}

fn raw_cues(src: &str) -> Vec<RawCue> {
    let src = src.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = Vec::new();
    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        if line == "STYLE" || line == "REGION" || line == "NOTE" || line.starts_with("NOTE ") {
            for next in lines.by_ref() {
                if next.trim().is_empty() { break; }
            }
            continue;
        }
        if !line.contains("-->") {
            continue;
        }
        let mut parts = line.splitn(2, "-->");
        let start = parts.next().and_then(parse_ts);
        let end = parts.next().and_then(|rest| rest.split_whitespace().next()).and_then(parse_ts);
        let (Some(start), Some(end)) = (start, end) else { continue };
        if end <= start { continue; }
        let mut text = Vec::new();
        while let Some(next) = lines.peek() {
            // YouTube sometimes emits a space-only first line before the
            // spoken line. It is not a cue separator until text has begun.
            if !next.is_empty() && next.trim().is_empty() && text.is_empty() {
                lines.next();
                continue;
            }
            if next.trim().is_empty() || next.contains("-->") {
                break;
            }
            text.push(next.to_string());
            lines.next();
        }
        if text.iter().map(String::len).sum::<usize>() <= 16_384 {
            out.push(RawCue { start, end, lines: text });
        }
        if out.len() >= 25_000 { break; }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// Parse WebVTT into cues where every spoken word appears once.
///
/// YouTube auto-captions "roll": each cue repeats the previous line above the
/// new one (with `<00:00:01.234><c> word</c>` timing tags) and 10 ms cues
/// re-show the settled line. Only the newest line of each real cue is kept.
pub fn parse_vtt(src: &str) -> Vec<CaptionCue> {
    if src.len() > 4 * 1024 * 1024 { return Vec::new(); }
    let raw = raw_cues(src);
    let rolling = raw.iter().any(|c| c.lines.iter().any(|l| TS_TAG.is_match(l)));
    let mut out: Vec<CaptionCue> = Vec::new();
    if rolling {
        let mut last_text = String::new();
        for cue in &raw {
            if cue.end - cue.start <= 0.05 {
                continue;
            }
            let Some(line) = cue.lines.iter().rev().find(|l| !clean_line(l).is_empty()) else { continue };
            let text = clean_line(line);
            if text == last_text && out.last().is_some_and(|c| cue.start <= c.end_s + 0.2) {
                continue;
            }
            // First timing tag marks the second word; the first starts with the cue.
            let start = cue.start;
            let end = cue.end.max(start + 0.1);
            if let Some(prev) = out.last_mut() {
                if prev.end_s > start {
                    prev.end_s = start.max(prev.start_s);
                }
            }
            out.push(CaptionCue { start_s: start, end_s: end, text: text.clone() });
            last_text = text;
        }
    } else {
        let mut prev_lines: Vec<String> = Vec::new();
        for cue in &raw {
            let lines: Vec<String> = cue.lines.iter().map(|l| clean_line(l)).filter(|l| !l.is_empty()).collect();
            // Drop leading lines repeated from the previous cue (rolling without tags).
            let mut skip = 0;
            while skip < lines.len() && prev_lines.contains(&lines[skip])
                && out.last().is_some_and(|c| cue.start <= c.end_s + 0.2) {
                skip += 1;
            }
            let fresh = &lines[skip..];
            if !lines.is_empty() {
                prev_lines = lines.clone();
            }
            if fresh.is_empty() {
                if let Some(prev) = out.last_mut() {
                    prev.end_s = prev.end_s.max(cue.end);
                }
                continue;
            }
            let text = fresh.join(" ");
            if let Some(prev) = out.last_mut() {
                if prev.text == text && cue.start <= prev.end_s + 0.2 {
                    prev.end_s = prev.end_s.max(cue.end);
                    continue;
                }
            }
            out.push(CaptionCue { start_s: cue.start, end_s: cue.end, text });
        }
    }
    out.retain(|c| c.end_s > c.start_s && !c.text.trim().is_empty());
    out
}

/// Keyword-overlap sliding window. Quote words weigh 3, quote word pairs in
/// order add 2, beat words weigh 1. Returns `(start, start + clip_seconds)`.
pub fn best_moment(cues: &[CaptionCue], beat_text: &str, quote: Option<&str>, clip_seconds: f32) -> Option<(f32, f32)> {
    if cues.is_empty() || !clip_seconds.is_finite() {
        return None;
    }
    let clip = clip_seconds.clamp(1.0, 3.0);
    let mut cues: Vec<_> = cues.iter().filter(|c|
        c.start_s.is_finite() && c.end_s.is_finite() && c.start_s >= 0.0 && c.end_s > c.start_s
        && !c.text.trim().is_empty()).collect();
    cues.sort_by(|a,b| a.start_s.total_cmp(&b.start_s));
    let last_end = cues.iter().map(|c| c.end_s).max_by(f32::total_cmp)?;
    if last_end < clip { return None; }
    let quote_words: Vec<String> = quote.map(content_words).unwrap_or_default();
    let quote_all: Vec<String> = quote.map(words).unwrap_or_default();
    let quote_pairs: Vec<(String, String)> =
        quote_all.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect();
    let beat_words: Vec<String> = content_words(beat_text);

    let mut best: Option<(f32, f32)> = None; // (score, start)
    for i in 0..cues.len() {
        let w_start = cues[i].start_s;
        let w_end = w_start + clip;
        let mut tokens: Vec<String> = Vec::new();
        for c in cues[i..].iter().take_while(|c| c.start_s < w_end) {
            tokens.extend(words(&c.text));
        }
        let set: std::collections::HashSet<&String> = tokens.iter().collect();
        let mut score = 0.0f32;
        let mut seen = std::collections::HashSet::new();
        for w in &quote_words {
            if set.contains(w) && seen.insert(w.clone()) {
                score += 3.0;
            }
        }
        for (a, b) in &quote_pairs {
            if tokens.windows(2).any(|t| &t[0] == a && &t[1] == b) {
                score += 2.0;
            }
        }
        let mut seen_beat = std::collections::HashSet::new();
        for w in &beat_words {
            if set.contains(w) && seen_beat.insert(w.clone()) {
                score += 1.0;
            }
        }
        if score <= 0.0 {
            continue;
        }
        // Anchor on the first quote word inside the first cue, interpolated.
        let mut start = w_start;
        let first_tokens = words(&cues[i].text);
        if let Some(first_q) = quote_all.iter().find(|w| quote_words.contains(w)) {
            if let Some(p) = first_tokens.iter().position(|t| t == first_q) {
                let span = cues[i].end_s - cues[i].start_s;
                start = w_start + span * p as f32 / first_tokens.len().max(1) as f32;
            }
        }
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, start));
        }
    }
    let (score, start) = best?;
    if score < 2.0 {
        return None;
    }
    let start = (start - 0.1).clamp(0.0, last_end - clip);
    Some((start, start + clip))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLLING: &str = "WEBVTT
Kind: captions
Language: en

00:00:00.000 --> 00:00:02.350 align:start position:0%
\x20
the<00:00:00.480><c> James</c><00:00:00.960><c> Webb</c><00:00:01.400><c> telescope</c>

00:00:02.350 --> 00:00:02.360 align:start position:0%
the James Webb telescope


00:00:02.360 --> 00:00:05.000 align:start position:0%
the James Webb telescope
just<00:00:02.800><c> found</c><00:00:03.100><c> something</c><00:00:03.500><c> strange</c>

00:00:05.000 --> 00:00:05.010 align:start position:0%
just found something strange


00:00:05.010 --> 00:00:07.000 align:start position:0%
just found something strange
in<00:00:05.400><c> deep</c><00:00:05.800><c> space</c>
";

    #[test]
    fn rolling_auto_captions_are_deduped() {
        let cues = parse_vtt(ROLLING);
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["the James Webb telescope", "just found something strange", "in deep space"]);
        assert_eq!(cues[1].start_s, 2.36);
        assert!(cues.windows(2).all(|w| w[0].end_s <= w[1].start_s + 1e-3));
        let all: Vec<String> = cues.iter().flat_map(|c| words(&c.text)).collect();
        assert_eq!(all.iter().filter(|w| *w == "webb").count(), 1);
    }

    #[test]
    fn plain_vtt_with_entities_and_repeats() {
        let src = "WEBVTT\n\n1\n00:01.000 --> 00:03.000\n<v Speaker>Hello &amp; welcome</v>\n\n2\n00:03.000 --> 00:05.000\nHello &amp; welcome\nto the show\n\n3\n00:05.000 --> 00:06.000\nto the show\n";
        let cues = parse_vtt(src);
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["Hello & welcome", "to the show"]);
        assert_eq!(cues[0].start_s, 1.0);
        assert_eq!(cues[1].end_s, 6.0);
    }

    #[test]
    fn heuristic_prefers_quote() {
        let cues = vec![
            CaptionCue { start_s: 0.0, end_s: 3.0, text: "welcome back to the channel telescope".into() },
            CaptionCue { start_s: 3.0, end_s: 6.0, text: "today we look at the telescope".into() },
            CaptionCue { start_s: 6.0, end_s: 9.0, text: "it found carbon dioxide on an exoplanet".into() },
            CaptionCue { start_s: 9.0, end_s: 12.0, text: "which nobody expected".into() },
        ];
        let (s, e) = best_moment(&cues, "The telescope saw something", Some("carbon dioxide on an exoplanet"), 2.0).unwrap();
        assert!((5.8..=7.0).contains(&s), "{s}");
        assert!((e - s - 2.0).abs() < 1e-4);
        assert!(best_moment(&cues, "pizza recipes", Some("mozzarella"), 2.0).is_none());
    }

    #[test]
    fn rejects_bad_timing_and_comment_blocks() {
        let src = "WEBVTT\n\nNOTE fake cue\n00:01.000 --> 00:03.000\nignored\n\n00:03.000 --> 00:02.000\nbackwards\n\n00:NaN --> 00:04.000\nnan\n\n00:61.000 --> 01:05.000\ninvalid seconds\n\n-1:00.000 --> 00:04.000\nnegative\n\n00:01.000 --> 00:02.000\nvalid\n";
        let cues = parse_vtt(src);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "valid");
        assert!(best_moment(&cues, "valid word", None, f32::NAN).is_none());
        let (s,e) = best_moment(&cues, "valid", Some("valid"), 6.0).unwrap_or((0.0, 3.0));
        assert!((1.0..=3.0).contains(&(e-s)));
    }
}
