# Roadmap

ClipCut already turns a typed topic into a finished short on your own
machine. The next releases make it safe to leave running on a shared
network, give you a real editor in the browser, add editing styles and
themes so every channel can have its own look, open it up to local models
and more languages, and connect it to the places you post and the tools you
automate with. It stays free to run, local first and one binary.

**How this file works.** Every planned item has an ID such as `CC-001`.
Tick its box (`- [x]`) in the same pull request that ships it. The
matching prompt for an AI coding agent is in
[ROADMAP-PROMPTS.md](ROADMAP-PROMPTS.md), linked from each item. The
website reads this file, so progress shows up there on the next deploy.

## Shipped
Status: Done
Everything in 0.1.0 and on `main` under Unreleased in the changelog.

- [x] **Topic to video pipeline**: Research, hook, script, beat sheet, visuals, voiceover and render, each saved so it can be reviewed and re-run.
- [x] **Review or Autopilot**: Pause after the hook, script and visuals to edit them, or go straight to a finished `final.mp4`.
- [x] **Real footage first**: YouTube moments cut to 1–3 seconds by their captions, highlighted article screenshots, captioned memes, GIFs, images and icons, with stock as the last resort.
- [x] **Local Kokoro voice**: A word-timed voiceover that runs on your CPU, with eSpeak and custom command fallbacks.
- [x] **Sound on every cut**: Synthesized sound effects on each cut and optional music beds ducked under the voice.
- [x] **Credits for every source**: `credits.txt` lists every clip and image, and each asset keeps a `.source.json`.
- [x] **Five caption styles**: Pop, Hormozi, MrBeast, Minimal and Karaoke, set per project or as the default.
- [x] **Three formats from one edit**: 9:16, 16:9 and 1:1, with each layout rebuilt rather than cropped.
- [x] **Caption files and thumbnail**: `.srt` and `.vtt` files and a 1280×720 thumbnail with every render.
- [x] **Batch queue**: Paste many topics or run `clipcut batch` and the videos are made one after another, with retry from the failed stage.
- [x] **Clip mode**: Cut the best moments of a long YouTube video into captioned shorts.
- [x] **Topic ideas**: Trending searches from Google Trends or news headlines, in the app and with `clipcut ideas`.
- [x] **YouTube publishing**: Upload now or on a schedule with your own free Google Cloud client, from the app, the CLI or the queue.
- [x] **Full command line**: `new`, `run`, `render --format`, `list`, `show`, `delete`, `queue`, `config` and `completions`, with short project IDs.
- [x] **Doctor and automatic setup**: `clipcut doctor` checks every dependency, and yt-dlp, Kokoro and SearXNG install themselves.
- [x] **Release binaries and installer**: Linux and Apple silicon builds with checksums, a one-line installer, and key rotation for the free APIs.

## v0.2: Steady and safe
Status: Now
Make runs survive limits and restarts, lock down the LAN app, and make ClipCut easy to install, update and test.

