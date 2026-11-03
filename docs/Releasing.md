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

The website and these docs are deployed to
[clipcut-sigma.vercel.app](https://clipcut-sigma.vercel.app). The Vercel
project is connected to this GitHub repo, with `main` as its production
branch. Pushes affecting website sources trigger deployment, and pull
requests get preview deployments. Unrelated app-only changes are skipped
by `ignoreCommand` in the root `vercel.json`.

`site/build.mjs` renders `docs/*.md`, `ROADMAP.md`, `ROADMAP-PROMPTS.md`,
`CHANGELOG.md`, `CONTRIBUTING.md` and `SECURITY.md` into pages next to the
landing page. Preview it locally with Node 20 or newer:

```bash
cd site
npm install
npm run dev     # builds, then serves on port 4321 with a Network URL
npm run watch   # rebuilds on every change; run it next to `npm run dev`
npm test        # build, internal links, checklist/prompt parity and size budgets
```

Use the printed Network URL when testing from a browser or another device.
The build fails on a link to a heading that doesn't exist, so a renamed
heading can't silently break a link. CI also checks every generated internal
URL, sidebar state, checklist status, prompt coverage and asset size budgets.

To deploy by hand, run `vercel deploy --prod` from the repo root.
Canonical metadata uses `SITE_URL`, then Vercel's
`VERCEL_PROJECT_PRODUCTION_URL`, then the production address above.
