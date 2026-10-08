//! Offline protocol/safety tests. No credentials, provider calls, global env
//! changes, or alternate Cargo target directory.
#![cfg(unix)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clipcut_core::{CaptionCue, Config, Progress, SourceRef, Tools};
use clipcut_llm::runner::{self, Call, LlmErrorKind, Provider, Tooling};
use clipcut_llm::{CandidateBrief, Llm, beats, prompts, schemas};
use serde_json::{Value, json};

struct Fake {
    root: PathBuf,
    executable: PathBuf,
}

impl Fake {
    fn new(responses: Vec<Value>) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "clipcut-llm-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        let executable = root.join("claude-fake");
        let responses_path = root.join("responses.json");
        std::fs::write(&responses_path, serde_json::to_vec(&responses).unwrap()).unwrap();
        let script = format!(
            r#"#!/usr/bin/env python3
import json, os, sys, time
root = {root}
responses = json.load(open(os.path.join(root, "responses.json")))
log = os.path.join(root, "calls.jsonl")
count = sum(1 for _ in open(log)) if os.path.exists(log) else 0
r = responses[min(count, len(responses)-1)]
if r.get("before_stdin"):
    time.sleep(10)
prompt = sys.stdin.read()
with open(log, "a") as f:
    f.write(json.dumps({{"argv":sys.argv[1:], "stdin":prompt, "cwd":os.getcwd(),
        "mode":os.stat(".").st_mode & 0o777, "files":os.listdir(".")}})+"\n")
if r.get("fork_hold"):
    pid = os.fork()
    if pid == 0:
        time.sleep(10)
        os._exit(0)
    with open(os.path.join(root,"child.pid"),"w") as f:
        f.write(str(pid))
if r.get("sleep"):
    time.sleep(r["sleep"])
if r.get("bytes"):
    sys.stdout.write("x"*r["bytes"])
else:
    sys.stdout.write(r.get("stdout",""))
sys.stdout.flush()
sys.stderr.write(r.get("stderr",""))
sys.stderr.flush()
sys.exit(r.get("exit",0))
"#,
            root = serde_json::to_string(&root.to_string_lossy()).unwrap()
        );
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, executable }
    }

    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn llm(&self) -> Llm {
        let mut config = Config::default();
        config.llm.skip_unslop = true;
        config.llm.timeout_seconds = 2;
        Llm::new(config, tools(Some(self.executable.clone()), None))
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn tools(claude: Option<PathBuf>, codex: Option<PathBuf>) -> Tools {
    Tools {
        ffmpeg: None,
        ffprobe: None,
        yt_dlp: None,
        chromium: None,
        claude,
        codex,
        espeak_ng: None,
        podman: None,
        docker: None,
        deno: None,
        node: None,
    }
}

fn success(value: Value) -> Value {
    json!({"stdout":json!({"type":"result","subtype":"success","is_error":false,"structured_output":value}).to_string()})
}

fn call(tooling: Tooling) -> Call {
    Call {
        label: "Offline test".into(),
        prompt: "Untrusted prompt with {{braces}}".into(),
        codex_prompt: None,
        schema: schemas::candidate(),
        model: "opus".into(),
        fast: false,
        tooling,
        system: None,
        max_turns: 3,
        timeout: Duration::from_secs(2),
        files: vec![],
    }
}

fn flag<'a>(args: &'a [Value], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|v| v == name)
        .and_then(|i| args.get(i + 1))
        .and_then(Value::as_str)
}

