# Troubleshooting

Start with:

```bash
clipcut doctor
```

It checks every tool, the voice model, search and your API keys, and says
how to fix each failure. **Settings → Setup** in the app shows the same
list. For more detail on any problem, run with `RUST_LOG=debug clipcut`.

## Install

**`clipcut: command not found` after installing.**
`~/.local/bin` is not on your `PATH`. Add
`export PATH="$HOME/.local/bin:$PATH"` to `~/.bashrc` or `~/.zshrc` and
open a new terminal.

**macOS: "clipcut cannot be opened because the developer cannot be verified".**
The release is not notarized. Run
`xattr -d com.apple.quarantine ~/.local/bin/clipcut` once. The installer
does this for you.

**Linux: `GLIBC_2.38 not found` (or 2.39).**
The release binaries need glibc 2.39 or newer (Ubuntu 24.04+, Debian 13+,
Fedora 40+). The ONNX Runtime used for the voice needs it, so building
from source does not help. On an older system, run ClipCut inside an
Ubuntu 24.04 container, for example with
[Distrobox](https://distrobox.it/): `distrobox create -i ubuntu:24.04`.

## Claude Code

**doctor says Claude Code is missing.**
Install it with `curl -fsSL https://claude.ai/install.sh | bash`, then run
`claude` once to sign in. If it lives somewhere unusual, set
`tools.claude` in [config.toml](Configuration.md#tools).

**doctor says Claude Code lacks the required isolation flags.**
Your Claude Code is too old. Run `claude update`.

**Stages fail with "not logged in" or an auth error.**
Run `claude` in a terminal and sign in again.

**A stage times out.**
Raise `llm.timeout_seconds` (up to 1800), or use a faster model in
`llm.model`.

## YouTube

**Visuals fall back to stock footage or images for most beats.**
Check the yt-dlp and JS runtime lines in `clipcut doctor`. yt-dlp needs
Node.js or Deno to pass YouTube's player checks; install one of them.

**"Sign in to confirm you're not a bot".**
YouTube has flagged your IP. Sign in to YouTube in a browser on this
machine and set `youtube.cookies_from_browser = "firefox"` (or `chrome`,
`brave`, `edge`) so yt-dlp can use that session.

**Downloads that used to work start failing.**
YouTube changes often. Update yt-dlp with `clipcut setup yt-dlp`.

## Voice

**"kokoro: model file not found".**
The model download did not finish. Run `clipcut setup kokoro`. Downloads
resume where they stopped.

**doctor says espeak-ng is missing.**
Kokoro uses espeak-ng to turn words into sounds. Install it with
`sudo apt install espeak-ng`, `sudo dnf install espeak-ng` or
`brew install espeak-ng`.

**A word is pronounced wrong.**
Spell it the way it sounds in the script, for example "Ny-AR-luh-thoh-tep".
The captions use the same text, so keep the change small.

## Search

**doctor says SearXNG is not reachable.**
ClipCut uses DuckDuckGo when SearXNG is down, so this is optional. To run
SearXNG, install Podman or Docker and run `clipcut setup searxng`. On
macOS, start the Podman machine first: `podman machine init && podman
machine start`.

**Research finds little or nothing.**
DuckDuckGo limits automated searches. Running SearXNG gives better and
steadier results.

## Rendering

**"Unknown encoder 'libx264'".**
Your ffmpeg was built without x264. On Fedora, enable RPM Fusion and run
`sudo dnf swap ffmpeg-free ffmpeg --allowerasing`. Elsewhere, install
ffmpeg from your distribution's main repository or from
<https://ffmpeg.org/download.html>.

**Article beats have no screenshot.**
Install Chromium, Chrome, Edge or Brave. If it is not found, set
`tools.chromium` to its path.

**The render is slow.**
Rendering runs on the CPU. Lower `video.fps` to 24 or raise `video.crf` to
23 for faster encodes. Each extra format under **Also export** is a full
render of its own.

## Clips

**"this video has no English captions, so its moments can't be found".**
Clip mode picks moments from the captions, so it needs English captions
(auto-generated ones are fine). Try another video.

## Publishing

See [Publish to YouTube](Usage.md#publish-to-youtube) for the setup.

**"YouTube made this upload private".**
YouTube locks uploads from Google Cloud projects that haven't passed its
API compliance audit to private. The video is on your channel; make it
public in YouTube Studio, or request the free
[audit](https://support.google.com/youtube/contact/yt_api_form) so ClipCut
can publish publicly and schedule.

**"YouTube access was revoked or has expired".**
While the OAuth consent screen is in *Testing*, Google ends sign-ins after 7
days. Press **Reconnect** in **Settings → YouTube** (or run
`clipcut publish --connect`), or publish the consent screen to production
to keep the sign-in.

**"this is a Web application client; create a Desktop app client instead".**
In Google Cloud, create another OAuth client ID of type **Desktop app** and
use its JSON.

**The sign-in page fails to load after allowing access.**
Google sends the browser to `http://127.0.0.1:…`, which only works on the
computer running ClipCut. Copy the full address from the address bar and
paste it into **Settings → YouTube** (or the terminal running
`clipcut publish --connect`).

**"Google sent no refresh token".**
Remove ClipCut at
[myaccount.google.com/permissions](https://myaccount.google.com/permissions)
and connect again.

**"YouTube's daily API quota is used up".**
The free quota allows about 6 uploads a day and resets at midnight Pacific
time. Failed queue items can be retried the next day (**Retry** on the
Queue page, or `clipcut queue retry <n>`); the video is not rendered again.

**"the YouTube Data API v3 isn't enabled in your Google Cloud project".**
Enable it in the
[API Library](https://console.cloud.google.com/apis/library/youtube.googleapis.com)
for the project your OAuth client belongs to.

**"Uploaded, but the thumbnail wasn't set".**
Custom thumbnails need a [verified channel](https://www.youtube.com/verify).
Shorts never get one; YouTube picks a frame.

## The app

**The page does not load from my phone.**
Check that `server.host` is `0.0.0.0`, that both devices are on the same
network, and that your firewall allows the port (default 7878). On Fedora:
`sudo firewall-cmd --add-port=7878/tcp`.

**"Address already in use".**
Another program (or another ClipCut) uses the port. Start with
`clipcut --port 7879`.

**"could not load …/config.toml".**
A setting in [config.toml](Configuration.md) is out of range or has a typo,
usually after a hand edit; the line below names it. Fix it with
`clipcut config set <setting> <value>`, or put it back to its default
with `clipcut config unset <setting>`. `clipcut config` shows the whole file.

## Still stuck?

[Open an issue](https://github.com/adeebahmad01/clipcut/issues/new/choose)
with the output of `clipcut doctor`, your OS, and the log lines around the
error.
