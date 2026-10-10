//! SRT and WebVTT caption files built from word timings, for uploading next
//! to the video or loading into another editor.

use crate::model::{CaptionCue, WordTiming};

/// Longest cue line, the usual broadcast limit.
const MAX_CHARS: usize = 42;
const MAX_SECONDS: f32 = 5.0;
/// A pause this long starts a new cue.
const BREAK_GAP_S: f32 = 0.6;
const MIN_SECONDS: f32 = 0.5;

/// Group words into readable one-line cues. Cues break at sentence ends,
/// long pauses, and the length limits, and never overlap.
pub fn cues_from_words(words: &[WordTiming]) -> Vec<CaptionCue> {
    let words: Vec<&WordTiming> = words
        .iter()
        .filter(|w| w.start_s.is_finite() && w.end_s.is_finite() && w.end_s >= w.start_s && w.start_s >= 0.0)
        .filter(|w| !clean(&w.text).is_empty())
        .collect();
    let mut cues: Vec<CaptionCue> = Vec::new();
    let mut current: Option<CaptionCue> = None;
    for (i, w) in words.iter().enumerate() {
        let text = clean(&w.text);
        if let Some(cue) = current.as_mut() {
            let fits = cue.text.chars().count() + 1 + text.chars().count() <= MAX_CHARS
                && w.end_s - cue.start_s <= MAX_SECONDS
                && w.start_s - cue.end_s < BREAK_GAP_S;
            if fits {
                cue.text.push(' ');
                cue.text.push_str(&text);
                cue.end_s = cue.end_s.max(w.end_s);
            } else {
                cues.push(current.take().unwrap());
            }
        }
        if current.is_none() {
            current = Some(CaptionCue { start_s: w.start_s, end_s: w.end_s, text });
        }
        let ends_sentence = w.text.trim_end().ends_with(['.', '!', '?']);
        let last = i + 1 == words.len();
        if ends_sentence || last {
            cues.extend(current.take());
        }
    }
    // Hold short cues on screen a little longer, but never into the next one.
    for i in 0..cues.len() {
        let next = cues.get(i + 1).map(|c| c.start_s);
        let cue = &mut cues[i];
        let mut end = cue.end_s.max(cue.start_s + MIN_SECONDS);
        if let Some(next) = next {
            end = end.min(next);
        }
        cue.end_s = end.max(cue.start_s);
    }
    cues
}

/// SubRip text.
pub fn to_srt(cues: &[CaptionCue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            stamp(c.start_s, ','),
            stamp(c.end_s, ','),
            clean(&c.text)
        ));
    }
    out
}

/// WebVTT text.
pub fn to_vtt(cues: &[CaptionCue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for c in cues {
        out.push_str(&format!("{} --> {}\n{}\n\n", stamp(c.start_s, '.'), stamp(c.end_s, '.'), clean(&c.text)));
    }
    out
}

fn stamp(seconds: f32, separator: char) -> String {
    let ms = (f64::from(seconds.max(0.0)) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02}{separator}{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// One line with no cue-timing arrow, which would break the file.
fn clean(text: &str) -> String {
    text.replace("-->", "->").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(spec: &[(&str, f32, f32)]) -> Vec<WordTiming> {
        spec.iter().map(|&(t, s, e)| WordTiming { text: t.into(), start_s: s, end_s: e }).collect()
    }

    #[test]
    fn breaks_at_sentences_and_pauses() {
        let w = words(&[
            ("Coffee", 0.0, 0.3),
            ("is", 0.35, 0.5),
            ("a", 0.5, 0.6),
            ("fruit.", 0.6, 1.0),
            ("Seriously,", 1.1, 1.5),
            ("a", 1.5, 1.6),
            ("berry.", 2.5, 2.9),
        ]);
        let cues = cues_from_words(&w);
        let text: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, ["Coffee is a fruit.", "Seriously, a", "berry."]);
        assert_eq!(cues[0].start_s, 0.0);
        assert_eq!(cues[0].end_s, 1.0);
        for pair in cues.windows(2) {
            assert!(pair[0].end_s <= pair[1].start_s);
        }
    }

    #[test]
    fn long_runs_split_by_length() {
        let w: Vec<WordTiming> = (0..40)
            .map(|i| WordTiming { text: "word".into(), start_s: i as f32 * 0.2, end_s: i as f32 * 0.2 + 0.15 })
            .collect();
        let cues = cues_from_words(&w);
        assert!(cues.len() > 3);
        assert!(cues.iter().all(|c| c.text.chars().count() <= MAX_CHARS));
        assert!(cues.iter().all(|c| c.end_s - c.start_s <= MAX_SECONDS + 0.01));
        assert_eq!(cues.iter().map(|c| c.text.split(' ').count()).sum::<usize>(), 40);
    }

    #[test]
    fn writes_srt_and_vtt() {
        let cues = vec![
            CaptionCue { start_s: 0.0, end_s: 1.25, text: "Hello --> there".into() },
            CaptionCue { start_s: 3661.5, end_s: 3662.0, text: "Line\nbreak".into() },
        ];
        assert_eq!(
            to_srt(&cues),
            "1\n00:00:00,000 --> 00:00:01,250\nHello -> there\n\n2\n01:01:01,500 --> 01:01:02,000\nLine break\n\n"
        );
        assert!(to_vtt(&cues).starts_with("WEBVTT\n\n00:00:00.000 --> 00:00:01.250\nHello -> there\n\n"));
    }

    #[test]
    fn ignores_malformed_and_empty_words() {
        let w = words(&[("", 0.0, 0.1), ("one", f32::NAN, 1.0), ("two", 0.2, 0.1), ("three", 0.3, 0.4)]);
        let cues = cues_from_words(&w);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "three");
        assert!(cues[0].end_s >= cues[0].start_s + MIN_SECONDS - 1e-6);
        assert!(cues_from_words(&[]).is_empty());
    }
}
