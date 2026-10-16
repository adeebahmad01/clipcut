# Changelog

All notable changes to ClipCut are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Publish to YouTube from the app or `clipcut publish`, with privacy and
  scheduled release times, using your own free Google Cloud OAuth client.
- Batch queue: a Queue page and `clipcut batch topics.txt` make videos one
  after another, and can publish each one as it renders.
- Topic ideas from Google Trends or the news, in the app and with
  `clipcut ideas`.
- Clip mode: cut the best moments of a long YouTube video into captioned
  shorts (`clipcut clip <url>`).
- Caption styles: Pop, Hormozi, MrBeast, Minimal and Karaoke.
- 1:1 square format, and exporting the other formats from the same edit.
- YouTube upload defaults under Settings → Preferences (`[publish]` in
  `config.toml`): the default privacy, the category and "made for kids".
  Uploads now use them; publishing without a privacy is private unless you
  change the default.
- `clipcut list` prints every project's ID, status (running, failed,
  stopped before a stage, rendered or published), length and topic, newest
  first, so you can find the ID for `run`, `render` and `publish`.
- `run`, `render` and `publish` accept the last few characters of a
  project ID (`clipcut render 3fa9c1`), its start, or `latest` for the
  newest project. An ambiguous ID lists the projects it matches.
- `clipcut show <id>` prints one project: its topic, title and video
  settings, how long each stage took or why it failed, the path of every
  file it made (flagging missing ones) and its YouTube links. `--path`
  prints only the video's path and `--json` the whole project, for scripts.
- `clipcut delete <id>...` removes projects and their files. It lists
  what it matched with the space it frees and asks first (`--yes` skips
  that), skips running projects unless you pass `--force`, and goes
  through the app when it is running.
- `clipcut queue` shows the batch queue (each item's status, project,
  publish plan and topic, with its video link or error) and can pause,
  resume, retry, remove and clear items, through the running app or in
  `queue.json` when it isn't running. `--json` prints the whole queue.
- `.srt` and `.vtt` caption files and a 1280×720 thumbnail with every
  render.
- Docs for all of the above in Usage, Configuration, API, Architecture and
  Troubleshooting, including the Troubleshooting → Publishing section the
  app links to.

## [0.1.0] - 2026-10-09

First public release.

- Web app and CLI that turn a topic into a 9:16 or 16:9 short video.
- Stages: research, hook, script, beat sheet, visuals, voiceover and
  render, each with review and re-run, plus an Autopilot mode.
- Visuals from YouTube moments (cut to 1–3 seconds), highlighted article
  screenshots, captioned Imgflip memes, GIPHY and Tenor GIFs, Wikimedia and
  Openverse images, Iconify icons, and Pexels and Pixabay stock footage.
- Local Kokoro-82M voiceover with word-timed captions; eSpeak and custom
  command fallbacks.
- Synthesized sound effects on every cut, optional music beds with ducking.
- `credits.txt` listing every source in each video.
- Automatic setup of yt-dlp, the Kokoro model and a SearXNG container.
- `clipcut doctor` checks every dependency and suggests the fix.
- Rotation of up to five API keys per service, with rate-limit rests.
- Release binaries for Linux (x86_64, ARM64) and macOS (Apple silicon),
  and a one-line installer.

[Unreleased]: https://github.com/adeebahmad01/clipcut/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/adeebahmad01/clipcut/releases/tag/v0.1.0
