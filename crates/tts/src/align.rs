//! Word timing estimation for synthesized speech. TTS engines here do not
//! report durations, so words are placed by phoneme weight and then pinned to
//! what the audio shows: pauses in the energy envelope anchor punctuation and
//! long gaps, and remaining boundaries snap to nearby energy dips.
//!
//! This is approximate acoustic timing, NOT forced alignment. No ASR or
//! acoustic phoneme recognizer matches the supplied transcript to the WAV.

use crate::audio::{HOP_S, silence_threshold};

#[derive(Debug, Clone, Copy)]
pub struct AlignWord {
    /// Relative spoken length (phoneme weight).
    pub weight: f32,
    /// Punctuation after the word: 0 none, 1 comma-like, 2 sentence end.
    pub break_after: u8,
}

/// A pause found in the envelope (seconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gap {
    pub start: f32,
    pub end: f32,
}

impl Gap {
    fn mid(&self) -> f32 {
        (self.start + self.end) * 0.5
    }
    fn len(&self) -> f32 {
        self.end - self.start
    }
}

const MIN_GAP_S: f32 = 0.07;
const STRONG_GAP_S: f32 = 0.10;
const MIN_WORD_S: f32 = 0.04;
const SNAP_FRAMES: isize = 5;

/// Speech extent and internal pauses of an envelope. Pauses are runs below
/// a -30 dB (relative) threshold; their edges are then pulled in to where
/// the energy actually starts/stops (-36 dB), so soft onsets and releases
/// stay with their words.
pub fn find_gaps(env: &[f32]) -> (f32, f32, Vec<Gap>) {
    let th = silence_threshold(env);
    let gap_th = th * 3.0;
    let edge_th = th * 1.5;
    let Some(first) = env.iter().position(|v| v.is_finite() && *v > th) else {
        return (0.0, 0.0, Vec::new());
    };
    let last = env
        .iter()
        .rposition(|v| v.is_finite() && *v > th)
        .unwrap_or(first);
    let mut gaps = Vec::new();
    let mut run_start: Option<usize> = None;
    for (i, v) in env.iter().enumerate().take(last + 1).skip(first) {
        if *v < gap_th {
            run_start.get_or_insert(i);
        } else if let Some(s) = run_start.take() {
            if (i - s) as f32 * HOP_S < MIN_GAP_S - 1e-4 {
                continue;
            }
            let (mut a, mut b) = (s, i);
            while a + 1 < b && env[a] > edge_th {
                a += 1;
            }
            while b > a + 1 && env[b - 1] > edge_th {
                b -= 1;
            }
            gaps.push(Gap {
                start: a as f32 * HOP_S,
                end: b as f32 * HOP_S,
            });
        }
    }
    (first as f32 * HOP_S, (last + 1) as f32 * HOP_S, gaps)
}

/// Monotonic matching of punctuation boundaries to pauses that prefers long
/// pauses near the estimate (a comma usually owns the longest nearby pause).
fn match_punctuation(
    punct: &[usize],
    est: &[f32],
    gaps: &[Gap],
    tolerance: f32,
) -> Vec<Option<usize>> {
    let (p, g) = (punct.len(), gaps.len());
    let score = |i: usize, j: usize| {
        let dist = (gaps[j].mid() - est[punct[i]]).abs();
        if dist > tolerance {
            return f32::NEG_INFINITY;
        }
        0.2 + 2.5 * gaps[j].len().min(0.6) - 0.8 * dist
    };
    // dp[i][j]: best total using the first i boundaries and first j gaps.
    let mut dp = vec![vec![0f32; g + 1]; p + 1];
    let mut choice = vec![vec![0u8; g + 1]; p + 1]; // 0 skip boundary, 1 skip gap, 2 match
    for i in 1..=p {
        for j in 0..=g {
            let mut best = (dp[i - 1][j], 0u8);
            if j > 0 {
                if dp[i][j - 1] > best.0 {
                    best = (dp[i][j - 1], 1);
                }
                let s = score(i - 1, j - 1);
                if s > 0.0 && dp[i - 1][j - 1] + s > best.0 {
                    best = (dp[i - 1][j - 1] + s, 2);
                }
            }
            dp[i][j] = best.0;
            choice[i][j] = best.1;
        }
    }
    let mut out = vec![None; p];
    let (mut i, mut j) = (p, g);
    while i > 0 {
        match choice[i][j] {
            2 => {
                out[i - 1] = Some(j - 1);
                i -= 1;
                j -= 1;
            }
            1 => j -= 1,
            _ => i -= 1,
        }
    }
    out
}

