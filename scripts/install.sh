#!/bin/sh
# ClipCut installer for Linux and macOS.
#
#   curl -fsSL https://github.com/adeebahmad01/clipcut/releases/latest/download/install.sh | sh
#
# Installs the clipcut binary and, unless told not to, the tools it needs:
# ffmpeg, espeak-ng, Chromium, Node.js, Python 3, Podman, Claude Code and the
# unslop skill. Every step asks before using sudo.
#
# Options (also settable as environment variables):
#   --version vX.Y.Z   install a specific release      (CLIPCUT_VERSION)
#   --dir PATH         where to put the binary         (CLIPCUT_INSTALL_DIR, default ~/.local/bin)
#   --no-deps          only install the binary         (CLIPCUT_NO_DEPS=1)
#   --yes              answer yes to every question    (CLIPCUT_YES=1)

set -eu

REPO="adeebahmad01/clipcut"
VERSION="${CLIPCUT_VERSION:-latest}"
INSTALL_DIR="${CLIPCUT_INSTALL_DIR:-$HOME/.local/bin}"
NO_DEPS="${CLIPCUT_NO_DEPS:-0}"
YES="${CLIPCUT_YES:-0}"

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --dir) INSTALL_DIR="$2"; shift 2 ;;
    --no-deps) NO_DEPS=1; shift ;;
    --yes|-y) YES=1; shift ;;
    -h|--help) echo "usage: install.sh [--version vX.Y.Z] [--dir PATH] [--no-deps] [--yes]"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
info() { printf '  %s\n' "$*"; }
warn() { printf '\033[33m  ! %s\033[0m\n' "$*" >&2; }
die() { printf '\033[31merror: %s\033[0m\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Questions read from the terminal so `curl | sh` still works.
ask() {
  [ "$YES" = 1 ] && return 0
  if [ ! -r /dev/tty ]; then
    return 1
  fi
  printf '%s [Y/n] ' "$1" > /dev/tty
  read -r answer < /dev/tty || return 1
  case "$answer" in [nN]*) return 1 ;; *) return 0 ;; esac
}

# ---------------------------------------------------------------- binary ---

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux) os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  MINGW*|MSYS*|CYGWIN*) die "Windows is not supported natively. Install WSL2 (wsl --install) and run this script inside Ubuntu." ;;
  *) die "unsupported OS: $os" ;;
esac
case "$arch" in
  x86_64|amd64) arch_part="x86_64" ;;
  aarch64|arm64) arch_part="aarch64" ;;
  *) die "unsupported CPU: $arch" ;;
esac
if [ "$os" = Darwin ] && [ "$arch_part" = x86_64 ]; then
  # A shell under Rosetta reports x86_64 on Apple silicon.
  if [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
    arch_part="aarch64"
  else
    die "Intel Macs are not supported yet: the ONNX Runtime build ClipCut uses for the voice has no Intel macOS version."
  fi
fi
target="$arch_part-$os_part"
asset="clipcut-$target.tar.gz"
if [ "$VERSION" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

if have curl; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif have wget; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "curl or wget is needed"
fi

bold "Installing ClipCut ($VERSION, $target)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
fetch "$base/$asset" "$tmp/$asset" || die "could not download $base/$asset"
if fetch "$base/$asset.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
  expected="$(cut -d' ' -f1 < "$tmp/$asset.sha256")"
  if have sha256sum; then
    actual="$(sha256sum "$tmp/$asset" | cut -d' ' -f1)"
  else
    actual="$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)"
  fi
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset"
  info "checksum ok"
fi
tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/clipcut-$target/clipcut" "$INSTALL_DIR/clipcut"
# Unsigned downloads are quarantined on macOS; clear the flag for this binary.
if [ "$os" = Darwin ] && have xattr; then
  xattr -d com.apple.quarantine "$INSTALL_DIR/clipcut" 2>/dev/null || true
fi
info "installed $("$INSTALL_DIR/clipcut" --version) to $INSTALL_DIR/clipcut"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) warn "$INSTALL_DIR is not on your PATH. Add this to your shell profile:"
     warn "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

if [ "$NO_DEPS" = 1 ]; then
  echo
  bold "Done. Run: clipcut"
  exit 0
fi

# ---------------------------------------------------------- dependencies ---

echo
bold "Checking the tools ClipCut uses"

if [ "$(id -u)" = 0 ]; then
  sudo=""
elif have sudo; then
  sudo="sudo"
else
  sudo=""
fi

pm=""
if [ "$os" = Darwin ]; then
  have brew && pm=brew
elif have apt-get; then pm=apt
elif have dnf; then pm=dnf
elif have pacman; then pm=pacman
elif have zypper; then pm=zypper
fi

# Package name per manager: pkg <tool> -> name ("" = skip).
pkg() {
  case "$pm:$1" in
    *:ffmpeg) echo ffmpeg ;;
    *:espeak-ng) echo espeak-ng ;;
    apt:chromium) echo chromium ;;
    dnf:chromium|pacman:chromium|zypper:chromium) echo chromium ;;
    brew:chromium) echo "--cask google-chrome" ;;
    apt:node|dnf:node|zypper:node) echo nodejs ;;
    pacman:node) echo nodejs ;;
    brew:node) echo node ;;
    apt:python3|dnf:python3|zypper:python3|brew:python3) echo python3 ;;
    pacman:python3) echo python ;;
    *:podman) echo podman ;;
    *) echo "" ;;
  esac
}

