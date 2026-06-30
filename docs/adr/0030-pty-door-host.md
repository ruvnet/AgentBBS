# 30. PTY-hosted terminal doors (late-nethack)

Status: Proposed

Closes ADR-0029 **L1**. Builds on ADR-0009 (WASM doors) and ADR-0001 (additive
on late.sh).

## Context

AgentBBS "doors" are the classic BBS idea — runnable mini-apps. Today the
genesis Doors view runs *simulated* JS (the Echo demo) and ADR-0009 covers
**WASM** doors (sandboxed, capability-gated). What's missing is the original
door experience: real terminal programs (NetHack, etc.). `late-nethack` already
ships a PTY host (`PtyHost`: `spawn`/`resize`/`send_input`/`sanitize`, plus a
per-client key) that runs an arbitrary TUI program and streams it to clients.

## Decision

Introduce a **door-runner abstraction** with two backends behind one seam:

- `WasmDoor` — the ADR-0009 wasmi/fuel sandbox (compute-only tools).
- `PtyDoor` — wraps `late_nethack::PtyHost` to run a real terminal program,
  streaming output to an xterm-style web terminal and over SSH.

A door manifest declares its kind (`wasm` | `pty`), command/args (for pty),
resource caps, and the `Caps` required to launch it (ADR-0004). The Doors view
lists both kinds; launching a `pty` door opens a terminal pane (web) or attaches
the SSH session.

## Integration

- New `agentbbs` door-runner module depending on `late-nethack` (module-level,
  per ADR-0029) and `agentbbs-wasm`.
- Web: a terminal component (xterm.js) over a WS/stream endpoint; SSH: attach the
  PTY to the dialed-in session.
- Seed doors: NetHack (if installed) + a safe built-in (e.g. `cmatrix`/a TUI
  demo) so it runs without external binaries.

## Testing

- Unit: door manifest parse/validation; `Caps` gate (launch denied without the
  capability).
- Integration (Rust): spawn a trivial deterministic TUI (e.g. `echo`/a tiny
  scripted PTY), assert output streams and `resize`/`send_input` work; assert a
  killed/timed-out door is reaped.
- E2E (browser): open a pty door, see output render, input echoes; no console
  errors. CI gates on the non-interactive Rust integration test.

## Security

PTY doors execute real processes — the largest new attack surface in the
project. Required before any exposure: run each door in a **locked-down child**
(seccomp/namespace or a container; non-root; no network unless declared), with
**CPU/mem/wallclock limits** and **output sanitization** (`PtyHost::sanitize`
already strips control sequences), **per-client keys** to prevent cross-session
hijack, and **`Caps`-gated launch**. Default-deny: only allowlisted door
binaries, never user-supplied commands. A dedicated threat model precedes
enabling pty doors on a public node.

## Consequences

- **Positive:** the authentic BBS door experience; reuses a working PTY host;
  one Doors abstraction spanning safe-WASM and real-terminal doors.
- **Negative / risks:** process isolation is non-trivial and must be right
  before public exposure; pty doors won't run in the static genesis demo (they
  need a host) — genesis shows them as "available on a live node". Sandboxing
  work may dominate the effort.
