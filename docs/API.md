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
| POST | `/api/projects` | `{ topic, notes?, format?, target_seconds?, voice?, autopilot?, caption_preset?, extra_formats? }` | `Project` (the pipeline starts right away) |
| GET | `/api/projects/{id}` | – | `Project` |
| PATCH | `/api/projects/{id}` | any of `{ topic, notes, voice, autopilot, format, target_seconds, chosen_hook, script_text, title, description, hashtags, caption_preset, extra_formats }` | `Project` |
| DELETE | `/api/projects/{id}` | – | `204` |
| POST | `/api/projects/{id}/run` | `{ from?: Stage, only?: bool }` | `202 Project` |
| POST | `/api/projects/{id}/cancel` | – | `202 Project` |
| POST | `/api/projects/{id}/duplicate` | – | `Project` (new id, research/hooks/script copied, later stages reset) |
| POST | `/api/projects/{id}/publish` | `{ format?, privacy?, publish_at? }` | `PublishRecord`; see [Publishing](#publishing) |

`format` is `vertical`, `horizontal` or `square`. `caption_preset` is
`pop`, `hormozi`, `mrbeast`, `minimal` or `karaoke`; `null` (or leaving it
out) uses `video.caption_preset` from the config. `extra_formats` lists the
other formats to render from the same edit; the main format and duplicates
are dropped.

`ProjectSummary`:
`{ id, topic, created_at, updated_at, format, running, stages: [StageState], title, duration_s, video_url, thumb_url, kind, published: [PublishRecord] }`.
`video_url` and `thumb_url` are null until a render exists. `kind` is
`story` (made from a topic) or `clip` (cut from a long video, see
[Clips](#clips)).

`running` is true while a pipeline task is active for the project. The server
runs at most one pipeline per project. Running a stage resets that stage and
every later stage to `pending` before it starts.

`PATCH` with `script_text` replaces the script text, re-splits it into
sentences and resets `beats` and every later stage. `chosen_hook` resets
`script` and every later stage. `format`, `caption_preset` and
`extra_formats` reset `render`.

A clip project's script comes from its source video: `PATCH` with `topic`,
`notes`, `chosen_hook`, `script_text` or a new `target_seconds` returns 400,
and so do beat edits and the beat `search`, `choose`, `auto` and `upload`
routes. While a project is uploading to YouTube, edits, `run`, `DELETE` and
another `publish` return 409.

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
| GET | `/api/projects/{id}/files/{*path}` | Any file inside the project dir (`assets/b03.mp4`, `voice/voice.wav`, `render/final.mp4`, `render/final-square.mp4`, `render/captions.srt`, `render/thumb.jpg`, `render/credits.txt`). Supports HTTP Range so `<video>` can seek. |
| GET | `/api/projects/{id}/still?t=1.5` | `image/png` frame of the current edit at `t` seconds (needs the voice stage done) |
| GET | `/api/projects/{id}/download?file=main` | A render file with `Content-Disposition: attachment`, named after the title |

`file` for `download` is one of:

| `file` | Downloads |
|---|---|
| `main` (default) | `render/final.mp4` |
| `vertical`, `horizontal`, `square` | That format's render (`render/final.mp4` for the main format, `render/final-{format}.mp4` for the others) |
| `srt`, `vtt` | `render/captions.srt` / `render/captions.vtt` |
| `thumb` | `render/thumb.jpg` (1280×720) |
| `credits` | `render/credits.txt` |

Any other value is a 400; a file that wasn't rendered is a 404. The
project's `render` object lists what exists: `variants: [{ format, path, width, height }]`,
`subtitles: { srt, vtt }` and `thumbnail_jpg`.

## Publishing

YouTube uploads use the user's own Google Cloud OAuth client ("Desktop
app" type), stored with its tokens in `<home>/youtube-oauth.json`. The
secret and tokens are never returned.

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/api/publish/youtube` | – | `AuthStatus` |
| PUT | `/api/publish/youtube` | `{ client_json }` (the downloaded JSON) or `{ client_id, client_secret }` | `AuthStatus`. A different client clears the sign-in |
| DELETE | `/api/publish/youtube?all=false` | – | `AuthStatus`. Signs out and revokes the token; `all=true` also forgets the client |
| POST | `/api/publish/youtube/connect` | – | `{ url }`: the Google sign-in page. A one-off listener on `http://127.0.0.1:<random port>` receives the redirect; the sign-in is valid for 10 minutes |
| POST | `/api/publish/youtube/callback-url` | `{ url }` | `AuthStatus`. Finishes a sign-in from the pasted `http://127.0.0.1…` address, for browsers on another device |
| POST | `/api/projects/{id}/publish` | `{ format?, privacy?, publish_at? }` | `PublishRecord` |

`AuthStatus`:
`{ configured, connected, client_id, channel, channel_id, connected_at, connecting }`.

`publish` uploads the render of `format` (default: the main format).
`privacy` is `public` (default), `unlisted` or `private`. `publish_at`
(RFC 3339, at most 730 days ahead) schedules the release: the video uploads
as private and goes public then, even if `privacy` is `unlisted`. A
`private` upload ignores `publish_at`. The request returns when the upload is
done; progress arrives on SSE under the project's id, and the upload keeps
going if the client disconnects. Errors: 400 when there is no render, no
render in that format, or no connection; 409 while the project is running
or already uploading; 502 when Google or YouTube refuse.

`PublishRecord` is appended to the project's `published` list:
`{ platform: "youtube", format, video_id, url, privacy, publish_at, uploaded_at }`.

## Queue

The queue makes videos one at a time, each with `autopilot: true`. It is
saved in `<home>/queue.json`.

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/api/queue` | – | `QueueState` |
| POST | `/api/queue` | `AddBody` (below) | `[QueueItem]`: the items added |
| PATCH | `/api/queue` | `{ paused }` | `QueueState`. Pausing lets the current item finish |
| DELETE | `/api/queue/{item}` | – | `QueueState`. Removes the item; a running item is cancelled and kept as `cancelled` |
| POST | `/api/queue/{item}/retry` | – | `QueueState`. A `failed` or `cancelled` item goes back to `waiting` and resumes its project |
| POST | `/api/queue/clear` | – | `{ removed }`: drops `done`, `failed` and `cancelled` items |

`AddBody`:
`{ topics?: [string], items?: [{ topic, notes?, ...options }], ...options, publish_every_minutes? }`,
where `options` are `{ format?, target_seconds?, voice?, caption_preset?, extra_formats?, publish?: { privacy?, publish_at? } }`.
Top-level options apply to `topics`. Blank topics are skipped; at most 200
per request. With `publish_every_minutes` (≤ 43200), item N is released N ×
that interval after the first. Asking to publish while YouTube isn't
connected is a 400.

`QueueState`: `{ items: [QueueItem], paused }`.
`QueueItem`: `{ id, topic, notes, ...options, project_id, status, error, added_at, started_at, finished_at, video_url }`,
with `status` one of `waiting`, `running`, `done`, `failed`, `cancelled`.
`video_url` is the YouTube link once published.

## Ideas

`GET /api/ideas?source=trends&geo=US&limit=12` returns
`[{ title, source, url, detail }]`.

- `source=trends` (default): Google Trends' trending searches for `geo`
  (a 2-letter country code, default `US`). Cached for 30 minutes.
- `source=news`: news results from the configured search, for `q`
  (default "top news today", at most 200 bytes).

`limit` is 1–25 (default 12). A failed fetch is a 502.

## Clips

`POST /api/clips` finds moments in a long YouTube video and makes one
`clip` project per moment.

```json
{
  "url": "https://www.youtube.com/watch?v=…",
  "count": 3,
  "min_s": 30,
  "max_s": 60,
  "format": "vertical",
  "caption_preset": "pop",
  "extra_formats": [],
  "start": true,
  "publish": { "privacy": "public", "publish_at": null },
  "publish_every_minutes": 1440,
  "job": "clips-abc123"
}
```

Only `url` is required. `count` is 1–5; `min_s` and `max_s` are 5–180.
The video needs English captions. With `start: true` (default) the clips
go into the queue; `publish` needs `start`. Progress while finding moments
arrives on SSE with `project_id` set to `job`. Response: `[Project]`.

## Live events (SSE)

`GET /api/events` returns `text/event-stream`. The UI keeps a single
EventSource open.

- `event: progress`: `data` is a `ProgressEvent`:
  `{ project_id, stage, beat_id, progress, message, level }`.
- `event: project`: `data` is `{ id, updated_at, running }`. Sent whenever
  `project.json` changes. The UI refetches `/api/projects/{id}` when it is
  viewing that project, debounced to about 300 ms.
- `event: queue`: `data` is the whole `QueueState`, sent on every queue
  change.
- `event: ping` every 15 s.
