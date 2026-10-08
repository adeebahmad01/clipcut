//! App-owned skill loading and fixed scanners. Agents receive documentation
//! and reports as prompt data; never paths, staged files, or enabled tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

use crate::runner::{self, Provider, Rejection, WorkDir};
use crate::{exact_hook, invalid, schemas, text};

const SCANNERS: [(&str, &str); 5] = [
    ("constraints", "extract_constraints.py"),
    ("banned", "banned_phrase_scan.py"),
    ("structure", "structure_scan.py"),
    ("silhouette", "silhouette_scan.py"),
    ("readability", "readability_metrics.py"),
];

pub(crate) struct Skill {
    root: PathBuf,
    pub(crate) preset: String,
    pub(crate) documentation: String,
}

pub(crate) fn skill_dir() -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.extend([
            home.join(".agents/skills/unslop"),
            home.join(".codex/skills/unslop"),
            home.join(".claude/skills/unslop"),
        ]);
    }
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        roots.insert(0, PathBuf::from(home).join("skills/unslop"));
    }
    roots
        .into_iter()
        .find(|p| p.join("SKILL.md").is_file() && p.join("references/core-contract.md").is_file())
}

impl Skill {
    pub(crate) fn load(root: PathBuf, preset: &str) -> anyhow::Result<Self> {
        let preset = preset.trim().to_ascii_lowercase();
        let file = match preset.as_str() {
            "crisp" => "crisp-human.md",
            "warm" => "warm-human.md",
            "expert" => "expert-human.md",
            "story" => "story-lean.md",
            _ => bail!("unsupported unslop preset; choose crisp, warm, expert, or story"),
        };
        let root = root.canonicalize().context("unslop skill directory")?;
        let mut documentation = String::new();
        for relative in [
            "SKILL.md".to_string(),
            "references/core-contract.md".into(),
            "references/commands/rewrite.md".into(),
            format!("presets/{file}"),
        ] {
            let path = root.join(&relative);
            ensure!(
                std::fs::metadata(&path)?.len() <= 64_000,
                "unslop document is too large"
            );
            let body = std::fs::read_to_string(&path)
                .with_context(|| format!("reading unslop {relative}"))?;
            ensure!(!body.trim().is_empty(), "empty unslop document: {relative}");
            documentation.push_str(&format!("\n--- {relative} ---\n{body}\n"));
        }
        ensure!(
            documentation.len() <= 180_000,
            "unslop documentation exceeds prompt budget"
        );
        for script in SCANNERS
            .iter()
            .map(|(_, s)| *s)
            .chain(["validate_preservation.py", "diff_check.py"])
        {
            ensure!(
                root.join("scripts").join(script).is_file(),
                "required unslop scanner missing: {script}"
            );
        }
        Ok(Self {
            root,
            preset,
            documentation,
        })
    }

    async fn report(
        &self,
        script: &str,
        args: &[PathBuf],
        strict: bool,
        cwd: &Path,
    ) -> anyhow::Result<Value> {
        // Only call sites with fixed, allowlisted script names can reach this.
        ensure!(
            SCANNERS.iter().any(|(_, s)| *s == script)
                || matches!(script, "validate_preservation.py" | "diff_check.py"),
            "unsupported scanner"
        );
        let mut cmd = Command::new("python3");
        cmd.arg("-B")
            .arg(self.root.join("scripts").join(script))
            .current_dir(cwd)
            .env("PYTHONDONTWRITEBYTECODE", "1");
        if strict {
            cmd.arg("--strict");
        }
        cmd.args(args);
        let done = runner::spawn(
            Provider::Claude,
            cmd,
            None,
            Duration::from_secs(15),
            "Unslop scanner",
            None,
        )
        .await?;
        ensure!(
            matches!(done.exit_code, Some(0 | 1)),
            "unslop {script} failed: {}",
            done.stderr
        );
        let report: Value = serde_json::from_str(&done.stdout)
            .with_context(|| format!("unslop {script} returned invalid JSON"))?;
        ensure!(
            report.is_object() && report.get("error").is_none(),
            "unslop {script} failed: {report}"
        );
        Ok(report)
    }

