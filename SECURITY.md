# Security

## Reporting a problem

Please report security problems privately through
[GitHub's private vulnerability reporting](https://github.com/adeebahmad01/clipcut/security/advisories/new),
not in a public issue. Include the version (`clipcut --version`), your OS,
and steps to reproduce. You should get a reply within a week.

Only the latest release gets fixes.

## How ClipCut is meant to be run

- **The web app has no login.** By default it listens on `0.0.0.0` so you
  can open it from your phone. Anyone who can reach the port can use it,
  read your projects and spend your API keys. On networks you don't trust,
  set `server.host = "127.0.0.1"` in `config.toml`.
- **API keys** are stored in `keys.json` with owner-only permissions
  (`0600`) and are never sent to the browser in full.
- **Claude Code** runs in a fresh empty directory with tools, skills,
  hooks and MCP servers disabled. Prompts include text from web pages, so
  ClipCut treats model output as untrusted data: it is parsed against a
  schema and never executed. If your `claude` lacks the flags needed for
  this isolation, ClipCut refuses to run it.
- **Downloads** go only into ClipCut's data directory. Model files are
  checked against their expected sizes, and the release archives have
  SHA-256 checksums.
