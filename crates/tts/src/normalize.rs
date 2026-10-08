//! Text normalization before phonemizing: numbers, money, abbreviations,
//! symbols and emoji become plain speakable words. Works word by word so every
//! original (on-screen) word keeps a link to what is actually spoken, which is
//! what the word timings need.

use std::sync::OnceLock;

use regex::Regex;

/// Clause punctuation kept in the spoken text; it drives Kokoro's prosody.
pub const CLAUSE_PUNCT: &[char] = &['.', ',', '!', '?', ';', ':', '—', '…'];

/// One on-screen word and the words spoken for it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpokenWord {
    /// The word as written in the script (trimmed of surrounding whitespace).
    pub original: String,
    /// Plain speakable text without trailing punctuation, e.g. `"forty-two percent"`.
    pub spoken: String,
    /// Clause punctuation after this word, if any.
    pub punct: Option<char>,
}

/// Full spoken text of a word list, punctuation included.
pub fn spoken_text(words: &[SpokenWord]) -> String {
    let mut out = String::new();
    for w in words {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w.spoken);
        if let Some(p) = w.punct {
            out.push(p);
        }
    }
    out
}

/// Normalize a whole sentence; see [`normalize_words`].
pub fn normalize(text: &str) -> String {
    spoken_text(&normalize_words(text))
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn is_emoji_or_symbol(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x1F000..=0x1FAFF   // emoji, pictographs, flags
        | 0x2600..=0x27BF   // misc symbols, dingbats
        | 0x2B00..=0x2BFF   // arrows, stars
        | 0x2190..=0x21FF   // arrows
        | 0x25A0..=0x25FF   // geometric shapes
        | 0xFE00..=0xFE0F   // variation selectors
        | 0x200B..=0x200F   // zero-width chars
        | 0x2060..=0x2064
        | 0xE0000..=0xE007F // tags
        | 0x20E3            // keycap
    ) || (c.is_control() && c != '\n' && c != '\t')
}

/// Whole-text cleanup before splitting into words.
fn prepare(text: &str) -> String {
    static ELLIPSIS: OnceLock<Regex> = OnceLock::new();
    static DASH: OnceLock<Regex> = OnceLock::new();
    let mut s: String = text
        .chars()
        .filter(|c| !is_emoji_or_symbol(*c))
        .map(|c| match c {
            '’' | '‘' | '`' | '´' | 'ʼ' => '\'',
            '“' | '”' | '„' | '«' | '»' => '"',
            '–' => '-',
            '\n' | '\t' | '\u{a0}' => ' ',
            _ => c,
        })
        .collect();
    s = re(&ELLIPSIS, r"\.\s*\.\s*\.(\s*\.)*")
        .replace_all(&s, "…")
        .into_owned();
    // Spaced hyphens / double hyphens / em dashes become one em dash glued to
    // the previous word ("word — next" -> "word— next").
    s = re(&DASH, r"\s*(?:\s-{1,2}\s|--|—)\s*")
        .replace_all(&s, "— ")
        .into_owned();
    s
}

