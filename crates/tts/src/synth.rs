//! The narration pipeline shared by every engine: normalize, phonemize, cut
//! into chunks, synthesize, trim, align words, place with pauses, master.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use clipcut_core::{Progress, WordTiming};

use crate::align::{AlignWord, align};
use crate::audio;
use crate::kokoro::{self, SAMPLE_RATE};
use crate::normalize::{SpokenWord, normalize_words};
use crate::phonemize::{self, MAX_TOKENS};
use crate::voices::VoicePack;

/// A ready-to-run engine.
pub(crate) enum Engine {
    Kokoro {
        model: PathBuf,
        voice: VoicePack,
        british: bool,
    },
    Espeak {
        exe: PathBuf,
        voice: String,
    },
    Command {
        template: String,
        ffmpeg: Option<PathBuf>,
    },
}

impl Engine {
    pub fn name(&self) -> &'static str {
        match self {
            Engine::Kokoro { .. } => "kokoro",
            Engine::Espeak { .. } => "espeak",
            Engine::Command { .. } => "command",
        }
    }
}

#[derive(Debug)]
struct ChunkPlan {
    /// Word indices within the sentence.
    words: Range<usize>,
    /// Spoken text with punctuation (espeak / command engines).
    text: String,
    /// Kokoro phonemes.
    phonemes: String,
    /// Pause after this chunk, before speed scaling.
    pause_after: f32,
}

struct SentencePlan {
    words: Vec<SpokenWord>,
    weights: Vec<f32>,
    chunks: Vec<ChunkPlan>,
}

pub(crate) struct Rendered {
    pub samples: Vec<f32>,
    pub words: Vec<WordTiming>,
    pub sentences: Vec<WordTiming>,
}

const LEAD_IN_S: f32 = 0.08;
const TAIL_S: f32 = 0.30;

fn break_strength(p: Option<char>) -> u8 {
    match p {
        Some('.' | '!' | '?') => 2,
        Some(_) => 1,
        None => 0,
    }
}

/// Silence after a chunk: short at commas, longer at sentence ends, longest
/// after questions and exclamations.
pub fn pause_after(punct: Option<char>, sentence_end: bool) -> f32 {
    match (sentence_end, punct) {
        (false, Some(',')) => 0.18,
        (false, Some(';' | ':' | '—' | '…')) => 0.22,
        (false, Some(_)) => 0.28,
        (false, None) => 0.08,
        (true, Some('?' | '!')) => 0.38,
        (true, Some(';' | ':')) => 0.24,
        (true, Some('—' | '…')) => 0.34,
        (true, _) => 0.28,
    }
}

/// espeak language for an espeak voice id like `en-gb+m3`.
pub fn espeak_lang(voice: &str) -> &'static str {
    if voice.starts_with("en-gb") || voice.starts_with("en-uk") {
        "en-gb"
    } else {
        "en-us"
    }
}

