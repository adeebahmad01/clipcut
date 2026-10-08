# Using ClipCut

- [Start the app](#start-the-app)
- [Make a video](#make-a-video)
- [The stages](#the-stages)
- [Reviewing and editing](#reviewing-and-editing)
- [The command line](#the-command-line)
- [Projects on disk](#projects-on-disk)
- [Sound effects and music](#sound-effects-and-music)
- [Voices](#voices)

## Start the app

```bash
clipcut
```

ClipCut prints a Local and a Network URL. The Network URL lets you use the
app from a phone or another computer on the same network. If Chromium or
Chrome is installed, ClipCut also opens the app in its own window.

| Option | Effect |
|---|---|
| `--no-window` | Don't open the app window |
| `--port 9000` | Listen on another port (default 7878) |
| `--host 127.0.0.1` | Only accept connections from this machine |
| `--home PATH` | Keep data somewhere else (also `$CLIPCUT_HOME`) |

The first start downloads yt-dlp and the voice model in the background.
The setup banner at the top of the page shows progress and anything that
still needs attention. **Settings → Setup** has the same checks as
`clipcut doctor`, with a button for each install.

## Make a video

1. Type a topic, for example *Why coffee is technically a fruit*.
2. Optionally add **Notes**: an angle, facts to include, a tone.
3. Pick **9:16** (1080×1920, for Shorts, Reels and TikTok) or **16:9**
   (1920×1080), a length, and a voice. **Preview** plays the voice.
4. Leave **Autopilot** off to review each stage, or turn it on to go
   straight to a finished video.
5. Press **Make**.

When the render finishes, the project page shows the video, a title,
description and hashtags with copy buttons, **Download MP4**, and
**Credits**, a list of every clip and image used and where it came from.

## The stages

| Stage | What happens |
|---|---|
| Research | Searches the web (SearXNG or DuckDuckGo) and has Claude write a short fact sheet with sources |
| Hook | Writes several opening lines and asks you to pick one |
| Script | Writes the narration, then runs the unslop pass if it is installed |
| Beat sheet | Splits the script into 1–3 second beats and decides what each beat shows: a YouTube moment, an article screenshot, a meme, a GIF, an icon or stock footage |
| Visuals | Finds and downloads everything: searches YouTube and cuts the exact moment, screenshots and highlights articles, captions meme templates |
| Voiceover | Speaks the script with Kokoro and times each word for the captions |
| Render | Composites every beat with its animation, captions, sound effect and music, then encodes with x264 |

## Reviewing and editing

With Autopilot off, ClipCut stops after Hook, Script and Beat sheet. You can:

- pick a different hook or write your own,
- edit the script text,
- change what a beat shows or swap its search terms,
- re-run any stage with the ↻ button next to it. Later stages are redone;
  earlier ones are kept.

**Duplicate** copies a project so you can try another take without losing
the first one. **Re-render** only redoes the final render, which is useful
after changing audio or caption settings.

## The command line

Everything in the app is also available without the UI:

```bash
clipcut new "How Australia lost the Great Emu War" --seconds 40
clipcut new "Quantum computers in 60 seconds" --horizontal --voice bf_emma --notes "no jargon"
```

`new` runs every stage with no pauses and prints the path of `final.mp4`.

```bash
clipcut run <project-id> --from script        # redo script and everything after it
clipcut run <project-id> --from assets --only # only redo visuals
clipcut render <project-id>                   # re-render
clipcut doctor                                # check every dependency
clipcut setup all                             # or: yt-dlp, kokoro, searxng
```

Stage names for `--from` are `research`, `hooks`, `script`, `beats`,
`assets`, `voice` and `render`. Project IDs are the folder names under
`projects/`, and are shown in the app's URL (`#/p/<id>`).

Set `RUST_LOG=debug` for detailed logs.

## Projects on disk

Each project is a folder in `<home>/projects/<id>/` (`<home>` is
`~/.local/share/clipcut` on Linux, `~/Library/Application Support/clipcut`
on macOS):

```
project.json        topic, settings, research, hooks, script, beat sheet and stage state
assets/             downloaded clips, screenshots, memes and GIFs, each with a .source.json
voice/              the voiceover and its word timings
render/final.mp4    the video
render/credits.txt  every source used
```

You can delete a project from the app, or just remove its folder.

## Sound effects and music

Every cut gets a sound effect: whoosh, swoosh, pop, click, ding, riser,
hit, typewriter, glitch, camera or record scratch. ClipCut synthesizes them
itself, so nothing needs downloading.

To use your own, drop WAV or MP3 files into `<home>/sfx/`, named after the
effect: `whoosh.wav`, `whoosh_2.wav`, `record_scratch.mp3` and so on. When
there are several files for one effect, ClipCut rotates through them.

For background music, put tracks (mp3, wav, ogg, m4a, flac or opus) into
`<home>/music/`. Each project picks one and keeps it on re-renders. The
music is ducked under the voice. Use tracks you have the rights to; the
YouTube Audio Library and Pixabay Music have free ones.

Volumes are in **Settings → Audio** or the `[audio]` section of
[config.toml](Configuration.md#audio).

## Voices

ClipCut uses [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M), which
runs on your CPU. The app lists the main voices; any Kokoro voice ID works
in `tts.voice` or `--voice`, such as `af_heart`, `af_bella`, `am_adam`,
`bm_george` or `bf_emma`. Names starting with `a` are American English,
`b` British English.

If Kokoro is not available, eSpeak gives a robotic but working voice. To
use another TTS program, see [tts.command](Configuration.md#tts).
