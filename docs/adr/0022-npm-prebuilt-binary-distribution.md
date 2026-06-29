# 0022. npm Distribution via Prebuilt Binary Fetch

## Status

Accepted

## Context

AgentBBS is implemented in Rust. Publishing it as a native binary means end users
must either have the Rust toolchain installed and wait through a full workspace
compile (~minutes), or find and install a platform-specific release artifact
manually. Neither path works for `npx agentbbs` — the standard "try it now"
entry point that Node.js users expect to work on a clean machine with no prior
setup.

The npm package must therefore ship or fetch the correct native binary
transparently on first run, without requiring Rust, without bundling large
binaries in the npm tarball (which would balloon registry size and install time),
and with a fallback chain that degrades gracefully.

## Decision

The npm package (`npm/package.json`, `bin: "agentbbs.js"`) ships only a small
Node.js launcher (`npm/bin/agentbbs.js`). On first run the launcher resolves the
real binary through the following priority chain:

1. **`$AGENTBBS_BIN` / `$AGENTBBS_WEB_BIN`** — explicit path override for
   advanced users or CI.
2. **Local repo build** — a `target/` debug or release build found relative to
   the working directory (for contributors running inside the repo).
3. **Cached prebuilt** — a previously downloaded binary in the per-version cache
   directory (avoids re-downloading on subsequent runs).
4. **Download prebuilt** — fetch the platform/arch asset from the GitHub release
   tagged `agentbbs-v{version}` (e.g. `agentbbs-linux-x64`,
   `agentbbs-web-darwin-arm64`, `agentbbs-win32-x64.exe`), verify its SHA-256
   against the co-published `sha256sums.txt`, write it to the cache, and exec it.
5. **`cargo build`** — if `cargo` is available and the download failed, build
   from source.
6. **Install instructions** — a clear error message with manual steps.

Release assets are named `{bin}-{platform}-{arch}[.exe]`, matching Node's
`process.platform` and `process.arch`. A `sha256sums.txt` file in the release
provides checksums for all assets; the launcher verifies the download before
caching or executing it.

## Consequences

**Positive**

- `npx agentbbs web` works on a clean Ubuntu/macOS/Windows machine with only
  Node >= 18 installed — no Rust, no `cargo install`, no manual download.
- The npm tarball is tiny (one JS file + README); binary weight stays in GitHub
  Releases, not the npm registry.
- SHA-256 verification on download prevents corrupted or tampered binaries from
  running silently.
- The fallback chain means existing contributors and CI never change workflow;
  the new launcher transparently uses whatever binary is already at hand.

**Negative / risks**

- On a first run with no cached binary and no network, the launcher falls all the
  way to `cargo build` or an error. `npx agentbbs` is not fully offline-capable
  on a clean machine.
- GitHub Releases imposes no formal SLA on asset download speed; large binaries
  (the Rust binary is typically 10–30 MB) may be slow to download on constrained
  connections.
- Every platform/arch pair requires a prebuilt in the release; any missing
  combination falls to the `cargo` fallback.
- The launcher's checksum step is correct only if `sha256sums.txt` is published
  atomically with the binaries; a partial release is a security window.

## Implementation

- `npm/package.json`: package name `agentbbs`, `bin.agentbbs = "bin/agentbbs.js"`,
  `files: ["bin/agentbbs.js", "README.md"]`, `engines.node = ">=18"`.
- `npm/bin/agentbbs.js`: launcher; platform detection (`assetPlatform()`);
  resolution chain (env → local build → cache → download → cargo → instructions);
  SHA-256 verification via `node:crypto`; exec via `node:child_process.spawn`.
- GitHub release tag convention: `agentbbs-v{npm-version}`.
- Asset naming: `agentbbs-{platform}-{arch}` and `agentbbs-web-{platform}-{arch}`
  (plus `.exe` on win32), with `sha256sums.txt`.
- Shipped in commit `7509891` (`feat(npm): fetch prebuilt binaries so npx agentbbs
  runs on a clean machine`).
