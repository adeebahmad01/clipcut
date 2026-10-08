# Installing ClipCut

ClipCut is one binary. It also calls a few tools that are installed
separately (ffmpeg, espeak-ng, Claude Code, and optionally a browser,
Node.js, Python and Podman). This page covers each one.

- [Quick install](#quick-install)
- [Manual install](#manual-install)
- [Dependencies by OS](#dependencies-by-os)
- [Claude Code](#claude-code)
- [The unslop skill](#the-unslop-skill)
- [What ClipCut downloads itself](#what-clipcut-downloads-itself)
- [Windows](#windows)
- [Build from source](#build-from-source)
- [Updating and uninstalling](#updating-and-uninstalling)

## Quick install

On Linux or macOS:

```bash
curl -fsSL https://github.com/adeebahmad01/clipcut/releases/latest/download/install.sh | sh
```

The script:

1. Downloads the archive for your OS and CPU from the latest release and
   checks its SHA-256.
2. Puts `clipcut` in `~/.local/bin`.
3. Finds which tools are missing and asks before installing each one with
   your package manager (apt, dnf, pacman, zypper or Homebrew). It only uses
   `sudo` for those package installs.
4. Offers to install Claude Code with its official installer, and to clone
   the unslop skill into `~/.claude/skills/unslop`.

Options:

| Flag | Environment variable | Effect |
|---|---|---|
| `--version v0.1.0` | `CLIPCUT_VERSION` | Install that release instead of the latest |
| `--dir PATH` | `CLIPCUT_INSTALL_DIR` | Install the binary somewhere else |
| `--no-deps` | `CLIPCUT_NO_DEPS=1` | Only install the binary |
| `--yes` | `CLIPCUT_YES=1` | Answer yes to every question |

Pass flags through `sh -s --`:

```bash
curl -fsSL https://github.com/adeebahmad01/clipcut/releases/latest/download/install.sh | sh -s -- --no-deps
```

If `~/.local/bin` is not on your `PATH`, the script says so. Add
`export PATH="$HOME/.local/bin:$PATH"` to `~/.bashrc` or `~/.zshrc`.

## Manual install

1. Download the archive for your platform from the
   [latest release](https://github.com/adeebahmad01/clipcut/releases/latest):

   | Platform | Archive |
   |---|---|
   | Linux x86_64 | `clipcut-x86_64-unknown-linux-gnu.tar.gz` |
   | Linux ARM64 (Raspberry Pi 5, Graviton…) | `clipcut-aarch64-unknown-linux-gnu.tar.gz` |
   | macOS Apple silicon (M1 and later) | `clipcut-aarch64-apple-darwin.tar.gz` |

   Linux builds need glibc 2.35 or newer (Ubuntu 22.04, Debian 12,
   Fedora 36 and later). Intel Macs are not supported yet: the ONNX
   Runtime build ClipCut uses for the voice has no Intel macOS version.

2. Check it and unpack it:

   ```bash
   sha256sum -c clipcut-x86_64-unknown-linux-gnu.tar.gz.sha256   # macOS: shasum -a 256 -c
   tar -xzf clipcut-x86_64-unknown-linux-gnu.tar.gz
   install -m 755 clipcut-x86_64-unknown-linux-gnu/clipcut ~/.local/bin/
   ```

3. On macOS the binary is not notarized, so Gatekeeper blocks the first
   run. Clear the quarantine flag once:

   ```bash
   xattr -d com.apple.quarantine ~/.local/bin/clipcut
   ```

4. Install the [dependencies](#dependencies-by-os), then run
   `clipcut doctor`.

## Dependencies by OS

| Tool | Required | Used for |
|---|---|---|
| ffmpeg (with ffprobe) | Yes | Cutting clips, probing media, encoding with x264 |
| espeak-ng | Yes, for the Kokoro voice | Turning words into phonemes |
| Claude Code | Yes | Research, hooks, script, beat sheet, clip and meme picks |
| Chromium, Chrome, Edge or Brave | Recommended | Article screenshots; the app window |
| Node.js or Deno | Recommended | yt-dlp needs a JS runtime for YouTube's player checks |
| Python 3 | Optional | Runs the unslop skill's scanners |
| Podman or Docker | Optional | Runs a local SearXNG for search; DuckDuckGo is the fallback |

### Ubuntu / Debian

```bash
sudo apt update
sudo apt install ffmpeg espeak-ng chromium nodejs python3 podman
```

On Ubuntu, `chromium` is a snap. If you prefer, install
[Google Chrome](https://www.google.com/chrome/) instead; ClipCut finds
either.

### Fedora

```bash
sudo dnf install ffmpeg espeak-ng chromium nodejs python3 podman
```

Fedora's default `ffmpeg-free` lacks x264. Enable
[RPM Fusion](https://rpmfusion.org/Configuration) and run
`sudo dnf swap ffmpeg-free ffmpeg --allowerasing` if renders fail with an
encoder error.

### Arch Linux

```bash
sudo pacman -S ffmpeg espeak-ng chromium nodejs python podman
```

### openSUSE

```bash
sudo zypper install ffmpeg espeak-ng chromium nodejs python3 podman
```

### macOS

With [Homebrew](https://brew.sh):

```bash
brew install ffmpeg espeak-ng node python podman
brew install --cask google-chrome
podman machine init && podman machine start   # only if you want SearXNG
```

### Download pages

If you would rather download each tool yourself:

- ffmpeg: <https://ffmpeg.org/download.html>
- espeak-ng: <https://github.com/espeak-ng/espeak-ng/releases>
- Claude Code: <https://docs.claude.com/en/docs/claude-code/setup>
- Chrome: <https://www.google.com/chrome/> or Chromium: <https://www.chromium.org/getting-involved/download-chromium/>
- Node.js: <https://nodejs.org/en/download>
- Python: <https://www.python.org/downloads/>
- Podman: <https://podman.io/docs/installation> or Docker: <https://docs.docker.com/get-docker/>

If a tool is installed somewhere ClipCut does not look, point at it in
`config.toml` (see [Configuration](Configuration.md#tools)).

## Claude Code

ClipCut does not call any LLM API directly. It runs the Claude Code CLI in
a locked-down, tool-free mode and uses your existing Claude login, so there
is no API key to paste into ClipCut.

```bash
curl -fsSL https://claude.ai/install.sh | bash
claude        # sign in once, then exit
```

`clipcut doctor` checks that your `claude` has the isolation flags ClipCut
relies on. If it reports missing flags, update Claude Code with
`claude update`.

Codex is detected but not used for generation: its read-only sandbox still
exposes tools, and ClipCut only runs agents with every tool disabled.

## The unslop skill

[unslop](https://github.com/theclaymethod/unslop) (MIT) rewrites the
script to remove phrases that read as AI-written. ClipCut loads it from
`~/.agents/skills/unslop`, `~/.codex/skills/unslop` or
`~/.claude/skills/unslop`, and runs its Python scanners. Without it,
ClipCut keeps the first draft.

```bash
git clone --depth 1 https://github.com/theclaymethod/unslop ~/.claude/skills/unslop
```

To turn the pass off, set `llm.skip_unslop = true`.

## What ClipCut downloads itself

On first start, in the background:

| Piece | Size | Where it goes |
|---|---|---|
| yt-dlp standalone binary | ~35 MB | `<home>/bin/yt-dlp` |
| Kokoro-82M fp16 model and voices | ~205 MB | `<home>/models/` |
| SearXNG container (only with podman/docker) | ~200 MB image | container `clipcut-searxng` on `127.0.0.1:8888` |

`<home>` is `~/.local/share/clipcut` on Linux and
`~/Library/Application Support/clipcut` on macOS. Run `clipcut setup all`
to install or update these by hand, or `clipcut setup yt-dlp` for just one.

## Windows

There is no native Windows build. Use WSL2:

```powershell
wsl --install -d Ubuntu
```

Open Ubuntu, then run the Linux installer. The Network URL that ClipCut
prints opens in your Windows browser.

## Build from source

You need Rust 1.88 or newer ([rustup.rs](https://rustup.rs)).

```bash
git clone https://github.com/adeebahmad01/clipcut
cd clipcut
cargo build --release
./target/release/clipcut
```

The first build downloads a static ONNX Runtime for the voice model, so it
needs network access. That runtime needs a C++ toolchain from GCC 14 or
newer on Linux. Ubuntu 24.04, Debian 13 and Fedora 40 have one. On
Ubuntu 22.04, install `g++-14` from the `ubuntu-toolchain-r/test` PPA and
build with `CC=gcc-14 CXX=g++-14 RUSTFLAGS="-C linker=gcc-14" cargo build --release`.

## Updating and uninstalling

Run the installer again to update. To remove ClipCut:

```bash
rm ~/.local/bin/clipcut
rm -rf ~/.local/share/clipcut              # macOS: ~/Library/Application\ Support/clipcut
podman rm -f clipcut-searxng               # if you used SearXNG
```

The second command deletes your projects and saved keys too.
