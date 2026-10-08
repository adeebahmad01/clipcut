//! espeak-ng grapheme-to-phoneme, converted to the misaki phoneme set Kokoro
//! v1.0 was trained on, plus Kokoro's token vocabulary.

use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Kokoro accepts at most this many tokens per call (512 minus the two pads).
pub const MAX_TOKENS: usize = 510;

/// Kokoro-82M v1.0 `vocab` (hexgrad/Kokoro-82M config.json).
pub const VOCAB: &[(char, i64)] = &[
    (';', 1),
    (':', 2),
    (',', 3),
    ('.', 4),
    ('!', 5),
    ('?', 6),
    ('—', 9),
    ('…', 10),
    ('"', 11),
    ('(', 12),
    (')', 13),
    ('“', 14),
    ('”', 15),
    (' ', 16),
    ('\u{0303}', 17),
    ('ʣ', 18),
    ('ʥ', 19),
    ('ʦ', 20),
    ('ʨ', 21),
    ('ᵝ', 22),
    ('ꭧ', 23),
    ('A', 24),
    ('I', 25),
    ('O', 31),
    ('Q', 33),
    ('S', 35),
    ('T', 36),
    ('W', 39),
    ('Y', 41),
    ('ᵊ', 42),
    ('a', 43),
    ('b', 44),
    ('c', 45),
    ('d', 46),
    ('e', 47),
    ('f', 48),
    ('h', 50),
    ('i', 51),
    ('j', 52),
    ('k', 53),
    ('l', 54),
    ('m', 55),
    ('n', 56),
    ('o', 57),
    ('p', 58),
    ('q', 59),
    ('r', 60),
    ('s', 61),
    ('t', 62),
    ('u', 63),
    ('v', 64),
    ('w', 65),
    ('x', 66),
    ('y', 67),
    ('z', 68),
    ('ɑ', 69),
    ('ɐ', 70),
    ('ɒ', 71),
    ('æ', 72),
    ('β', 75),
    ('ɔ', 76),
    ('ɕ', 77),
    ('ç', 78),
    ('ɖ', 80),
    ('ð', 81),
    ('ʤ', 82),
    ('ə', 83),
    ('ɚ', 85),
    ('ɛ', 86),
    ('ɜ', 87),
    ('ɟ', 90),
    ('ɡ', 92),
    ('ɥ', 99),
    ('ɨ', 101),
    ('ɪ', 102),
    ('ʝ', 103),
    ('ɯ', 110),
    ('ɰ', 111),
    ('ŋ', 112),
    ('ɳ', 113),
    ('ɲ', 114),
    ('ɴ', 115),
    ('ø', 116),
    ('ɸ', 118),
    ('θ', 119),
    ('œ', 120),
    ('ɹ', 123),
    ('ɾ', 125),
    ('ɻ', 126),
    ('ʁ', 128),
    ('ɽ', 129),
    ('ʂ', 130),
    ('ʃ', 131),
    ('ʈ', 132),
    ('ʧ', 133),
    ('ʊ', 135),
    ('ʋ', 136),
    ('ʌ', 138),
    ('ɣ', 139),
    ('ɤ', 140),
    ('χ', 142),
    ('ʎ', 143),
    ('ʒ', 147),
    ('ʔ', 148),
    ('ˈ', 156),
    ('ˌ', 157),
    ('ː', 158),
    ('ʰ', 162),
    ('ʲ', 164),
    ('↓', 169),
    ('→', 171),
    ('↗', 172),
    ('↘', 173),
    ('ᵻ', 177),
];

pub fn token_id(c: char) -> Option<i64> {
    VOCAB.iter().find(|(v, _)| *v == c).map(|(_, id)| *id)
}

/// Phoneme string -> Kokoro token ids (unknown symbols dropped).
pub fn tokenize(phonemes: &str) -> Vec<i64> {
    phonemes.chars().filter_map(token_id).collect()
}

