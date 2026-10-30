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
- `clipcut completions <shell>` prints a tab-completion script for bash,
  zsh, fish, elvish or PowerShell. It completes commands, options, and the
  accepted values of `--from`, `--format`, `--captions`, `--privacy`,
  `--publish` and `setup`, which `--help` now lists too.
- `clipcut config` prints the settings and gets, sets and unsets single
  ones (`clipcut config set video.format horizontal`), checking each value
  like the app does. Changes go through the running app, which uses them
  at once, and it can fix a config.toml that ClipCut won't start with
  (which the error now points to). `--path` prints where the file is and
  `--json` prints every setting.
- `clipcut render <id>` takes `--format`, `--horizontal`, `--captions`
  and `--also`, so a finished project can be rendered again in another
  format or caption style, or with other extra formats (`--no-also` drops
  them), without redoing the visuals or voiceover. With the app running
  the change goes through it.
- `clipcut duplicate <id>` copies a project to try another take, as the
  app's Duplicate button does: the research, hook and script are kept and
  the later stages are made again by `clipcut run`. With the app running
  the copy goes through it.
- `clipcut stop <id>...` stops projects the app is making, as the
  project page's Cancel button does, and says how to carry on with
  `clipcut run`. `--force` marks a project stopped when ClipCut quit while
  it ran. Ctrl-C in `clipcut new`, `run` and `render` now stops the
  project too, rather than leaving it marked as running.
- `clipcut edit <id>` changes a project's topic, notes, length, voice,
  script (`--script file.txt`, or `-` for stdin) and YouTube title,
  description and hashtags, as the project page does, and says what to run
  to make the video again. With the app running the change goes through
  it.
- `clipcut keys` lists the free API keys, masked, and adds, removes and
  clears them as Settings → Keys does. A key can be pasted at a prompt or
  piped in so it stays out of shell history. With the app running the
  change goes through it, and the list shows which keys are resting after
  a rate limit or were rejected.
- `clipcut voices` lists the narration voices that `--voice` and
  `tts.voice` take, with the default marked, and `clipcut voices say
  <voice>` writes a short WAV sample of one (or of a blend such as
  `af_heart:0.6+am_michael:0.4`), saying which voice spoke if that one
  isn't available. `--voice` now tab-completes the voice IDs.
- `clipcut` prints a QR code of the Network URL under it, so a phone's
  camera opens the app without typing the address, and Settings → Setup
  shows the same code on a computer (`GET /api/status/qr.svg`). `--no-qr`
  leaves it out, as does output that isn't a terminal. When the app only
  listens on `::1` or another loopback address, the Network line now says
  "not exposed" and `/api/status` returns no `network_url`, and a specific
  `--host` address is shown as it is instead of the guessed LAN address.
- `clipcut beats <id>` lists a project's beat sheet: each beat's time,
  visual kind, status, source and narration, with any errors (`--json` for
  scripts). `lock` and `unlock` choose which visuals `clipcut run --from
  assets` keeps, and `redo` finds a new visual for single beats now,
  optionally with another `--kind` or `--query`. With the app running the
  changes go through it. Locking or unlocking a beat in the app no longer
  clears the rendered video.
- `clipcut beats <id> use <beat> <file>` shows your own video, GIF or
  image on a beat and locks it, as the upload button on a beat in the app
  does. The file is checked and copied into the project, and with the app
  running it goes through the app's upload.
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
