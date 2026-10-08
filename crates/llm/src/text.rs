//! Deterministic text helpers: whitespace normalization, word counts,
//! narration cleanup and the sentence splitter.

/// Collapse every run of whitespace (newlines included) into one space.
pub fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Spoken words: whitespace tokens containing at least one letter or digit.
pub fn word_count(text: &str) -> usize {
    text.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count()
}

/// Lowercased letters and digits only, for fuzzy word comparison.
pub fn norm_word(word: &str) -> String {
    word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Normalized word sequence of a text (punctuation-only tokens dropped).
pub fn norm_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(norm_word).filter(|w| !w.is_empty()).collect()
}

/// Keep at most `max` words.
pub fn truncate_words(text: &str, max: usize) -> String {
    text.split_whitespace().take(max).collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` chars on a word boundary.
pub fn truncate_chars(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for word in text.split_whitespace() {
        let next_len = out.chars().count() + usize::from(!out.is_empty()) + word.chars().count();
        if next_len > max {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() { text.chars().take(max).collect() } else { out }
}

/// Remove markdown citation links the agents like to append, e.g.
/// `([example.com](https://example.com/x))` or `[1]`, and stray markdown.
pub fn strip_citations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // "([label](url))" or "[label](url)"
        let paren = chars[i] == '(' && chars.get(i + 1) == Some(&'[');
        if paren || chars[i] == '[' {
            let start = if paren { i + 1 } else { i };
            if let Some(end) = markdown_link_end(&chars, start) {
                // Inline "[the study](url)" keeps its words; "[site.com](url)"
                // and parenthesized links are citations and go away.
                let close = (start + 1..end).find(|&k| chars[k] == ']').unwrap_or(start + 1);
                let label: String = chars[start + 1..close].iter().collect();
                let looks_like_site = !label.contains(' ') && label.contains('.');
                if !paren && !looks_like_site {
                    out.push_str(&label);
                    i = end;
                    continue;
                }
                let mut j = end;
                if paren && chars.get(j) == Some(&')') {
                    j += 1;
                }
                i = j;
                continue;
            }
            // Footnote markers like [1] or [12].
            if chars[i] == '[' {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j > i + 1 && chars.get(j) == Some(&']') {
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    normalize_ws(&out.replace("**", "").replace("__", "")).replace(" .", ".").replace(" ,", ",")
}

/// `[label](url)` starting at `start` ('['): index just past the ')'.
fn markdown_link_end(chars: &[char], start: usize) -> Option<usize> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    let close = (start + 1..chars.len()).find(|&k| chars[k] == ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = (close + 2..chars.len()).find(|&k| chars[k] == ')' || chars[k].is_whitespace())?;
    (chars[end] == ')').then_some(end + 1)
}

/// Turn model output into a clean single-paragraph narration: no markdown,
/// stage directions, speaker labels or wrapping quotes.
pub fn clean_narration(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('#').trim();
        if line.is_empty() {
            continue;
        }
        // Drop speaker labels like "Narrator:" / "VO:".
        let line = match line.split_once(':') {
            Some((label, rest))
                if label.len() <= 12
                    && ["narrator", "vo", "voiceover", "voice over", "script", "host"]
                        .contains(&label.trim().to_lowercase().as_str()) =>
            {
                rest.trim()
            }
            _ => line,
        };
        lines.push(line.to_string());
    }
    let joined = lines.join(" ");
    // Stage directions in brackets: [beat], (pause), *dramatic music*.
    let mut out = String::with_capacity(joined.len());
    let mut depth_sq = 0usize;
    for c in joined.chars() {
        match c {
            '[' => depth_sq += 1,
            ']' if depth_sq > 0 => depth_sq -= 1,
            _ if depth_sq > 0 => {}
            '*' | '`' | '_' => {}
            _ => out.push(c),
        }
    }
    let out = normalize_ws(&out);
    let trimmed = out.trim_matches(|c| matches!(c, '"' | '“' | '”')).trim();
    // Only strip wrapping quotes when they are a balanced pair around everything.
    if out.starts_with(['"', '“']) && out.ends_with(['"', '”']) && !trimmed.contains(['"', '“', '”']) {
        trimmed.to_string()
    } else {
        out
    }
}

const CLOSERS: &[char] = &['"', '\'', '”', '’', ')', ']', '»'];
const OPENERS: &[char] = &['"', '\'', '“', '‘', '(', '[', '«', '¿', '¡'];

/// Abbreviations that never end a sentence when followed by a capitalized word.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "mt", "ft", "gen", "gov", "sen", "rep", "lt", "col", "capt",
    "sgt", "rev", "hon", "pres", "vs", "v", "vol", "fig", "approx", "dept", "univ", "jan", "feb", "mar", "apr", "jun",
    "jul", "aug", "sep", "sept", "oct", "nov", "dec", "al", "cf", "ca", "mx", "mme", "messrs", "est",
];

/// Abbreviations that can also end a sentence ("…founded Apple Inc. It grew").
/// They split only when the next word is a common sentence starter.
const SOFT_ABBREVIATIONS: &[&str] = &["inc", "ltd", "co", "corp", "llc", "bros"];

const SENTENCE_STARTERS: &[&str] = &[
    "a", "an", "and", "as", "at", "but", "by", "even", "every", "for", "from", "he", "her", "here", "his", "how", "i",
    "if", "in", "it", "its", "it's", "just", "meanwhile", "most", "my", "no", "not", "now", "of", "on", "one", "or",
    "our", "she", "so", "some", "that", "that's", "the", "their", "then", "there", "there's", "these", "they",
    "this", "those", "to", "today", "we", "what", "when", "where", "which", "while", "who", "why", "with", "yet",
    "you", "your", "yes", "still", "after", "before", "since", "until", "because", "nobody", "everyone",
];

fn is_starter(word: &str) -> bool {
    let w: String = word
        .trim_start_matches(OPENERS)
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '’')
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace('’', "'");
    SENTENCE_STARTERS.contains(&w.as_str())
}

/// Does a sentence end after token `tok`, given the following token `next`?
fn ends_sentence(tok: &str, next: &str) -> bool {
    let core = tok.trim_end_matches(CLOSERS);
    let Some(last) = core.chars().last() else { return false };
    if !matches!(last, '.' | '!' | '?' | '…') {
        return false;
    }
    // The next sentence must start with a capital, a digit, or an opener + capital.
    let next_core = next.trim_start_matches(OPENERS);
    let starts = next_core.chars().next().is_some_and(|c| c.is_uppercase() || c.is_ascii_digit());
    if !starts {
        return false;
    }
    if last != '.' || core.ends_with("..") {
        return true;
    }
    let word = core.trim_end_matches('.').trim_start_matches(OPENERS);
    let lower = word.to_lowercase();
    // Single-letter initials: "J. K. Rowling".
    if word.chars().count() == 1 && word.chars().all(|c| c.is_alphabetic()) {
        return false;
    }
    // "No. 1" is an abbreviation only before a number.
    if lower == "no" || lower == "nos" {
        return !next_core.chars().next().is_some_and(|c| c.is_ascii_digit());
    }
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return false;
    }
    // Dotted acronyms "U.S." / "e.g." and soft abbreviations: split only before
    // an obvious sentence starter.
    let dotted = word.contains('.') && word.split('.').all(|p| p.chars().count() <= 2 && p.chars().all(char::is_alphabetic));
    if dotted || SOFT_ABBREVIATIONS.contains(&lower.as_str()) {
        return is_starter(next);
    }
    true
}

/// Split narration into sentences. Whitespace is normalized first, so
/// `split_sentences(t).join(" ") == normalize_ws(t)` always holds.
pub fn split_sentences(text: &str) -> Vec<String> {
    let text = normalize_ws(text);
    if text.is_empty() {
        return Vec::new();
    }
    let tokens: Vec<&str> = text.split(' ').collect();
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        current.push(tok);
        if let Some(next) = tokens.get(i + 1)
            && ends_sentence(tok, next)
        {
            out.push(current.join(" "));
            current.clear();
        }
    }
    if !current.is_empty() {
        out.push(current.join(" "));
    }
    out
}