/// Split a sentence into on-screen words with their spoken form.
pub fn normalize_words(text: &str) -> Vec<SpokenWord> {
    let prepared = prepare(text);
    let tokens: Vec<&str> = prepared.split_whitespace().collect();
    let mut out: Vec<SpokenWord> = Vec::new();
    // Words appended after the next token's spoken text ("$5 million" -> "five million dollars").
    let mut pending_suffix: Option<&'static str> = None;
    let mut prev_was_number = false;
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        let (lead, core, trail) = split_punct(token);
        let next_core = tokens.get(i + 1).map(|t| split_punct(t).1).unwrap_or("");
        let mut punct = strongest_punct(trail);
        // Opening paren/bracket acts like a comma before it.
        if lead.contains(['(', '[']) {
            if let Some(prev) = out.last_mut() {
                prev.punct.get_or_insert(',');
            }
        }
        if trail.contains([')', ']']) && punct.is_none() {
            punct = Some(',');
        }

        let mut spoken;
        let mut skip_next = false;
        let carried_suffix = pending_suffix.take();
        if core.is_empty() {
            // A bare punctuation token ("—", "…", "&").
            spoken = match token.trim_matches(|c: char| CLAUSE_PUNCT.contains(&c)) {
                "&" => "and".to_string(),
                "+" => "plus".to_string(),
                "=" => "equals".to_string(),
                "@" => "at".to_string(),
                "%" => "percent".to_string(),
                _ => String::new(),
            };
            if spoken.is_empty() {
                if let (Some(p), Some(prev)) = (punct, out.last_mut()) {
                    prev.punct = Some(merge_punct(prev.punct, p));
                }
                i += 1;
                continue;
            }
        } else {
            let (word, abbrev_period) = expand_core(
                core,
                trail,
                next_core,
                prev_was_number,
                &mut pending_suffix,
                &mut skip_next,
            );
            spoken = word;
            if abbrev_period && punct.is_none() && i + 1 == tokens.len() {
                // "... etc." ends the sentence: the period does double duty.
                punct = Some('.');
            }
        }

        if let Some(suffix) = carried_suffix.filter(|_| is_scale(core)) {
            spoken.push(' ');
            spoken.push_str(suffix);
        }

        let mut original = token.to_string();
        if skip_next {
            if let Some(next) = tokens.get(i + 1) {
                original.push(' ');
                original.push_str(next);
                let (_, _, t2) = split_punct(next);
                if let Some(p) = strongest_punct(t2) {
                    punct = Some(p);
                }
            }
            i += 1;
        }
        prev_was_number = core.chars().any(|c| c.is_ascii_digit())
            && core.chars().last().is_some_and(|c| c.is_ascii_digit());
        let spoken = spoken.split_whitespace().collect::<Vec<_>>().join(" ");
        if spoken.is_empty() {
            if let (Some(p), Some(prev)) = (punct, out.last_mut()) {
                prev.punct = Some(merge_punct(prev.punct, p));
            }
        } else {
            out.push(SpokenWord {
                original,
                spoken,
                punct,
            });
        }
        i += 1;
    }
    out
}

fn is_scale(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "thousand" | "million" | "billion" | "trillion"
    )
}

fn split_punct(token: &str) -> (&str, &str, &str) {
    let is_edge = |c: char| {
        CLAUSE_PUNCT.contains(&c)
            || matches!(
                c,
                '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '*' | '_' | '~' | '¿' | '¡'
            )
    };
    let start = token.find(|c: char| !is_edge(c)).unwrap_or(token.len());
    let rest = &token[start..];
    let end = rest
        .rfind(|c: char| !is_edge(c))
        .map(|i| i + rest[i..].chars().next().unwrap().len_utf8())
        .unwrap_or(0);
    let (mut core, mut trail) = (&rest[..end], &rest[end..]);
    // Keep a trailing period that is part of an abbreviation ("etc.", "U.S.").
    if trail.starts_with('.') && abbreviation(&format!("{core}.")).is_some() {
        core = &rest[..end + 1];
        trail = &rest[end + 1..];
    } else if trail.starts_with('.') && is_dotted_initialism(&format!("{core}.")) {
        core = &rest[..end + 1];
        trail = &rest[end + 1..];
    }
    // "rock'n'roll" keeps inner apostrophes; trailing "'" of plural possessive is fine to drop.
    (&token[..start], core, trail)
}

fn strongest_punct(trail: &str) -> Option<char> {
    let order = ['?', '!', '.', '…', ';', ':', '—', ','];
    order.into_iter().find(|p| trail.contains(*p))
}

fn merge_punct(prev: Option<char>, new: char) -> char {
    let rank = |c: char| match c {
        '?' => 7,
        '!' => 6,
        '.' => 5,
        '…' => 4,
        ';' => 3,
        ':' => 2,
        '—' => 1,
        _ => 0,
    };
    match prev {
        Some(p) if rank(p) >= rank(new) => p,
        _ => new,
    }
}

fn is_dotted_initialism(s: &str) -> bool {
    // "U.S.", "a.m.", "D.C."
    let parts: Vec<&str> = s.trim_end_matches('.').split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| p.chars().count() == 1 && p.chars().all(char::is_alphabetic))
}

