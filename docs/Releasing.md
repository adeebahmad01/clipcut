# Releasing

Releases are built by `.github/workflows/release.yml` when a `v*` tag is
pushed.

1. Update `version` in `[workspace.package]` in `Cargo.toml`, then run
   `cargo build` so `Cargo.lock` picks it up.
2. Move the `## [Unreleased]` notes in `CHANGELOG.md` under a new
   `## [X.Y.Z] - YYYY-MM-DD` heading and update the links at the bottom.
   The release workflow copies that section into the release notes.
3. Commit, tag and push:

   ```bash
   git commit -am "Release vX.Y.Z"
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```

4. The workflow builds on native runners for each target, then publishes
   a GitHub release with:

   - `clipcut-<target>.tar.gz` and `clipcut-<target>.tar.gz.sha256` for
     `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` and
     `aarch64-apple-darwin`. Intel macOS is left out because ort has no
     prebuilt ONNX Runtime for it.
   - `install.sh`
   - `SHA256SUMS`

Asset names carry no version number, so
`releases/latest/download/clipcut-<target>.tar.gz` always points at the
newest build. The website and README rely on that.

To rebuild a release that already exists, run the workflow from the
Actions tab with the tag as input. It replaces the assets.

## Website

`site/` is deployed to GitHub Pages by `.github/workflows/pages.yml` on
every push to `main` that changes it. It is static HTML; open
`site/index.html` in a browser to preview.
