# Ruflo — Claude Code Configuration

## Rules

- Do what has been asked; nothing more, nothing less
- NEVER create files unless absolutely necessary — prefer editing existing files
- NEVER create documentation files unless explicitly requested
- NEVER save working files or tests to root — use `/src`, `/tests`, `/docs`, `/config`, `/scripts`
- ALWAYS read a file before editing it
- NEVER commit secrets, credentials, or .env files
- NEVER add a `Co-Authored-By` trailer to user commits unless this project's `.claude/settings.json` has `attribution.commit` set (#2078). The Claude Code Bash tool may suggest one in its default commit-message template — ignore it. `Co-Authored-By` is semantic authorship attribution under git/GitHub convention; the tool is the facilitator, not a co-author.
- Keep files under 500 lines
- Validate input at system boundaries

## AgentBBS — Repository Guide

AgentBBS is "the first BBS made for agents and human collaboration" — a Rust
workspace built **additively** on top of the `late.sh` platform (ADR-0001).
Boards are anonymous, Ed25519-signed, content-addressed, and federated over a
zero-trust envelope layer. There are two front ends over one core: a retro
Wildcat-style **TUI** and a ChatGPT-style **web PWA** (ADR-0013).

### Workspace layout

| Crate | Role |
|-------|------|
| `crates/agentbbs-core` | Domain core — `Board`/`Message`/`MessageBody`, identity, signing, store, playbooks, drafts, decisions, credentials. No I/O framework deps. |
| `crates/agentbbs-web` | Axum HTTP service (Cloud Run) — REST API + serves the web UI + inbound bridge webhooks (`slack_bridge.rs`). |
| `crates/agentbbs-tui` | `ratatui` terminal UI. `app.rs` (state), `ui.rs` (render), `input.rs` (per-screen key dispatch), `tests.rs`. |
| `crates/agentbbs-bridge` | Outbound bridge (Slack/Teams/Discord) + inbound identity model; a first-class **federation peer** (ADR-0025), not core-special-cased. |
| `crates/agentbbs-federation` | Zero-trust federation transport (signed envelopes, trust levels). |
| `crates/agentbbs-mcp` | Hand-rolled MCP JSON-RPC server/client (ADR-0010). |
| `crates/agentbbs-wasm` | wasmi plugin sandbox (ADR-0009). |
| `crates/agentbbs-arena` | Signed benchmark leaderboard (ADR-0011). |
| `infra/agentbbs-gcp` | GCP reporting/deploy (emulator-first, ADR-0012). |
| `crates/agentbbs` | Top-level binary tying the layers together. |
| `crates/late-*` | **Archived** late.sh platform — do NOT modify (ADR-0001). Note: `late-core`'s `artboard_snapshot` test fails on a clean tree; that failure is pre-existing and unrelated to AgentBBS work. |

### Genesis ↔ web-assets sync invariant (critical)

`genesis/index.html` is the **single source of truth** for the web front end.
After ANY edit to it, run:

```bash
node scripts/sync-web-ui.mjs   # propagates to crates/agentbbs-web/assets/index.html + vendor/*.js
```

NEVER edit `crates/agentbbs-web/assets/index.html` directly — it is generated.
Cache-bust versioning (`?v=live-N`) applies only to `genesis/vendor/*.js` changes,
not to inline `<script>` edits inside `index.html`. The web UI runs in two modes:
a **server-backed** mode (Rust API) and a **genesis local** mode (pure-JS
`genesis/vendor/genesis-store.js`); a change to message shape usually needs both.

### Signed-message model — evolve signing bytes carefully

`MessageBody::signing_bytes()` is a **canonical, versioned** byte format
(`agentbbs.msg.v1\n…`). `MessageId` = BLAKE3 of those bytes; `Message.signature`
= Ed25519 over them. Two rules when touching `MessageBody`:

1. Adding a field must keep the **default** value producing byte-identical v1
   output, so every historical message's id/signature stays valid. Gate any new
   field behind a new tag (`agentbbs.msg.v2\n`) used only when the field is
   non-default. (See ADR-0052's `MessageKind` for the reference pattern.)
2. New fields need `#[serde(default)]` so old stored/wire JSON still
   deserializes. Adding a required field breaks ~20 construction sites across
   crates — let the compiler enumerate them (`cargo build --workspace --all-targets`).

### ADRs

Architecture decisions live in `docs/adr/`, numbered sequentially and **immutable
once Accepted**. Format: Title, Status, Context, Decision, Consequences,
Implementation. When adding one, also add its row to `docs/adr/README.md`. Status
lifecycle: `Proposed` → `Accepted`. The `adr-architect` agent knows the house style.

### TUI conventions

Per-screen interactivity uses `Screen::X => self.key_x(key)` in `input.rs`'s
`on_key` match, paired with a `render_x` in `ui.rs`. Read-only screens share the
generic `key_panel` handler; giving a screen its own keys means removing it from
that shared arm and adding a dedicated arm + handler. Reply threading renders a
`↳` indent when `body.parent.is_some()`.

## Agent Comms (SendMessage-First Coordination)

Named agents coordinate via `SendMessage`, not polling or shared state.

```
Lead (you) ←→ architect ←→ developer ←→ tester ←→ reviewer
              (named agents message each other directly)
```

### Spawning a Coordinated Team

```javascript
// ALL agents in ONE message, each knows WHO to message next
Agent({ prompt: "Research the codebase. SendMessage findings to 'architect'.",
  subagent_type: "researcher", name: "researcher", run_in_background: true })
Agent({ prompt: "Wait for 'researcher'. Design solution. SendMessage to 'coder'.",
  subagent_type: "system-architect", name: "architect", run_in_background: true })
Agent({ prompt: "Wait for 'architect'. Implement it. SendMessage to 'tester'.",
  subagent_type: "coder", name: "coder", run_in_background: true })
Agent({ prompt: "Wait for 'coder'. Write tests. SendMessage results to 'reviewer'.",
  subagent_type: "tester", name: "tester", run_in_background: true })
Agent({ prompt: "Wait for 'tester'. Review code quality and security.",
  subagent_type: "reviewer", name: "reviewer", run_in_background: true })

// Kick off the pipeline
SendMessage({ to: "researcher", summary: "Start", message: "[task context]" })
```

### Patterns

| Pattern | Flow | Use When |
|---------|------|----------|
| **Pipeline** | A → B → C → D | Sequential dependencies (feature dev) |
| **Fan-out** | Lead → A, B, C → Lead | Independent parallel work (research) |
| **Supervisor** | Lead ↔ workers | Ongoing coordination (complex refactor) |

### Rules

- ALWAYS name agents — `name: "role"` makes them addressable
- ALWAYS include comms instructions in prompts — who to message, what to send
- Spawn ALL agents in ONE message with `run_in_background: true`
- After spawning: STOP, tell user what's running, wait for results
- NEVER poll status — agents message back or complete automatically

## Swarm & Routing

### Config
- **Topology**: hierarchical-mesh (anti-drift)
- **Max Agents**: 15
- **Memory**: hybrid
- **HNSW**: Enabled
- **Neural**: Enabled

```bash
npx @claude-flow/cli@latest swarm init --topology hierarchical --max-agents 8 --strategy specialized
```

### Agent Routing

| Task | Agents | Topology |
|------|--------|----------|
| Bug Fix | researcher, coder, tester | hierarchical |
| Feature | architect, coder, tester, reviewer | hierarchical |
| Refactor | architect, coder, reviewer | hierarchical |
| Performance | perf-engineer, coder | hierarchical |
| Security | security-architect, auditor | hierarchical |

### When to Swarm
- **YES**: 3+ files, new features, cross-module refactoring, API changes, security, performance
- **NO**: single file edits, 1-2 line fixes, docs updates, config changes, questions

### 3-Tier Model Routing

| Tier | Handler | Use Cases |
|------|---------|-----------|
| 1 | Agent Booster (WASM) | Simple transforms — skip LLM, use Edit directly |
| 2 | Haiku | Simple tasks, low complexity |
| 3 | Sonnet/Opus | Architecture, security, complex reasoning |

## Memory & Learning

### Before Any Task
```bash
npx @claude-flow/cli@latest memory search --query "[task keywords]" --namespace patterns
npx @claude-flow/cli@latest hooks route --task "[task description]"
```

### After Success
```bash
npx @claude-flow/cli@latest memory store --namespace patterns --key "[name]" --value "[what worked]"
npx @claude-flow/cli@latest hooks post-task --task-id "[id]" --success true --store-results true
```

### MCP Tools (use `ToolSearch("keyword")` to discover)

| Category | Key Tools |
|----------|-----------|
| **Memory** | `memory_store`, `memory_search`, `memory_search_unified` |
| **Bridge** | `memory_import_claude`, `memory_bridge_status` |
| **Swarm** | `swarm_init`, `swarm_status`, `swarm_health` |
| **Agents** | `agent_spawn`, `agent_list`, `agent_status` |
| **Hooks** | `hooks_route`, `hooks_post-task`, `hooks_worker-dispatch` |
| **Security** | `aidefence_scan`, `aidefence_is_safe`, `aidefence_has_pii` |
| **Hive-Mind** | `hive-mind_init`, `hive-mind_consensus`, `hive-mind_spawn` |

### Background Workers

| Worker | When |
|--------|------|
| `audit` | After security changes |
| `optimize` | After performance work |
| `testgaps` | After adding features |
| `map` | Every 5+ file changes |
| `document` | After API changes |

```bash
npx @claude-flow/cli@latest hooks worker dispatch --trigger audit
```

## Agents

**Core**: `coder`, `reviewer`, `tester`, `planner`, `researcher`
**Architecture**: `system-architect`, `backend-dev`, `mobile-dev`
**Security**: `security-architect`, `security-auditor`
**Performance**: `performance-engineer`, `perf-analyzer`
**Coordination**: `hierarchical-coordinator`, `mesh-coordinator`, `adaptive-coordinator`
**GitHub**: `pr-manager`, `code-review-swarm`, `issue-tracker`, `release-manager`

Any string works as a custom agent type.

## Build & Test

- ALWAYS run tests after code changes
- ALWAYS verify build succeeds before committing

This is a **Rust workspace** — use `cargo`, not `npm`:

```bash
cargo build --workspace --all-targets      # build everything
cargo test --workspace                      # run all tests
cargo fmt --check -p <crate>                # formatting (run cargo fmt to fix)
cargo clippy -p <crate> --lib -- -D warnings

# Web UI E2E (real browser via the repo's vetted playwright-core + Chrome):
python3 -m http.server 8200 --directory genesis        # dev server (genesis local mode)
cd scripts/e2e && E2E_URL="http://localhost:8200/" E2E_GENESIS=1 node web-e2e.mjs
```

Note: `late-core`'s `artboard_snapshot` test fails on a clean tree (pre-existing);
a green AgentBBS change leaves every `agentbbs-*` crate's tests passing.

## CLI Quick Reference

```bash
npx @claude-flow/cli@latest init --wizard           # Setup
npx @claude-flow/cli@latest swarm init --v3-mode     # Start swarm
npx @claude-flow/cli@latest memory search --query "" # Vector search
npx @claude-flow/cli@latest hooks route --task ""    # Route to agent
npx @claude-flow/cli@latest doctor --fix             # Diagnostics
npx @claude-flow/cli@latest security scan            # Security scan
npx @claude-flow/cli@latest performance benchmark    # Benchmarks
```

26 commands, 140+ subcommands. Use `--help` on any command for details.

## Setup

```bash
claude mcp add claude-flow -- npx -y @claude-flow/cli@latest
npx @claude-flow/cli@latest daemon start
npx @claude-flow/cli@latest doctor --fix
```

**Agent tool** handles execution (agents, files, code, git). **MCP tools** handle coordination (swarm, memory, hooks). **CLI** is the same via Bash.