    async fn scan_file(&self, path: &Path, cwd: &Path) -> anyhow::Result<Value> {
        let mut reports = serde_json::Map::new();
        for (name, script) in SCANNERS {
            let report = self
                .report(script, &[path.to_path_buf()], false, cwd)
                .await?;
            let field = if name == "constraints" {
                "constraints"
            } else if name == "banned" {
                "violations"
            } else {
                "flags"
            };
            ensure!(
                report.get(field).is_some_and(Value::is_array),
                "unslop {script} omitted {field}"
            );
            reports.insert(name.into(), report);
        }
        Ok(Value::Object(reports))
    }

    pub(crate) async fn scan(&self, source: &str) -> anyhow::Result<Value> {
        ensure!(
            !source.trim().is_empty() && source.len() <= 128_000,
            "unslop source must be nonempty and bounded"
        );
        let dir = WorkDir::create()?;
        let path = dir.0.join("original.txt");
        std::fs::write(&path, source)?;
        self.scan_file(&path, &dir.0).await
    }

    pub(crate) async fn validate(
        &self,
        original: &str,
        transformed: &str,
        before: &Value,
    ) -> anyhow::Result<()> {
        let dir = WorkDir::create()?;
        let source = dir.0.join("original.txt");
        let output = dir.0.join("transformed.txt");
        let constraints = dir.0.join("constraints.json");
        std::fs::write(&source, original)?;
        std::fs::write(&output, transformed)?;
        std::fs::write(&constraints, serde_json::to_vec(&before["constraints"])?)?;
        // Scientific facts and safety rules always receive the stronger gate.
        let preservation = self
            .report(
                "validate_preservation.py",
                &[source.clone(), output.clone(), constraints],
                true,
                &dir.0,
            )
            .await?;
        ensure!(
            preservation["passed"] == true
                && preservation["missing_count"] == 0
                && preservation["warnings"]
                    .as_array()
                    .is_some_and(Vec::is_empty),
            "unslop preservation failed: {preservation}"
        );
        let after = self.scan_file(&output, &dir.0).await?;
        validate_scans(before, &after)?;
        let diff = self
            .report("diff_check.py", &[source, output], false, &dir.0)
            .await?;
        ensure!(
            diff["excessive_change"] == false,
            "unslop exceeded minimal-edit budget: {diff}"
        );
        Ok(())
    }
}

#[derive(Deserialize)]
struct Finding {
    source: String,
    replacement: String,
    reason: String,
}

#[derive(Deserialize)]
struct Protection {
    source: String,
    reason: String,
}

#[derive(Deserialize)]
struct Rewrite {
    text: String,
    findings: Vec<Finding>,
    protections: Vec<Protection>,
}

