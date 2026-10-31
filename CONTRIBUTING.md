# Contributing to ClipCut

Thanks for helping. Bug reports, fixes, new sources and docs are all
welcome.

## Before you start

For anything bigger than a small fix, open an issue first so we can agree
on the approach. Keep each pull request to one change.

## Setup

You need Rust 1.88+, ffmpeg and espeak-ng (see
[Installation](docs/Installation.md)).

```bash
git clone https://github.com/adeebahmad01/clipcut
cd clipcut
cargo build
cargo test --workspace
cargo run -p clipcut -- --no-window
```

Tests don't call Claude or the network. To try a full run you need Claude
Code signed in.

## Working on the UI

The UI lives in `crates/app/web/` as plain HTML, CSS and JavaScript, with
no build step. It is embedded in the binary, so rebuild after changing it,
or use the mock server, which serves the files straight from disk with
fake data:

```bash
node tools/mock-server.mjs
```

Then open the Network URL it prints.

## Working on the website and docs

The website is `site/`, and it renders the Markdown in `docs/` and the
repo root into its docs section. Edit the Markdown, not the generated HTML.
See [Releasing](docs/Releasing.md#website) to preview it locally.

## Picking up a roadmap item

[ROADMAP.md](ROADMAP.md) lists what comes next, each item with an ID such
as `CC-012`. [ROADMAP-PROMPTS.md](ROADMAP-PROMPTS.md) has a ready prompt
for each one that you can hand to an AI coding agent or follow yourself.
Tick the item's box (`- [x]`) in the same pull request that ships it; the
website's roadmap updates from that file on the next deploy.

## Guidelines

- Match the style of the code around your change. Run `cargo fmt`.
- Add or update tests for behavior changes.
- Keep ClipCut free to run: new sources should work without a paid key,
  or with a free key that is optional.
- Never weaken the Claude isolation flags in `crates/llm/src/runner.rs`.
- Update the docs and `CHANGELOG.md` under `[Unreleased]`.

## Releases

See [docs/Releasing.md](docs/Releasing.md).

By contributing, you agree that your work is released under the
[MIT license](LICENSE).