pm_install() {
  case "$pm" in
    brew) # shellcheck disable=SC2086
          brew install $1 ;;
    apt) $sudo apt-get install -y "$1" ;;
    dnf) $sudo dnf install -y "$1" ;;
    pacman) $sudo pacman -S --needed --noconfirm "$1" ;;
    zypper) $sudo zypper --non-interactive install "$1" ;;
  esac
}

browser_found() {
  for b in chromium chromium-browser google-chrome google-chrome-stable microsoft-edge; do
    have "$b" && return 0
  done
  [ -d "/Applications/Google Chrome.app" ] || [ -d "/Applications/Chromium.app" ] || [ -d "/Applications/Microsoft Edge.app" ]
}

missing=""
if ! have ffmpeg || ! have ffprobe; then missing="$missing ffmpeg"; fi
if ! have espeak-ng && ! have espeak; then missing="$missing espeak-ng"; fi
browser_found || missing="$missing chromium"
if ! have node && ! have deno; then missing="$missing node"; fi
have python3 || missing="$missing python3"
if ! have podman && ! have docker; then missing="$missing podman"; fi

if [ -z "$missing" ]; then
  info "system tools: all present"
elif [ -z "$pm" ]; then
  if [ "$os" = Darwin ]; then
    warn "Homebrew is not installed, so these must be installed by hand:$missing"
    warn "Install Homebrew from https://brew.sh and run this script again."
  else
    warn "No supported package manager found. Install these yourself:$missing"
  fi
else
  info "missing:$missing"
  apt_updated=0
  for tool in $missing; do
    name="$(pkg "$tool")"
    [ -n "$name" ] || continue
    case "$tool" in
      ffmpeg|espeak-ng) why="required" ;;
      chromium) why="article screenshots and the app window" ;;
      node) why="lets yt-dlp pass YouTube's player checks" ;;
      python3) why="runs the unslop script checks" ;;
      podman) why="private web search via SearXNG; DuckDuckGo is used without it" ;;
      *) why="" ;;
    esac
    if ask "Install $tool ($why) with $pm?"; then
      if [ "$pm" = apt ] && [ "$apt_updated" = 0 ]; then $sudo apt-get update -q; apt_updated=1; fi
      pm_install "$name" || warn "could not install $tool; install it yourself"
    else
      info "skipped $tool"
    fi
  done
fi

# Claude Code writes the scripts. Its official installer needs no sudo.
if have claude; then
  info "Claude Code: present"
elif ask "Install Claude Code (writes the scripts; required)?"; then
  curl -fsSL https://claude.ai/install.sh | bash || warn "Claude Code install failed; see https://docs.claude.com/en/docs/claude-code/setup"
  info "run \`claude\` once to sign in before making a video"
else
  warn "skipped Claude Code. ClipCut cannot write scripts without it."
fi

# The unslop skill polishes the narration. Optional: drafts are kept without it.
skill_found=0
for d in "$HOME/.agents/skills/unslop" "$HOME/.codex/skills/unslop" "$HOME/.claude/skills/unslop"; do
  [ -f "$d/SKILL.md" ] && skill_found=1
done
if [ "$skill_found" = 1 ]; then
  info "unslop skill: present"
elif have git && ask "Install the unslop skill (makes narration sound less like AI)?"; then
  mkdir -p "$HOME/.claude/skills"
  if git clone --depth 1 -q https://github.com/theclaymethod/unslop.git "$HOME/.claude/skills/unslop"; then
    info "installed unslop to ~/.claude/skills/unslop"
  else
    warn "could not clone the unslop skill"
  fi
fi

echo
bold "Done."
info "Run \`clipcut\` to start the app, or \`clipcut doctor\` to check every piece."
info "On first start ClipCut downloads yt-dlp, the Kokoro voice model and SearXNG by itself."