pub(crate) fn parse(
    value: Value,
    original: &str,
    hook: &str,
    min: usize,
    max: usize,
    scan: &Value,
) -> Result<String, Rejection> {
    schemas::validate(&value, &schemas::unslop()).map_err(invalid)?;
    let raw: Rewrite = serde_json::from_value(value).map_err(invalid)?;
    if !exact_hook(original, hook) || !exact_hook(&raw.text, hook) {
        return Err(Rejection::Invalid(
            "the opening hook is immutable and must remain byte-exact".into(),
        ));
    }
    let count = text::word_count(&raw.text);
    if !(min..=max).contains(&count) {
        return Err(Rejection::Invalid(format!(
            "unslop output has {count} words; keep {min}-{max}"
        )));
    }
    let mut edits = Vec::new();
    let mut covered = Vec::new();
    for finding in &raw.findings {
        let matches: Vec<_> = original.match_indices(&finding.source).take(2).collect();
        if finding.source.is_empty()
            || matches.len() != 1
            || finding.reason.trim().is_empty()
            || finding.source == finding.replacement
        {
            return Err(Rejection::Invalid("every finding needs a unique exact source span, contextual reason, and actual repair".into()));
        }
        let start = matches[0].0;
        let end = start + finding.source.len();
        if start < hook.len() || finding.source.contains('\n') || finding.replacement.contains('\n')
        {
            return Err(Rejection::Invalid(
                "repairs cannot touch the hook, cross paragraphs, or change paragraph structure"
                    .into(),
            ));
        }
        edits.push((start, end, finding.replacement.as_str()));
        covered.push((start, end));
    }
    for protection in &raw.protections {
        if protection.source.is_empty() || protection.reason.trim().is_empty() {
            return Err(Rejection::Invalid(
                "every protection needs an exact source span and contextual reason".into(),
            ));
        }
        let matches: Vec<_> = original.match_indices(&protection.source).collect();
        if matches.is_empty() {
            return Err(Rejection::Invalid(
                "protected span is not present in the source".into(),
            ));
        }
        covered.extend(
            matches
                .into_iter()
                .map(|(start, span)| (start, start + span.len())),
        );
    }
    edits.sort_by_key(|e| e.0);
    if edits.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(Rejection::Invalid("confirmed edits overlap".into()));
    }
    for protection in &raw.protections {
        if original
            .match_indices(&protection.source)
            .any(|(a, s)| edits.iter().any(|(b, e, _)| a < *e && a + s.len() > *b))
        {
            return Err(Rejection::Invalid(
                "a confirmed edit overlaps protected source".into(),
            ));
        }
    }
    let mut reconstructed = original.to_string();
    for &(start, end, replacement) in edits.iter().rev() {
        reconstructed.replace_range(start..end, replacement);
    }
    if reconstructed != raw.text {
        return Err(Rejection::Invalid("output changed bytes outside its confirmed repairs; with no findings return the source exactly".into()));
    }
    // A scanner hit is only a candidate. Context must confirm or protect each
    // hard hit, including immutable-hook hits (which must be protected).
    for row in scan["banned"]["violations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["severity"] == "hard")
    {
        let offset = scan_offset(original, row).ok_or_else(|| {
            Rejection::Invalid("hard scanner hit has no valid source location".into())
        })?;
        let length = row["phrase"]
            .as_str()
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0);
        let end = original[offset..]
            .char_indices()
            .nth(length)
            .map(|(i, _)| offset + i)
            .unwrap_or(original.len());
        if !covered.iter().any(|(a, b)| *a <= offset && *b >= end) {
            return Err(Rejection::Invalid(
                "every hard scanner candidate needs a contextual finding or protection".into(),
            ));
        }
    }
    if protected_tokens(original) != protected_tokens(&raw.text) {
        return Err(Rejection::Invalid(
            "rewrite changed numbers or force-bearing never/must/all".into(),
        ));
    }
    Ok(raw.text)
}

fn scan_offset(text: &str, row: &Value) -> Option<usize> {
    let line = usize::try_from(row["line_number"].as_u64()?)
        .ok()?
        .checked_sub(1)?;
    let col = usize::try_from(row["column"].as_u64()?)
        .ok()?
        .checked_sub(1)?;
    let start = if line == 0 {
        0
    } else {
        text.match_indices('\n').nth(line - 1)?.0 + 1
    };
    let line_text = text[start..].split('\n').next()?;
    line_text.char_indices().nth(col).map(|(i, _)| start + i)
}

fn protected_tokens(source: &str) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    // Exact numeric sequences and force words supplement the skill's Python
    // preservation gate, which also handles citations, names, units and quotes.
    for word in source.split(|c: char| !c.is_alphanumeric() && !matches!(c, '.' | ',' | '%' | '-'))
    {
        if word.chars().any(|c| c.is_ascii_digit()) {
            *result.entry(word.to_string()).or_default() += 1;
        } else {
            let force = word.trim_matches(|c: char| !c.is_alphanumeric());
            if matches!(
                force.to_ascii_lowercase().as_str(),
                "never" | "must" | "all"
            ) {
                *result.entry(force.to_string()).or_default() += 1;
            }
        }
    }
    result
}

fn banned_counts(scan: &Value) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for row in scan["banned"]["violations"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if row["severity"] == "hard" || row["category"] == "anti_slop_register" {
            let key = format!("{}:{}", row["category"], row["phrase"]);
            *counts.entry(key).or_default() += 1;
        }
    }
    counts
}

