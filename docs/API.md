# ClipCut HTTP API

The `clipcut serve` process (axum, default `0.0.0.0:7878`) serves the web UI
at `/` and this JSON API under `/api`. The UI is plain HTML/CSS/JS embedded in
the binary (`crates/app/web/`), with no build step and no framework.

All bodies are JSON. JSON shapes are the serde forms of the types in
`crates/core/src/model.rs` and `config.rs` (snake_case fields, enums as
snake_case strings). Errors are `{"error": "message"}` with a 4xx/5xx status.

## Status and settings

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/api/status` | – | `Status` (below) |
| GET | `/api/config` | – | `Config` |
| PUT | `/api/config` | `Config` (full object) | `Config` (as saved) |
| GET | `/api/keys` | – | `{ services: [{ id, label, slots: [{ slot, hint, state }] }] }` with `state` = `ready`, `cooling_down` or `invalid`. All `KNOWN_SERVICES` are listed, including ones with no keys. |
| PUT | `/api/keys/{service}` | `{ keys: [string] }` (≤ 5; an empty string keeps the existing key in that slot) | same as GET `/api/keys` |
| GET | `/api/voices` | – | `[{ id, label, engine }]` |
| POST | `/api/voices/preview` | `{ voice, text }` | `audio/wav` bytes |
| POST | `/api/setup` | `{ what: "yt-dlp" \| "kokoro" \| "searxng" \| "all" }` | `202 { ok: true }`; progress arrives on SSE with `project_id = "_setup"` |

`Status`:

```json
{
  "version": "0.1.0",
  "home": "/home/me/.local/share/clipcut",
  "network_url": "http://192.168.1.20:7878",
  "tools": { "ffmpeg": "/usr/bin/ffmpeg", "yt_dlp": null, "...": "..." },
  "checks": [ { "name": "ffmpeg", "ok": true, "detail": "7.1", "fix": null } ],
  "ready": true
}
```

`checks` covers tools, the TTS engine, the LLM CLI, search (SearXNG/DDG) and
keys. A failing check carries a `fix` string: a command or a hint, which may
name a `/api/setup` target.

## Projects

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/api/projects` | – | `[ProjectSummary]`, newest first |
| POST | `/api/projects` | `{ topic, notes?, format?, target_seconds?, voice?, autopilot? }` | `Project` (the pipeline starts right away) |
| GET | `/api/projects/{id}` | – | `Project` |
| PATCH | `/api/projects/{id}` | any of `{ topic, notes, voice, autopilot, format, target_seconds, chosen_hook, script_text, title, description, hashtags }` | `Project` |
| DELETE | `/api/projects/{id}` | – | `204` |
| POST | `/api/projects/{id}/run` | `{ from?: Stage, only?: bool }` | `202 Project` |
| POST | `/api/projects/{id}/cancel` | – | `202 Project` |
| POST | `/api/projects/{id}/duplicate` | – | `Project` (new id, research/hooks/script copied, later stages reset) |

`ProjectSummary`:
`{ id, topic, created_at, updated_at, format, running, stages: [StageState], title, duration_s, video_url, thumb_url }`.
`video_url` and `thumb_url` are null until a render exists.

`running` is true while a pipeline task is active for the project. The server
runs at most one pipeline per project. Running a stage resets that stage and
every later stage to `pending` before it starts.

`PATCH` with `script_text` replaces the script text, re-splits it into
sentences and resets `beats` and every later stage. `chosen_hook` resets
`script` and every later stage.

### Pipeline pauses

With `autopilot: true` the pipeline runs every stage from research to render.
With `autopilot: false` it stops after these stages:

- `hooks`: the user picks a hook (`PATCH chosen_hook`), then `run`.
- `script`: the user may edit (`PATCH script_text`), then `run`.
- `assets`: the user reviews visuals per beat, then `run`.

A paused stage shows `status: "done"`. The next stage stays `pending` and
`running` is false. The UI shows a "Continue" button that calls
`POST /run` with no body.

## Beats (visual review)

| Method | Path | Body | Response |
|---|---|---|---|
| PATCH | `/api/projects/{id}/beats/{beat_id}` | any of `{ text, visual: VisualPlan, locked }` | `Beat` |
| POST | `/api/projects/{id}/beats/{beat_id}/search` | `{ kind?: VisualKind, query?: string }` | `Beat` with fresh `candidates` (no download) |
| POST | `/api/projects/{id}/beats/{beat_id}/choose` | `{ index }` (into `candidates`) | `Beat` (asset fetched; `locked = true`) |
| POST | `/api/projects/{id}/beats/{beat_id}/auto` | – | `Beat`: re-runs the automatic search, fallbacks and pick for this beat only; clears `locked` |
| POST | `/api/projects/{id}/beats/{beat_id}/upload` | `multipart/form-data`, field `file` | `Beat` (user file becomes the asset; `locked = true`) |

`search`, `choose` and `auto` can take 5–60 s. The UI shows a spinner on that
beat and also listens to SSE for beat messages.

## Files and previews

| Method | Path | Response |
|---|---|---|
| GET | `/api/projects/{id}/files/{*path}` | Any file inside the project dir (`assets/b03.mp4`, `voice/voice.wav`, `render/final.mp4`, `render/credits.txt`). Supports HTTP Range so `<video>` can seek. |
| GET | `/api/projects/{id}/still?t=1.5` | `image/png` frame of the current edit at `t` seconds (needs the voice stage done) |
| GET | `/api/projects/{id}/download` | `render/final.mp4` with `Content-Disposition: attachment` |

## Live events (SSE)

`GET /api/events` returns `text/event-stream`. The UI keeps a single
EventSource open.

- `event: progress`: `data` is a `ProgressEvent`:
  `{ project_id, stage, beat_id, progress, message, level }`.
- `event: project`: `data` is `{ id, updated_at, running }`. Sent whenever
  `project.json` changes. The UI refetches `/api/projects/{id}` when it is
  viewing that project, debounced to about 300 ms.
- `event: ping` every 15 s.