/// At least one spoken phoneme, rather than just spaces, punctuation and
/// stress/length markers. A period alone is not successful speech.
pub fn has_speech(phonemes: &str) -> bool {
    phonemes.chars().any(|c| {
        token_id(c).is_some()
            && !crate::normalize::CLAUSE_PUNCT.contains(&c)
            && !matches!(
                c,
                ' ' | '"'
                    | '('
                    | ')'
                    | '“'
                    | '”'
                    | 'ˈ'
                    | 'ˌ'
                    | 'ː'
                    | '↓'
                    | '→'
                    | '↗'
                    | '↘'
                    | '\u{0303}'
            )
    })
}

/// espeak voice (language) for a Kokoro voice id: `b*` voices are British.
pub fn espeak_lang_for_voice(voice: &str) -> &'static str {
    if voice.trim_start().starts_with('b') {
        "en-gb"
    } else {
        "en-us"
    }
}

/// Run espeak-ng in IPA mode over several independent lines at once. Each input
/// line ends a clause, so espeak returns one line per input line; when it does
/// not (odd input), every line is phonemized on its own.
pub async fn espeak_ipa_lines(
    espeak: &Path,
    lang: &str,
    lines: &[String],
) -> anyhow::Result<Vec<String>> {
    let cleaned: Vec<String> = lines.iter().map(|l| clean_for_espeak(l)).collect();
    let nonempty: Vec<usize> = (0..cleaned.len())
        .filter(|&i| !cleaned[i].trim().is_empty())
        .collect();
    let mut out = vec![String::new(); lines.len()];
    if nonempty.is_empty() {
        return Ok(out);
    }
    let joined = nonempty
        .iter()
        .map(|&i| cleaned[i].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let raw = run_espeak(espeak, lang, &joined).await?;
    let got: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    if got.len() == nonempty.len() {
        for (&i, line) in nonempty.iter().zip(got) {
            out[i] = line.trim().to_string();
        }
    } else {
        for &i in &nonempty {
            let raw = run_espeak(espeak, lang, &cleaned[i]).await?;
            out[i] = raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    Ok(out)
}

/// Text espeak can read without inventing clause breaks.
fn clean_for_espeak(text: &str) -> String {
    text.chars()
        .map(|c| {
            if crate::normalize::CLAUSE_PUNCT.contains(&c) || matches!(c, '"' | '(' | ')' | '\n') {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

async fn run_espeak(espeak: &Path, lang: &str, text: &str) -> anyhow::Result<String> {
    let mut child = Command::new(espeak)
        .args(["-q", "--ipa", "--tie=^", "-v", lang])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start {}: {e}", espeak.display()))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = format!("{text}\n");
    let writer = tokio::spawn(async move {
        stdin.write_all(input.as_bytes()).await?;
        stdin.shutdown().await
    });
    let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("espeak-ng timed out"))??;
    let written = writer.await?;
    anyhow::ensure!(
        output.status.success(),
        "espeak-ng failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    written?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Convert raw espeak IPA (with `^` ties) to misaki phonemes, the way
/// misaki's `EspeakFallback` does, then keep only Kokoro vocabulary.
pub fn espeak_to_misaki(raw: &str, british: bool) -> String {
    static LANG_SWITCH: OnceLock<Regex> = OnceLock::new();
    static SYLLABIC: OnceLock<Regex> = OnceLock::new();
    let lang_switch =
        LANG_SWITCH.get_or_init(|| Regex::new(r"\([a-z]{2,3}(?:-[a-z0-9]+)?\)").unwrap());
    let syllabic = SYLLABIC.get_or_init(|| Regex::new(r"(\S)\x{0329}").unwrap());

    let mut ps = lang_switch.replace_all(raw.trim(), "").into_owned();
    if british {
        ps = ps.replace("e^ə", "ɛː").replace("ə^ʊ", "Q");
    }
    // Longest first, as in misaki.
    const E2M: &[(&str, &str)] = &[
        ("ʔˌn\u{0329}", "ʔn"),
        ("ʔn\u{0329}", "ʔn"),
        ("a^ɪ", "I"),
        ("a^ʊ", "W"),
        ("d^ʒ", "ʤ"),
        ("e^ɪ", "A"),
        ("t^ʃ", "ʧ"),
        ("ɔ^ɪ", "Y"),
        ("ə^l", "ᵊl"),
        ("ʲo", "jo"),
        ("ʲə", "jə"),
        ("ʲ", ""),
        ("ɚ", "əɹ"),
        ("r", "ɹ"),
        ("x", "k"),
        ("ç", "k"),
        ("ɐ", "ə"),
        ("ɬ", "l"),
        ("\u{0303}", ""),
    ];
    for (old, new) in E2M {
        ps = ps.replace(old, new);
    }
    // Bare `e` (not part of e^ɪ) is also an A in misaki.
    ps = ps.replace('e', "A");
    ps = syllabic
        .replace_all(&ps, "ᵊ$1")
        .into_owned()
        .replace('\u{0329}', "");
    if british {
        ps = ps.replace("iə", "ɪə");
    } else {
        ps = ps
            .replace("o^ʊ", "O")
            .replace("ɜːɹ", "ɜɹ")
            .replace("ɜː", "ɜɹ")
            .replace("ɪə", "iə")
            .replace('ː', "");
    }
    // espeak < 1.52 writes `o` where misaki expects `ɔ`.
    ps = ps.replace('o', "ɔ");
    ps = ps.replace('ɾ', "T").replace('ʔ', "t");
    ps = ps.replace('^', "");
    let kept: String = ps.chars().filter(|c| token_id(*c).is_some()).collect();
    kept.split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Relative duration weight of a phoneme string, used to place word
/// boundaries: vowels and diphthongs take longer than consonants.
pub fn phoneme_weight(ps: &str) -> f32 {
    let mut w: f32 = 0.0;
    for c in ps.chars() {
        w += match c {
            'ˈ' | 'ˌ' | 'ː' | ' ' => 0.0,
            'A' | 'I' | 'O' | 'W' | 'Y' | 'Q' => 1.35,
            'a' | 'e' | 'i' | 'o' | 'u' | 'æ' | 'ɑ' | 'ɐ' | 'ɒ' | 'ɔ' | 'ɛ' | 'ɜ' | 'ɪ' | 'ʊ'
            | 'ʌ' | 'ɚ' => 1.0,
            'ə' | 'ᵊ' | 'ᵻ' | 'ɨ' => 0.6,
            'T' | 'ɾ' => 0.35,
            c if crate::normalize::CLAUSE_PUNCT.contains(&c) => 0.0,
            _ => 0.7,
        };
    }
    w.max(0.4)
}

/// Fallback weight when no phonemes are available: letters, with vowels heavier.
pub fn text_weight(text: &str) -> f32 {
    let w: f32 = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| if "aeiouyAEIOUY".contains(c) { 1.0 } else { 0.7 })
        .sum();
    w.max(0.6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_is_unique_and_complete() {
        assert_eq!(VOCAB.len(), 114);
        let mut ids: Vec<i64> = VOCAB.iter().map(|(_, i)| *i).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 114);
        assert_eq!(token_id('ˈ'), Some(156));
        assert_eq!(token_id('#'), None);
    }

    #[test]
    fn misaki_conversion() {
        // espeak-ng 1.51: "the church judge my house boy go button little"
        let raw = "ðə t^ʃˈɜːt^ʃ d^ʒˈʌd^ʒ ma^ɪ hˈa^ʊs bˈɔ^ɪ ɡˌo^ʊ bˈʌʔn̩ lˈɪɾə^l";
        assert_eq!(
            espeak_to_misaki(raw, false),
            "ðə ʧˈɜɹʧ ʤˈʌʤ mI hˈWs bˈY ɡˌO bˈʌtn lˈɪTᵊl"
        );
        assert_eq!(espeak_to_misaki("wˈɔː^ɾɚ", false), "wˈɔTəɹ");
        assert_eq!(espeak_to_misaki("(fr)bɔ̃(en)", false), "bɔ");
        assert_eq!(espeak_to_misaki("ɡˈə^ʊ", true), "ɡˈQ");
    }

    #[test]
    fn weights() {
        assert!(phoneme_weight("strˈɛŋθ") > phoneme_weight("ɐ"));
        assert!(text_weight("extraordinary") > text_weight("a"));
        assert!(!has_speech(" . ˈ?!"));
        assert!(has_speech("həlˈO"));
    }
}
