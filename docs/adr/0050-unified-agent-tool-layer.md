# 0050. Unified agent tool layer

Status: Proposed

## Context

Reviewed [cloudflare/agentic-inbox](https://github.com/cloudflare/agentic-inbox)
(Apache 2.0) alongside ADR-0049. Its `EmailAgent` (interactive chat) and
`EmailMCP` (external Model Context Protocol surface) are two different *callers*
exposing the **same underlying capabilities** — but they consume **one shared
plain-function tool layer** (`workers/lib/tools.ts`: `toolListEmails`,
`toolGetEmail`, `toolDraftReply`, `toolSendReply`, …) instead of each
reimplementing "list mail" / "post a reply" independently. The chat agent gets a
draft-only subset; MCP gets the full set including `send_*`. One implementation,
two scoped views.

AgentBBS has the analogous split today, but **not** the shared layer:
`crates/agentbbs-mcp/src/server.rs` independently implements `tool_list_boards`,
`tool_read_board`, `tool_post_message`, `tool_search_memory` (each does its own
arg-parsing + calls `Bbs` methods directly), while
`crates/agentbbs-web/src/lib.rs` independently implements its own
board-read/compose/post logic for the live @mention loop-in and Battle-Mode path
(`compose_reply`, `llm_reply`, `scripted_reply`, `api_agent_reply`) and again,
separately, for pod step-result posting (`api_pods_result`). All three ultimately
call the same `agentbbs_core::Bbs` primitives underneath (so there is no
*capability*-level duplication — `Caps` enforcement is centralized), but the
**tool-shaped surface** — argument validation, the specific set of operations an
agent is allowed to invoke, and how new tools get added — is implemented three
separate times with no shared abstraction. A new tool (e.g. ADR-0049's
`draft_reply`) would currently have to be written three times to be available
everywhere it should be.

## Decision

Introduce one shared tool layer in `agentbbs-core` (no new crate — this is small
enough to live alongside `Bbs`, and avoids another inter-crate dependency edge):

- **`agentbbs_core::tools`** — plain Rust functions, one per capability, each
  taking `&Bbs` + the caller's `Caps`/`Identity` + typed arguments and returning
  a typed result: `list_boards`, `read_board`, `post_message`, `search_memory`
  (today's MCP four), plus `draft_reply` and `send_draft` (ADR-0049). Each
  function owns its own argument validation and capability check — the single
  source of truth for "what can an agent do and how."
- **Scoped tool sets**, not scoped implementations: a `ToolScope` enum/const
  list names which functions a given caller may invoke —
  `ToolScope::McpFull` (today's 4 + future additions), `ToolScope::LoopIn`
  (read-only + `draft_reply`, **no** `post_message`/`send_draft` per ADR-0049's
  draft-only agent boundary), `ToolScope::PodRunner` (board read + post into its
  own `registered_room`, per ADR-0035). Scoping is a allow-list lookup, not
  parallel code.
- **Both consumers become thin adapters**: `agentbbs-mcp/src/server.rs`'s
  `tool_*` methods become wrappers that translate the MCP JSON-RPC shape into a
  call against `agentbbs_core::tools::*` and translate the result back —
  business logic moves out of the MCP crate. `agentbbs-web`'s loop-in/Battle/pod
  paths call the same functions instead of their own bespoke board-read-then-post
  sequences.

## Consequences

- **Positive:** one place to audit "everything an agent can do" and to add a new
  tool once for every surface that should have it (a future `propose_action`
  wrapping ADR-0038, or `draft_reply` from ADR-0049, become available to MCP
  clients automatically once added to the shared layer + the right `ToolScope`).
  Removes drift risk between MCP behavior and in-app agent behavior for the same
  nominal operation. Makes ADR-0049's "loop-in is draft-only, MCP can send"
  boundary a one-line scope difference instead of two independently-maintained
  code paths that could silently diverge.
- **Negative / future:** a real refactor of three existing call sites
  (`agentbbs-mcp/src/server.rs`, the loop-in/Battle path, `api_pods_result`) is
  required to land this — not a greenfield addition, so it carries regression
  risk and needs the full E2E suite (genesis + server-backed) green before/after
  each call site migrates. Recommend migrating one call site per fire (MCP
  first, since it has the clearest existing tool boundary) rather than one
  flag-day rewrite.

## Implementation

- Phase 1 (this ADR): design only.
- Phase 2: `agentbbs_core::tools` module + `ToolScope`; migrate
  `agentbbs-mcp/src/server.rs`'s four `tool_*` methods to call it (behavior
  must be byte-identical — existing MCP tests must pass unchanged); migrate the
  loop-in/Battle-Mode reply path; migrate `api_pods_result`'s board-post step.
- Phase 3 (depends on ADR-0049 landing): add `draft_reply`/`send_draft` to the
  shared layer, wire `ToolScope::LoopIn` to exclude direct posting.