/// Synthesize all sentences with `engine`. `espeak` is used for phonemes
/// (required by Kokoro, optional for word weights otherwise).
pub(crate) async fn render(
    engine: &Engine,
    espeak: Option<&Path>,
    sentences: &[String],
    speed: f32,
    progress: &Progress,
) -> anyhow::Result<Rendered> {
    let speed = if speed.is_finite() && speed > 0.0 {
        speed.clamp(0.5, 2.0)
    } else {
        1.0
    };
    let plans = plan(engine, espeak, sentences, progress).await?;
    let total_sentences = plans.len().max(1);
    let rate = SAMPLE_RATE as f32;

    let mut out: Vec<f32> = vec![0.0; (LEAD_IN_S * rate) as usize];
    let mut words = Vec::new();
    let mut regions = Vec::new();
    let scratch = Scratch::new()?;
    let mut speech_len = out.len();
    for (si, plan) in plans.iter().enumerate() {
        let mut sentence_start: Option<f32> = None;
        let mut sentence_end = out.len() as f32 / rate;
        for chunk in &plan.chunks {
            let raw = synth_chunk(engine, chunk, speed, &scratch).await?;
            audio::validate_speech(&raw, SAMPLE_RATE).map_err(|e| {
                e.context(format!("{} output for sentence {}", engine.name(), si + 1))
            })?;
            let trimmed = audio::trim_silence(&raw, SAMPLE_RATE);
            audio::validate_speech(&trimmed, SAMPLE_RATE)?;
            let offset = out.len() as f32 / rate;
            let chunk_words = &plan.words[chunk.words.clone()];
            let env = audio::rms_envelope(&trimmed, SAMPLE_RATE);
            let input: Vec<AlignWord> = chunk
                .words
                .clone()
                .map(|i| AlignWord {
                    weight: plan.weights[i],
                    break_after: break_strength(plan.words[i].punct),
                })
                .collect();
            let dur = trimmed.len() as f32 / rate;
            let timings = align(&env, &input);
            anyhow::ensure!(
                timings.len() == chunk_words.len(),
                "no speech to align in sentence {}",
                si + 1
            );
            for (w, (s, e)) in chunk_words.iter().zip(timings) {
                let word = timing(&w.original, offset + s.min(dur), offset + e.min(dur));
                anyhow::ensure!(
                    word.end_s > word.start_s,
                    "no usable audio interval for word {:?}",
                    w.original
                );
                words.push(word);
            }
            out.extend_from_slice(&trimmed);
            speech_len = out.len();
            if let Some(first) = words
                .len()
                .checked_sub(chunk_words.len())
                .and_then(|i| words.get(i))
            {
                sentence_start.get_or_insert(first.start_s);
            }
            if let Some(last) = words.last() {
                if !chunk_words.is_empty() {
                    sentence_end = last.end_s;
                }
            }
            out.extend(std::iter::repeat_n(
                0.0,
                (chunk.pause_after / speed * rate) as usize,
            ));
        }
        let start = sentence_start.unwrap_or(out.len() as f32 / rate);
        regions.push(timing(&sentences[si], start, sentence_end.max(start)));
        progress.fraction(
            (si + 1) as f32 / total_sentences as f32,
            format!("Voiced sentence {}/{}", si + 1, total_sentences),
        );
    }
    // Replace the last sentence's pause with a fixed tail.
    out.truncate(speech_len);
    out.extend(std::iter::repeat_n(0.0, (TAIL_S * rate) as usize));
    audio::normalize_loudness(&mut out, SAMPLE_RATE);
    audio::validate_speech(&out, SAMPLE_RATE)?;
    let duration = out.len() as f32 / rate;
    validate_timings(&words, duration)?;
    validate_timings(&regions, duration)?;
    Ok(Rendered {
        samples: out,
        words,
        sentences: regions,
    })
}

fn timing(text: &str, start: f32, end: f32) -> WordTiming {
    let r = |v: f32| (v * 1000.0).round() / 1000.0;
    WordTiming {
        text: text.to_string(),
        start_s: r(start),
        end_s: r(end.max(start)),
    }
}

pub(crate) fn validate_input(sentences: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        !sentences.is_empty(),
        "nothing to say: narration contains no sentences"
    );
    for (i, sentence) in sentences.iter().enumerate() {
        anyhow::ensure!(
            !normalize_words(sentence).is_empty(),
            "sentence {} contains no speakable words",
            i + 1
        );
    }
    Ok(())
}

fn validate_timings(timings: &[WordTiming], duration: f32) -> anyhow::Result<()> {
    anyhow::ensure!(!timings.is_empty(), "TTS returned no speech timings");
    let mut previous_end = 0.0;
    for word in timings {
        anyhow::ensure!(
            word.start_s.is_finite()
                && word.end_s.is_finite()
                && word.start_s >= previous_end
                && word.end_s > word.start_s
                && word.end_s <= duration,
            "invalid TTS timing for {:?}: {}..{} (duration {duration})",
            word.text,
            word.start_s,
            word.end_s
        );
        previous_end = word.end_s;
    }
    Ok(())
}

