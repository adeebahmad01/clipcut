//! Text stages through the local coding-agent CLIs (Claude Code by default,
//! Codex as fallback): research, hooks, script (+ unslop), beat sheet, and
//! small structured picks used while fetching visuals.
//!
//! PUBLIC API CONTRACT — the app crate codes against these signatures.

pub mod beats;
pub mod prompts;
pub mod runner;
pub mod schemas;
pub mod text;
mod unslop;

use std::path::PathBuf;
use std::time::Duration;

use clipcut_core::{
    CaptionCue, Config, Fact, HookOption, MemePlan, Progress, Research, Script, SourceRef, Tools,
    VisualKind, VisualPlan,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use beats::BeatRepair;
use runner::{Call, Rejection, Tooling};
pub use runner::{LlmError, LlmErrorKind, Provider};
use text::{
    clean_narration, normalize_ws, split_sentences, strip_citations, truncate_chars,
    truncate_words, word_count,
};

#[derive(Clone)]
pub struct Llm {
    pub config: Config,
    pub tools: Tools,
}

/// What the beat-sheet stage produces per beat (ids/timings are added by the app).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeatPlan {
    pub text: String,
    pub visual: VisualPlan,
}

/// Brief for one beat when choosing between candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateBrief {
    pub index: usize,
    pub title: String,
    pub provider: String,
    #[serde(default)]
    pub detail: String,
}

/// Narration pace used for word budgets.
pub const WORDS_PER_SECOND: f32 = 2.6;

/// Word budget for a target duration: (target, min, max).
pub fn word_budget(target_seconds: u32) -> (usize, usize, usize) {
    let words = (target_seconds.max(5) as f32 * WORDS_PER_SECOND).round() as usize;
    (
        words,
        (words as f32 * 0.8).round() as usize,
        (words as f32 * 1.2).round() as usize,
    )
}

fn or_none(s: &str) -> &str {
    if s.trim().is_empty() {
        "(none)"
    } else {
        s.trim()
    }
}

fn invalid<E: std::fmt::Display>(e: E) -> Rejection {
    Rejection::Invalid(format!("the JSON did not match the schema ({e})"))
}

fn clean_field(s: &str) -> String {
    normalize_ws(&strip_citations(s))
}

fn http_url(s: Option<String>) -> Option<String> {
    let s = s?.trim().to_string();
    if s.len() > 4096
        || !(s.starts_with("https://") || s.starts_with("http://"))
        || s.contains('\\')
        || s.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let url = url::Url::parse(&s).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(s)
}

/// Research rendered as prompt context.
pub fn research_brief(research: &Research) -> String {
    if research.facts.is_empty() {
        return "(no supplied evidence: do not invent facts or fill gaps from memory)".into();
    }
    // JSON preserves provenance and makes evidence boundaries unambiguous.
    serde_json::to_string(research).expect("Research is serializable")
}

/// Validate and bound evidence before it enters a prompt. Returned URLs retain
/// their exact supplied spelling: we never repair or guess a citation.
fn prepare_sources(sources: &[SourceRef]) -> Result<Vec<SourceRef>, Rejection> {
    let mut out: Vec<SourceRef> = Vec::new();
    let mut bytes = 0;
    for source in sources {
        let Some(url) = http_url(Some(source.url.clone())) else {
            return Err(Rejection::Unusable(
                "research context contains an invalid HTTP(S) URL".into(),
            ));
        };
        if source.snippet.trim().is_empty() {
            continue;
        }
        if out.iter().any(|s| s.url == url) {
            continue;
        }
        if out.len() == 24 {
            return Err(Rejection::Unusable(
                "research context exceeds 24 distinct sources; select relevant excerpts first"
                    .into(),
            ));
        }
        let title = normalize_ws(&source.title);
        let snippet = source.snippet.trim().to_string();
        if title.is_empty() || title.len() > 2000 || snippet.len() > 16_000 {
            return Err(Rejection::Unusable(
                "research context needs a nonempty title and an excerpt of at most 16000 bytes"
                    .into(),
            ));
        }
        bytes += title.len() + url.len() + snippet.len();
        if bytes > 128_000 {
            return Err(Rejection::Unusable(
                "research evidence exceeds 128000 bytes; select shorter excerpts first".into(),
            ));
        }
        out.push(SourceRef {
            title,
            url,
            snippet,
        });
    }
    if out.is_empty() {
        return Err(Rejection::Unusable(
            "no usable research evidence was supplied; the app must search/fetch sources first"
                .into(),
        ));
    }
    Ok(out)
}

fn normalize_hashtags(tags: Vec<String>, topic: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags
        .iter()
        .flat_map(|t| t.split_whitespace().map(str::to_string).collect::<Vec<_>>())
    {
        let body: String = tag
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if body.is_empty() {
            continue;
        }
        let tag = format!("#{body}");
        if !out.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            out.push(tag);
        }
    }
    out.truncate(5);
    for fallback in ["#shorts", "#facts", "#learnontiktok"] {
        if out.len() >= 3 {
            break;
        }
        if !out.iter().any(|t| t.eq_ignore_ascii_case(fallback)) {
            out.push(fallback.to_string());
        }
    }
    let _ = topic;
    out
}

