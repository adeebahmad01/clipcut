# Using ClipCut

- [Start the app](#start-the-app)
- [Make a video](#make-a-video)
- [The stages](#the-stages)
- [Reviewing and editing](#reviewing-and-editing)
- [Captions, formats and thumbnails](#captions-formats-and-thumbnails)
- [Clip a long video](#clip-a-long-video)
- [The queue](#the-queue)
- [Publish to YouTube](#publish-to-youtube)
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
3. Pick **9:16** (1080×1920, for Shorts, Reels and TikTok), **16:9**
   (1920×1080) or **1:1** (1080×1080, for feeds), a length, a voice and a
   caption style. **Preview** plays the voice. Tick **Also export** to get
   the other shapes from the same edit.
4. Leave **Autopilot** off to review each stage, or turn it on to go
   straight to a finished video.
5. Press **Make**.

When the render finishes, the project page shows the video and a
**Ready to post** kit:

- the title, description and hashtags, each with a copy button, and
  **Copy all**,
- **Download MP4**, plus one button per extra format,
- **SRT** and **VTT** caption files,
- **Thumbnail**, a 1280×720 JPEG,
- **Credits**, a list of every clip and image used and where it came from,
- **Publish to YouTube**, once you [connect a channel](#publish-to-youtube).

Not sure what to make? **Need an idea?** under the topic box lists what is
trending on Google Trends or in the news. Click one to use it as the topic.

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

**Duplicate** (or `clipcut duplicate <id>`) copies a project so you can
try another take without losing the first one. **Re-render** only redoes the final render, which is useful
after changing audio or caption settings.

## Captions, formats and thumbnails

### Caption styles

| Style | Look |
|---|---|
| Pop (default) | Heavy uppercase, three words at a time; the spoken word pops in the accent color |
| Hormozi | Condensed uppercase with a bright box behind the spoken word |
| MrBeast | Two huge words at a time, thick outline, bouncing colors |
| Minimal | Small lowercase text with a soft shadow, no motion |
| Karaoke | A longer line that fills in word by word as it is spoken |

Pick a style when you make a video, or change it later under **Project
details → Captions**. **Settings → Preferences → Caption style** sets the
default for new projects (`video.caption_preset` in
[config.toml](Configuration.md#video)). Changing a project's style resets
the Render stage, so continue the pipeline or press **Re-render** to see it.

### Formats

Each project has a main format and can export the other two from the same
edit. ClipCut lays out every beat, caption and card again for each shape
rather than cropping the finished video, so nothing important falls off the
edge. Set them with **Also export** when you make the video or under
**Project details**. The main video is `render/final.mp4`; the others are
`render/final-horizontal.mp4`, `render/final-square.mp4` and so on. If an
extra format fails to render, the main video is kept.

### Caption files and thumbnail

Every render also writes:

- `render/captions.srt` and `render/captions.vtt`, timed to the voiceover,
  for YouTube, editors such as Premiere and Resolve, or the web,
- `render/thumb.jpg`, a 1280×720 thumbnail: the best photo or video frame
  from the beats, cropped around its subject, with the hook in large type.
  Without a usable image it uses a gradient background.

The thumbnail is made fresh on every render.

## Clip a long video

Switch the home page to **Clip a long video**, paste a YouTube link and
press **Find clips**. ClipCut reads the video's English captions, asks
Claude for the strongest self-contained moments (no intros, sponsor reads
or outros), and turns each one into its own project. If Claude can't pick,
ClipCut falls back to the stretches where the most is said.

| Option | Default | Range |
|---|---|---|
| How many clips | 3 | 1–5 |
| Length | 30–60 s | 15–30, 30–60 or 45–90 s in the app; 5–180 s from the command line |

Each clip keeps the original audio and its speaker. A wide video in a tall
frame sits in a band over a blurred copy of itself, with the hook above and
captions below; cuts punch in on new sentences with a sound effect. Clips
get no music bed. The clips go into the [queue](#the-queue) and render one
after another.

A clip's script comes from the source video, so you can't edit its topic,
hook, script or beats. You can still change its format, caption style and
extra exports. The video needs English captions, auto-generated ones
included.

## The queue

The **Queue** page makes many videos one after another, each on
Autopilot. Paste topics into the box, one per line (blank lines and lines
starting with `#` are skipped, up to 200 at a time), pick the format,
length, voice, caption style and extra exports for the batch, and press
**Queue N videos**. **Need an idea?** works here too and adds the topic as a
new line.

- Videos are made one at a time, in order. A failure is recorded on that
  item and the queue moves on.
- **Pause** stops new videos from starting; the current one finishes.
- **Stop** (✕ on a running item) cancels it. **Retry** on a failed or
  cancelled item puts it back in line and picks up from the stage that
  stopped, so finished work is not redone.
- **Clear finished** removes done, failed and cancelled items.
- The queue is saved in `<home>/queue.json`. If ClipCut is closed while a
  video is being made, that video starts again where it left off on the
  next start.

With YouTube connected, the queue can [publish](#publish-to-youtube) each
video when it renders. Set a privacy, an optional first release time, and
**then every N h** to space the rest out. Without a first release time the
first video goes out as soon as it renders and the next ones are spaced from
the moment you queued them.

## Publish to YouTube

ClipCut uploads with your own free Google Cloud OAuth client, so no
ClipCut account or paid key is involved and the secret stays on your
computer.

### Connect a channel (once, about 10 minutes)

1. [Create a Google Cloud project](https://console.cloud.google.com/projectcreate).
   Any name works and no billing account is needed.
2. [Enable the YouTube Data API v3](https://console.cloud.google.com/apis/library/youtube.googleapis.com)
   for it.
3. Set up the [OAuth consent screen](https://console.cloud.google.com/apis/credentials/consent):
   choose **External**, then add your own Google account under **Test
   users**.
4. Under [Credentials](https://console.cloud.google.com/apis/credentials),
   pick **Create credentials → OAuth client ID → Desktop app** and download
   the JSON. A *Web application* client does not work.
5. In ClipCut, open **Settings → YouTube**, paste the JSON or choose the
   file, save it, and press **Connect YouTube**. Sign in with the Google
   account that owns your channel and allow every permission.

From the command line:

```bash
clipcut publish --client client_secret_1234.json --connect
```

Google sends the browser back to an `http://127.0.0.1:…` address, which
only loads on the computer running ClipCut. If you are using ClipCut from
another device, that page fails to load: copy its full address from the
address bar and paste it into **Signing in from another device?** in
Settings (or into the terminal for `--connect`).

The client and the sign-in are kept in `<home>/youtube-oauth.json`, readable
only by you. **Disconnect** signs out and revokes ClipCut's access;
**Forget client** also deletes the saved client.

### Publish a video

Once connected, the **Ready to post** kit has a **Publish to YouTube**
section. It uses the kit's title, description and hashtags, and adds a
credits list of every source to the description.

- **Privacy:** Public, Unlisted or Private. It starts at the default from
  **Settings → Preferences → YouTube uploads** (Private unless you change
  it), which also sets the category and the "made for kids" flag for every
  upload. See [`[publish]`](Configuration.md#publish).
- **Release time:** leave it empty to publish now, or pick a time to
  schedule. Scheduled videos upload as private and go public at that time.
  YouTube can't schedule unlisted videos, so an unlisted video with a
  release time goes public at that time; private videos ignore it.
- **Format:** which render to upload, when the project has more than one.

Vertical and square videos up to 3 minutes become Shorts. Longer videos and
16:9 videos also get the thumbnail. Each upload is listed under the kit
with its link. Publishing the same render again uploads a second copy.

### What Google allows

- **Uploads stay private until Google audits your project.** YouTube locks
  videos uploaded through an unaudited API project to private. Request the
  free [API compliance audit](https://support.google.com/youtube/contact/yt_api_form)
  to publish publicly or on a schedule.
- **Test sign-ins last 7 days.** While the consent screen is in *Testing*,
  reconnect once a week, or publish the consent screen to production and
  click through the "unverified app" warning.
- **About 6 uploads a day.** The free quota is 10,000 units a day and each
  upload costs about 1,600. It resets at midnight Pacific time.
- **Custom thumbnails need a [verified channel](https://www.youtube.com/verify).**

TikTok isn't supported: its posting API only lets unaudited apps post
privately. Download the 9:16 MP4 and upload it in the TikTok app.

## The command line

Everything in the app is also available without the UI:

```bash
clipcut new "How Australia lost the Great Emu War" --seconds 40
clipcut new "Quantum computers in 60 seconds" --horizontal --voice bf_emma --notes "no jargon"
clipcut new "The loudest sound ever recorded" --captions hormozi --also horizontal,square
```

`new` runs every stage with no pauses and prints the path of `final.mp4`.
`new`, `render`, `batch` and `clip` share these options:

| Option | Effect |
|---|---|
| `--format vertical\|horizontal\|square` | Main format (`--horizontal` is a shortcut) |
| `--captions pop\|hormozi\|mrbeast\|minimal\|karaoke` | Caption style |
| `--also horizontal,square` | Also export these formats from the same edit |

On `render` they change the project before rendering it again from the
same beats, visuals and voiceover, as changing them on the project page
does. `--also` replaces the extra formats there, and `--no-also` stops
exporting them:

```bash
clipcut render 3fa9c1 --format square --captions minimal
clipcut render latest --also horizontal       # add a 16:9 copy
clipcut render latest --no-also               # only the main format again
```

With the app running, the change goes through it, so it is refused while
the app is working on that project.

Make several videos, cut clips from a long video, and find topics:

```bash
clipcut batch topics.txt --seconds 45          # one video per line, one after another
clipcut clip "https://youtu.be/…" --count 3 --min 30 --max 60
clipcut ideas                                  # what's trending on Google Trends (--geo GB for another country)
clipcut ideas --news --q "space"               # news headlines, optionally about something
```

If the app is running, `batch` and `clip` add their videos to its queue and
return right away; follow them on the Queue page or with `clipcut queue`.
Otherwise they make the videos in the terminal and print a `[done]` or
`[failed]` line for each.

`clipcut queue` shows the queue and does what the Queue page's buttons do:

```bash
clipcut queue                 # every item: status, project, publish plan, topic
clipcut queue retry 2         # put failed or cancelled item 2 back in line
clipcut queue remove 3        # take item 3 out; one being made is stopped
clipcut queue pause           # start no new videos (resume starts them again)
clipcut queue clear           # remove done, failed and cancelled items
clipcut queue --json          # the whole queue, for scripts
```

```
#  STATUS   PROJECT  PUBLISH                     TOPIC
1  done     3fa9c1   public                      Why the Moon is drifting away
   https://youtu.be/…
2  failed   b07e44   -                           How Australia lost the Great Emu War
   Voiceover failed: espeak-ng is missing
3  waiting  -        unlisted, 2026-10-13 15:00  The loudest sound ever recorded
```

Items are named by their number in that list, or by their ID from
`--json`. PROJECT is the end of the project's ID, which `show`, `run` and
`render` accept. With the app running, `clipcut queue` goes through it;
otherwise it edits `queue.json`, and the app picks up the changes when it
starts.

Publish to YouTube (see [Publish to YouTube](#publish-to-youtube) for the
one-time setup):

```bash
clipcut publish --client client_secret.json --connect   # save the OAuth client and sign in
clipcut publish <project-id>                             # upload now, with the default privacy
clipcut publish <project-id> --privacy unlisted --format square
clipcut publish <project-id> --at "2026-10-12 15:00"     # schedule (local time or RFC 3339)
clipcut publish --disconnect                             # sign out
clipcut batch topics.txt --publish public --start "2026-10-12 15:00" --every 1d
```

`--publish` on `batch` and `clip` uploads each video as it renders, and
`--every` (`90m`, `2h`, `1d`) spaces the release times.

```bash
clipcut list                                  # every project's ID, status, length and topic
clipcut list -n 5                             # only the newest five
clipcut show <project-id>                     # one project: stages, files, uploads
clipcut run <project-id> --from script        # redo script and everything after it
clipcut run <project-id> --from assets --only # only redo visuals
clipcut render <project-id>                   # re-render
clipcut render <project-id> --horizontal      # re-render as 16:9
clipcut delete <project-id>...                # delete projects and their files
clipcut duplicate <project-id>                # copy a project to try another take
clipcut stop <project-id>...                  # stop projects the app is making
clipcut doctor                                # check every dependency
clipcut setup all                             # or: yt-dlp, kokoro, searxng
```

Stage names for `--from` are `research`, `hooks`, `script`, `beats`,
`assets`, `voice` and `render`. `clipcut list` prints the project IDs,
newest first:

```
ID                      STATUS               LENGTH  TOPIC
20261012-091500-3fa9c1  published              0:42  Why the Moon is drifting away
20261011-180210-b07e44  failed: voiceover         -  How Australia lost the Great Emu War
20261011-120033-9d21aa  stopped before hook       -  The loudest sound ever recorded
```

They are also the folder names under `projects/`, and are shown in the
app's URL (`#/p/<id>`).

`run`, `render`, `publish`, `show`, `delete`, `duplicate` and `stop` don't
need the whole ID. Give its last few
characters (`3fa9c1`), its start (`20261012-0915`), or `latest` for the
newest project. Any part of at least four characters works as long as only
one project's ID starts or ends with it; otherwise ClipCut lists the
matches and asks for more:

```bash
clipcut render 3fa9c1
clipcut run latest --from voice
clipcut publish latest --privacy unlisted
```

`clipcut show` prints what a project is, how long each stage took or why it
failed, the full path of every file it made (marking any that are missing)
and its YouTube links:

```
20261012-091500-3fa9c1  published

Topic:   Why the Moon is drifting away
Title:   The Moon Is Leaving, 3.8 cm a Year
Video:   vertical (also square), 40s target, hormozi captions, voice af_heart
Created: 2026-10-12 09:15

Stages
  Research    done in 1m 05s
  Hook        done in 19s
  Script      done in 43s
  Beat sheet  done in 1m 32s
  Visuals     done in 3m 35s
  Voiceover   done in 30s
  Render      done in 2m 10s

Files
  Video      ~/.local/share/clipcut/projects/20261012-091500-3fa9c1/render/final.mp4  (0:42, 1080x1920)
  Square     ~/.local/share/clipcut/projects/20261012-091500-3fa9c1/render/final-square.mp4  (1080x1080)
  Captions   ~/.local/share/clipcut/projects/20261012-091500-3fa9c1/render/captions.srt
             ~/.local/share/clipcut/projects/20261012-091500-3fa9c1/render/captions.vtt
  Thumbnail  ~/.local/share/clipcut/projects/20261012-091500-3fa9c1/render/thumbnail.jpg
  Credits    ~/.local/share/clipcut/projects/20261012-091500-3fa9c1/render/credits.txt
  Folder     ~/.local/share/clipcut/projects/20261012-091500-3fa9c1  (37 MB)

Published
  youtube vertical  public  https://youtu.be/…  uploaded 2026-10-12 10:02
```

For scripts, `--path` prints only the video's path (and fails if the
project has no video yet), and `--json` prints the whole project as saved
in `project.json`:

```bash
mpv "$(clipcut show latest --path)"
clipcut show 3fa9c1 --json | jq -r '.script.title'
```

`clipcut delete` shows the projects it matched and how much space they
use, and asks before deleting them. `--yes` (`-y`) skips the question,
which scripts need. A project with a running stage is left alone: stop it
first with `clipcut stop`, or pass `--force` if ClipCut quit while it was
running. When the app
is running on the same data directory, the CLI asks it to do the deleting,
so it stops anything it is doing with the project first.

```bash
clipcut delete 3fa9c1 b07e44
clipcut delete latest --yes
```

`clipcut duplicate` does what the project page's Duplicate button does: it
makes a new project with the same topic, notes and video settings, keeps
the finished research, hook and script, and leaves the beat sheet,
visuals, voiceover and render to be made again. A clip keeps its moment
and cuts it again. The original isn't touched. It prints the new ID and
how to go on:

```
$ clipcut duplicate 3fa9c1
Copied 20261012-091500-3fa9c1 to 20261021-101500-ab12cd (kept: research, hook, script).
Make it with `clipcut run ab12cd`, or redo the script too with `clipcut run ab12cd --from script`.
```

The copy is the newest project, so `clipcut run latest` works too, and
`--from hooks` writes new hooks and a script to go with them. When the app
is running, the copy is made through it and shows up there straight away.

`clipcut stop` does what the project page's Cancel button does: the app
stops working on the project and marks the stage it was on as cancelled.
Everything finished before that stage is kept, so `clipcut run` picks up
from there:

```
$ clipcut stop 3fa9c1
Stopped 20261012-091500-3fa9c1 during visuals. Continue with `clipcut run 3fa9c1`.
```

A project made by `clipcut new`, `run` or `render` in a terminal stops
with Ctrl-C there, which leaves it the same way (a second Ctrl-C quits
without waiting). If ClipCut quit while a project was running, the project
still says `running` in `clipcut list` although nothing is making it;
`clipcut stop --force` marks it stopped. Starting the app does the same
for every such project. To stop a video the queue is making, use
`clipcut queue remove` instead, which keeps the item as cancelled so it
can be retried.

### Settings

`clipcut config` changes the settings in
[config.toml](Configuration.md) without opening the app or editing the
file. Keys are written `section.setting`, as in the Configuration page:

```bash
clipcut config                                  # the whole file, with defaults filled in
clipcut config get video.format                 # one value: vertical
clipcut config get audio                        # a section, as key = value lines
clipcut config set video.format horizontal
clipcut config set video.captions off           # true/false, yes/no, on/off or 1/0
clipcut config set youtube.channel_blocklist "Channel A, Channel B"
clipcut config set tools.ffmpeg /opt/ffmpeg/bin/ffmpeg
clipcut config set youtube.cookies_from_browser ""   # an empty value clears an optional setting
clipcut config unset video.format               # back to the default (a section works too)
clipcut config --path                           # where config.toml is
clipcut config --json                           # every setting as JSON, for scripts
```

`set` checks the value the same way the app does and names the setting if
it is out of range, so the file is never left half-valid. When the app is
running, the change goes through it and it uses the new value straight
away, except `server.*` settings, which take effect when you restart it.
`clipcut config` also works when a hand edit left config.toml with a value
ClipCut won't start with: it shows the file and points to the bad setting,
and `set` or `unset` fixes it.

### Shell completions

`clipcut completions <shell>` prints a completion script for bash, zsh,
fish, elvish or PowerShell. Tab then completes commands and options, and
the values of `--from`, `--format`, `--also`, `--captions`, `--privacy`,
`--publish` and `setup`. In bash and zsh it also completes setting names
after `clipcut config get`, `set` and `unset`. Save it where your shell looks for completions,
then open a new terminal:

```bash
# bash (needs the bash-completion package)
mkdir -p ~/.local/share/bash-completion/completions
clipcut completions bash > ~/.local/share/bash-completion/completions/clipcut

# zsh: add `fpath=(~/.zfunc $fpath)` before `compinit` in ~/.zshrc
mkdir -p ~/.zfunc
clipcut completions zsh > ~/.zfunc/_clipcut

# fish
mkdir -p ~/.config/fish/completions
clipcut completions fish > ~/.config/fish/completions/clipcut.fish
```

Run the same command again after updating ClipCut so new commands and
options complete too.

Set `RUST_LOG=debug` for detailed logs.

## Projects on disk

Each project is a folder in `<home>/projects/<id>/` (`<home>` is
`~/.local/share/clipcut` on Linux, `~/Library/Application Support/clipcut`
on macOS):

```
project.json        topic, settings, research, hooks, script, beat sheet, stage state and YouTube uploads
assets/             downloaded clips, screenshots, memes and GIFs, each with a .source.json
voice/              the voiceover and its word timings
render/final.mp4    the video
render/final-*.mp4  extra formats (final-horizontal.mp4, final-square.mp4…)
render/captions.srt caption files, also captions.vtt
render/thumb.jpg    the 1280×720 thumbnail (thumb.png is the full-size cover)
render/credits.txt  every source used
```

You can delete a project from the app, with `clipcut delete <id>`, or by
removing its folder.

Next to `projects/`, `queue.json` holds the [queue](#the-queue) and
`youtube-oauth.json` the [YouTube](#publish-to-youtube) client and
sign-in.

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