async fn plan(
    engine: &Engine,
    espeak: Option<&Path>,
    sentences: &[String],
    progress: &Progress,
) -> anyhow::Result<Vec<SentencePlan>> {
    validate_input(sentences)?;
    let words: Vec<Vec<SpokenWord>> = sentences.iter().map(|s| normalize_words(s)).collect();
    let (lang, british) = match engine {
        Engine::Kokoro { british, .. } => (if *british { "en-gb" } else { "en-us" }, *british),
        Engine::Espeak { voice, .. } => (espeak_lang(voice), espeak_lang(voice) == "en-gb"),
        Engine::Command { .. } => ("en-us", false),
    };
    let is_kokoro = matches!(engine, Engine::Kokoro { .. });
    if is_kokoro && espeak.is_none() {
        anyhow::bail!(
            "Kokoro needs espeak-ng for phonemes; install espeak-ng (e.g. `sudo dnf install espeak-ng`)"
        );
    }

    // Word phonemes (weights, and the fallback for oversize clauses).
    let flat_words: Vec<String> = words.iter().flatten().map(|w| w.spoken.clone()).collect();
    let word_ps: Vec<String> = match espeak {
        Some(exe) => match phonemize::espeak_ipa_lines(exe, lang, &flat_words).await {
            Ok(raw) => raw
                .iter()
                .map(|raw| phonemize::espeak_to_misaki(raw, british))
                .collect(),
            Err(e) if !is_kokoro => {
                progress.warn(format!(
                    "Phoneme weights unavailable ({e:#}); using approximate text weights"
                ));
                vec![String::new(); flat_words.len()]
            }
            Err(e) => return Err(e),
        },
        None => vec![String::new(); flat_words.len()],
    };
    if is_kokoro {
        for (word, ps) in flat_words.iter().zip(&word_ps) {
            anyhow::ensure!(phonemize::has_speech(ps), "no Kokoro phonemes for {word:?}");
        }
    }

    // Clauses in sentence context (Kokoro only).
    let clause_ranges: Vec<Vec<Range<usize>>> = words.iter().map(|ws| clauses(ws)).collect();
    let clause_ps: Vec<String> = if is_kokoro {
        let lines: Vec<String> = words
            .iter()
            .zip(&clause_ranges)
            .flat_map(|(ws, cs)| {
                cs.iter().map(|r| {
                    ws[r.clone()]
                        .iter()
                        .map(|w| w.spoken.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            })
            .collect();
        let raw = phonemize::espeak_ipa_lines(espeak.unwrap(), lang, &lines).await?;
        raw.iter()
            .map(|r| phonemize::espeak_to_misaki(r, british))
            .collect()
    } else {
        Vec::new()
    };

    let mut plans = Vec::with_capacity(words.len());
    let mut word_base = 0usize;
    let mut clause_base = 0usize;
    for (ws, cs) in words.into_iter().zip(clause_ranges) {
        let ps = &word_ps[word_base..word_base + ws.len()];
        let weights: Vec<f32> = ws
            .iter()
            .zip(ps)
            .map(|(w, p)| {
                if w.spoken.trim().is_empty() {
                    0.3
                } else if p.is_empty() {
                    phonemize::text_weight(&w.spoken)
                } else {
                    phonemize::phoneme_weight(p)
                }
            })
            .collect();
        let chunks = if ws.is_empty() {
            Vec::new()
        } else if is_kokoro {
            kokoro_chunks(
                &ws,
                ps,
                &cs,
                &clause_ps[clause_base..clause_base + cs.len()],
            )?
        } else {
            let mut text = crate::normalize::spoken_text(&ws);
            if ws.last().and_then(|w| w.punct).is_none() {
                text.push('.');
            }
            vec![ChunkPlan {
                words: 0..ws.len(),
                text,
                phonemes: String::new(),
                pause_after: pause_after(ws.last().and_then(|w| w.punct), true),
            }]
        };
        word_base += ws.len();
        clause_base += cs.len();
        plans.push(SentencePlan {
            words: ws,
            weights,
            chunks,
        });
    }
    Ok(plans)
}

/// Word ranges ending at clause punctuation.
fn clauses(words: &[SpokenWord]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, w) in words.iter().enumerate() {
        if w.punct.is_some() || i + 1 == words.len() {
            out.push(start..i + 1);
            start = i + 1;
        }
    }
    out
}

fn with_punct(ps: &str, punct: Option<char>) -> String {
    match punct {
        Some(p) => format!("{ps}{p}"),
        None => ps.to_string(),
    }
}

/// Group clauses into chunks of at most MAX_TOKENS tokens; split oversize
/// clauses between words.
fn kokoro_chunks(
    words: &[SpokenWord],
    word_ps: &[String],
    clause_ranges: &[Range<usize>],
    clause_ps: &[String],
) -> anyhow::Result<Vec<ChunkPlan>> {
    anyhow::ensure!(!words.is_empty(), "cannot chunk empty speech");
    anyhow::ensure!(
        words.len() == word_ps.len() && clause_ranges.len() == clause_ps.len(),
        "phoneme/word count mismatch"
    );
    let last = words.len() - 1;
    let final_punct = words[last].punct.or(Some('.'));
    let punct_of = |i: usize| {
        if i == last {
            final_punct
        } else {
            words[i].punct
        }
    };

    // Units: (word range, phonemes) small enough to fit, in order.
    let mut units: Vec<(Range<usize>, String)> = Vec::new();
    for (range, ps) in clause_ranges.iter().zip(clause_ps) {
        let clause = with_punct(ps, punct_of(range.end - 1));
        anyhow::ensure!(
            phonemize::has_speech(&clause),
            "clause contains no Kokoro speech phonemes"
        );
        if phonemize::tokenize(&clause).len() <= MAX_TOKENS {
            units.push((range.clone(), clause));
            continue;
        }
        for i in range.clone() {
            let p = with_punct(
                &word_ps[i],
                if i + 1 == range.end {
                    punct_of(i)
                } else {
                    None
                },
            );
            let toks = phonemize::tokenize(&p);
            anyhow::ensure!(
                toks.len() <= MAX_TOKENS,
                "word {:?} exceeds Kokoro's {MAX_TOKENS}-token limit; refusing to truncate speech",
                words[i].original
            );
            units.push((i..i + 1, p));
        }
    }

    let mut chunks: Vec<ChunkPlan> = Vec::new();
    let mut cur: Option<(Range<usize>, String)> = None;
    for (range, ps) in units {
        cur = match cur.take() {
            None => Some((range, ps)),
            Some((r, p)) => {
                let joined = format!("{p} {ps}");
                if phonemize::tokenize(&joined).len() <= MAX_TOKENS {
                    Some((r.start..range.end, joined))
                } else {
                    chunks.push(make_chunk(r, p, last, &punct_of));
                    Some((range, ps))
                }
            }
        };
    }
    if let Some((r, p)) = cur {
        chunks.push(make_chunk(r, p, last, &punct_of));
    }
    Ok(chunks)
}

fn make_chunk(
    r: Range<usize>,
    phonemes: String,
    last: usize,
    punct_of: &dyn Fn(usize) -> Option<char>,
) -> ChunkPlan {
    let end = r.end - 1;
    ChunkPlan {
        pause_after: pause_after(punct_of(end), end == last),
        words: r,
        text: String::new(),
        phonemes,
    }
}

async fn synth_chunk(
    engine: &Engine,
    chunk: &ChunkPlan,
    speed: f32,
    scratch: &Scratch,
) -> anyhow::Result<Vec<f32>> {
    match engine {
        Engine::Kokoro { model, voice, .. } => {
            let tokens = phonemize::tokenize(&chunk.phonemes);
            anyhow::ensure!(
                phonemize::has_speech(&chunk.phonemes),
                "chunk contains no Kokoro speech phonemes"
            );
            kokoro::infer(model, voice, tokens, speed).await
        }
        Engine::Espeak { exe, voice } => {
            let text_file = scratch.file("txt");
            let wav = scratch.file("wav");
            tokio::fs::write(&text_file, &chunk.text).await?;
            let rate = ((170.0 * speed).round() as i32).clamp(80, 450).to_string();
            clipcut_core::tools::run(
                exe,
                &[
                    "-v",
                    voice,
                    "-s",
                    &rate,
                    "-w",
                    &wav.to_string_lossy(),
                    "-f",
                    &text_file.to_string_lossy(),
                ],
                None,
                Duration::from_secs(120),
            )
            .await?;
            let (samples, sr) = audio::read_wav(&wav)?;
            Ok(audio::resample(&samples, sr, SAMPLE_RATE))
        }
        Engine::Command { template, ffmpeg } => {
            let text_file = scratch.file("txt");
            let wav = scratch.file("wav");
            tokio::fs::write(&text_file, &chunk.text).await?;
            let cmd = template
                .replace("{text_file}", &shell_quote(&text_file.to_string_lossy()))
                .replace("{out_wav}", &shell_quote(&wav.to_string_lossy()));
            clipcut_core::tools::run(
                Path::new("sh"),
                &["-c", &cmd],
                None,
                Duration::from_secs(600),
            )
            .await?;
            anyhow::ensure!(wav.exists(), "TTS command did not write {{out_wav}}: {cmd}");
            match ffmpeg {
                Some(ff) => {
                    let conv = scratch.file("wav");
                    let tempo = format!("atempo={speed:.3}");
                    let mut args = vec!["-hide_banner", "-loglevel", "error", "-y", "-i"];
                    let wav_s = wav.to_string_lossy().into_owned();
                    let conv_s = conv.to_string_lossy().into_owned();
                    args.push(&wav_s);
                    if (speed - 1.0).abs() > 0.01 {
                        args.extend(["-af", tempo.as_str()]);
                    }
                    args.extend([
                        "-ac",
                        "1",
                        "-ar",
                        "24000",
                        "-c:a",
                        "pcm_s16le",
                        conv_s.as_str(),
                    ]);
                    clipcut_core::tools::run(ff, &args, None, Duration::from_secs(120)).await?;
                    Ok(audio::read_wav(&conv)?.0)
                }
                None => {
                    anyhow::ensure!(
                        (speed - 1.0).abs() <= 0.01,
                        "ffmpeg is needed to apply tts.speed for the command engine; install ffmpeg or set speed to 1"
                    );
                    let (samples, sr) = audio::read_wav(&wav)?;
                    Ok(audio::resample(&samples, sr, SAMPLE_RATE))
                }
            }
        }
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Per-call temp directory, removed on drop.
struct Scratch {
    dir: PathBuf,
    counter: AtomicU64,
}

impl Scratch {
    fn new() -> anyhow::Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "clipcut-tts-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            counter: AtomicU64::new(0),
        })
    }
    fn file(&self, ext: &str) -> PathBuf {
        self.dir.join(format!(
            "{}.{ext}",
            self.counter.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(spoken: &str, punct: Option<char>) -> SpokenWord {
        SpokenWord {
            original: spoken.into(),
            spoken: spoken.into(),
            punct,
        }
    }

    #[test]
    fn clause_split_and_chunking() {
        let ws = vec![w("hello", Some(',')), w("big", None), w("world", None)];
        assert_eq!(clauses(&ws), vec![0..1, 1..3]);
        let ps = vec![
            "həlˈO".to_string(),
            "bˈɪɡ".to_string(),
            "wˈɜɹld".to_string(),
        ];
        let chunks = kokoro_chunks(
            &ws,
            &ps,
            &clauses(&ws),
            &["həlˈO".into(), "bˈɪɡ wˈɜɹld".into()],
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].phonemes, "həlˈO, bˈɪɡ wˈɜɹld.");
        assert_eq!(chunks[0].pause_after, 0.28);
    }

    #[test]
    fn oversize_sentences_split_at_commas() {
        let clause = "ab ".repeat(130); // 390 tokens
        let ws = vec![w("x", Some(',')), w("y", Some('?'))];
        let ps = vec![clause.trim().to_string(), clause.trim().to_string()];
        let chunks = kokoro_chunks(&ws, &ps, &clauses(&ws), &ps).unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].phonemes.ends_with(','));
        assert_eq!(chunks[0].pause_after, 0.18);
        assert!(chunks[1].phonemes.ends_with('?'));
        assert_eq!(chunks[1].pause_after, 0.38);
        assert!(
            chunks
                .iter()
                .all(|c| phonemize::tokenize(&c.phonemes).len() <= MAX_TOKENS)
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/tmp/a b'c"), r"'/tmp/a b'\''c'");
    }

    #[tokio::test]
    async fn empty_narration_is_not_a_silent_success() {
        let engine = Engine::Espeak {
            exe: "/missing/espeak".into(),
            voice: "en-us".into(),
        };
        let progress = Progress::log_only("empty-input-test");
        for sentences in [vec![], vec![String::new()], vec!["... 🚀".to_string()]] {
            assert!(
                render(&engine, None, &sentences, 1.0, &progress)
                    .await
                    .is_err()
            );
        }
    }

    #[test]
    fn oversized_word_is_not_silently_truncated() {
        let ws = vec![w("oversized", Some('.'))];
        let ps = vec!["a".repeat(MAX_TOKENS + 1)];
        assert!(
            kokoro_chunks(&ws, &ps, &clauses(&ws), &ps)
                .unwrap_err()
                .to_string()
                .contains("refusing to truncate")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn silent_command_is_not_successful_speech() {
        let scratch = Scratch::new().unwrap();
        let fixture = scratch.file("wav");
        audio::write_wav(&fixture, &vec![0.0; SAMPLE_RATE as usize], SAMPLE_RATE).unwrap();
        let engine = Engine::Command {
            template: format!("cp {} {{out_wav}}", shell_quote(&fixture.to_string_lossy())),
            ffmpeg: None,
        };
        let err = render(
            &engine,
            None,
            &["Real words.".into()],
            1.0,
            &Progress::log_only("silent-test"),
        )
        .await
        .err()
        .expect("silent output must fail");
        assert!(format!("{err:#}").contains("silent"), "{err:#}");
    }
}
