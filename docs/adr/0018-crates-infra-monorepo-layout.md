# 0018. Crates-Plus-Infra Monorepo Layout

## Status

Accepted

## Context

As AgentBBS grew, the repository root became cluttered: 13 Rust workspace
crates (`late-*`, `agentbbs-*`, `vendor`), Terraform/deploy assets, and
infrastructure config all lived at the top level alongside `Cargo.toml`,
`Dockerfile`, and `README.md`. This made it hard to see the Rust/infra boundary
at a glance, confused tooling path expectations, and gave new contributors no
clear signal about where to look for what.

CI (`cargo test --workspace`, path-based triggers) was already agnostic to the
physical layout of crate directories, so a restructure would not require CI
changes — only path fixups inside the affected config files.

## Decision

Reorganize the cluttered root into two clean sub-trees while preserving all git
history through `git mv`:

- **`crates/`** receives all 13 Rust workspace members: `late-cli`, `late-core`,
  `late-nethack`, `late-ssh`, `late-web`, `agentbbs`, `agentbbs-arena`,
  `agentbbs-core`, `agentbbs-federation`, `agentbbs-mcp`, `agentbbs-tui`,
  `agentbbs-wasm`, `agentbbs-web`, and `vendor/`.
- **`infra/`** receives `agentbbs-gcp/` (the Firestore/Pub/Sub reporter, Cloud
  Function, and Terraform, see ADR-0012) alongside the existing Terraform and
  deploy assets.

Path fixups applied atomically in the same commit (PR #2):
- `Cargo.toml`: workspace `members` glob, `late-core` path dep, `asterion-core`
  patch path.
- `crates/agentbbs-gcp/Cargo.toml`: `agentbbs-core` path adjusted to the new
  sibling layout.
- `Dockerfile`: all `COPY`, `cargo-watch`, and `cd` paths updated; the runtime
  `ServeDir` destination (`/app/late-web/static`) is preserved — only build
  source paths changed.
- `default.nix`: version-read path and `node_modules` filter.
- `.gitignore`: `late-ssh` patterns repointed to `crates/late-ssh`.

The restructure was validated end-to-end: all 2 030 unit tests green, full
E2E suite passing, Docker image builds.

## Consequences

**Positive**

- The monorepo root is a clean index of major components (code, infra, genesis,
  docs, npm) rather than a flat list of 13 crate directories.
- The Rust / infrastructure boundary is immediately visible: `crates/` = Rust,
  `infra/` = GCP + Terraform.
- `git mv` preserves blame and log history across every file; no history was
  squashed.
- Downstream tools (IDE workspaces, `cargo-watch`, Docker multi-stage) required
  only path string updates — no logic changes.

**Negative / risks**

- Any external link to a raw GitHub file path at the old crate locations is now
  a 404 (GitHub does not redirect within-repo path moves).
- Editors with cached workspace roots need a one-time refresh; existing `cargo
  build` artifacts under `target/` remain valid (Cargo keyed on crate name, not
  path).
- The `infra/agentbbs-gcp/Cargo.toml` is now inside `infra/`, which is outside
  the `crates/` workspace glob; it must remain an explicitly named workspace
  member if added to the root manifest.

## Implementation

- `Cargo.toml` (workspace root): updated `members`, patch paths.
- `crates/` — all Rust workspace crates (see member list above).
- `infra/` — `agentbbs-gcp/`, `terraform/`, deploy scripts, monitoring.
- `Dockerfile`, `default.nix`, `.gitignore` — path fixups.
- PR #2 (`chore/restructure-and-e2e`): 975 `git mv` operations; build+E2E
  verified before merge.