fn abbreviation(word: &str) -> Option<&'static str> {
    Some(match word {
        "Mr." | "Mr" => "mister",
        "Mrs." | "Mrs" => "missus",
        "Ms." => "miz",
        "Dr." => "doctor",
        "Prof." => "professor",
        "Sr." => "senior",
        "Jr." => "junior",
        "St." => "saint",
        "Mt." => "mount",
        "Ft." => "fort",
        "Gen." => "general",
        "Capt." => "captain",
        "Lt." => "lieutenant",
        "Sgt." => "sergeant",
        "Gov." => "governor",
        "Sen." => "senator",
        "Rep." => "representative",
        "Pres." => "president",
        "vs." | "vs" | "Vs." => "versus",
        "etc." => "et cetera",
        "e.g." => "for example",
        "i.e." => "that is",
        "approx." => "approximately",
        "est." => "established",
        "Inc." => "incorporated",
        "Ltd." => "limited",
        "Corp." => "corporation",
        "Co." => "company",
        "Ave." => "avenue",
        "Blvd." => "boulevard",
        "Jan." => "January",
        "Feb." => "February",
        "Mar." => "March",
        "Apr." => "April",
        "Aug." => "August",
        "Sep." | "Sept." => "September",
        "Oct." => "October",
        "Nov." => "November",
        "Dec." => "December",
        "a.m." | "A.M." => "A M",
        "p.m." | "P.M." => "P M",
        "w/" => "with",
        "w/o" => "without",
        _ => return None,
    })
}

pub(crate) fn is_abbreviation(word: &str) -> bool {
    abbreviation(word).is_some() || is_dotted_initialism(word)
}

fn unit_word(unit: &str, one: bool) -> Option<&'static str> {
    let (singular, plural) = match unit {
        "km" => ("kilometer", "kilometers"),
        "m" => ("meter", "meters"),
        "cm" => ("centimeter", "centimeters"),
        "mm" => ("millimeter", "millimeters"),
        "kg" => ("kilogram", "kilograms"),
        "g" => ("gram", "grams"),
        "mg" => ("milligram", "milligrams"),
        "lb" | "lbs" => ("pound", "pounds"),
        "oz" => ("ounce", "ounces"),
        "ft" => ("foot", "feet"),
        "mi" => ("mile", "miles"),
        "mph" => ("mile per hour", "miles per hour"),
        "kph" | "km/h" => ("kilometer per hour", "kilometers per hour"),
        "ml" | "mL" => ("milliliter", "milliliters"),
        "l" | "L" => ("liter", "liters"),
        "hrs" | "hr" | "h" => ("hour", "hours"),
        "min" | "mins" => ("minute", "minutes"),
        "sec" | "secs" | "s" => ("second", "seconds"),
        "ms" => ("millisecond", "milliseconds"),
        "yrs" | "yr" => ("year", "years"),
        "GB" => ("gigabyte", "gigabytes"),
        "MB" => ("megabyte", "megabytes"),
        "TB" => ("terabyte", "terabytes"),
        "°F" => ("degree Fahrenheit", "degrees Fahrenheit"),
        "°C" => ("degree Celsius", "degrees Celsius"),
        "°" => ("degree", "degrees"),
        _ => return None,
    };
    Some(if one { singular } else { plural })
}

