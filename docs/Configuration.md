# Configuration

ClipCut keeps its settings in `config.toml` inside its data directory:

| OS | Path |
|---|---|
| Linux | `~/.local/share/clipcut/config.toml` |
| macOS | `~/Library/Application Support/clipcut/config.toml` |

`--home PATH` or `$CLIPCUT_HOME` moves the whole data directory.

Most settings can be changed in the app under **Settings**, which writes
this file. You can also edit it by hand while ClipCut is stopped. Every key
is optional: a missing key uses the default shown below. ClipCut checks
every value on load and names the field if one is out of range.

API keys are stored separately in `keys.json` in the same directory, with
owner-only permissions. Manage them in **Settings → Keys**. The YouTube
publishing client and sign-in live in `youtube-oauth.json`, also
owner-only; manage them in **Settings → YouTube** (see
[Publish to YouTube](Usage.md#publish-to-youtube)).

## Full default file

```toml
[server]
host = "0.0.0.0"
port = 7878
open_window = true

[llm]
provider = "claude"
model = "opus"
fast_model = "sonnet"
timeout_seconds = 600
unslop_preset = "crisp"
skip_unslop = false

[video]
format = "vertical"
fps = 30
target_seconds = 45
min_clip_seconds = 1.0
max_clip_seconds = 3.0
captions = true
caption_preset = "pop"
crf = 20

[search]
searxng_url = "http://127.0.0.1:8888"
manage_searxng = true
duckduckgo_fallback = true

[youtube]
max_results = 8
max_video_minutes = 40
# cookies_from_browser = "firefox"
channel_blocklist = []

[tts]
engine = "kokoro"
voice = "am_michael"
speed = 1.1
command = ""

[audio]
voice_volume = 1.0
sfx_volume = 0.55
music_volume = 0.14
duck_music = true
use_music = true

[publish]
privacy = "private"
category_id = "22"
made_for_kids = false

[tools]
# ffmpeg = "/opt/ffmpeg/bin/ffmpeg"
```

## server

| Key | Default | Meaning |
|---|---|---|
| `host` | `"0.0.0.0"` | Address to listen on. `0.0.0.0` makes the app reachable from other devices on your network; `127.0.0.1` keeps it on this machine |
| `port` | `7878` | Port for the web app |
| `open_window` | `true` | Open the app in a Chromium window on start |

ClipCut has no login. On a shared or public network, set
`host = "127.0.0.1"`.

## llm

| Key | Default | Meaning |
|---|---|---|
| `provider` | `"claude"` | Only `claude` generates. `codex` is accepted but refused at run time, because Codex cannot run with every tool disabled |
| `model` | `"opus"` | Model for research, hooks, script and beat sheet |
| `fast_model` | `"sonnet"` | Model for quick picks: meme templates and clip moments |
| `timeout_seconds` | `600` | Per-call limit, 30–1800 |
| `unslop_preset` | `"crisp"` | Style for the unslop rewrite: `crisp`, `warm`, `expert` or `story` |
| `skip_unslop` | `false` | Keep the first draft of the script |

## video

| Key | Default | Meaning |
|---|---|---|
| `format` | `"vertical"` | `vertical` (1080×1920), `horizontal` (1920×1080) or `square` (1080×1080) |
| `fps` | `30` | Frame rate, 1–60 |
| `target_seconds` | `45` | Target length, 10–600 |
| `min_clip_seconds` | `1.0` | Shortest beat, 1–3 |
| `max_clip_seconds` | `3.0` | Longest beat, 1–3, at least `min_clip_seconds` |
| `captions` | `true` | Burn in word-by-word captions |
| `caption_preset` | `"pop"` | Caption style for new projects: `pop`, `hormozi`, `mrbeast`, `minimal` or `karaoke`. See [Caption styles](Usage.md#caption-styles) |
| `crf` | `20` | x264 quality, 0–51. Lower is better quality and a bigger file |

The format, length, voice and caption style chosen in the app override
these defaults for that project.

## search

| Key | Default | Meaning |
|---|---|---|
| `searxng_url` | `"http://127.0.0.1:8888"` | A SearXNG instance with the JSON output format enabled |
| `manage_searxng` | `true` | Let ClipCut start its own `clipcut-searxng` container with podman or docker |
| `duckduckgo_fallback` | `true` | Use DuckDuckGo's HTML results when SearXNG is unreachable |

To use a SearXNG you already run, set `searxng_url` to it, set
`manage_searxng = false`, and make sure `json` is listed under
`search.formats` in its `settings.yml`.

## youtube

| Key | Default | Meaning |
|---|---|---|
| `max_results` | `8` | Search results considered per beat |
| `max_video_minutes` | `40` | Skip longer videos, which are slow to search |
| `cookies_from_browser` | unset | Passed to `yt-dlp --cookies-from-browser`, e.g. `"firefox"` or `"chrome"`. Use it when YouTube asks to sign in to confirm you're not a bot |
| `channel_blocklist` | `[]` | Channel names or IDs never to clip from |

## tts

| Key | Default | Meaning |
|---|---|---|
| `engine` | `"kokoro"` | `kokoro` (local neural voice), `espeak` (robotic fallback) or `command` |
| `voice` | `"am_michael"` | Kokoro voice ID, such as `af_heart`, `bm_george` or `bf_emma` |
| `speed` | `1.1` | Speaking rate, 0.5–2.0 |
| `command` | `""` | For `engine = "command"`: a shell command that reads `{text_file}` and writes a WAV to `{out_wav}` |

Example using [Piper](https://github.com/rhasspy/piper):

```toml
[tts]
engine = "command"
command = "piper --model en_US-lessac-medium.onnx --output_file {out_wav} < {text_file}"
```

The environment variable `CLIPCUT_KOKORO_VARIANT` picks the Kokoro model
file: `fp16` (default, ~177 MB), `fp32` (~325 MB) or `int8` (~92 MB, faster
and slightly rougher). Run `clipcut setup kokoro` after changing it.

## audio

| Key | Default | Meaning |
|---|---|---|
| `voice_volume` | `1.0` | Voiceover level, 0–2 |
| `sfx_volume` | `0.55` | Sound effect level, 0–2 |
| `music_volume` | `0.14` | Background music level, 0–2 |
| `duck_music` | `true` | Lower the music while the voice is speaking |
| `use_music` | `true` | Use a track from the `music/` folder when there is one |

See [Sound effects and music](Usage.md#sound-effects-and-music) for the
`sfx/` and `music/` folders.

## publish

Defaults for [publishing to YouTube](Usage.md#publish-to-youtube). Change
them in **Settings → Preferences → YouTube uploads**.

| Key | Default | Meaning |
|---|---|---|
| `privacy` | `"private"` | Privacy preselected in the app, and used by `clipcut publish` without `--privacy` and by API requests that leave `privacy` out: `public`, `unlisted` or `private` |
| `category_id` | `"22"` | YouTube category of every upload, as its numeric id: `22` People & Blogs, `24` Entertainment, `27` Education, `28` Science & Technology. The app lists them all |
| `made_for_kids` | `false` | Declare uploads as made for kids. YouTube then turns off comments and personalized ads on them |

## tools

ClipCut looks for each program in its own `bin/` folder, `~/.local/bin`,
the Homebrew folders and your `PATH`. To use a specific copy, give its path
here. The keys are `ffmpeg`, `ffprobe`, `yt-dlp`, `chromium`, `claude`,
`codex`, `espeak-ng`, `podman`, `docker`, `deno` and `node`.

```toml
[tools]
ffmpeg = "/opt/ffmpeg/bin/ffmpeg"
ffprobe = "/opt/ffmpeg/bin/ffprobe"
chromium = "/usr/bin/brave-browser"
```

## Environment variables

| Variable | Effect |
|---|---|
| `CLIPCUT_HOME` | Data directory |
| `CLIPCUT_KOKORO_VARIANT` | `fp16`, `fp32` or `int8` |
| `RUST_LOG` | Log level, e.g. `debug` or `info,clipcut_sources=debug` |
