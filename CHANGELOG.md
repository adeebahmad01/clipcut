# Changelog

All notable changes to ClipCut are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
- Release binaries for Linux (x86_64, ARM64) and macOS (Apple silicon,
  Intel), and a one-line installer.

[Unreleased]: https://github.com/adeebahmad01/clipcut/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/adeebahmad01/clipcut/releases/tag/v0.1.0