/// Expand one token core. Returns the spoken text and whether a trailing
/// period was consumed by an abbreviation.
fn expand_core(
    core: &str,
    trail: &str,
    next: &str,
    prev_was_number: bool,
    pending_suffix: &mut Option<&'static str>,
    skip_next: &mut bool,
) -> (String, bool) {
    let _ = trail;
    static MONEY: OnceLock<Regex> = OnceLock::new();
    static PERCENT: OnceLock<Regex> = OnceLock::new();
    static ORDINAL: OnceLock<Regex> = OnceLock::new();
    static DECADE: OnceLock<Regex> = OnceLock::new();
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    static SUFFIXED: OnceLock<Regex> = OnceLock::new();
    static RANGE: OnceLock<Regex> = OnceLock::new();
    static TIMES: OnceLock<Regex> = OnceLock::new();
    static TIME: OnceLock<Regex> = OnceLock::new();
    static HASH: OnceLock<Regex> = OnceLock::new();
    static FRACTION: OnceLock<Regex> = OnceLock::new();
    static UNIT: OnceLock<Regex> = OnceLock::new();

    if let Some(a) = abbreviation(core) {
        return (a.to_string(), core.ends_with('.'));
    }
    if core == "No." || core == "no." {
        if next.starts_with(|c: char| c.is_ascii_digit()) {
            return ("number".into(), true);
        }
    }
    if is_dotted_initialism(core) {
        let letters: Vec<String> = core
            .split('.')
            .filter(|p| !p.is_empty())
            .map(|p| p.to_uppercase())
            .collect();
        return (letters.join(" "), true);
    }
    match core {
        "&" => return ("and".into(), false),
        "+" => return ("plus".into(), false),
        "=" => return ("equals".into(), false),
        "@" => return ("at".into(), false),
        "%" => return ("percent".into(), false),
        "#" => return ("number".into(), false),
        _ => {}
    }

    // $1,299.99 / €5 / £3.5bn / $2M
    let money = re(
        &MONEY,
        r"^(-)?([$£€¥])(\d[\d,]*)(?:\.(\d+))?(k|K|m|M|mn|b|B|bn|t|T|tn)?$",
    );
    if let Some(c) = money.captures(core) {
        let (one, many, cent_one, cents) = match &c[2] {
            "$" => ("dollar", "dollars", "cent", "cents"),
            "£" => ("pound", "pounds", "penny", "pence"),
            "€" => ("euro", "euros", "cent", "cents"),
            _ => ("yen", "yen", "sen", "sen"),
        };
        let neg = if c.get(1).is_some() { "minus " } else { "" };
        let int: u64 = c[3].replace(',', "").parse().unwrap_or(0);
        let frac = c.get(4).map(|m| m.as_str());
        if let Some(scale) = c.get(5).map(|m| scale_word(m.as_str())) {
            return (
                format!("{neg}{} {scale} {many}", decimal_words(int, frac)),
                false,
            );
        }
        if is_scale(next) {
            *pending_suffix = Some(many);
            return (format!("{neg}{}", decimal_words(int, frac)), false);
        }
        return match frac {
            Some(f) if f.len() == 2 => {
                let cents_n: u64 = f.parse().unwrap_or(0);
                let dollars = format!(
                    "{} {}",
                    number_words(int),
                    if int == 1 { one } else { many }
                );
                let cent_part = format!(
                    "{} {}",
                    number_words(cents_n),
                    if cents_n == 1 { cent_one } else { cents }
                );
                (
                    format!(
                        "{neg}{}",
                        match (int, cents_n) {
                            (0, _) => cent_part,
                            (_, 0) => dollars,
                            _ => format!("{dollars} and {cent_part}"),
                        }
                    ),
                    false,
                )
            }
            Some(f) => (
                format!("{neg}{} {many}", decimal_words(int, Some(f))),
                false,
            ),
            None => (
                format!(
                    "{neg}{} {}",
                    number_words(int),
                    if int == 1 { one } else { many }
                ),
                false,
            ),
        };
    }

    if let Some(c) = re(&PERCENT, r"^(-)?(\d[\d,]*)(?:\.(\d+))?%$").captures(core) {
        let int: u64 = c[2].replace(',', "").parse().unwrap_or(0);
        let neg = if c.get(1).is_some() { "minus " } else { "" };
        return (
            format!(
                "{neg}{} percent",
                decimal_words(int, c.get(3).map(|m| m.as_str()))
            ),
            false,
        );
    }
    if let Some(c) = re(&ORDINAL, r"^(\d[\d,]*)(st|nd|rd|th|ST|ND|RD|TH)$").captures(core) {
        let n: u64 = c[1].replace(',', "").parse().unwrap_or(0);
        return (ordinal_words(n), false);
    }
    // 1990s, 90s, '90s, 1800's
    if let Some(c) = re(&DECADE, r"^'?(\d{2}|\d{4})'?s$").captures(core) {
        let digits = &c[1];
        let base = if digits.len() == 4 {
            year_words(digits.parse().unwrap_or(0))
        } else {
            number_words(digits.parse().unwrap_or(0))
        };
        return (pluralize_last(&base), false);
    }
    if let Some(c) = re(&TIME, r"^(\d{1,2}):(\d{2})$").captures(core) {
        let h: u64 = c[1].parse().unwrap_or(0);
        let m: u64 = c[2].parse().unwrap_or(0);
        let lower = next.to_ascii_lowercase();
        let ampm = matches!(lower.as_str(), "am" | "pm" | "a.m." | "p.m.");
        let minutes = match m {
            0 if ampm => String::new(),
            0 => " o'clock".to_string(),
            1..=9 => format!(" oh {}", number_words(m)),
            _ => format!(" {}", number_words(m)),
        };
        return (format!("{}{minutes}", number_words(h)), false);
    }
    if matches!(core, "am" | "pm" | "AM" | "PM") && prev_was_number {
        return (format!("{} M", core[..1].to_uppercase()), false);
    }
    if let Some(c) = re(&RANGE, r"^(\d[\d,]*)[-–](\d[\d,]*)$").captures(core) {
        let a = &c[1];
        let b = &c[2];
        let words = |s: &str| {
            let n: u64 = s.replace(',', "").parse().unwrap_or(0);
            if s.len() == 4 && !s.contains(',') {
                year_words(n)
            } else {
                number_words(n)
            }
        };
        return (format!("{} to {}", words(a), words(b)), false);
    }
    if let Some(c) = re(&TIMES, r"^(\d[\d,]*)(?:\.(\d+))?[xX×]$").captures(core) {
        let n: u64 = c[1].replace(',', "").parse().unwrap_or(0);
        return (
            format!("{} times", decimal_words(n, c.get(2).map(|m| m.as_str()))),
            false,
        );
    }
    if let Some(c) = re(&HASH, r"^#(\d+)$").captures(core) {
        return (
            format!("number {}", number_words(c[1].parse().unwrap_or(0))),
            false,
        );
    }
    if let Some(c) = re(&FRACTION, r"^(\d{1,3})/(\d{1,3})$").captures(core) {
        let a: u64 = c[1].parse().unwrap_or(0);
        let b: u64 = c[2].parse().unwrap_or(1);
        return (fraction_words(a, b), false);
    }
    if let Some(c) = re(&SUFFIXED, r"^(\d[\d,]*)(?:\.(\d+))?(k|K|M|B|bn|mn|T)$").captures(core) {
        let n: u64 = c[1].replace(',', "").parse().unwrap_or(0);
        return (
            format!(
                "{} {}",
                decimal_words(n, c.get(2).map(|m| m.as_str())),
                scale_word(&c[3])
            ),
            false,
        );
    }
    // 5km, 10kg, 98.6°F
    if let Some(c) = re(&UNIT, r"^(\d[\d,]*)(?:\.(\d+))?([a-zA-Z°/]+[FC]?)$").captures(core) {
        let n: u64 = c[1].replace(',', "").parse().unwrap_or(0);
        let frac = c.get(2).map(|m| m.as_str());
        if let Some(u) = unit_word(&c[3], n == 1 && frac.is_none()) {
            return (format!("{} {u}", decimal_words(n, frac)), false);
        }
    }
    if let Some(c) = re(&NUMBER, r"^([-−])?(\d[\d,]*)(?:\.(\d+))?$").captures(core) {
        let raw = &c[2];
        let n: u64 = raw.replace(',', "").parse().unwrap_or(0);
        let neg = if c.get(1).is_some() { "minus " } else { "" };
        let frac = c.get(3).map(|m| m.as_str());
        let lower_next = next.to_ascii_lowercase();
        let looks_like_year = raw.len() == 4
            && frac.is_none()
            && neg.is_empty()
            && ((1100..=1999).contains(&n) || (2010..=2099).contains(&n))
            && !matches!(
                lower_next.as_str(),
                "people"
                    | "dollars"
                    | "times"
                    | "units"
                    | "miles"
                    | "feet"
                    | "meters"
                    | "km"
                    | "kg"
            );
        let words = if looks_like_year {
            year_words(n)
        } else {
            decimal_words(n, frac)
        };
        // "5 km": the unit is the next token; speak it as a word there.
        if let Some(u) = unit_word(next, n == 1 && frac.is_none()) {
            if next.chars().count() <= 4 || next.starts_with('°') {
                *skip_next = true;
                return (format!("{neg}{words} {u}"), false);
            }
        }
        return (format!("{neg}{words}"), false);
    }
    (core.to_string(), false)
}

