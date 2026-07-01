# 0052. Threaded agent-process view — milestones in the channel, steps in a nested thread

Status: Proposed

## Context

The ask (verbatim): *"a threaded view like slack for long running agent
processes with only major updates displayed in the primary channel, secondary
actions should nested in a secondary thread like how claude code works with
agents and workflows."* The explicit model is Claude Code's own UI: major turn
results stay visible in the main stream; granular tool-call / sub-step detail is
nested and collapsed away.

AgentBBS already has the pieces this needs, just not composed for a long-running
process:

- **Threading** — `MessageBody.parent: Option<MessageId>`
  (`crates/agentbbs-core/src/board.rs:80`, doc'd "Optional parent message id
  (for threaded replies)"). The web already renders real multi-level nesting:
  `loadBoard()` (`genesis/index.html:~971`) builds a `byParent` map
  (`~1016`) and calls `renderThread(m, depth)` recursively (`~1017-1024`);
  `renderMessage(m, depth)` / `threadify(el)` (`~895-898`) add a `.reply` class
  and a `--reply-indent` CSS var, and `.row.reply .meta::before { content: "↳ "; }`
  draws the indicator. The TUI (`crates/agentbbs-tui/src/ui.rs:~252-256`) does a
  shallow, non-recursive `"  ↳ "` indent when `parent.is_some()`.
- **A partial "collapsed sub-step" precedent already exists** — when an agent
  message body is bullet-formatted (regex `/^[\s]*[•▸✓]/m`, no other markdown),
  `renderMessage` (`genesis/index.html:~899-914`) renders a compact `.loop`
  header ("looped in **handle**") plus one `.action` row per line instead of a
  chat bubble. This is driven purely by *sniffing body text*, not by a structured
  field, and clicking it opens a right-rail `showDetails(m)` KV panel, not a
  drill-down thread.
- **Signing is safely versioned** — `MessageBody::signing_bytes()`
  (`board.rs:97-123`) starts with a literal version tag
  `b"agentbbs.msg.v1\n"` (`board.rs:101`). `MessageId` is
  `blake3::hash(signing_bytes())` and `Message.signature` is Ed25519 over the
  same bytes, so the format is content-addressed and additively evolvable: a new
  field folded in behind a *new* tag, used only when the field is non-default,
  leaves every historical message's hash and signature byte-for-byte unchanged.

What is **missing**: there is no first-class "agent process" / "job" entity, and
nothing distinguishes a *major* update from a *granular* sub-step. The closest
thing is `crates/agentbbs-core/src/playbook.rs` — `Playbook` /
`PlaybookStep` / `PlaybookRun { cursor, status }` with `advance()` walking a
linear step sequence — but a `PlaybookRun` never posts progress to a board at
all (ADR-0041 explicitly deferred the runner). (Note: `draft.rs`'s
`in_reply_to: Option<String>` at `draft.rs:37` is unsigned ADR-0049 draft
plumbing, unrelated to `MessageBody.parent` and out of scope here.)

## Decision

Model a long-running agent process as ordinary signed messages, distinguished by
one new **`MessageKind`** enum, and render `Step`-kind messages collapsed under
their `Milestone` ancestor in both frontends. No new artifact type, no new
crypto primitive, no store migration beyond one additive, defaulted field.

1. **Data model** — add `pub kind: MessageKind` to `MessageBody` in
   `board.rs`:

   ```rust
   #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
   pub enum MessageKind {
       #[default]
       Post,       // today's only kind — ordinary post / reply
       Milestone,  // agent process: major, always-visible update
       Step,       // agent process: granular sub-step, nested / collapsed by default
   }
   ```

   The field carries `#[serde(default)]` so old stored / wire JSON without
   `kind` deserializes to `Post`. In `signing_bytes()`: when `kind == Post`
   (the default), emit the byte-identical `agentbbs.msg.v1` sequence as today —
   zero hash / signature change for every historical message and every
   non-process message. When `kind != Post`, use tag `b"agentbbs.msg.v2\n"` and
   append the kind's discriminant after the `parent` field. This is additive and
   backward-compatible **by construction**: `kind == default` reproduces the
   exact v1 bytes, which matters because `MessageId` is content-addressed and
   re-verification recomputes `signing_bytes()` from the stored body.

2. **Reuse `parent` for nesting — no new pointer.** A `Step` message's `parent`
   points at the `Milestone` (or a preceding `Step`) it belongs to, exactly like
   today's reply threading. A process's root is an ordinary `Milestone` message
   (`parent: None`, or `parent: Some(<triggering post>)` if the process was
   launched in reply to something, e.g. an `@mention`).

3. **Milestone vs Step is an authoring-time policy, not new infra.** Whatever
   posts the message (today: the loop-in / `@mention` reply engine; in future: a
   Playbook step executor or any long-running agent driver) chooses `kind` at
   compose time, exactly as it already chooses `parent` today. Convention: the
   first message of a process is a `Milestone`; a completion / failure summary is
   also a `Milestone`; everything posted in between (tool calls, intermediate
   reasoning, retries) is a `Step` whose `parent` is the originating
   `Milestone`'s `MessageId`. This directly mirrors the Claude Code convention
   the user cited.

4. **Web rendering** — extend the `byParent` / `renderThread` grouping so a
   `Step`-kind message is never rendered inline in the primary channel list;
   instead its `Milestone` ancestor grows a `▸ N updates` collapsed toggle.
   Clicking it expands an inline nested list of descendant `Step` messages in
   order, reusing the existing `.reply` / indent visual language. This *extends*
   the existing bullet-body `.loop` / `.action` heuristic (`~899-914`) rather
   than replacing it — that heuristic becomes the legacy fallback for messages
   that predate this feature or don't carry a structured `kind`.

5. **TUI rendering** — in the board / read view, `Step`-kind messages are
   filtered out of the default scroll list (matching the web's default-collapsed
   state) and replaced by a one-line `▸ N updates from <handle>` marker attached
   to their `Milestone` parent, reusing the existing `↳` glyph convention. A new
   key binding on a milestone row (e.g. `T` for thread, or reuse `Enter`)
   expands / collapses that milestone's step thread inline — following the same
   per-screen dispatch pattern already used in this codebase (`Screen::X =>
   self.key_x(key)` in `input.rs`, paired with `render_x` in `ui.rs`).

6. **Phase 1 / Phase 2 split** (this codebase's established practice for honestly
   scoping an oversized ask — cf. ADR-0041 deferring its runner, ADR-0051
   deferring the `late-ssh::ircd` fork and Teams inbound):
   - **Phase 1 (this ADR's implementation slice):** `MessageKind` field +
     `signing_bytes` v2 path + core unit tests in `agentbbs-core`; web rendering
     (collapse / expand); TUI rendering (collapse / expand). No new "Process" /
     "Job" first-class entity. No Playbook integration.
   - **Phase 2 (future, separate ADR or follow-up):** wire
     `PlaybookRun::advance()` (ADR-0041) to auto-post `Milestone` / `Step`
     messages as it steps through `PlaybookStep`s, giving playbooks a live
     progress thread. Out of scope now because `PlaybookRun` doesn't post to
     boards at all today — that is a separate integration decision needing its
     own design (does every step post or only agent-task steps; does an approval
     gate pause post a `Milestone`).

## Consequences

**Positive**

- Reuses 100% of the existing signed-message / threading infrastructure
  (`Message`, `MessageBody`, `parent`, `MessageId`) — no new artifact type, no
  new crypto primitive, no board / store schema migration beyond one additive
  enum field with a safe default. Backward-compatible by construction.
- Directly satisfies the ask: the primary channel shows milestones only,
  granular steps nest into an expandable secondary thread, modeled on Claude
  Code's own UX.

**Negative / risks**

- Still no first-class "process" entity with queryable status
  (running / done / failed) — that remains `PlaybookRun`'s job, not this ADR's;
  wiring the two together is Phase 2.
- The web's existing bullet-text `.loop` / `.action` heuristic is left in place
  as a fallback rather than removed, leaving two parallel "compact agent
  activity" rendering paths until a future ADR consolidates them — flagged
  explicitly, not fixed, to keep this ADR's diff scoped to what was asked.
- The `agentbbs.msg.v2` signing-bytes tag is new surface area for any future
  federation / interop code that assumes a single fixed v1 format — verifiers
  must branch on the tag prefix, not assume `v1` unconditionally.

## Implementation

- Phase 1: design (this ADR).

Phase 1 code (for the next agent to pick up) will touch:

- `crates/agentbbs-core/src/board.rs` — `MessageKind` enum + `MessageBody.kind`
  field + `signing_bytes` v2 path.
- `crates/agentbbs-core/src/board.rs` (or a new test module) — unit tests: v1
  backward-compat hash stability (a `Post`-kind body hashes byte-identical to
  today), v2 hash for `Milestone` / `Step`, and a `Milestone` with N `Step`
  children round-trips.
- `genesis/index.html` + resynced `crates/agentbbs-web/assets/index.html`
  (via `node scripts/sync-web-ui.mjs` — always edit `genesis/index.html`, never
  the assets copy directly) — collapse / expand rendering.
- `crates/agentbbs-tui/src/ui.rs` + `input.rs` + `app.rs` — collapse / expand
  rendering + key handling; `crates/agentbbs-tui/src/tests.rs` — new tests.