impl Llm {
    pub fn new(config: Config, tools: Tools) -> Self {
        Self { config, tools }
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.llm.timeout_seconds.clamp(1, 1800))
    }

    fn base_call(&self, label: &str, prompt: String, schema: Value, fast: bool) -> Call {
        let timeout = if fast {
            self.timeout().min(Duration::from_secs(240))
        } else {
            self.timeout()
        };
        Call {
            label: label.into(),
            prompt: format!("{}\n\n{prompt}", prompts::OUTPUT_ONLY),
            codex_prompt: None,
            schema,
            model: if fast {
                self.config.llm.fast_model.clone()
            } else {
                self.config.llm.model.clone()
            },
            fast,
            tooling: Tooling::None,
            system: Some(prompts::SYSTEM_WRITER),
            max_turns: 3,
            timeout,
            files: Vec::new(),
        }
    }

    /// Installed providers in the order they should be tried for this call.
    pub fn providers(&self, _web: bool) -> Vec<(Provider, PathBuf)> {
        let claude = self.tools.claude.clone().map(|p| (Provider::Claude, p));
        let codex = self.tools.codex.clone().map(|p| (Provider::Codex, p));
        let codex_first = self
            .config
            .llm
            .provider
            .trim()
            .eq_ignore_ascii_case("codex");
        let order = if codex_first {
            [codex, claude]
        } else {
            [claude, codex]
        };
        order.into_iter().flatten().collect()
    }

    /// Run a structured call: retry once with a nudge on invalid output, then
    /// fall back to the other CLI on any failure.
    async fn run<T>(
        &self,
        call: Call,
        progress: Option<&Progress>,
        parse: impl Fn(Value, bool) -> Result<T, Rejection>,
    ) -> Result<T, LlmError> {
        let providers = self.providers(false);
        if providers.is_empty() {
            return Err(LlmError::new(
                None,
                LlmErrorKind::NotInstalled,
                "no coding-agent CLI found: install Claude Code (`claude`) or Codex (`codex`) and sign in",
            ));
        }
        let mut last: Option<LlmError> = None;
        let mut primary: Option<LlmError> = None;
        let mut failures: Vec<String> = Vec::new();
        for (i, (provider, program)) in providers.iter().enumerate() {
            let provider = *provider;
            let mut prompt = call.prompt.clone();
            for attempt in 0..2 {
                let err = match runner::invoke(provider, program, &call, &prompt, progress).await {
                    Ok(value) => match schemas::validate(&value, &call.schema)
                        .map_err(invalid)
                        .and_then(|_| parse(value, false))
                    {
                        Ok(out) => return Ok(out),
                        Err(Rejection::Invalid(reason)) => {
                            LlmError::new(Some(provider), LlmErrorKind::InvalidOutput, reason)
                        }
                        Err(Rejection::Unusable(reason)) => {
                            LlmError::new(Some(provider), LlmErrorKind::Unusable, reason)
                        }
                    },
                    Err(e) => e,
                };
                tracing::warn!(%provider, label = %call.label, attempt, error = %err, "agent call failed");
                let retry = err.kind == LlmErrorKind::InvalidOutput && attempt == 0;
                failures.push(err.to_string());
                if provider != Provider::Codex || err.kind != LlmErrorKind::Unusable {
                    primary = Some(err.clone());
                }
                if retry {
                    if let Some(p) = progress {
                        p.info(format!(
                            "{}: output was invalid ({}); asking {provider} again",
                            call.label, err.message
                        ));
                    }
                    prompt = runner::nudge(&call.prompt, &err.message);
                }
                last = Some(err);
                if !retry {
                    break;
                }
            }
            if let (Some(p), Some(err), Some((next, _))) = (progress, &last, providers.get(i + 1)) {
                p.warn(format!("{}: {err}. Trying {next} instead.", call.label));
            }
        }
        let mut error = primary.or(last).expect("at least one provider ran");
        // Do not hide the primary auth/rate failure behind a disabled fallback.
        error.message = format!("all available providers failed: {}", failures.join("; "));
        Err(error)
    }

    /// Legacy entry point. An agent never browses; call `research_with_context`
    /// after the app has gathered evidence. No provider is invoked here.
    pub async fn research(
        &self,
        _topic: &str,
        _notes: &str,
        _progress: &Progress,
    ) -> anyhow::Result<Research> {
        anyhow::bail!(
            "research requires app-supplied evidence; call research_with_context(topic, notes, sources, progress)"
        )
    }

    /// Evidence-only analysis. The app owns search/fetch; agents have no tools.
    /// Facts and source URLs are validated against the supplied evidence, and
    /// returned source titles/snippets always come from the app, not the model.
    pub async fn research_with_context(
        &self,
        topic: &str,
        notes: &str,
        sources: &[SourceRef],
        progress: &Progress,
    ) -> anyhow::Result<Research> {
        let sources = prepare_sources(sources).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let today = chrono::Local::now().format("%A, %B %-d, %Y").to_string();
        let evidence = serde_json::to_string(&sources)?;
        let prompt = prompts::render(
            prompts::RESEARCH,
            &[
                ("today", today.as_str()),
                ("topic", topic.trim()),
                ("notes", or_none(notes)),
                ("sources", evidence.as_str()),
            ],
        );
        let mut call = self.base_call("Researching", prompt, schemas::research(), false);
        call.system = Some(prompts::SYSTEM_RESEARCH);
        progress.info(format!(
            "Analyzing {} app-supplied sources (no agent tools)…",
            sources.len()
        ));
        let research = self
            .run(call, Some(progress), |v, _| {
                parse_research_with_context(v, &sources)
            })
            .await?;
        progress.info(format!(
            "Research: {} facts from {} sources",
            research.facts.len(),
            research.sources.len()
        ));
        Ok(research)
    }

    /// 3-5 hook options, best first.
    pub async fn hooks(
        &self,
        topic: &str,
        notes: &str,
        research: &Research,
        progress: &Progress,
    ) -> anyhow::Result<Vec<HookOption>> {
        let brief = research_brief(research);
        let prompt = prompts::render(
            prompts::HOOKS,
            &[
                ("topic", topic.trim()),
                ("notes", or_none(notes)),
                ("research", &brief),
            ],
        );
        let call = self.base_call("Writing hooks", prompt, schemas::hooks(), false);
        progress.info("Writing hook options…");
        Ok(self.run(call, Some(progress), parse_hooks).await?)
    }

    /// Draft the narration around `hook`, then run it through the unslop skill.
    pub async fn script(
        &self,
        topic: &str,
        notes: &str,
        research: &Research,
        hook: &str,
        target_seconds: u32,
        progress: &Progress,
    ) -> anyhow::Result<Script> {
        let hook = normalize_ws(hook);
        let (words, min_words, max_words) = word_budget(target_seconds);
        let brief = research_brief(research);
        let (seconds, words_s, min_s, max_s) = (
            target_seconds.to_string(),
            words.to_string(),
            min_words.to_string(),
            max_words.to_string(),
        );
        let prompt = prompts::render(
            prompts::SCRIPT,
            &[
                ("topic", topic.trim()),
                ("notes", or_none(notes)),
                ("research", &brief),
                ("hook", &hook),
                ("seconds", &seconds),
                ("words", &words_s),
                ("min_words", &min_s),
                ("max_words", &max_s),
            ],
        );
        let call = self.base_call("Writing the script", prompt, schemas::script(), false);
        progress.info(format!(
            "Writing a {target_seconds} s script (~{words} words)…"
        ));
        let draft = self
            .run(call, Some(progress), |v, lenient| {
                parse_script(v, lenient, &hook, words, topic)
            })
            .await?;
        progress.info(format!("Draft: {} words", word_count(&draft.text)));

        let mut text = draft.text.clone();
        if self.config.llm.skip_unslop {
            progress.info("Skipping the unslop pass (disabled in settings)");
        } else {
            match self
                .unslop(&draft.text, &hook, target_seconds, progress)
                .await
            {
                Ok(rewritten) => {
                    progress.info(format!(
                        "Unslop: {} → {} words",
                        word_count(&draft.text),
                        word_count(&rewritten)
                    ));
                    text = rewritten;
                }
                Err(e) => progress.warn(format!("Unslop pass failed, keeping the draft: {e}")),
            }
        }
        let sentences = split_sentences(&text);
        Ok(Script {
            text,
            sentences,
            draft: draft.text,
            title: draft.title,
            description: draft.description,
            hashtags: draft.hashtags,
        })
    }

    /// Rewrite `text` with the unslop skill; Err means "keep the draft".
    pub async fn unslop(
        &self,
        text: &str,
        hook: &str,
        target_seconds: u32,
        progress: &Progress,
    ) -> anyhow::Result<String> {
        let skill = unslop::Skill::load(unslop_skill_dir().ok_or_else(|| anyhow::anyhow!("unslop is not installed in ~/.agents/skills, ~/.codex/skills, or ~/.claude/skills"))?, &self.config.llm.unslop_preset)?;
        let (_, min_words, max_words) = word_budget(target_seconds);
        let scan = skill.scan(text).await?;
        let scan_report = serde_json::to_string(&scan)?;
        let source_json = serde_json::to_string(text)?;
        let hook_json = serde_json::to_string(hook)?;
        let (seconds, min_s, max_s) = (
            target_seconds.to_string(),
            min_words.to_string(),
            max_words.to_string(),
        );
        let vars = [
            ("preset", skill.preset.as_str()),
            ("text", source_json.as_str()),
            ("hook", hook_json.as_str()),
            ("skill_text", skill.documentation.as_str()),
            ("scan_report", scan_report.as_str()),
            ("seconds", seconds.as_str()),
            ("min_words", min_s.as_str()),
            ("max_words", max_s.as_str()),
        ];
        let call = self.base_call(
            "Unslopping the script",
            prompts::render(prompts::UNSLOP, &vars),
            schemas::unslop(),
            false,
        );
        progress.info(format!(
            "Removing confirmed AI tells with embedded unslop ({})…",
            skill.preset
        ));
        let out = self
            .run(call, Some(progress), |v, _| {
                unslop::parse(v, text, hook, min_words, max_words, &scan)
            })
            .await?;
        skill.validate(text, &out, &scan).await?;
        Ok(out)
    }

    /// Split the script into 1-3 s beats and plan a visual for each.
    pub async fn beats(
        &self,
        topic: &str,
        script: &Script,
        research: &Research,
        progress: &Progress,
    ) -> anyhow::Result<Vec<BeatPlan>> {
        let script_text = normalize_ws(&script.text);
        let sentences = if script.sentences.is_empty()
            || normalize_ws(&script.sentences.join(" ")) != script_text
        {
            split_sentences(&script_text)
        } else {
            script.sentences.iter().map(|s| normalize_ws(s)).collect()
        };
        let words = word_count(&script_text);
        let min_beats = (words / 8).max(3);
        let max_beats = (words / 3).max(min_beats + 2);
        let seconds = ((words as f32) / WORDS_PER_SECOND).round() as u32;
        let numbered = sentences
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let facts = research
            .facts
            .iter()
            .take(12)
            .map(|f| format!("- {}", f.text.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        let keywords = research.keywords.join(", ");
        let (words_s, seconds_s, min_s, max_s) = (
            words.to_string(),
            seconds.to_string(),
            min_beats.to_string(),
            max_beats.to_string(),
        );
        let prompt = prompts::render(
            prompts::BEATS,
            &[
                ("topic", topic.trim()),
                ("keywords", or_none(&keywords)),
                ("facts", or_none(&facts)),
                ("sentences", &numbered),
                ("words", &words_s),
                ("seconds", &seconds_s),
                ("min_beats", &min_s),
                ("max_beats", &max_s),
            ],
        );
        let call = self.base_call("Planning beats", prompt, schemas::beats(), false);
        progress.info("Cutting the script into beats and planning visuals…");
        let planned = self.run(call, Some(progress), parse_beats).await?;
        let (beats, repair) = beats::reconcile_beats(&script_text, planned);
        match repair {
            BeatRepair::Exact => {}
            BeatRepair::Realigned => progress.info("Beat texts drifted slightly from the script; re-aligned them"),
            BeatRepair::Rechunked => progress.warn("Beat texts did not match the script; re-cut the beats and carried the visuals over"),
        }
        let beats = balance_kinds(
            beats
                .into_iter()
                .map(|b| BeatPlan {
                    visual: beats::sanitize_plan(&b.text, b.visual),
                    text: b.text,
                })
                .collect(),
        );
        progress.info(format!(
            "Beat sheet: {} beats ({})",
            beats.len(),
            kind_mix(&beats)
        ));
        Ok(beats)
    }

    /// Choose the caption window (start,end seconds) in a video that best
    /// shows `beat_text`; `None` if nothing fits.
    pub async fn pick_moment(
        &self,
        beat_text: &str,
        quote: Option<&str>,
        video_title: &str,
        cues: &[CaptionCue],
        clip_seconds: f32,
    ) -> anyhow::Result<Option<(f32, f32)>> {
        if cues.is_empty() {
            return Ok(None);
        }
        let clip = if clip_seconds.is_finite() && clip_seconds > 0.0 {
            clip_seconds
        } else {
            3.0
        };
        let video_end = cues
            .iter()
            .map(|c| c.end_s)
            .fold(0.0f32, f32::max)
            .max(clip);
        let window = cue_window(cues, beat_text, quote, 400);
        let lines = window
            .iter()
            .map(|c| {
                format!(
                    "[{:.1}] {}",
                    c.start_s,
                    truncate_chars(&normalize_ws(&c.text), 160)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (min_len, max_len) = ((clip * 0.7).max(0.5), clip * 1.5);
        let vars = [
            ("clip", format!("{clip:.1}")),
            ("beat", normalize_ws(beat_text)),
            (
                "quote",
                quote
                    .map(normalize_ws)
                    .filter(|q| !q.is_empty())
                    .unwrap_or_else(|| "(none)".into()),
            ),
            ("title", normalize_ws(video_title)),
            ("end", format!("{video_end:.1}")),
            ("cues", lines),
            ("min_len", format!("{min_len:.1}")),
            ("max_len", format!("{max_len:.1}")),
        ];
        let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let call = self.base_call(
            "Finding the moment",
            prompts::render(prompts::PICK_MOMENT, &vars),
            schemas::moment(),
            true,
        );
        Ok(self
            .run(call, None, |v, lenient| {
                parse_moment(v, lenient, clip, video_end)
            })
            .await?)
    }

    /// Pick an Imgflip template (by exact name from `templates`) and write the caption.
    pub async fn pick_meme(
        &self,
        beat_text: &str,
        plan: Option<&MemePlan>,
        templates: &[String],
    ) -> anyhow::Result<MemePlan> {
        let idea = plan
            .map(|p| {
                let caption = if p.caption.trim().is_empty() {
                    format!("{} / {}", p.top_text, p.bottom_text)
                } else {
                    p.caption.clone()
                };
                format!(
                    "template \"{}\", caption \"{}\"",
                    p.template.trim(),
                    caption.trim()
                )
            })
            .unwrap_or_else(|| "(none)".into());
        let list = if templates.is_empty() {
            "(any well-known Imgflip template)".to_string()
        } else {
            templates.join("\n")
        };
        let prompt = prompts::render(
            prompts::PICK_MEME,
            &[
                ("beat", &normalize_ws(beat_text)),
                ("idea", &idea),
                ("templates", &list),
            ],
        );
        let call = self.base_call("Picking a meme", prompt, schemas::meme(templates), true);
        let fallback = plan.cloned();
        Ok(self
            .run(call, None, |v, lenient| {
                parse_meme(v, lenient, templates, fallback.as_ref())
            })
            .await?)
    }

    /// Pick the best candidate index for a beat (or None if all are off-topic).
    pub async fn pick_candidate(
        &self,
        beat_text: &str,
        query: &str,
        candidates: &[CandidateBrief],
    ) -> anyhow::Result<Option<usize>> {
        if candidates.is_empty() {
            return Ok(None);
        }
        let list = candidates
            .iter()
            .map(|c| {
                let detail = truncate_chars(&normalize_ws(&c.detail), 240);
                let title = normalize_ws(&c.title);
                if detail.is_empty() {
                    format!("{}. [{}] {title}", c.index, c.provider)
                } else {
                    format!("{}. [{}] {title} | {detail}", c.index, c.provider)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let normalized_beat = normalize_ws(beat_text);
        let prompt = prompts::render(
            prompts::PICK_CANDIDATE,
            &[
                ("beat", normalized_beat.as_str()),
                ("query", query.trim()),
                ("candidates", list.as_str()),
            ],
        );
        let call = self.base_call("Choosing a visual", prompt, schemas::candidate(), true);
        let valid: Vec<usize> = candidates.iter().map(|c| c.index).collect();
        Ok(self
            .run(call, None, |v, _| match v.get("index") {
                None => Err(invalid("missing index")),
                Some(Value::Null) => Ok(None),
                Some(i) => match i
                    .as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .filter(|i| valid.contains(i))
                {
                    Some(i) => Ok(Some(i)),
                    None => Err(Rejection::Invalid(format!(
                        "index {i} is not one of the listed candidates {valid:?}"
                    ))),
                },
            })
            .await?)
    }
}

/// App-owned discovery; a spawned agent never reads these paths.
pub fn unslop_skill_dir() -> Option<PathBuf> {
    unslop::skill_dir()
}

#[derive(Deserialize)]
struct RawResearch {
    web_search_ok: bool,
    summary: String,
    facts: Vec<Fact>,
    sources: Vec<SourceRef>,
    angles: Vec<String>,
    keywords: Vec<String>,
}

/// Compatibility parser. It checks citations against the declared evidence;
/// callers with app evidence should use `parse_research_with_context` instead.
/// The legacy lenient flag never relaxes the contract.
pub fn parse_research(value: Value, _lenient: bool) -> Result<Research, Rejection> {
    schemas::validate(&value, &schemas::research()).map_err(invalid)?;
    let raw: RawResearch = serde_json::from_value(value.clone()).map_err(invalid)?;
    let sources = prepare_sources(&raw.sources)?;
    parse_research_with_context(value, &sources)
}

/// Validate every citation against evidence supplied by the app, never against
/// source records invented by the model. This does not prove claim entailment.
pub fn parse_research_with_context(
    value: Value,
    sources: &[SourceRef],
) -> Result<Research, Rejection> {
    schemas::validate(&value, &schemas::research()).map_err(invalid)?;
    let evidence = prepare_sources(sources)?;
    let raw: RawResearch = serde_json::from_value(value).map_err(invalid)?;
    if !raw.web_search_ok || raw.facts.len() < 3 {
        return Err(Rejection::Unusable(format!(
            "supplied evidence returned too little ({} facts, evidence_ok={})",
            raw.facts.len(),
            raw.web_search_ok
        )));
    }
    if raw.facts.len() > 15 {
        return Err(Rejection::Invalid(
            "return 3-15 distinct supported facts, not padded evidence".into(),
        ));
    }
    let summary = clean_field(&raw.summary);
    if summary.is_empty() {
        return Err(Rejection::Invalid("summary is empty".into()));
    }
    let mut cited: Vec<SourceRef> = Vec::new();
    for s in raw.sources {
        let source = evidence.iter().find(|e| e.url == s.url).ok_or_else(|| {
            Rejection::Invalid(format!(
                "source URL {:?} was not supplied by the app",
                s.url
            ))
        })?;
        if !cited.iter().any(|e| e.url == source.url) {
            cited.push(source.clone());
        }
    }
    let mut facts: Vec<Fact> = Vec::new();
    for fact in raw.facts {
        let text = clean_field(&fact.text);
        if text.is_empty() || facts.iter().any(|f| f.text.eq_ignore_ascii_case(&text)) {
            return Err(Rejection::Invalid(
                "fact text must be nonempty and distinct; do not pad evidence".into(),
            ));
        }
        let source = cited
            .iter()
            .find(|e| Some(&e.url) == fact.source_url.as_ref())
            .ok_or_else(|| {
                Rejection::Invalid(
                    "every fact needs an app-supplied URL also listed in sources".into(),
                )
            })?;
        facts.push(Fact {
            text,
            source_url: Some(source.url.clone()),
            source_title: Some(source.title.clone()),
        });
    }
    let list = |v: Vec<String>, max: usize| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in v.iter().map(|s| clean_field(s)).filter(|s| !s.is_empty()) {
            if !out.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
                out.push(s);
            }
        }
        out.truncate(max);
        out
    };
    Ok(Research {
        summary,
        facts,
        sources: cited,
        angles: list(raw.angles, 5),
        keywords: list(raw.keywords, 20),
    })
}

/// Validate hook options: 3-5, at most 14 words each, best first.
pub fn parse_hooks(value: Value, _lenient: bool) -> Result<Vec<HookOption>, Rejection> {
    schemas::validate(&value, &schemas::hooks()).map_err(invalid)?;
    #[derive(Deserialize)]
    struct Raw {
        hooks: Vec<HookOption>,
    }
    let raw: Raw = serde_json::from_value(value).map_err(invalid)?;
    if !(3..=5).contains(&raw.hooks.len()) {
        return Err(Rejection::Invalid(
            "return 3-5 distinct hook options".into(),
        ));
    }
    let mut hooks: Vec<HookOption> = raw
        .hooks
        .into_iter()
        .map(|h| HookOption {
            text: clean_narration(&strip_citations(&h.text)),
            angle: clean_field(&h.angle),
            score: h.score,
            why: clean_field(&h.why),
        })
        .collect();
    if hooks.iter().any(|h| {
        h.text.is_empty()
            || h.angle.is_empty()
            || h.why.is_empty()
            || !h.score.is_finite()
            || !(0.0..=10.0).contains(&h.score)
    }) {
        return Err(Rejection::Invalid(
            "every hook needs nonempty text, angle, why, and a score from 0 to 10".into(),
        ));
    }
    let long: Vec<String> = hooks
        .iter()
        .filter(|h| word_count(&h.text) > 14)
        .map(|h| format!("\"{}\"", h.text))
        .collect();
    if !long.is_empty() {
        return Err(Rejection::Invalid(format!(
            "these hooks are over 14 words: {}",
            long.join(", ")
        )));
    }
    let mut seen: Vec<Vec<String>> = Vec::new();
    for h in &hooks {
        let key = text::norm_words(&h.text);
        if seen.contains(&key) {
            return Err(Rejection::Invalid("hook options must be distinct".into()));
        }
        seen.push(key);
    }
    // Stable: the model's own order breaks ties.
    hooks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(hooks)
}

/// Validate the script draft (`sentences` is filled by the caller).
pub fn parse_script(
    value: Value,
    _lenient: bool,
    hook: &str,
    words: usize,
    topic: &str,
) -> Result<Script, Rejection> {
    schemas::validate(&value, &schemas::script()).map_err(invalid)?;
    #[derive(Deserialize)]
    struct Raw {
        text: String,
        title: String,
        description: String,
        hashtags: Vec<String>,
    }
    let raw: Raw = serde_json::from_value(value).map_err(invalid)?;
    let text = clean_narration(&strip_citations(&raw.text));
    let n = word_count(&text);
    let (min, max) = (
        (words as f32 * 0.8).round() as usize,
        (words as f32 * 1.2).round() as usize,
    );
    if n < min || n > max {
        let (smin, smax) = (
            (words as f32 * 0.8).round() as usize,
            (words as f32 * 1.2).round() as usize,
        );
        return Err(Rejection::Invalid(format!(
            "the script has {n} words; it must have {smin}-{smax} words (about {words})"
        )));
    }
    if !exact_hook(&text, hook) {
        return Err(Rejection::Invalid(format!(
            "the script must start with the hook word for word: \"{hook}\""
        )));
    }
    let title = truncate_chars(&clean_field(&raw.title).trim_matches('"').to_string(), 70);
    if title.is_empty() || raw.description.trim().is_empty() {
        return Err(Rejection::Invalid(
            "title and description must be nonempty".into(),
        ));
    }
    Ok(Script {
        sentences: split_sentences(&text),
        draft: text.clone(),
        text,
        title,
        description: clean_field(&raw.description),
        hashtags: normalize_hashtags(raw.hashtags, topic),
    })
}

/// Deserialize the beat sheet and sanitize each visual (texts are reconciled by the caller).
pub fn parse_beats(value: Value, _lenient: bool) -> Result<Vec<BeatPlan>, Rejection> {
    schemas::validate(&value, &schemas::beats()).map_err(invalid)?;
    #[derive(Deserialize)]
    struct Raw {
        beats: Vec<BeatPlan>,
    }
    let raw: Raw = serde_json::from_value(value).map_err(invalid)?;
    if raw
        .beats
        .iter()
        .any(|b| b.text.trim().is_empty() || b.visual.query.trim().is_empty())
    {
        return Err(Rejection::Invalid(
            "every beat needs nonempty narration and a visual query".into(),
        ));
    }
    let beats: Vec<BeatPlan> = raw
        .beats
        .into_iter()
        .map(|b| BeatPlan {
            text: normalize_ws(&b.text),
            visual: b.visual,
        })
        .collect();
    if beats.len() < 2 {
        return Err(Rejection::Invalid(format!(
            "only {} beats; cut the whole script into beats of 3-8 words",
            beats.len()
        )));
    }
    Ok(beats)
}

/// Enforce the hard mix rules deterministically: at most one stock beat
/// (extra ones become YouTube b-roll searches) and no run of 3+ identical kinds
/// when a beat can be flipped to a text card.
pub fn balance_kinds(mut beats: Vec<BeatPlan>) -> Vec<BeatPlan> {
    let mut stock_seen = false;
    for b in &mut beats {
        if b.visual.kind == VisualKind::Stock {
            if stock_seen {
                b.visual.kind = VisualKind::Youtube;
            }
            stock_seen = true;
        }
    }
    for i in 2..beats.len() {
        let k = beats[i].visual.kind;
        if k == beats[i - 1].visual.kind && k == beats[i - 2].visual.kind {
            // Flip the middle beat if it has an overlay worth showing as a card,
            // else the current one, to a different but safe kind.
            let target = if beats[i - 1].visual.on_screen_text.is_some() {
                i - 1
            } else {
                i
            };
            let text = beats[target].text.clone();
            let v = &mut beats[target].visual;
            // Meme/GIF queries make poor footage searches, so those become text cards.
            v.kind = match k {
                VisualKind::Youtube
                | VisualKind::Stock
                | VisualKind::Article
                | VisualKind::Text => VisualKind::Image,
                VisualKind::Image => VisualKind::Youtube,
                VisualKind::Gif | VisualKind::Meme => VisualKind::Text,
            };
            v.meme = None;
            if v.kind == VisualKind::Text {
                v.query = truncate_words(&text, 6);
            }
            beats[target].visual = beats::sanitize_plan(&text, beats[target].visual.clone());
        }
    }
    beats
}

/// "youtube 5, text 3, ..." summary for progress messages.
pub fn kind_mix(beats: &[BeatPlan]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for b in beats {
        let name = serde_json::to_value(b.visual.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, c)) => *c += 1,
            None => counts.push((name, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts
        .iter()
        .map(|(n, c)| format!("{n} {c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// At most `max` cues, centered on the ones that best match the beat/quote.
pub fn cue_window<'a>(
    cues: &'a [CaptionCue],
    beat: &str,
    quote: Option<&str>,
    max: usize,
) -> &'a [CaptionCue] {
    if cues.len() <= max {
        return cues;
    }
    let target: Vec<String> = text::norm_words(&format!("{} {}", quote.unwrap_or(""), beat))
        .into_iter()
        .filter(|w| w.len() > 3)
        .collect();
    let mut best = (0usize, 0usize);
    for (i, _) in cues.iter().enumerate() {
        let span = &cues[i..(i + 3).min(cues.len())];
        let words: Vec<String> = span
            .iter()
            .flat_map(|c| text::norm_words(&c.text))
            .collect();
        let score = target.iter().filter(|t| words.contains(t)).count();
        if score > best.1 {
            best = (i, score);
        }
    }
    let start = best.0.saturating_sub(max / 2).min(cues.len() - max);
    &cues[start..start + max]
}

/// Validate a picked moment and clamp it to a usable clip.
pub fn parse_moment(
    value: Value,
    _lenient: bool,
    clip: f32,
    video_end: f32,
) -> Result<Option<(f32, f32)>, Rejection> {
    schemas::validate(&value, &schemas::moment()).map_err(invalid)?;
    if !clip.is_finite() || clip <= 0.0 || !video_end.is_finite() || video_end <= 0.0 {
        return Err(Rejection::Unusable(
            "clip and video duration must be finite and positive".into(),
        ));
    }
    let found = value["found"].as_bool().expect("validated boolean");
    let num = |k: &str| {
        value
            .get(k)
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .filter(|v| v.is_finite())
    };
    if !found {
        if !value["start"].is_null() || !value["end"].is_null() {
            return Err(Rejection::Invalid(
                "found=false requires null start and end".into(),
            ));
        }
        return Ok(None);
    }
    let Some(mut start) = num("start") else {
        return Err(Rejection::Invalid(
            "found=true needs a numeric start".into(),
        ));
    };
    let Some(mut end) = num("end") else {
        return Err(Rejection::Invalid("found=true needs a numeric end".into()));
    };
    if end <= start {
        return Err(Rejection::Invalid("end must be greater than start".into()));
    }
    start = start.clamp(0.0, (video_end - 0.5).max(0.0));
    let len = end - start;
    if !(clip * 0.5..=clip * 1.6).contains(&len) {
        end = start + clip;
    }
    if end > video_end {
        end = video_end;
        start = (end - clip).max(0.0);
    }
    if end - start < 0.3 {
        return Ok(None);
    }
    Ok(Some((start, end)))
}

/// Map the meme answer onto a canonical template name and trim the captions.
pub fn parse_meme(
    value: Value,
    _lenient: bool,
    templates: &[String],
    fallback: Option<&MemePlan>,
) -> Result<MemePlan, Rejection> {
    schemas::validate(&value, &schemas::meme_plan()).map_err(invalid)?;
    let raw: MemePlan = serde_json::from_value(value).map_err(invalid)?;
    let wanted = normalize_ws(&raw.template);
    let canonical = |name: &str| -> Option<String> {
        let key = text::norm_words(name).join(" ");
        templates
            .iter()
            .find(|t| t.eq_ignore_ascii_case(name.trim()) || text::norm_words(t).join(" ") == key)
            .cloned()
    };
    let template = if templates.is_empty() {
        wanted
    } else {
        match canonical(&wanted) {
            Some(t) => t,
            None => {
                return Err(Rejection::Invalid(format!(
                    "\"{wanted}\" is not in the template list; use a name exactly as listed"
                )));
            }
        }
    };
    if template.is_empty() {
        return Err(Rejection::Invalid("template is empty".into()));
    }
    let mut caption = truncate_words(&clean_field(&raw.caption), 12);
    let top_text = truncate_words(&clean_field(&raw.top_text), 8);
    let bottom_text = truncate_words(&clean_field(&raw.bottom_text), 8);
    if caption.is_empty() && top_text.is_empty() && bottom_text.is_empty() {
        caption = fallback
            .map(|p| truncate_words(&p.caption, 12))
            .unwrap_or_default();
    }
    Ok(MemePlan {
        template,
        caption,
        top_text,
        bottom_text,
    })
}

/// Byte-exact opening, never a fuzzy prefix match or a partial word.
pub(crate) fn exact_hook(text: &str, hook: &str) -> bool {
    !hook.is_empty()
        && text.strip_prefix(hook).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with(char::is_whitespace)
                || hook.ends_with(|c: char| !c.is_alphanumeric())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn research_json(n: usize, ok: bool) -> Value {
        let facts: Vec<Value> = (0..n)
            .map(|i| json!({"text": format!("Fact {i} ([site](https://x.org/{i}))."), "source_url": format!("https://x.org/{i}"), "source_title": "X"}))
            .collect();
        let sources: Vec<Value> = (0..n).map(|i| json!({"title":"X","url":format!("https://x.org/{i}"),"snippet":format!("Source excerpt {i}")})).collect();
        json!({"web_search_ok": ok, "summary": "Sum.", "facts": facts, "sources": sources,
               "angles": ["a", "A", "b"], "keywords": ["k1", "k2"]})
    }

    #[test]
    fn research_validation() {
        let r = parse_research(research_json(9, true), false).unwrap();
        assert_eq!(r.facts.len(), 9);
        assert_eq!(r.facts[0].text, "Fact 0.");
        assert_eq!(r.sources.len(), 9, "every fact cites declared evidence");
        assert_eq!(r.angles, vec!["a", "b"]);
        assert!(matches!(
            parse_research(research_json(9, false), false),
            Err(Rejection::Unusable(_))
        ));
        assert!(matches!(
            parse_research(research_json(2, true), false),
            Err(Rejection::Unusable(_))
        ));
        assert!(parse_research(research_json(5, true), false).is_ok());
        assert!(parse_research(research_json(5, true), true).is_ok());
        assert!(matches!(
            parse_research(research_json(20, true), true),
            Err(Rejection::Invalid(_))
        ));
    }

    #[test]
    fn hooks_validation() {
        let v = json!({"hooks": [
            {"text": "Short hook one.", "angle": "a", "score": 6, "why": "w"},
            {"text": "\"Short hook two.\"", "angle": "a", "score": 9, "why": "w"},
            {"text": "Short hook three.", "angle": "a", "score": 7, "why": "w"},
        ]});
        let hooks = parse_hooks(v, false).unwrap();
        assert_eq!(hooks[0].text, "Short hook two.");
        assert_eq!(hooks.len(), 3);
        let long = json!({"hooks": [{"text": "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen", "angle": "", "score": 5, "why": ""}]});
        assert!(matches!(
            parse_hooks(long.clone(), false),
            Err(Rejection::Invalid(_))
        ));
        assert!(matches!(
            parse_hooks(long, true),
            Err(Rejection::Invalid(_))
        ));
    }

    #[test]
    fn script_validation() {
        let hook = "Octopuses have three hearts.";
        let body = "Two pump blood through the gills. ".repeat(12);
        let v = json!({"text": format!("{hook} {body}"), "title": "x".repeat(90), "description": "d", "hashtags": ["octopus", "#science"]});
        let s = parse_script(v.clone(), false, hook, 78, "octopus").unwrap();
        assert!(s.title.chars().count() <= 70);
        assert_eq!(
            s.hashtags[..2],
            ["#octopus".to_string(), "#science".to_string()]
        );
        assert!(s.hashtags.len() >= 3);
        assert_eq!(s.sentences.join(" "), s.text);
        assert!(matches!(
            parse_script(v.clone(), false, hook, 200, "o"),
            Err(Rejection::Invalid(_))
        ));
        assert!(matches!(
            parse_script(
                v,
                false,
                "A completely different opening line here.",
                78,
                "o"
            ),
            Err(Rejection::Invalid(_))
        ));
    }

    #[test]
    fn moment_clamping() {
        let v = json!({"found": true, "start": 10.0, "end": 13.0});
        assert_eq!(
            parse_moment(v, false, 3.0, 100.0).unwrap(),
            Some((10.0, 13.0))
        );
        let v = json!({"found": true, "start": 98.0, "end": 140.0});
        assert_eq!(
            parse_moment(v, false, 3.0, 100.0).unwrap(),
            Some((97.0, 100.0))
        );
        let v = json!({"found": false, "start": null, "end": null});
        assert_eq!(parse_moment(v, false, 3.0, 100.0).unwrap(), None);
        let v = json!({"found": true, "start": null, "end": null});
        assert!(parse_moment(v, false, 3.0, 100.0).is_err());
    }

    #[test]
    fn meme_mapping() {
        let templates = vec!["Drake Hotline Bling".to_string(), "Two Buttons".to_string()];
        let v = json!({"template": "drake hotline bling", "caption": "one two three four five six seven eight nine ten eleven twelve thirteen", "top_text": "", "bottom_text": ""});
        let m = parse_meme(v, false, &templates, None).unwrap();
        assert_eq!(m.template, "Drake Hotline Bling");
        assert_eq!(word_count(&m.caption), 12);
        let v = json!({"template": "Nope", "caption": "c", "top_text": "", "bottom_text": ""});
        assert!(parse_meme(v.clone(), false, &templates, None).is_err());
        assert!(parse_meme(v, true, &templates, None).is_err());
    }

    #[test]
    fn research_citations_are_allowlisted_and_metadata_is_app_owned() {
        let value = research_json(3, true);
        let sources: Vec<SourceRef> = serde_json::from_value(value["sources"].clone()).unwrap();
        let mut altered = value.clone();
        altered["sources"][0]["title"] = json!("Invented title");
        altered["sources"][0]["snippet"] = json!("Invented excerpt");
        altered["facts"][0]["source_title"] = json!("Invented title");
        let parsed = parse_research_with_context(altered, &sources).unwrap();
        assert_eq!(parsed.sources[0].snippet, sources[0].snippet);
        assert_eq!(parsed.facts[0].source_title.as_deref(), Some("X"));
        let mut altered = value.clone();
        altered["facts"][0]["source_url"] = json!("https://invented.example/");
        assert!(parse_research_with_context(altered, &sources).is_err());
        let mut altered = value;
        altered["sources"][0]["url"] = json!("https://invented.example/");
        assert!(parse_research_with_context(altered, &sources).is_err());
    }

    #[test]
    fn retries_do_not_relax_structural_or_semantic_contracts() {
        assert!(parse_research(json!({"facts":[]}), true).is_err());
        assert!(parse_beats(json!({"beats":[{"text":"Some words","visual":{}}]}), true).is_err());
        assert!(parse_moment(json!({"found":false}), true, 3.0, 10.0).is_err());
        assert!(
            parse_moment(
                json!({"found":true,"start":2.0,"end":null}),
                true,
                3.0,
                10.0
            )
            .is_err()
        );
        let value = json!({"text":"The hook changes. Some facts remain.","title":"Title","description":"Description","hashtags":["#shorts"]});
        assert!(parse_script(value, true, "The hook stays.", 7, "topic").is_err());
        assert!(!exact_hook("A headlineExtra words", "A headline"));
    }

    #[test]
    fn malformed_urls_and_empty_evidence_fail_closed() {
        for url in [
            "https:example.com",
            "https://",
            "https://user:pass@example.com",
            "https://bad example.com",
            "file:///tmp/data",
            "https://example.com\\other",
        ] {
            assert!(http_url(Some(url.into())).is_none(), "{url}");
        }
        assert!(prepare_sources(&[]).is_err());
        let source = SourceRef {
            title: "Example".into(),
            url: "https://example.com".into(),
            snippet: " ".into(),
        };
        assert!(prepare_sources(&[source]).is_err());
    }

    #[test]
    fn kind_balancing() {
        let plan = |kind| VisualPlan {
            kind,
            query: "q".into(),
            alt_queries: vec![],
            on_screen_text: None,
            icon: None,
            meme: None,
            quote: None,
            sfx: None,
        };
        let beats: Vec<BeatPlan> = [
            VisualKind::Stock,
            VisualKind::Stock,
            VisualKind::Youtube,
            VisualKind::Youtube,
            VisualKind::Youtube,
        ]
        .into_iter()
        .map(|k| BeatPlan {
            text: "a b c".into(),
            visual: plan(k),
        })
        .collect();
        let out = balance_kinds(beats);
        let kinds: Vec<VisualKind> = out.iter().map(|b| b.visual.kind).collect();
        assert_eq!(kinds.iter().filter(|k| **k == VisualKind::Stock).count(), 1);
        for w in kinds.windows(3) {
            assert!(!(w[0] == w[1] && w[1] == w[2]), "{kinds:?}");
        }
    }

    #[test]
    fn cue_window_centers_on_match() {
        let cues: Vec<CaptionCue> = (0..1000)
            .map(|i| CaptionCue {
                start_s: i as f32,
                end_s: i as f32 + 1.0,
                text: if i == 700 {
                    "octopus hearts".into()
                } else {
                    "blah".into()
                },
            })
            .collect();
        let w = cue_window(&cues, "three octopus hearts", None, 100);
        assert_eq!(w.len(), 100);
        assert!(w.iter().any(|c| c.text == "octopus hearts"));
    }
}