fn scale_word(s: &str) -> &'static str {
    match s {
        "k" | "K" => "thousand",
        "m" | "M" | "mn" => "million",
        "b" | "B" | "bn" => "billion",
        _ => "trillion",
    }
}

fn pluralize_last(words: &str) -> String {
    let (head, last) = match words.rfind([' ', '-']) {
        Some(i) => (&words[..=i], &words[i + 1..]),
        None => ("", words),
    };
    let plural = if let Some(stem) = last.strip_suffix('y') {
        format!("{stem}ies")
    } else if last.ends_with('x') || last.ends_with('s') {
        format!("{last}es")
    } else {
        format!("{last}s")
    };
    format!("{head}{plural}")
}

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

fn below_thousand(n: u64) -> String {
    let mut parts = Vec::new();
    let hundreds = n / 100;
    let rest = n % 100;
    if hundreds > 0 {
        parts.push(format!("{} hundred", ONES[hundreds as usize]));
    }
    if rest > 0 || hundreds == 0 {
        parts.push(below_hundred(rest));
    }
    parts.join(" ")
}

fn below_hundred(n: u64) -> String {
    if n < 20 {
        ONES[n as usize].to_string()
    } else if n % 10 == 0 {
        TENS[(n / 10) as usize].to_string()
    } else {
        format!("{}-{}", TENS[(n / 10) as usize], ONES[(n % 10) as usize])
    }
}

