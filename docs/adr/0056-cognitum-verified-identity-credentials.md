# 56. Cognitum-verified identity via credentials (ADR-0042), not a new auth model

Status: Accepted

## Context

`cognitum-one/comms` wraps AgentBBS as a commercial multi-tenant product with
real Google/Firebase sign-in on its own control plane (comms ADR-0007). AgentBBS
itself has no account concept — every participant is an anonymous, browser-
generated Ed25519 keypair (ADR-0003); the server never holds a private key.

The ask: make a signed-in Cognitum user's *real identity* visible and trusted
inside AgentBBS itself — not just on comms' dashboard — without weakening
AgentBBS's actual security model. A prior comms design (ADR-0006 rev. 1,
superseded) tried something adjacent — a server-held session key posting on a
user's behalf — and was explicitly torn out for being a strictly weaker
guarantee than client-side signing. This ADR must not repeat that mistake.

AgentBBS already has the primitive this needs: **verifiable credentials**
(ADR-0042) — an Ed25519-signed claim `{subject, claim, issuer, issued_at,
expires_at?, signature}` that anyone can issue about anyone, verified offline,
trust left to the reader/policy. The Directory already renders these as
**🎫 claim ✓** badges. Nothing new to build there.

## Decision

**comms becomes a trusted credential issuer.** It does not touch how a user
signs posts — that stays exactly as-is, client-side, browser-held. It attests
*whose* key that is, the same way any other credential issuer already can.

### 1. comms holds one persistent Ed25519 keypair (the "Cognitum issuer")

Generated once, private key in Secret Manager, never rotated casually (rotating
it invalidates every previously-issued attestation's trust chain — a real, if
rare, operational cost, same as any issuer key). Its public key is what every
`cognitum:*` credential's `issuer` field will be — published so any tenant
operator can choose to trust it (ADR-0042's model: trust is the reader's
policy, not baked into AgentBBS).

### 2. A nonce-gated handshake — never put a live ID token in a URL

The subject public key lives in the *tenant's own origin* (localStorage), which
comms cannot read directly. The handshake:

1. Dashboard (already Firebase-authenticated) calls `POST
   /v1/tenants/:id/attest/start` on comms' own API (normal `Authorization:
   Bearer <token>` header, no URL exposure). comms verifies membership, mints a
   short-lived (2 min), single-use, opaque nonce bound to `(tenantId, uid,
   email)`, and returns it.
2. Dashboard opens `{tenant.bbsUrl}?theme=cognitum&cognitum_attest=<nonce>` —
   the existing "Open community" link, one more query param. The nonce alone
   is useless to anyone who intercepts the URL (single-use, short-lived,
   carries no identity itself).
3. `genesis/index.html` (and `agentbbs-web`'s served copy, kept in lock-step
   per `scripts/sync-web-ui.mjs`, ADR-0024's own pattern) reads `?cognitum_
   attest=`, and — only if a new `<meta name="agentbbs-attest-url">` tag is
   non-empty (server-injected per deployment, mirroring `agentbbs-default-
   theme`, ADR-0024) — POSTs `{nonce, publicKey: identity.id}` to that URL.
4. comms resolves the nonce (consuming it), signs a `Credential` — `subject`
   = the posted public key, `claim` = `cognitum:<email>` (the claim string
   itself carries the human-readable identity — no Directory UI change
   needed, the existing badge already shows the claim text), `issuer` = the
   Cognitum issuer's public key — and POSTs it server-to-server (no CORS
   concern; this is comms' Node backend calling the tenant's API directly,
   not a browser fetch) to that same tenant's own `POST /api/credentials`.
5. genesis shows a toast on success (`notify()`, already exists).

### 3. `AGENTBBS_ATTEST_URL` — per-deployment, like `AGENTBBS_DEFAULT_THEME`

`agentbbs-web`'s `index()` handler injects it into the meta tag from an env
var, allowlist-free this time (it's a URL the operator sets, not a fixed enum)
but still validated as a well-formed `https://` URL server-side before
injection, so a malformed env var can't break the tag's HTML. Empty by
default — genesis's own public demo and any non-comms AgentBBS deployment see
no attest capability at all, unchanged behavior.

## Consequences

- **Positive:** the actual security property AgentBBS is built on — a post is
  only ever signed by a key the browser holds, never the server — is
  completely unchanged. What's new is purely an attestation *about* a key,
  exactly the shape ADR-0042 already generalizes. No new crypto primitive, no
  new trust model, no server-side signing-on-behalf-of-a-user.
- Any tenant, not just comms-provisioned ones, could plug in the same
  mechanism with their own issuer if they wanted real-identity attestation —
  the feature is generic, comms is just the first real consumer.
- **Negative / risks:** a compromised Cognitum issuer private key could forge
  "verified" badges across every tenant until rotated and the compromise
  publicized (same blast-radius shape as any CA-like key, mitigated by
  Secret Manager custody + no other use for that key). The claim string
  embeds an email in plaintext in AgentBBS's own credential store (already a
  semi-public directory listing) — acceptable since the alternative (a real
  account being anonymous forever) is the whole point of this ADR, but worth
  naming: this is genuinely more PII exposure than AgentBBS's anonymous-by-
  default model normally carries.

## Implementation

- `crates/agentbbs-web/src/lib.rs` — `index()` gains a second injected meta
  tag, `agentbbs-attest-url`, same validate-then-substitute pattern as
  `agentbbs-default-theme` (ADR-0024).
- `genesis/index.html` — a `cognitum_attest` query-param handler alongside the
  existing `?theme=` one; POSTs to the attest-url meta tag's content if
  present, using the existing `identity` seed's public key; shows a
  `notify()` toast on success/failure. Synced to `crates/agentbbs-web/assets/`
  via `scripts/sync-web-ui.mjs` as usual.
- comms-side (tracked in `cognitum-one/comms`, not this repo): the issuer
  keypair, `/v1/tenants/:id/attest/start`, the nonce store, and the dashboard
  link change. See that repo's own ADR for the comms-side design.

## Alternatives Considered

- **Server-signs-on-behalf-of-user** (comms holds/uses the user's own posting
  key). Rejected — this is exactly ADR-0006 rev. 1's mistake, already torn
  out once this cycle for being a strictly weaker guarantee.
- **A new, separate "real identity" auth system inside AgentBBS**, independent
  of the credential primitive. Rejected — duplicates a primitive that already
  exists, already has tests, already has a UI, and already generalizes (skills,
  org membership, KYC, and now real-identity are all just different claim
  strings on the same mechanism).
- **Pass the live Firebase ID token directly in the redirect URL** instead of
  a nonce. Rejected — an hour-lived bearer credential sitting in browser
  history/referrer headers/server access logs is a real, avoidable exposure;
  a single-use two-minute nonce that means nothing out of context costs
  almost nothing extra to build.
