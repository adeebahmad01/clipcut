# Architecture

ClipCut is a Rust workspace that builds one binary, `clipcut`. The web UI
is plain HTML, CSS and JavaScript, embedded in the binary at build time.

```
crates/
  core/      config, data paths, tool detection, API key rotation, shared types
  llm/       runs Claude Code, prompts and JSON schemas, the unslop pass
  sources/   search, YouTube, GIFs, memes, images, icons, article screenshots, setup downloads
  tts/       Kokoro-82M through ONNX Runtime, espeak phonemes, word alignment
  render/    the director, frame compositing, sound effects, ffmpeg encoding
  app/       the clipcut binary: CLI, HTTP server, pipeline, project store, doctor
    web/     the UI (embedded with rust-embed)
assets/fonts Anton, Archivo Black, Inter (embedded)
tools/       mock-server.mjs for working on the UI without the backend
```

Dependencies only point down the list: `app` uses everything, `render`
uses `core` and `tts`, and `core` uses nothing else in the workspace.

## The pipeline

`crates/app/src/pipeline.rs` runs the stages in order and saves the
project after each one, so any stage can be re-run from the UI or with
`clipcut run <id> --from <stage>`.

1. **Research.** `sources::search` queries SearXNG, or DuckDuckGo if that
   fails. The pages are fetched and cleaned by ClipCut, then passed to
   Claude, which writes a fact sheet that cites them.
2. **Hooks.** Claude writes several opening lines.
3. **Script.** Claude writes the narration from the fact sheet and chosen
   hook. If the unslop skill is installed, a second call rewrites it and
   its scanners check the result.
4. **Beats.** Claude splits the script into 1–3 second beats and gives
   each a visual type and search terms. `llm::beats` fixes timing and
   rejects malformed output.
5. **Assets.** For each beat, `sources` searches the matching providers,
   ranks candidates, and downloads one. YouTube beats search captions with
   yt-dlp to find the moment where the words are spoken and cut only those
   seconds. Article beats load the page in headless Chromium over CDP,
   highlight the matching sentence and screenshot it. Every asset gets a
   `.source.json` recording where it came from.
6. **Voice.** `tts` normalizes the text, turns it into phonemes with
   espeak-ng, runs Kokoro and aligns each word to its time in the audio.
7. **Render.** `render::director` turns beats, assets and word timings
   into a timeline: entrance animations, captions, icons, sound effect
   cues. Frames are drawn with tiny-skia and resvg and piped into ffmpeg,
   and the audio is mixed separately (voice, synthesized effects, ducked
   music) and muxed in.

Progress is sent to the UI over server-sent events (`/api/events`).

## Running Claude safely

`llm::runner` starts `claude` in a new empty temporary directory, with the
prompt on stdin, safe mode, plan permissions, and tools, skills, hooks and
MCP servers turned off. If the installed CLI does not accept one of these
flags, the call fails rather than retrying with weaker isolation. Output is
parsed against a JSON schema and used only as data.

## Keys and rate limits

`core::keys` stores up to five keys per service. Requests rotate through
them. A key that gets a rate-limit response rests for a while; a key that
is rejected as invalid is marked and skipped until you change it.

## Data directory

`core::paths` resolves `--home`, then `$CLIPCUT_HOME`, then the platform
data directory. Writes go through `core::atomic`, which writes a temporary
file and renames it, so a crash never leaves a half-written project or
config.

## The HTTP API

The UI talks to `crates/app/src/server.rs` (axum). The endpoints are listed
in [API.md](API.md).