/// Place `words` in a chunk whose envelope is `env` (frames of [`HOP_S`]).
/// Returns (start, end) per word, monotonic and non-overlapping, covering
/// the speech from its onset to its release.
pub fn align(env: &[f32], words: &[AlignWord]) -> Vec<(f32, f32)> {
    let n = words.len();
    if n == 0 {
        return Vec::new();
    }
    let (s0, s1, gaps) = find_gaps(env);
    if s1 <= s0 {
        return Vec::new();
    }
    if n == 1 || s1 - s0 < MIN_WORD_S * n as f32 {
        return even_split(s0, s1, words);
    }

    // Weight-proportional estimate of each boundary on a time axis that skips
    // the detected pauses (speech does not advance during them).
    let est = estimate_boundaries(s0, s1, &gaps, words);

    // Anchor punctuation boundaries to pauses.
    let mut anchors: Vec<Option<Gap>> = vec![None; n - 1];
    let mut used = vec![false; gaps.len()];
    let tolerance = (0.25 * (s1 - s0)).max(0.5);
    let punct: Vec<usize> = (0..n - 1).filter(|&b| words[b].break_after > 0).collect();
    for (k, m) in match_punctuation(&punct, &est, &gaps, tolerance)
        .into_iter()
        .enumerate()
    {
        if let Some(g) = m {
            anchors[punct[k]] = Some(gaps[g]);
            used[g] = true;
        }
    }

    // Long unclaimed pauses almost always sit between words: give each to the
    // nearest free boundary that keeps anchors in order.
    let mut order: Vec<usize> = (0..gaps.len())
        .filter(|&g| !used[g] && gaps[g].len() >= STRONG_GAP_S)
        .collect();
    order.sort_by(|&a, &b| gaps[b].len().partial_cmp(&gaps[a].len()).unwrap());
    for g in order {
        let gap = gaps[g];
        let candidate = (0..n - 1)
            .filter(|&b| anchors[b].is_none())
            .filter(|&b| {
                anchors[..b]
                    .iter()
                    .flatten()
                    .all(|a| a.end <= gap.start + 1e-4)
                    && anchors[b + 1..]
                        .iter()
                        .flatten()
                        .all(|a| a.start >= gap.end - 1e-4)
            })
            .min_by(|&a, &b| {
                (est[a] - gap.mid())
                    .abs()
                    .partial_cmp(&(est[b] - gap.mid()).abs())
                    .unwrap()
            });
        if let Some(b) = candidate {
            if (est[b] - gap.mid()).abs() <= 0.35 {
                anchors[b] = Some(gap);
            }
        }
    }

    // Distribute the words between anchors by weight.
    let mut starts = vec![0f32; n];
    let mut ends = vec![0f32; n];
    let mut seg_first = 0usize;
    let mut seg_t0 = s0;
    for b in 0..=n - 1 {
        let (seg_end_t, next_t0) = if b == n - 1 {
            (s1, s1)
        } else if let Some(gap) = anchors[b] {
            (gap.start, gap.end)
        } else {
            continue;
        };
        let ws = &words[seg_first..=b];
        let total: f32 = ws.iter().map(weight).sum();
        let span = (seg_end_t - seg_t0).max(0.0);
        let min_word = MIN_WORD_S.min(span / ws.len() as f32);
        let weighted_span = (span - min_word * ws.len() as f32).max(0.0);
        let mut t = seg_t0;
        for (k, w) in ws.iter().enumerate() {
            let i = seg_first + k;
            starts[i] = t;
            t += min_word + weighted_span * weight(w) / total;
            ends[i] = t;
        }
        ends[b] = seg_end_t;
        seg_first = b + 1;
        seg_t0 = next_t0;
    }

    // Snap free boundaries to the quietest frame nearby.
    let th = silence_threshold(env);
    for b in 0..n - 1 {
        if anchors[b].is_some() {
            continue;
        }
        let t = ends[b];
        let centre = (t / HOP_S).round() as isize;
        let mut best = (centre, f32::INFINITY);
        for f in centre - SNAP_FRAMES..=centre + SNAP_FRAMES {
            if f < 0 || f as usize >= env.len() {
                continue;
            }
            // Prefer dips, lightly biased toward the estimate.
            let energy = env[f as usize];
            let v = if energy.is_finite() {
                energy.max(th)
            } else {
                th
            };
            let v = v * (1.0 + 0.04 * (f - centre).abs() as f32);
            if v < best.1 {
                best = (f, v);
            }
        }
        let snapped = best.0 as f32 * HOP_S;
        let lo = starts[b] + MIN_WORD_S;
        let hi = ends[b + 1] - MIN_WORD_S;
        if lo <= hi {
            let t = snapped.clamp(lo, hi);
            ends[b] = t;
            starts[b + 1] = t;
        }
    }

    // Final pass: strictly ordered, minimum length, inside [s0, s1].
    let mut out = Vec::with_capacity(n);
    let mut prev_end = s0;
    for i in 0..n {
        let start = starts[i].max(prev_end);
        let end = ends[i].max(start + MIN_WORD_S);
        out.push((start, end));
        prev_end = end;
    }
    if let Some(last) = out.last_mut() {
        last.1 = last.1.max(s1);
    }
    // If minimum lengths pushed past the end, compress back proportionally.
    let over = out.last().map(|l| l.1).unwrap_or(s1);
    if over > s1 {
        let scale = (s1 - s0) / (over - s0);
        for w in &mut out {
            w.0 = s0 + (w.0 - s0) * scale;
            w.1 = s0 + (w.1 - s0) * scale;
        }
    }
    if let Some(last) = out.last_mut() {
        last.1 = s1;
    }
    out
}