- [ ] **CC-001 Access token for the web UI**: Requests from other devices need a secret token that the printed Network URL carries, so nobody else on the Wi-Fi can use the app or spend your keys. ([prompt](ROADMAP-PROMPTS.md#cc-001-access-token-for-the-web-ui))
- [ ] **CC-002 QR code for the Network URL**: The terminal and Settings show a QR code of the Network URL so you can open the app on your phone without typing it. ([prompt](ROADMAP-PROMPTS.md#cc-002-qr-code-for-the-network-url))
- [ ] **CC-003 Wait out Claude usage limits**: When Claude Code hits a usage limit, the queue pauses until it resets and then carries on, instead of failing every video left. ([prompt](ROADMAP-PROMPTS.md#cc-003-wait-out-claude-usage-limits))
- [ ] **CC-004 Retry flaky beat downloads**: A beat whose download times out or drops is retried with backoff before ClipCut falls back to another kind of visual. ([prompt](ROADMAP-PROMPTS.md#cc-004-retry-flaky-beat-downloads))
- [ ] **CC-005 Resume uploads after a restart**: A YouTube upload cut off by closing ClipCut picks up where it stopped on the next start. ([prompt](ROADMAP-PROMPTS.md#cc-005-resume-uploads-after-a-restart))
- [ ] **CC-006 Shared asset cache**: The same YouTube moment, GIF or image used again, in another project or on a re-run, comes from a local cache instead of a new download. ([prompt](ROADMAP-PROMPTS.md#cc-006-shared-asset-cache))
- [ ] **CC-007 Disk cleanup command**: `clipcut clean` and Settings show how much space the cache, old renders and leftovers use, and remove what you pick. ([prompt](ROADMAP-PROMPTS.md#cc-007-disk-cleanup-command))
- [ ] **CC-008 Log file and bug report**: ClipCut keeps a rotating log file, and `clipcut doctor --report` writes one redacted file to attach to an issue. ([prompt](ROADMAP-PROMPTS.md#cc-008-log-file-and-bug-report))
- [ ] **CC-009 Self-update command**: `clipcut update` installs the newest release after checking its checksum, and the app tells you when one is out. ([prompt](ROADMAP-PROMPTS.md#cc-009-self-update-command))
- [ ] **CC-010 Docker image**: A container image with ffmpeg, espeak-ng and Chromium runs ClipCut on distros whose glibc is too old for the release binary. ([prompt](ROADMAP-PROMPTS.md#cc-010-docker-image))
- [ ] **CC-011 Homebrew tap**: `brew install adeebahmad01/clipcut/clipcut` installs ClipCut and its dependencies, and each release updates the formula. ([prompt](ROADMAP-PROMPTS.md#cc-011-homebrew-tap))
- [ ] **CC-012 Format and lint checks in CI**: The workspace is formatted once, and CI fails on unformatted code or Clippy warnings from then on. ([prompt](ROADMAP-PROMPTS.md#cc-012-format-and-lint-checks-in-ci))
- [ ] **CC-013 Complete the mock server**: The UI mock server fakes the queue, clips, ideas and YouTube routes, so every page can be worked on without the backend. ([prompt](ROADMAP-PROMPTS.md#cc-013-complete-the-mock-server))
- [ ] **CC-014 Browser tests for the web UI**: Playwright tests drive the main flows against the mock server in CI and catch a broken page before it ships. ([prompt](ROADMAP-PROMPTS.md#cc-014-browser-tests-for-the-web-ui))
- [ ] **CC-015 Document every optional key**: The README and docs list every service ClipCut can use a free key for, including Unsplash, Brave Search and Tavily. ([prompt](ROADMAP-PROMPTS.md#cc-015-document-every-optional-key))

## v0.3: A real editor
Status: Next
Let you shape the cut by hand in the browser, check it quickly before the final render, and render faster.

- [ ] **CC-016 Beat timeline**: The project page shows every beat on a timeline sized by its length, with a playhead that follows the voiceover. ([prompt](ROADMAP-PROMPTS.md#cc-016-beat-timeline))
- [ ] **CC-017 Drag to reorder beats**: Drag a beat to a new place and the narration, voiceover and render follow the new order. ([prompt](ROADMAP-PROMPTS.md#cc-017-drag-to-reorder-beats))
- [ ] **CC-018 Split and merge beats**: Split a long beat at a word to give it two visuals, or merge two short beats into one. ([prompt](ROADMAP-PROMPTS.md#cc-018-split-and-merge-beats))
- [ ] **CC-019 Trim YouTube cuts by hand**: Nudge where a YouTube moment starts when the automatic cut lands a little early or late. ([prompt](ROADMAP-PROMPTS.md#cc-019-trim-youtube-cuts-by-hand))
- [ ] **CC-020 Crop and pan stills**: Drag a box on an image or screenshot to choose what the camera zooms toward. ([prompt](ROADMAP-PROMPTS.md#cc-020-crop-and-pan-stills))
- [ ] **CC-021 Undo and redo edits**: Step back through script, hook and beat edits on a project, even after a reload. ([prompt](ROADMAP-PROMPTS.md#cc-021-undo-and-redo-edits))
- [ ] **CC-022 Keyboard shortcuts**: Play, step through beats, re-run a stage and continue the pipeline from the keyboard, with a `?` overlay that lists the keys. ([prompt](ROADMAP-PROMPTS.md#cc-022-keyboard-shortcuts))
- [ ] **CC-023 Fast preview render**: A low-resolution preview renders in a fraction of the time, so you can check the edit before the full render. ([prompt](ROADMAP-PROMPTS.md#cc-023-fast-preview-render))
- [ ] **CC-024 GPU video encoding**: Encode with NVENC, VAAPI, Quick Sync or VideoToolbox when ffmpeg has them, falling back to x264. ([prompt](ROADMAP-PROMPTS.md#cc-024-gpu-video-encoding))
- [ ] **CC-025 Render only new formats**: Adding an extra format renders just that one and keeps the videos that are already up to date. ([prompt](ROADMAP-PROMPTS.md#cc-025-render-only-new-formats))
- [ ] **CC-026 Installable phone app**: Add ClipCut to your phone's home screen and it opens full screen with its own icon. ([prompt](ROADMAP-PROMPTS.md#cc-026-installable-phone-app))
- [ ] **CC-027 Accessibility pass**: Every page works with a keyboard and a screen reader, with visible focus, labelled controls and announced progress. ([prompt](ROADMAP-PROMPTS.md#cc-027-accessibility-pass))
- [ ] **CC-028 Light and high-contrast themes**: The app follows your system's light or dark setting, offers a high-contrast mode, and lets you pick its accent color. ([prompt](ROADMAP-PROMPTS.md#cc-028-light-and-high-contrast-themes))
- [ ] **CC-029 Saved video presets**: Save a format, length, voice, caption style and notes as a named preset and pick it for new videos and batches. ([prompt](ROADMAP-PROMPTS.md#cc-029-saved-video-presets))
- [ ] **CC-030 Search and filter projects**: Find a project on the home page by topic or title and filter by status, format or kind. ([prompt](ROADMAP-PROMPTS.md#cc-030-search-and-filter-projects))

## v0.4: Editing styles and themes
Status: Later
Give every video a deliberate look and pace, and let you save that look as a theme you can share.

- [ ] **CC-031 Cut pacing presets**: Pick Documentary, Hype, Explainer or Storytime and the cut length, punch-ins, sound effects and overlays change to match. ([prompt](ROADMAP-PROMPTS.md#cc-031-cut-pacing-presets))
- [ ] **CC-032 Transition packs**: Choose a set of transitions between beats, such as clean cuts, whips, glitches or film burns, instead of one fixed mix. ([prompt](ROADMAP-PROMPTS.md#cc-032-transition-packs))
- [ ] **CC-033 Zoom and Ken Burns variants**: Choose how footage and stills move, from still to slow drift to hard punch-ins, per video or per beat. ([prompt](ROADMAP-PROMPTS.md#cc-033-zoom-and-ken-burns-variants))
- [ ] **CC-034 Cut on the music beat**: Visual cuts snap to the beats of the music track, while the voice and captions stay where they are. ([prompt](ROADMAP-PROMPTS.md#cc-034-cut-on-the-music-beat))
- [ ] **CC-035 Color grades and LUTs**: Apply a built-in grade or your own `.cube` LUT to the whole video so mixed footage looks like one piece. ([prompt](ROADMAP-PROMPTS.md#cc-035-color-grades-and-luts))
- [ ] **CC-036 Motion text templates**: Pick how on-screen text animates, such as stamp, slide, typewriter or highlight sweep, from a set of templates. ([prompt](ROADMAP-PROMPTS.md#cc-036-motion-text-templates))
- [ ] **CC-037 B-roll density control**: A slider sets how busy the edit is, from calm with few overlays to dense with sub-cuts, icons and text on most beats. ([prompt](ROADMAP-PROMPTS.md#cc-037-b-roll-density-control))
- [ ] **CC-038 Split-screen and picture-in-picture**: Show two visuals at once, side by side or one inset in a corner, for comparisons and reactions. ([prompt](ROADMAP-PROMPTS.md#cc-038-split-screen-and-picture-in-picture))
- [ ] **CC-039 Reaction overlay styles**: Drop reaction GIFs and stickers over footage as overlays, in a corner or as a quick pop, instead of as full beats. ([prompt](ROADMAP-PROMPTS.md#cc-039-reaction-overlay-styles))
- [ ] **CC-040 Style picker with previews**: Choose a pacing, transition, motion and grade combination from cards that play a short sample of each look. ([prompt](ROADMAP-PROMPTS.md#cc-040-style-picker-with-previews))
- [ ] **CC-041 Brand kit**: Set your colors and fonts once, and every caption, card, icon and thumbnail uses them instead of a random palette. ([prompt](ROADMAP-PROMPTS.md#cc-041-brand-kit))
- [ ] **CC-042 Logo watermark**: Place your logo in a corner of every video at the size and opacity you choose, clear of the platform buttons. ([prompt](ROADMAP-PROMPTS.md#cc-042-logo-watermark))
- [ ] **CC-043 Intro and outro cards**: Add a short branded opener and a closing card with a call to action to every video. ([prompt](ROADMAP-PROMPTS.md#cc-043-intro-and-outro-cards))
- [ ] **CC-044 Lower thirds**: Name a speaker, source or place with an animated lower third that matches your brand. ([prompt](ROADMAP-PROMPTS.md#cc-044-lower-thirds))
- [ ] **CC-045 Caption theme editor**: Make your own caption style with its font, colors, outline, box, size, position, words per line and word animation. ([prompt](ROADMAP-PROMPTS.md#cc-045-caption-theme-editor))
- [ ] **CC-046 Shareable theme files**: Export your brand, caption theme and editing style as one file that someone else can import. ([prompt](ROADMAP-PROMPTS.md#cc-046-shareable-theme-files))
- [ ] **CC-047 Thumbnail templates**: Pick a thumbnail layout and get a few variants per video to choose from before you upload. ([prompt](ROADMAP-PROMPTS.md#cc-047-thumbnail-templates))

## v0.5: Models, voices and sources
Status: Later
Write with a local or other model, speak and caption more languages, and find visuals in more free libraries.

- [ ] **CC-048 Ollama and local models**: Write the research, hooks, script and beat sheet with a model running in Ollama, so no account is needed at all. ([prompt](ROADMAP-PROMPTS.md#cc-048-ollama-and-local-models))
- [ ] **CC-049 OpenAI-compatible model servers**: Point ClipCut at LM Studio, llama.cpp, vLLM or any OpenAI-compatible endpoint you choose. ([prompt](ROADMAP-PROMPTS.md#cc-049-openai-compatible-model-servers))
- [ ] **CC-050 Choose a model per stage**: Use one model for the script and a cheaper or local one for the beat sheet and quick picks. ([prompt](ROADMAP-PROMPTS.md#cc-050-choose-a-model-per-stage))
- [ ] **CC-051 Videos in other languages**: Pick a language for a project and the research, script, captions and title are written in it. ([prompt](ROADMAP-PROMPTS.md#cc-051-videos-in-other-languages))
- [ ] **CC-052 Kokoro voices beyond English**: Use Kokoro's Spanish, French, Italian, Portuguese and Hindi voices with the right pronunciation. ([prompt](ROADMAP-PROMPTS.md#cc-052-kokoro-voices-beyond-english))
- [ ] **CC-053 Piper as a built-in voice**: Download and use Piper voices from the app for languages Kokoro does not cover, without a shell command. ([prompt](ROADMAP-PROMPTS.md#cc-053-piper-as-a-built-in-voice))
- [ ] **CC-054 Optional ElevenLabs voices**: Use ElevenLabs voices with your own key if you want them, while the local voice stays the default. ([prompt](ROADMAP-PROMPTS.md#cc-054-optional-elevenlabs-voices))
- [ ] **CC-055 Translate a finished video**: Make a copy of a video in another language that keeps its beats and visuals and only redoes the words and voice. ([prompt](ROADMAP-PROMPTS.md#cc-055-translate-a-finished-video))
- [ ] **CC-056 Pronunciation dictionary**: Tell the voice how to say a name once, and the captions keep the real spelling. ([prompt](ROADMAP-PROMPTS.md#cc-056-pronunciation-dictionary))
- [ ] **CC-057 Blend voices in the app**: Mix two Kokoro voices with a slider and preview the result before you use it. ([prompt](ROADMAP-PROMPTS.md#cc-057-blend-voices-in-the-app))
- [ ] **CC-058 Clip videos in any language**: Clip mode works with any caption language and keeps the clip's captions in that language. ([prompt](ROADMAP-PROMPTS.md#cc-058-clip-videos-in-any-language))
- [ ] **CC-059 Local footage library**: Drop your own clips and photos into a folder and ClipCut searches them first for matching beats. ([prompt](ROADMAP-PROMPTS.md#cc-059-local-footage-library))
- [ ] **CC-060 Internet Archive footage**: Search public-domain and openly licensed films on the Internet Archive for video beats. ([prompt](ROADMAP-PROMPTS.md#cc-060-internet-archive-footage))
- [ ] **CC-061 NASA image and video library**: Space and science beats can use NASA's public-domain photos and footage. ([prompt](ROADMAP-PROMPTS.md#cc-061-nasa-image-and-video-library))
- [ ] **CC-062 Wikipedia images and facts**: Beats about a person, place or thing can use its Wikipedia lead image, and research can cite its summary. ([prompt](ROADMAP-PROMPTS.md#cc-062-wikipedia-images-and-facts))
- [ ] **CC-063 Social posts as cards**: A beat can show a real public post from Bluesky, Mastodon or X as a clean, credited card. ([prompt](ROADMAP-PROMPTS.md#cc-063-social-posts-as-cards))
- [ ] **CC-064 Reddit threads as cards**: A beat can show a Reddit post or top comment as a card, with your own free Reddit app key. ([prompt](ROADMAP-PROMPTS.md#cc-064-reddit-threads-as-cards))
- [ ] **CC-065 Chart beats from numbers**: When the script quotes figures from the fact sheet, a beat can show them as an animated bar or line chart. ([prompt](ROADMAP-PROMPTS.md#cc-065-chart-beats-from-numbers))

## v0.6: Integrations and automation
Status: Later
Post to more platforms, hand your edit to other tools, and let automations and AI agents drive ClipCut.

- [ ] **CC-066 Instagram and Facebook Reels**: Post a vertical render as a Reel to an Instagram professional account or a Facebook Page, from the app or the queue. ([prompt](ROADMAP-PROMPTS.md#cc-066-instagram-and-facebook-reels))
- [ ] **CC-067 TikTok draft uploads**: Send a render to your TikTok inbox as a draft, ready to post from the TikTok app. ([prompt](ROADMAP-PROMPTS.md#cc-067-tiktok-draft-uploads))
- [ ] **CC-068 Bluesky video posts**: Post a render to Bluesky with its title and hashtags, using an app password. ([prompt](ROADMAP-PROMPTS.md#cc-068-bluesky-video-posts))
- [ ] **CC-069 Threads video posts**: Post a render to Threads with its caption, through your own Meta app. ([prompt](ROADMAP-PROMPTS.md#cc-069-threads-video-posts))
- [ ] **CC-070 LinkedIn video posts**: Post a render to your LinkedIn profile with its title and description. ([prompt](ROADMAP-PROMPTS.md#cc-070-linkedin-video-posts))
- [ ] **CC-071 X video posts**: Post a render to X with its title and hashtags, using your own developer app where its free access allows it. ([prompt](ROADMAP-PROMPTS.md#cc-071-x-video-posts))
- [ ] **CC-072 YouTube stats per video**: See views, likes and comments for every published video on its project page, a Stats page and `clipcut show`. ([prompt](ROADMAP-PROMPTS.md#cc-072-youtube-stats-per-video))
- [ ] **CC-073 Chat notifications**: Get a Discord, Slack or Telegram message when a video renders, publishes or fails. ([prompt](ROADMAP-PROMPTS.md#cc-073-chat-notifications))
- [ ] **CC-074 Webhooks and after-render commands**: Send a signed webhook or run your own command when a video renders or publishes. ([prompt](ROADMAP-PROMPTS.md#cc-074-webhooks-and-after-render-commands))
- [ ] **CC-075 Trigger videos from automations**: Start videos and batches from n8n, Zapier, Make or a cron job with a scoped API token and ready-made recipes. ([prompt](ROADMAP-PROMPTS.md#cc-075-trigger-videos-from-automations))
- [ ] **CC-076 MCP server for AI agents**: `clipcut mcp` lets Claude Code and other agents make, check and publish videos through ClipCut's tools. ([prompt](ROADMAP-PROMPTS.md#cc-076-mcp-server-for-ai-agents))
- [ ] **CC-077 Topic feeds from RSS and Sheets**: Fill the queue from RSS feeds, YouTube channel feeds or a Google Sheet on a schedule. ([prompt](ROADMAP-PROMPTS.md#cc-077-topic-feeds-from-rss-and-sheets))
- [ ] **CC-078 Auto-clip new channel uploads**: Watch YouTube channels you choose and turn each new long upload into clips. ([prompt](ROADMAP-PROMPTS.md#cc-078-auto-clip-new-channel-uploads))
- [ ] **CC-079 Export to editing software**: Export the edit as OpenTimelineIO, EDL or XML for DaVinci Resolve, Premiere and Final Cut, with the media alongside. ([prompt](ROADMAP-PROMPTS.md#cc-079-export-to-editing-software))
- [ ] **CC-080 Project export and import**: Pack a project with its assets into one file and open it on another machine. ([prompt](ROADMAP-PROMPTS.md#cc-080-project-export-and-import))
- [ ] **CC-081 Copy renders to cloud storage**: Copy each finished video to S3-compatible storage, Google Drive or Dropbox through rclone. ([prompt](ROADMAP-PROMPTS.md#cc-081-copy-renders-to-cloud-storage))

## v1.0: Every desktop, stable API
Status: Later
Run natively on every desktop OS and package manager, and promise an API that does not break.

- [ ] **CC-082 Native Windows build**: Download and run `clipcut.exe` on Windows without WSL. ([prompt](ROADMAP-PROMPTS.md#cc-082-native-windows-build))
- [ ] **CC-083 Intel Mac build**: Release builds for Intel Macs, with the voice running on a separately loaded ONNX Runtime. ([prompt](ROADMAP-PROMPTS.md#cc-083-intel-mac-build))
- [ ] **CC-084 AUR package**: Arch users install and update ClipCut with their AUR helper. ([prompt](ROADMAP-PROMPTS.md#cc-084-aur-package))
- [ ] **CC-085 Nix flake**: `nix run github:adeebahmad01/clipcut` builds and runs ClipCut with every tool it needs. ([prompt](ROADMAP-PROMPTS.md#cc-085-nix-flake))
- [ ] **CC-086 Versioned HTTP API**: A documented `/api/v1` that scripts and other tools can rely on not to break between releases. ([prompt](ROADMAP-PROMPTS.md#cc-086-versioned-http-api))

## Ideas
Status: Exploring
Bigger ideas that need a proof of concept before they get a milestone.

- [ ] **CC-087 Voice cloning from a sample**: Record a short sample of your own voice and narrate videos with it, all on your machine. ([prompt](ROADMAP-PROMPTS.md#cc-087-voice-cloning-from-a-sample))
- [ ] **CC-088 Review links for collaborators**: Share a read-only link to a project so someone can watch it and leave notes per beat. ([prompt](ROADMAP-PROMPTS.md#cc-088-review-links-for-collaborators))
- [ ] **CC-089 Codex and Gemini with tools disabled**: Let the Codex and Gemini CLIs write scripts once they can run with every tool turned off, keeping ClipCut's isolation rules. ([prompt](ROADMAP-PROMPTS.md#cc-089-codex-and-gemini-with-tools-disabled))
- [ ] **CC-090 Chapters for long videos**: Long 16:9 videos get chapter cards and YouTube chapter timestamps in the description. ([prompt](ROADMAP-PROMPTS.md#cc-090-chapters-for-long-videos))
