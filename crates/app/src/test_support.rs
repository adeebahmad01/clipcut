use std::path::PathBuf;

use clipcut_core::{Beat, Config, Paths, Project, Status, VisualKind};

use crate::state::AppState;

pub struct TestHome(pub PathBuf);

impl TestHome {
    pub fn new() -> Self {
        let home = std::env::temp_dir().join(format!("clipcut-app-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        Self(home)
    }

    pub fn state(&self) -> AppState {
        let mut config = Config::default();
        config.tts.engine = "espeak".into();
        config.search.manage_searxng = false;
        config.save(&self.0.join("config.toml")).unwrap();
        AppState::load(Paths {
            home: self.0.clone(),
        })
        .unwrap()
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn finished_project() -> Project {
    let mut project = Project::new(
        "A fixture topic",
        "",
        clipcut_core::config::Format::Vertical,
        15,
    );
    project.research = Some(Default::default());
    project.hooks = serde_json::from_value(serde_json::json!([
        {"text": "A fixture hook", "angle": "A fixture", "why": "Specific"}
    ]))
    .unwrap();
    project.chosen_hook = Some(0);
    project.script = serde_json::from_value(serde_json::json!({
        "text": "One fixture sentence.", "sentences": ["One fixture sentence."]
    }))
    .unwrap();
    project.beats = vec![text_beat()];
    project.voice_track = serde_json::from_value(serde_json::json!({
        "path": "voice/voice.wav", "duration_s": 1.5, "sample_rate": 24000,
        "engine": "fixture", "voice": "fixture", "words": [], "sentences": []
    }))
    .unwrap();
    project.render = serde_json::from_value(serde_json::json!({
        "path": "render/final.mp4", "credits_path": "render/credits.txt",
        "duration_s": 1.5, "width": 1080, "height": 1920, "rendered_at": chrono::Utc::now()
    }))
    .unwrap();
    for stage in &mut project.stages {
        stage.status = Status::Done;
        stage.progress = 1.0;
    }
    project
}

pub fn text_beat() -> Beat {
    serde_json::from_value(serde_json::json!({
        "id": "b01", "text": "One fixture sentence.",
        "visual": { "kind": VisualKind::Text, "query": "fixture", "on_screen_text": "One fixture" },
        "status": "ready", "start_s": 0.0, "end_s": 1.5
    }))
    .unwrap()
}