fn new_flags<'a>(before: &'a Value, after: &'a Value, name: &str) -> Vec<&'a Value> {
    after[name]["flags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            !before[name]["flags"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|old| {
                    old["metric"] == row["metric"]
                        && old["severity"] == row["severity"]
                        && old["value"].as_f64().unwrap_or(0.0)
                            >= row["value"].as_f64().unwrap_or(0.0)
                })
        })
        .collect()
}

fn validate_scans(before: &Value, after: &Value) -> anyhow::Result<()> {
    let old = banned_counts(before);
    ensure!(
        !banned_counts(after)
            .iter()
            .any(|(key, n)| *n > old.get(key).copied().unwrap_or(0)),
        "unslop introduced hard or anti_slop_register scanner hits"
    );
    let structure = new_flags(before, after, "structure");
    ensure!(
        structure.is_empty(),
        "unslop introduced or worsened structural damage"
    );
    let silhouettes = new_flags(before, after, "silhouette");
    ensure!(
        !silhouettes.iter().any(|f| f["severity"] == "hard"),
        "unslop introduced hard silhouette damage"
    );
    // Soft silhouette alone is advisory. Corroborating structural damage is
    // rejected above; corroborating hard/register hits are rejected as well.
    let old_flags: Vec<&str> = before["readability"]["flags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for flag in after["readability"]["flags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        ensure!(
            !flag.to_ascii_lowercase().contains("staccato") || old_flags.contains(&flag),
            "unslop introduced or worsened staccato cadence"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty_scan() -> Value {
        json!({"banned":{"violations":[]},"structure":{"flags":[]},"silhouette":{"flags":[]},"readability":{"flags":[]}})
    }

    #[test]
    fn exact_noop_and_minimal_reconstruction() {
        let source = "The hook stays. Here's the thing: the valve opens slowly.";
        let value = json!({"text":source,"findings":[],"protections":[]});
        assert_eq!(
            parse(value, source, "The hook stays.", 1, 100, &empty_scan()).unwrap(),
            source
        );
        let changed = source.replace("Here's the thing: ", "");
        let value = json!({"text":changed,"findings":[{"source":"Here's the thing: ","replacement":"","reason":"formulaic framing"}],"protections":[]});
        assert_eq!(
            parse(value, source, "The hook stays.", 1, 100, &empty_scan()).unwrap(),
            changed
        );
        let value =
            json!({"text":format!("{source} New conclusion."),"findings":[],"protections":[]});
        assert!(parse(value, source, "The hook stays.", 1, 100, &empty_scan()).is_err());
    }

    #[test]
    fn protects_hook_facts_force_words_and_paragraphs() {
        let source = "The hook stays.\n\nAll clients must never exceed 20 requests.";
        for replacement in [
            "Clients should avoid exceeding 20 requests.",
            "All clients must never exceed 21 requests.",
            "All clients must never exceed\n20 requests.",
        ] {
            let value = json!({"text":source.replace("All clients must never exceed 20 requests.",replacement),
                "findings":[{"source":"All clients must never exceed 20 requests.","replacement":replacement,"reason":"vague framing"}],"protections":[]});
            assert!(parse(value, source, "The hook stays.", 1, 100, &empty_scan()).is_err());
        }
        let value = json!({"text":source.replace("hook","opening"),"findings":[{"source":"hook","replacement":"opening","reason":"word choice"}],"protections":[]});
        assert!(parse(value, source, "The hook stays.", 1, 100, &empty_scan()).is_err());
        let source = "The hook stays. Guarding every request is a must.";
        let value = json!({"text":source.replace("a must.","optional."),
            "findings":[{"source":"a must.","replacement":"optional.","reason":"word choice"}],"protections":[]});
        assert!(parse(value, source, "The hook stays.", 1, 100, &empty_scan()).is_err());
        assert_eq!(
            protected_tokens("All, must. never!"),
            protected_tokens("All must never")
        );
    }

    #[test]
    fn hard_hits_need_contextual_accounting() {
        let source = "The hook stays. This is a game-changer.";
        let scan = json!({"banned":{"violations":[{"phrase":"game-changer","severity":"hard","line_number":1,"column":26}]}});
        let mut value = json!({"text":source,"findings":[],"protections":[]});
        assert!(parse(value.clone(), source, "The hook stays.", 1, 100, &scan).is_err());
        value["protections"] = json!([{"source":"This is a game-changer.","reason":"quoted attribution must remain intact"}]);
        assert!(parse(value, source, "The hook stays.", 1, 100, &scan).is_ok());
    }

    #[test]
    fn scanner_gates_block_new_damage_but_not_soft_advisories() {
        let before = empty_scan();
        let mut after = empty_scan();
        after["silhouette"]["flags"] = json!([{"metric":"symmetry","severity":"soft","value":1}]);
        assert!(validate_scans(&before, &after).is_ok());
        after["readability"]["flags"] = json!(["Staccato cadence: 3 consecutive tiny sentences"]);
        assert!(validate_scans(&before, &after).is_err());
        assert!(validate_scans(&after, &after).is_ok());
        let mut after = empty_scan();
        after["banned"]["violations"] =
            json!([{"phrase":"no fluff","severity":"soft","category":"anti_slop_register"}]);
        assert!(validate_scans(&before, &after).is_err());
    }

    #[tokio::test]
    async fn app_owned_scanner_battery_is_isolated_and_blocks_preservation_loss() {
        let fixture = WorkDir::create().unwrap();
        for relative in [
            "SKILL.md",
            "references/core-contract.md",
            "references/commands/rewrite.md",
            "presets/crisp-human.md",
        ] {
            let path = fixture.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                "Preserve source facts and repair only confirmed findings.",
            )
            .unwrap();
        }
        std::fs::create_dir_all(fixture.0.join("scripts")).unwrap();
        let scanner = r#"
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
assert os.environ.get("PYTHONDONTWRITEBYTECODE") == "1"
assert not pathlib.Path("SKILL.md").exists(), "scanner runs outside skill root"
strict = args and args[0] == "--strict"
if strict:
    args = args[1:]
assert all(pathlib.Path(p).resolve().parent == pathlib.Path.cwd().resolve() for p in args)
original = pathlib.Path(args[0]).read_text()
if name == "extract_constraints.py":
    report = {"constraints": []}
elif name == "banned_phrase_scan.py":
    report = {"violations": []}
elif name in ("structure_scan.py", "silhouette_scan.py"):
    report = {"flags": []}
elif name == "readability_metrics.py":
    report = {"flags": ["Preexisting soft cadence is advisory"]}
elif name == "validate_preservation.py":
    assert strict and len(args) == 3
    assert json.loads(pathlib.Path(args[2]).read_text()) == {"constraints": []}
    same = original == pathlib.Path(args[1]).read_text()
    report = {"passed": same, "missing_count": 0 if same else 1, "warnings": []}
elif name == "diff_check.py":
    report = {"excessive_change": False}
else:
    raise AssertionError("unexpected scanner")
print(json.dumps(report))
sys.exit(1 if name == "readability_metrics.py" or report.get("passed") is False else 0)
"#;
        for script in SCANNERS
            .iter()
            .map(|(_, s)| *s)
            .chain(["validate_preservation.py", "diff_check.py"])
        {
            std::fs::write(fixture.0.join("scripts").join(script), scanner).unwrap();
        }
        let skill = Skill::load(fixture.0.clone(), "crisp").unwrap();
        assert!(
            skill
                .documentation
                .contains("--- references/core-contract.md ---")
        );
        assert!(Skill::load(fixture.0.clone(), "../../other-preset").is_err());
        let source = "The hook stays. All clients must never exceed 20 requests.";
        let before = skill.scan(source).await.unwrap();
        skill.validate(source, source, &before).await.unwrap();
        assert!(
            skill
                .validate(source, &source.replace("20", "21"), &before)
                .await
                .is_err()
        );
        assert!(!fixture.0.join("scripts/__pycache__").exists());
    }
}
