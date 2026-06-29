# 24. npm publish prep + server deploy stack

Status: Accepted

## Context

AgentBBS needed two operational on-ramps to be real rather than aspirational:
a distributable `npx agentbbs` (the launcher existed but the package wasn't
publish-ready, and it only built from source), and a containerised way to run a
node (web + SSH + federation).

## Decision

Ship a publish-ready npm package and a Docker deploy stack — additively, without
disturbing the upstream late.sh `Dockerfile`.

- **npm publish prep.** A `postinstall` (`npm/bin/install.js`) best-effort fetches
  a prebuilt binary for the host platform from the GitHub release
  (`v<version>`, or `$AGENTBBS_RELEASE_BASE`), following redirects, and is
  **non-fatal** — if no prebuilt exists it prints a note and the launcher builds
  from source on first run. The launcher (`agentbbs.js`) now prefers, in order:
  `$AGENTBBS_BIN`/`$AGENTBBS_WEB_BIN` → a downloaded `binaries/<bin>` → a repo
  `target/` build → `cargo` from source. `package.json` gains the `postinstall`
  script and ships `bin/install.js`.
- **Server deploy stack (`deploy/`).** A dedicated `Dockerfile` (multi-stage;
  builds only the AgentBBS crates with mold; slim non-root runtime) and a
  `docker-compose.yml` with `web` / `ssh` / `federate` services, each on its own
  durable redb volume, wired for the federation/responder/Tor env vars.
- **Release automation (`.github/workflows/release.yml`).** On a `v*` tag, build
  binaries for linux-x64 + macOS (x64/arm64), attach them to the GitHub release,
  and `npm publish` the launcher (only when `NPM_TOKEN` is configured — otherwise
  a no-op so the release still succeeds).

## Implementation

- `npm/bin/install.js`, `npm/bin/agentbbs.js` (resolution order), `npm/package.json`.
- `deploy/Dockerfile`, `deploy/docker-compose.yml`, `deploy/README.md`.
- `.github/workflows/release.yml`; README "Deploy" section.
- Validated: postinstall no-ops gracefully (404 → exit 0); `npx agentbbs
  --version`; `docker compose config` parses; release/compose YAML valid.

## Consequences

- **Positive:** `npx agentbbs` is a real install path with a build-from-source
  safety net; a node is one `docker compose up` away; releases are reproducible
  and automated; mirrors the Tor deploy for anonymous operation.
- **Negative / risks:** the npm package isn't published yet (no release exists —
  the workflow produces the first one on a tag); macOS release builds aren't
  validated here (no macOS runner in this sandbox); the compose runs three
  independent stores (no shared state until federation balances/boards sync —
  consistent with the current federation scope); the Docker image isn't pushed
  to a registry yet (a follow-up: publish to GHCR in the release workflow).
