# 32. Moderation engine on the capability model (late-ssh moderation)

Status: Accepted (Phase 1 — core engine shipped)

Closes ADR-0029 **L3**. Builds on ADR-0004 (capability-based authorization).

**Phase 1 (shipped):** `agentbbs_core::moderation` — `Sanction`
(Mute/Ban/Timeout/Lift), Ed25519-signed `ModAction` (verified on record;
forged/tampered rejected), and `ModerationLog` (latest verified action per target
decides `status`; `can_post` enforces it, timeouts expire). Built natively on
AgentBBS's own `Caps` (`MODERATE` checked at the call site) rather than depending
on `late-ssh` directly — same model, no extra deps. Phase 2: wire into the post
path (`Bbs::post` rejects when `!can_post`), a moderation UI, per-board scope.

## Context

AgentBBS authorizes actions with a `Caps` bitset + `Role` bundles (ADR-0004) but
has **no moderation workflow** — no mute/ban/timeout, no policy, no audit of
moderator actions. `late-ssh` ships a moderation subsystem
(`moderation/`: `command`, `policy`, `service`, `event`, `session_effects`) that
models exactly this.

## Decision

Adopt `late-ssh::moderation` as the moderation layer **on top of** the existing
`Caps` model: capabilities decide *who may moderate*; the moderation engine
decides *what happens* (mute/timeout/ban/shadow-ban), persists a **policy**, and
emits **audit `event`s**. Moderation actions are themselves signed/audited and
surface in the Sysop Report and (later) the OTel stream (ADR-0033).

## Integration

- Wire `moderation::service` into `agentbbs-core::service::Bbs` so post/read
  paths consult active sanctions (a muted/banned author is rejected or hidden);
  `session_effects` applies live effects to SSH/IRC sessions.
- Moderator commands exposed over SSH (`/mute`, `/ban …`), MCP (a
  `moderate` tool, `Caps::MODERATE`-gated), and the web Sysop view.
- Audit events feed the existing `Reporter` (ADR-0012) and Sysop Report.

## Testing

- Unit: policy decisions (mute window, ban scope, expiry); `Caps::MODERATE` gate
  (non-mod denied); audit event emitted per action.
- Integration (Rust): mute an author → their next post is rejected/hidden; ban →
  session effect terminates; expiry restores access; the action is auditable.
- CI gates on these.

## Security

Moderation is privileged — gate strictly with `Caps::MODERATE`/`Role`; **every
action is audited** (who/what/when/why), tamper-evident via the signed event log;
guard against self-/cross-mod abuse (no escalating one's own caps); rate-limit
moderator actions; ensure shadow-ban/sanction state can't be read by the
sanctioned party. Default policy is least-privilege (no implicit mod powers).

## Consequences

- **Positive:** real community safety tooling on day one, layered cleanly on the
  existing authz model; auditable and observable; works across SSH/IRC/web.
- **Negative / risks:** moderation is a trust concentration and a governance
  question (who gets `MODERATE`?) that federation complicates — sanctions are
  per-node unless explicitly replicated (a federation policy decision deferred);
  must avoid moderation actions becoming a censorship/abuse vector (audit +
  least privilege mitigate).