fn assert_isolated(record: &Value) {
    let args = record["argv"].as_array().unwrap();
    assert_eq!(args[0], "-p");
    assert_eq!(flag(args, "--tools"), Some(""));
    assert_eq!(flag(args, "--disallowedTools"), Some("mcp__*"));
    assert_eq!(flag(args, "--permission-mode"), Some("plan"));
    assert_eq!(flag(args, "--permission-prompts"), Some("none"));
    assert_eq!(flag(args, "--mcp-config"), Some(runner::EMPTY_MCP));
    assert_eq!(flag(args, "--setting-sources"), Some("user"));
    assert!(
        flag(args, "--system-prompt")
            .unwrap()
            .contains(prompts::SYSTEM_SAFE)
    );
    for name in [
        "--safe-mode",
        "--disable-slash-commands",
        "--no-chrome",
        "--strict-mcp-config",
        "--no-session-persistence",
    ] {
        assert!(args.iter().any(|v| v == name), "missing {name}");
    }
    for name in [
        "--allowedTools",
        "--add-dir",
        "--dangerously-skip-permissions",
        "--bare",
    ] {
        assert!(!args.iter().any(|v| v == name), "unsafe {name}");
    }
    assert_eq!(
        record["files"],
        json!([]),
        "no staged files or repository access"
    );
    assert_eq!(record["mode"], 0o700);
    assert!(
        !PathBuf::from(record["cwd"].as_str().unwrap()).exists(),
        "isolated cwd removed"
    );
}

fn hooks() -> Value {
    json!({"hooks":[
        {"text":"The hook stays.","angle":"mechanism","score":9,"why":"The evidence describes this mechanism."},
        {"text":"Three valves open slowly.","angle":"number","score":8,"why":"The evidence describes three valves."},
        {"text":"Pressure moves the valve.","angle":"cause","score":7,"why":"The evidence describes pressure."}
    ]})
}

fn context() -> Vec<SourceRef> {
    vec![SourceRef {
        title: "App source".into(),
        url: "https://example.org/valve".into(),
        snippet:
            "Three valves open slowly. Pressure moves the valve. The gate closes under gravity."
                .into(),
    }]
}

fn research() -> Value {
    let url = &context()[0].url;
    json!({"web_search_ok":true,"summary":"The source describes the valves.",
        "facts":[
            {"text":"Three valves open slowly.","source_url":url,"source_title":"App source"},
            {"text":"Pressure moves the valve.","source_url":url,"source_title":"App source"},
            {"text":"The gate closes under gravity.","source_url":url,"source_title":"App source"}],
        "sources":context(),"angles":["valve mechanism"],"keywords":["valve","pressure"]})
}

#[tokio::test]
async fn all_legacy_tool_hints_are_tools_disabled_and_stdin_only() {
    let fake = Fake::new(vec![success(json!({"index":1}))]);
    for tooling in [
        Tooling::None,
        Tooling::Web,
        Tooling::Unslop {
            skill_dir: "/not/staged".into(),
        },
    ] {
        let c = call(tooling);
        let result = runner::invoke(Provider::Claude, &fake.executable, &c, &c.prompt, None)
            .await
            .unwrap();
        assert_eq!(result["index"], 1);
    }
    let records = fake.calls();
    assert_eq!(records.len(), 3);
    for record in records {
        assert_isolated(&record);
        assert_eq!(record["stdin"], "Untrusted prompt with {{braces}}");
        assert!(
            !record["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == &record["stdin"])
        );
    }
}

#[tokio::test]
async fn retry_is_bounded_and_keeps_identical_security_flags() {
    let fake = Fake::new(vec![success(json!({"hooks":[]})), success(hooks())]);
    let llm = fake.llm();
    let evidence = clipcut_llm::parse_research_with_context(research(), &context()).unwrap();
    let out = llm
        .hooks("valves", "", &evidence, &Progress::log_only("test"))
        .await
        .unwrap();
    assert_eq!(out.len(), 3);
    let records = fake.calls();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["argv"], records[1]["argv"]);
    assert!(
        records[1]["stdin"]
            .as_str()
            .unwrap()
            .contains("YOUR PREVIOUS ANSWER WAS INVALID")
    );
    records.iter().for_each(assert_isolated);
}

