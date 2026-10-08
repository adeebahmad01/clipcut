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

**Linux: `GLIBC_2.35 not found`.**
The release binaries need glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+).
On older systems, [build from source](Installation.md#build-from-source).

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
23 for faster encodes.

## The app

**The page does not load from my phone.**
Check that `server.host` is `0.0.0.0`, that both devices are on the same
network, and that your firewall allows the port (default 7878). On Fedora:
`sudo firewall-cmd --add-port=7878/tcp`.

**"Address already in use".**
Another program (or another ClipCut) uses the port. Start with
`clipcut --port 7879`.

## Still stuck?

[Open an issue](https://github.com/adeebahmad01/clipcut/issues/new/choose)
with the output of `clipcut doctor`, your OS, and the log lines around the
error.