/// Cardinal number in words: 1234 -> "one thousand two hundred thirty-four".
pub fn number_words(n: u64) -> String {
    if n < 1000 {
        return below_thousand(n);
    }
    const SCALES: [&str; 7] = [
        "",
        "thousand",
        "million",
        "billion",
        "trillion",
        "quadrillion",
        "quintillion",
    ];
    let mut groups = Vec::new();
    let mut rest = n;
    while rest > 0 {
        groups.push(rest % 1000);
        rest /= 1000;
    }
    let mut parts = Vec::new();
    for (i, g) in groups.iter().enumerate().rev() {
        if *g == 0 {
            continue;
        }
        let words = below_thousand(*g);
        if SCALES[i].is_empty() {
            parts.push(words);
        } else {
            parts.push(format!("{words} {}", SCALES[i]));
        }
    }
    parts.join(" ")
}

/// "3" + "14" -> "three point one four".
pub fn decimal_words(int: u64, frac: Option<&str>) -> String {
    let mut s = number_words(int);
    if let Some(f) = frac.filter(|f| !f.is_empty()) {
        s.push_str(" point");
        for d in f.chars().filter_map(|c| c.to_digit(10)) {
            s.push(' ');
            s.push_str(ONES[d as usize]);
        }
    }
    s
}

/// Years read in pairs: 1984 -> "nineteen eighty-four", 2025 -> "twenty twenty-five".
pub fn year_words(n: u64) -> String {
    if !(1000..=9999).contains(&n) {
        return number_words(n);
    }
    let hi = n / 100;
    let lo = n % 100;
    if (2000..=2009).contains(&n) {
        return number_words(n);
    }
    match lo {
        0 => format!("{} hundred", below_hundred(hi)),
        1..=9 => format!("{} oh {}", below_hundred(hi), ONES[lo as usize]),
        _ => format!("{} {}", below_hundred(hi), below_hundred(lo)),
    }
}

/// 1 -> "first", 22 -> "twenty-second", 100 -> "one hundredth".
pub fn ordinal_words(n: u64) -> String {
    let cardinal = number_words(n);
    let (head, last) = match cardinal.rfind([' ', '-']) {
        Some(i) => (&cardinal[..=i], &cardinal[i + 1..]),
        None => ("", cardinal.as_str()),
    };
    let ord = match last {
        "one" => "first".to_string(),
        "two" => "second".to_string(),
        "three" => "third".to_string(),
        "five" => "fifth".to_string(),
        "eight" => "eighth".to_string(),
        "nine" => "ninth".to_string(),
        "twelve" => "twelfth".to_string(),
        l if l.ends_with('y') => format!("{}ieth", &l[..l.len() - 1]),
        l => format!("{l}th"),
    };
    format!("{head}{ord}")
}

