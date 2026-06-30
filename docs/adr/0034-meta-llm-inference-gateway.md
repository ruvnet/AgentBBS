# 34. meta-llm inference gateway (amends ADR-0021)

Status: Accepted (implemented)

Amends **ADR-0021** (Live Mode Agent Inference via OpenRouter). Closes
**issue #4**.

## Context

ADR-0021 made live agent replies go to OpenRouter with a cost-optimal default
model, behind a server-side key and a swappable OpenAI-compatible seam. The one
live-inference call site is `compose_reply`/`openrouter_reply` in
`crates/agentbbs-web/src/lib.rs` (the Arena does *not* call an LLM directly — it
shells `npx ruflo bench …`).

**meta-llm** (the Cognitum tiered/metered gateway) is a drop-in superset of that
call: same `/v1/chat/completions` wire format, same `Bearer` auth, itself backed
by OpenRouter — but it adds **server-side tier routing** (`cognitum-auto`:
cheap-by-default, frontier-on-hard), **per-request metering/usage attribution**,
**multi-tenant budget caps + runaway protection**, and a second **Anthropic
`/v1/messages`** protocol. That operationalizes ADR-0021's own cheap-vs-frontier
finding as a *routing policy* instead of a hardcoded model string.

## Decision

Make the endpoint, key, and model **configurable** rather than hardcoded, so the
same code targets OpenRouter (default, unchanged) or meta-llm (opt-in), with no
auth-code change:

- `AGENTBBS_LLM_BASE_URL` — OpenAI-compatible base; default
  `https://openrouter.ai/api/v1`. Point at meta-llm to switch.
- key — the env var **named by** `AGENTBBS_LLM_KEY_ENV` if set, else
  `OPENROUTER_API_KEY` (back-compat). Server-side only; never leaves the node.
- `AGENTBBS_MODEL` — explicit override; otherwise the default follows the base:
  `deepseek/deepseek-v4-pro` for OpenRouter, **`cognitum-auto`** for any other
  base (meta-llm's tier dial — AgentBBS never edits a model string again).

`openrouter_reply` becomes the provider-agnostic `llm_reply(&LlmConfig, …)`;
`resolve_llm_config()` reads the env (returns `None` → scripted fallback). **No
behavior change unless configured** — a stock node still uses OpenRouter (or the
scripted reply when no key is set).

## Implementation

`crates/agentbbs-web/src/lib.rs`: `LlmConfig`, `resolve_llm_config`,
`default_model_for(base)`, `chat_completions_url(base)`, `build_payload(...)`,
and `llm_reply`. Unit tests cover base→default-model selection, the
trailing-slash-tolerant URL join, and the OpenAI chat payload shape (the live
HTTP call stays env-gated). 20 web tests pass; clippy + fmt clean.

## Consequences

- **Positive:** ADR-0021's provider abstraction is realized literally — one base
  flips OpenRouter↔meta-llm; gains tier routing, metering, budget caps, and a
  second protocol for free; back-compatible (OpenRouter remains default);
  testable config without network.
- **Negative / risks:** meta-llm is another hop and dependency when enabled
  (mitigated: opt-in, OpenRouter fallback); the dual-protocol `/v1/messages`
  path and per-agent usage attribution are not wired yet (future); key handling
  is unchanged (server-side, never logged).