fn even_split(s0: f32, s1: f32, words: &[AlignWord]) -> Vec<(f32, f32)> {
    // Very short audio cannot hold MIN_WORD_S per word. Reserve half of each
    // equal share instead: even extreme weights must not round a word down to
    // zero duration or push the boundary beyond the actual clip.
    let total: f64 = words.iter().map(|w| weight(w) as f64).sum();
    let span = (s1 - s0) as f64;
    let minimum = (MIN_WORD_S as f64).min(span / (2.0 * words.len() as f64));
    let weighted_span = (span - minimum * words.len() as f64).max(0.0);
    let mut t = s0 as f64;
    let mut out = Vec::with_capacity(words.len());
    for w in words {
        let start = t as f32;
        t += minimum + weighted_span * weight(w) as f64 / total;
        out.push((start, (t as f32).min(s1)));
    }
    if let Some(last) = out.last_mut() {
        last.1 = s1;
    }
    out
}

fn weight(word: &AlignWord) -> f32 {
    if word.weight.is_finite() && word.weight > 0.0 {
        word.weight.clamp(0.05, 1e6)
    } else {
        1.0
    }
}

fn estimate_boundaries(s0: f32, s1: f32, gaps: &[Gap], words: &[AlignWord]) -> Vec<f32> {
    let pause: f32 = gaps.iter().map(|g| g.len()).sum();
    let speech = (s1 - s0 - pause).max(1e-3);
    let total: f32 = words.iter().map(weight).sum();
    let mut cum = 0.0;
    let mut out = Vec::with_capacity(words.len().saturating_sub(1));
    for w in &words[..words.len() - 1] {
        cum += weight(w);
        // Speech-time offset -> wall time, stepping over pauses.
        let mut remaining = speech * cum / total;
        let mut t = s0;
        for g in gaps {
            let before = g.start - t;
            if remaining <= before {
                break;
            }
            remaining -= before;
            t = g.end;
        }
        // A boundary that lands exactly at a pause belongs in it.
        out.push((t + remaining).min(s1));
    }
    // Snap estimates that fall just after a pause back into it.
    for e in &mut out {
        if let Some(g) = gaps.iter().find(|g| (*e - g.end).abs() < 0.03) {
            *e = g.mid();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Envelope from (seconds, loud?) segments.
    fn env(parts: &[(f32, bool)]) -> Vec<f32> {
        let mut e = Vec::new();
        for (secs, loud) in parts {
            let frames = (secs / HOP_S).round() as usize;
            e.extend((0..frames).map(|i| {
                if *loud {
                    0.2 + 0.05 * ((i % 7) as f32 / 7.0)
                } else {
                    0.0005
                }
            }));
        }
        e
    }

    fn check_valid(t: &[(f32, f32)]) {
        for w in t {
            assert!(w.1 > w.0, "{t:?}");
        }
        for p in t.windows(2) {
            assert!(p[1].0 >= p[0].1 - 1e-5, "{t:?}");
        }
    }

    #[test]
    fn comma_anchors_to_pause() {
        // "Hello there, my friend": pause after "there".
        let e = env(&[
            (0.05, false),
            (0.6, true),
            (0.25, false),
            (0.7, true),
            (0.05, false),
        ]);
        let words = [
            AlignWord {
                weight: 3.0,
                break_after: 0,
            },
            AlignWord {
                weight: 3.0,
                break_after: 1,
            },
            AlignWord {
                weight: 2.0,
                break_after: 0,
            },
            AlignWord {
                weight: 4.0,
                break_after: 2,
            },
        ];
        let t = align(&e, &words);
        check_valid(&t);
        assert!((t[0].0 - 0.05).abs() < 0.02, "{t:?}");
        assert!((t[1].1 - 0.65).abs() < 0.02, "{t:?}");
        assert!((t[2].0 - 0.90).abs() < 0.02, "{t:?}");
        assert!((t[3].1 - 1.60).abs() < 0.02, "{t:?}");
    }

    #[test]
    fn unmarked_long_gap_becomes_word_boundary() {
        let e = env(&[(0.4, true), (0.15, false), (0.4, true), (0.4, true)]);
        let words = [
            AlignWord {
                weight: 1.0,
                break_after: 0,
            },
            AlignWord {
                weight: 1.0,
                break_after: 0,
            },
            AlignWord {
                weight: 1.0,
                break_after: 0,
            },
        ];
        let t = align(&e, &words);
        check_valid(&t);
        assert!((t[0].1 - 0.40).abs() < 0.02, "{t:?}");
        assert!((t[1].0 - 0.55).abs() < 0.02, "{t:?}");
    }

    #[test]
    fn snaps_to_dips() {
        // Two words of equal weight but unequal length, with a short dip
        // (shorter than a pause) between them.
        let mut e = env(&[(0.7, true), (0.04, false), (0.3, true)]);
        e[0] = 0.0005;
        let words = [
            AlignWord {
                weight: 1.0,
                break_after: 0,
            },
            AlignWord {
                weight: 1.0,
                break_after: 0,
            },
        ];
        let t = align(&e, &words);
        check_valid(&t);
        // Estimate is ~0.52 s; the dip at 0.70-0.74 is outside the snap
        // window, so the boundary stays near the estimate.
        assert!((t[0].1 - 0.52).abs() < 0.08, "{t:?}");
        let e2 = env(&[(0.5, true), (0.04, false), (0.5, true)]);
        let t2 = align(&e2, &words);
        assert!((t2[0].1 - 0.51).abs() < 0.03, "{t2:?}");
    }

    #[test]
    fn comma_prefers_the_real_pause() {
        // A short dip right at the estimate, and the actual comma pause a bit later.
        let e = env(&[
            (0.8, true),
            (0.08, false),
            (0.3, true),
            (0.4, false),
            (0.8, true),
        ]);
        let words = [
            AlignWord {
                weight: 4.0,
                break_after: 0,
            },
            AlignWord {
                weight: 1.0,
                break_after: 1,
            },
            AlignWord {
                weight: 4.0,
                break_after: 0,
            },
        ];
        let t = align(&e, &words);
        check_valid(&t);
        assert!((t[1].1 - 1.18).abs() < 0.02, "{t:?}");
        assert!((t[2].0 - 1.58).abs() < 0.02, "{t:?}");
    }

    #[test]
    fn degenerate_inputs() {
        assert!(align(&env(&[(0.5, true)]), &[]).is_empty());
        let t = align(
            &env(&[(0.02, true)]),
            &[AlignWord {
                weight: 1.0,
                break_after: 0,
            }; 3],
        );
        assert_eq!(t.len(), 3);
        check_valid(&t);
        let silent = align(
            &vec![0.0; 50],
            &[AlignWord {
                weight: 1.0,
                break_after: 0,
            }],
        );
        assert!(silent.is_empty());
        assert!(
            align(
                &[],
                &[AlignWord {
                    weight: 1.0,
                    break_after: 0
                }]
            )
            .is_empty()
        );
    }

    #[test]
    fn short_audio_does_not_invent_duration() {
        let e = env(&[(0.02, true)]);
        let t = align(
            &e,
            &[AlignWord {
                weight: 1.0,
                break_after: 0,
            }; 3],
        );
        check_valid(&t);
        assert!(
            t.last().unwrap().1 <= e.len() as f32 * HOP_S + 1e-6,
            "{t:?}"
        );
    }

    #[test]
    fn nonfinite_weights_do_not_create_invalid_timings() {
        let e = env(&[(0.4, true)]);
        let words = [
            AlignWord {
                weight: f32::INFINITY,
                break_after: 0,
            },
            AlignWord {
                weight: f32::NAN,
                break_after: 0,
            },
            AlignWord {
                weight: -1.0,
                break_after: 0,
            },
        ];
        let t = align(&e, &words);
        check_valid(&t);
        assert!(t.iter().all(|(s, e)| s.is_finite() && e.is_finite()));
        assert!(t.last().unwrap().1 <= 0.4 + 1e-6, "{t:?}");
    }

    #[test]
    fn timing_bounds_hold_for_short_audio_and_extreme_weights() {
        for frames in [1, 2, 5, 10, 50, 200, 500] {
            for count in [1, 2, 3, 8, 32, 64] {
                for with_gaps in [false, true] {
                    let e: Vec<f32> = (0..frames)
                        .map(|i| {
                            if with_gaps && i % 40 >= 25 {
                                0.0001
                            } else {
                                0.2
                            }
                        })
                        .collect();
                    let words: Vec<AlignWord> = (0..count)
                        .map(|i| AlignWord {
                            weight: [f32::MAX, 1e-30, f32::NAN, 1.0][i % 4],
                            break_after: if i % 3 == 1 { 1 } else { 0 },
                        })
                        .collect();
                    let t = align(&e, &words);
                    assert_eq!(t.len(), count);
                    check_valid(&t);
                    let (_, end, _) = find_gaps(&e);
                    assert!(
                        t.iter()
                            .all(|(s, e)| s.is_finite() && e.is_finite() && *s >= 0.0 && *e <= end),
                        "{frames} frames / {count} words: {t:?} exceeds {end}"
                    );
                }
            }
        }
    }
}