#[tokio::test]
async fn invalid_retry_never_becomes_lenient_success() {
    let fake = Fake::new(vec![success(json!({"hooks":[]}))]);
    let evidence = clipcut_llm::parse_research_with_context(research(), &context()).unwrap();
    let error = fake
        .llm()
        .hooks("valves", "", &evidence, &Progress::log_only("test"))
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<runner::LlmError>().unwrap().kind,
        LlmErrorKind::InvalidOutput
    );
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn source_allowlist_survives_retry_and_empty_context_never_spawns() {
    let mut invented = research();
    invented["facts"][0]["source_url"] = json!("https://invented.example/fact");
    let fake = Fake::new(vec![success(invented)]);
    let progress = Progress::log_only("test");
    assert!(
        fake.llm()
            .research_with_context("valves", "", &[], &progress)
            .await
            .is_err()
    );
    assert!(fake.llm().research("valves", "", &progress).await.is_err());
    assert!(fake.calls().is_empty());
    assert!(
        fake.llm()
            .research_with_context("valves", "", &context(), &progress)
            .await
            .is_err()
    );
    assert_eq!(fake.calls().len(), 2);
    for row in fake.calls() {
        assert_isolated(&row);
        assert!(
            row["stdin"]
                .as_str()
                .unwrap()
                .contains(&context()[0].snippet)
        );
    }
}

#[tokio::test]
async fn codex_fails_closed_then_falls_back_without_spawning_it() {
    let fake = Fake::new(vec![success(hooks())]);
    let codex = Fake::new(vec![success(json!({"index":0}))]);
    let mut llm = fake.llm();
    llm.config.llm.provider = "codex".into();
    llm.tools.codex = Some(codex.executable.clone());
    let evidence = clipcut_llm::parse_research_with_context(research(), &context()).unwrap();
    assert!(
        llm.hooks("valves", "", &evidence, &Progress::log_only("test"))
            .await
            .is_ok()
    );
    assert_eq!(fake.calls().len(), 1);
    assert!(codex.calls().is_empty());
    llm.tools.claude = None;
    let error = llm
        .hooks("valves", "", &evidence, &Progress::log_only("test"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("generation disabled"));
    assert!(codex.calls().is_empty());
}

#[tokio::test]
async fn rate_failure_is_not_retried_or_hidden_by_disabled_codex() {
    let fake = Fake::new(vec![
        json!({"stdout":"warning: proxy failed","stderr":"HTTP 429 rate_limit_error","exit":1}),
    ]);
    let mut llm = fake.llm();
    llm.tools.codex = Some(fake.executable.clone());
    let evidence = clipcut_llm::parse_research_with_context(research(), &context()).unwrap();
    let error = llm
        .hooks("valves", "", &evidence, &Progress::log_only("test"))
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<runner::LlmError>().unwrap().kind,
        LlmErrorKind::RateLimit
    );
    assert_eq!(fake.calls().len(), 1);
    assert!(error.to_string().contains("generation disabled"));
}

