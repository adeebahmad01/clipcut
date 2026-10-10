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