fn fraction_words(a: u64, b: u64) -> String {
    match (a, b) {
        (1, 2) => "one half".into(),
        (_, 2) => format!("{} halves", number_words(a)),
        (1, 4) => "one quarter".into(),
        (_, 4) => format!("{} quarters", number_words(a)),
        (_, 0) => format!("{} slash zero", number_words(a)),
        (_, b) if b <= 20 => {
            let ord = ordinal_words(b);
            if a == 1 {
                format!("one {ord}")
            } else {
                format!("{} {}s", number_words(a), ord)
            }
        }
        _ => format!("{} over {}", number_words(a), number_words(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> String {
        normalize(s)
    }

    #[test]
    fn cardinals() {
        assert_eq!(number_words(0), "zero");
        assert_eq!(number_words(42), "forty-two");
        assert_eq!(number_words(100), "one hundred");
        assert_eq!(number_words(1234), "one thousand two hundred thirty-four");
        assert_eq!(number_words(2_000_000), "two million");
        assert_eq!(number_words(1_000_001), "one million one");
    }

    #[test]
    fn years_and_decades() {
        assert_eq!(n("In 1984 it began."), "In nineteen eighty-four it began.");
        assert_eq!(
            n("By 2025, everything"),
            "By twenty twenty-five, everything"
        );
        assert_eq!(n("in 2007"), "in two thousand seven");
        assert_eq!(n("since 1905"), "since nineteen oh five");
        assert_eq!(n("the 1990s"), "the nineteen nineties");
        assert_eq!(n("the '80s"), "the eighties");
        assert_eq!(
            n("from 1939-1945"),
            "from nineteen thirty-nine to nineteen forty-five"
        );
    }

    #[test]
    fn money_and_percent() {
        assert_eq!(n("It cost $5."), "It cost five dollars.");
        assert_eq!(n("$1 bill"), "one dollar bill");
        assert_eq!(n("$4.99 each"), "four dollars and ninety-nine cents each");
        assert_eq!(
            n("$1.5 million in sales"),
            "one point five million dollars in sales"
        );
        assert_eq!(n("$3B deal"), "three billion dollars deal");
        assert_eq!(n("up 45%"), "up forty-five percent");
        assert_eq!(n("4.5% of"), "four point five percent of");
        assert_eq!(n("£20"), "twenty pounds");
    }

    #[test]
    fn ordinals_decimals_misc() {
        assert_eq!(n("the 1st and 22nd"), "the first and twenty-second");
        assert_eq!(n("3rd place, 11th hour"), "third place, eleventh hour");
        assert_eq!(n("pi is 3.14"), "pi is three point one four");
        assert_eq!(n("1,000,000 people"), "one million people");
        assert_eq!(n("10x faster"), "ten times faster");
        assert_eq!(n("rock & roll"), "rock and roll");
        assert_eq!(n("at 3:30 pm"), "at three thirty P M");
        assert_eq!(n("5 km away"), "five kilometers away");
        assert_eq!(n("a 10kg bag"), "a ten kilograms bag");
        assert_eq!(n("-5 degrees"), "minus five degrees");
        assert_eq!(n("#1 hit"), "number one hit");
        assert_eq!(n("1/2 cup"), "one half cup");
        assert_eq!(n("100K views"), "one hundred thousand views");
    }

    #[test]
    fn abbreviations_and_punctuation() {
        assert_eq!(
            n("Dr. Smith vs. Mr. Jones."),
            "doctor Smith versus mister Jones."
        );
        assert_eq!(n("apples, pears, etc."), "apples, pears, et cetera.");
        assert_eq!(n("the U.S. army"), "the U S army");
        assert_eq!(n("Wait... what?!"), "Wait… what?");
        assert_eq!(n("“Quoted” words 🚀🔥 here"), "Quoted words here");
        assert_eq!(n("It's not -- no"), "It's not— no");
        assert_eq!(n("It's not — no"), "It's not— no");
        assert_eq!(n("(see above) then"), "see above, then");
    }

    #[test]
    fn keeps_original_words() {
        let words = normalize_words("He paid $1.5 million, in 1999!");
        let originals: Vec<&str> = words.iter().map(|w| w.original.as_str()).collect();
        assert_eq!(originals, ["He", "paid", "$1.5", "million,", "in", "1999!"]);
        assert_eq!(words[2].spoken, "one point five");
        assert_eq!(words[3].spoken, "million dollars");
        assert_eq!(words[3].punct, Some(','));
        assert_eq!(words[5].punct, Some('!'));
        // Emoji-only tokens disappear.
        assert_eq!(normalize_words("Wow 🔥 nice").len(), 2);
        // A merged number + unit keeps both originals together.
        let w = normalize_words("run 5 km today");
        assert_eq!(w[1].original, "5 km");
    }
}