#[tokio::test]
async fn final_envelope_error_nonzero_and_log_output_are_rejected() {
    let responses = vec![
        json!({"stdout":json!({"type":"result","subtype":"error_during_execution","is_error":true,
            "result":"authentication_error","structured_output":{"index":1}}).to_string()}),
        json!({"stdout":json!({"type":"result","subtype":"success","is_error":false,"structured_output":{"index":1}}).to_string(),"exit":1}),
        json!({"stdout":json!({"type":"assistant","structured_output":{"index":1}}).to_string()}),
        json!({"stdout":r#"{"index":1}"#}),
    ];
    let fake = Fake::new(responses);
    let c = call(Tooling::None);
    for expected in [
        LlmErrorKind::Auth,
        LlmErrorKind::Failed,
        LlmErrorKind::InvalidOutput,
        LlmErrorKind::InvalidOutput,
    ] {
        let error = runner::invoke(Provider::Claude, &fake.executable, &c, &c.prompt, None)
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
    }
}

#[tokio::test]
async fn file_staging_is_rejected_without_invocation() {
    let fake = Fake::new(vec![success(json!({"index":1}))]);
    let mut c = call(Tooling::None);
    c.files.push(("../leak".into(), "secret".into()));
    assert_eq!(
        runner::invoke(Provider::Claude, &fake.executable, &c, &c.prompt, None)
            .await
            .unwrap_err()
            .kind,
        LlmErrorKind::Unusable
    );
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn deadlines_cover_exit_inherited_pipes_and_stdin() {
    for response in [
        json!({"sleep":10}),
        json!({"fork_hold":true,"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"structured_output\":{\"index\":1}}"}),
        json!({"before_stdin":true}),
    ] {
        let fake = Fake::new(vec![response]);
        let mut c = call(Tooling::None);
        c.timeout = Duration::from_millis(200);
        let started = Instant::now();
        let error = runner::invoke(
            Provider::Claude,
            &fake.executable,
            &c,
            &"x".repeat(500_000),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, LlmErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
        if let Ok(pid) = std::fs::read_to_string(fake.root.join("child.pid")) {
            // A reparented zombie is dead, not a running leaked descendant.
            tokio::time::sleep(Duration::from_millis(20)).await;
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
                assert_eq!(stat.split(") ").nth(1).unwrap().chars().next(), Some('Z'));
            }
        }
    }
}

#[tokio::test]
async fn output_capture_is_bounded() {
    let fake = Fake::new(vec![json!({"bytes":runner::MAX_CAPTURE_BYTES+100})]);
    let c = call(Tooling::None);
    let error = runner::invoke(Provider::Claude, &fake.executable, &c, &c.prompt, None)
        .await
        .unwrap_err();
    assert_eq!(error.kind, LlmErrorKind::Failed);
    assert!(error.message.contains("capture limit"));
}

#[tokio::test]
async fn offline_pipeline_smoke_preserves_script_and_citation_contracts() {
    let narration =
        "The hook stays. Three valves open slowly under pressure. The gate closes under gravity.";
    let visual = json!({"kind":"youtube","query":"valve demonstration","alt_queries":[],"on_screen_text":null,
        "icon":null,"meme":null,"quote":null,"sfx":null});
    let fake = Fake::new(vec![
        success(research()),
        success(hooks()),
        success(
            json!({"text":narration,"title":"Three valves","description":"A pressure-operated gate.","hashtags":["#shorts","#valve","#pressure"]}),
        ),
        success(
            json!({"beats":[{"text":"The hook stays.","visual":visual},{"text":"Three valves open slowly under pressure. The gate closes under gravity.","visual":visual}]}),
        ),
        success(json!({"found":true,"start":1.0,"end":3.0})),
        success(
            json!({"template":"Two Buttons","caption":"pressure or gravity","top_text":"","bottom_text":""}),
        ),
        success(json!({"index":7})),
    ]);
    let llm = fake.llm();
    let progress = Progress::log_only("offline-pipeline");
    let research = llm
        .research_with_context("valves", "", &context(), &progress)
        .await
        .unwrap();
    let hooks = llm.hooks("valves", "", &research, &progress).await.unwrap();
    let script = llm
        .script("valves", "", &research, &hooks[0].text, 5, &progress)
        .await
        .unwrap();
    assert_eq!(script.text, narration);
    assert_eq!(script.sentences.join(" "), script.text);
    let beat_sheet = llm
        .beats("valves", &script, &research, &progress)
        .await
        .unwrap();
    assert!(beats::beats_match_script(&script.text, &beat_sheet));
    let cues = vec![CaptionCue {
        start_s: 0.0,
        end_s: 5.0,
        text: "valve demonstration".into(),
    }];
    assert!(
        llm.pick_moment("The valve moves.", None, "valves", &cues, 2.0)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        llm.pick_meme("pressure or gravity", None, &["Two Buttons".into()])
            .await
            .unwrap()
            .template,
        "Two Buttons"
    );
    let candidates = vec![CandidateBrief {
        index: 7,
        title: "Valves".into(),
        provider: "youtube".into(),
        detail: String::new(),
    }];
    assert_eq!(
        llm.pick_candidate("valves", "valve demonstration", &candidates)
            .await
            .unwrap(),
        Some(7)
    );
    let records = fake.calls();
    assert_eq!(records.len(), 7);
    records.iter().for_each(assert_isolated);
}