/// Does `text` start with `prefix`, comparing normalized words?
/// Returns the fraction of the prefix's words that match in order (0..=1).
pub fn prefix_overlap(text: &str, prefix: &str) -> f32 {
    let p = norm_words(prefix);
    if p.is_empty() {
        return 1.0;
    }
    let t = norm_words(text);
    let window = &t[..t.len().min(p.len() + 3)];
    lcs_len(window, &p) as f32 / p.len() as f32
}

/// Longest common subsequence length of two word lists.
pub fn lcs_len(a: &[String], b: &[String]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    for x in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y { prev[j] + 1 } else { prev[j + 1].max(cur[j]) };
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str, expected: &[&str]) {
        let got = split_sentences(text);
        assert_eq!(got, expected, "splitting {text:?}");
        assert_eq!(got.join(" "), normalize_ws(text));
    }

    #[test]
    fn splits_basic_sentences() {
        check("Octopuses have three hearts. Two pump blood to the gills! Why? Nobody knows…  Really.", &[
            "Octopuses have three hearts.",
            "Two pump blood to the gills!",
            "Why?",
            "Nobody knows…",
            "Really.",
        ]);
    }

    #[test]
    fn keeps_abbreviations_and_decimals() {
        check("Dr. Jane Goodall spent 60 years in Gombe. It costs $3.5 billion. Prices rose 2.4% in the U.S. last year.", &[
            "Dr. Jane Goodall spent 60 years in Gombe.",
            "It costs $3.5 billion.",
            "Prices rose 2.4% in the U.S. last year.",
        ]);
        check("J. K. Rowling wrote it. Mr. Smith agreed. No. 1 on the charts.", &[
            "J. K. Rowling wrote it.",
            "Mr. Smith agreed.",
            "No. 1 on the charts.",
        ]);
    }

    #[test]
    fn dotted_acronyms_split_only_before_starters() {
        check("He moved to the U.S. The rest is history.", &["He moved to the U.S.", "The rest is history."]);
        check("The U.S. Navy built it.", &["The U.S. Navy built it."]);
        check("It shipped at 9 a.m. Then it broke.", &["It shipped at 9 a.m.", "Then it broke."]);
        check("Apple Inc. It grew. Acme Inc. Products sell.", &["Apple Inc.", "It grew.", "Acme Inc. Products sell."]);
    }

    #[test]
    fn handles_quotes_and_lowercase_continuations() {
        check("He said \"stop.\" Then he left. Wait... what happened?", &[
            "He said \"stop.\"",
            "Then he left.",
            "Wait... what happened?",
        ]);
        check("“Is it real?” she asked. Yes.", &["“Is it real?” she asked.", "Yes."]);
        check("", &[]);
        check("no punctuation at all", &["no punctuation at all"]);
    }

    #[test]
    fn cleans_narration() {
        assert_eq!(clean_narration("Narrator: Octopuses have *three* hearts.\n\n[beat] Wild."), "Octopuses have three hearts. Wild.");
        assert_eq!(clean_narration("\"Octopuses have three hearts.\""), "Octopuses have three hearts.");
        assert_eq!(clean_narration("\"Stop,\" he said. \"Now.\""), "\"Stop,\" he said. \"Now.\"");
        assert_eq!(clean_narration("## Script\nThey \"swim\" fast."), "Script They \"swim\" fast.");
    }

    #[test]
    fn strips_citations() {
        assert_eq!(
            strip_citations("Octopuses have three hearts ([nhm.ac.uk](https://www.nhm.ac.uk/x)). Blue blood [2]."),
            "Octopuses have three hearts. Blue blood."
        );
        assert_eq!(strip_citations("See [the study](https://a.b/c) now"), "See the study now");
        assert_eq!(strip_citations("Blue blood [nasa.gov](https://nasa.gov/x)."), "Blue blood.");
    }

    #[test]
    fn counts_and_truncates() {
        assert_eq!(word_count("Three hearts — nine brains, 1 octopus."), 6);
        assert_eq!(truncate_chars("Why octopuses have three hearts and blue blood", 20), "Why octopuses have");
        assert!(prefix_overlap("Your heart beats. Theirs stops.", "Your heart beats.") > 0.99);
    }
}
