# 0056. Slack "Cognitum One Agent" ↔ GCP Cognitum services — MCP connector now, bot backend phased

Status: Accepted
Updated: 2026-08-17 — Surface A applied (MCP connector `Cognitum`→`mcpsse`, No
Auth, registered + app installed/reinstalled with `mcp:connect`). Surface B (the
bot backend) is no longer hypothetical: it is built and deployed as its own repo
**`cognitum-one/slack`** (Rust Socket-Mode worker on Cloud Run
`cognitum-slack`), so the Context line "no Slack bot backend exists anywhere in
the project" is now historical — it was true at authoring, superseded by that
repo. See `cognitum-one/slack` ADR-0001..0004.

## Context

The **Cognitum One Agent** Slack app (App ID `A0BQP38AY22`, workspace Cognitum
`T0BQ9JEQKUP`) exists as a manifest checked into this repo at
`cognitumoneagentmanifest.yaml`: socket-mode, an `assistant_view` AI-app
surface, a `/cognitum` slash command, and bot events (`app_mention`,
`message.im`, `assistant_thread_started`, …). Slack's app settings now offer an
**MCP Servers** page ("Add MCP Server": Name / URL / Auth type) — the Slackbot
MCP client discovers a remote MCP server's tools and invokes them from
conversation. The question this ADR answers: how does this Slack app connect to
the GCP Cognitum services, and what has to exist where?

A live survey of GCP project `cognitum-20260110` (2026-08-17, `gcloud` as
`ruv@ruv.net`) established the ground truth:

- **`mcpsse`** (Cloud Run, `https://mcpsse-63rzcdswba-uc.a.run.app`, invoker =
  `allUsers`) is a **live Cognitum MCP server v1.1.0**. Verified by probe: a
  Streamable-HTTP `initialize` POST returns a JSON-RPC result with
  `content-type: application/json` (protocol `2024-11-05`), and `tools/list`
  returns 8 tools: `health_check`, `catalog_browse`, `lead_subscribe`,
  `order_status`, `contact_send`, `security_verify`, `fleet_status`,
  `docs_search`, plus 14 KB resources. Public tools need no auth; the
  protected pair (`order_status`, `fleet_status`) needs an `X-API-Key` header.
- **`https://cognitum.one/mcp` is NOT an MCP endpoint.** It is the Firebase
  Hosting SPA catch-all and returns the website HTML (GET 200 `text/html`,
  POST 405). Verified by probe; do not register it in Slack.
- **`apimcptools`** (Cloud Run) is IAM-locked — invoker only
  `apigateway-sa@cognitum-20260110.iam.gserviceaccount.com`. Slack cannot
  present GCP IAM credentials, so its tools are unreachable from Slack without
  a proxy route or app-layer auth in front.
- **No Slack bot backend exists anywhere in the project**: Secret Manager has
  no Slack secrets (no bot token, no signing secret, no app token), and no
  service handles Slack events. The manifest's socket-mode/events surface is
  currently unserved.

Slack's MCP client constraints (docs.slack.dev, "Slackbot MCP Client"):
transport is **Streamable HTTP**; connecting a server adds the `mcp:connect`
bot scope; tools must respond within **60 seconds**; and exactly four auth
types exist — **No auth**, **Slack identity auth**, **Dynamic Client
Registration (OAuth)**, **Manual OAuth**. There is **no plain bearer/API-key
option**, so `mcpsse`'s `X-API-Key` protected tools cannot be unlocked by the
connector as-is.

Relationship to the existing AgentBBS Slack bridge (ADR-0025): `agentbbs-web`
already serves `POST /api/bridge/slack/events` (`slack_bridge.rs` —
signing-secret HMAC verify, `channel→board` map, bridge-subkey signing). That
is a **different integration** — it mirrors Slack channel traffic into AgentBBS
boards. Cognitum One Agent is the Slack front end for the Cognitum meta-LLM.
One Slack app could eventually serve both, but both would contend for the same
app-level event-subscription config (one request URL, one event set), so this
ADR keeps them separate apps until a deliberate merge decision.

## Decision

Integrate in two surfaces, only the first of which is buildable today with
zero new code:

**Surface A — MCP connector (now, config-only).** Register the existing
`mcpsse` service in the Slack app's MCP Servers dialog:

- Name: `Cognitum`
- URL: `https://mcpsse-63rzcdswba-uc.a.run.app`
- Auth type: **No auth** (`auth_type: "no_auth"`)

This exposes the 6 public tools + docs/resources to Slackbot conversations.
App Settings auto-adds `mcp:connect` and the MCP block to the hosted manifest;
after saving, re-export the manifest and update
`cognitumoneagentmanifest.yaml` so the repo copy stays the source of truth.
Verification = the dialog's Save succeeding, then the MCP Servers **Logs** tab
showing tool discovery/calls.

**Surface B — bot backend for mentions/DMs/`/cognitum` (phased).** The
manifest's conversational surface needs a backend that does not exist yet.
Recommended shape: **HTTP events mode on Cloud Run** — flip
`socket_mode_enabled: false`, set `settings.event_subscriptions.request_url`,
`settings.interactivity.request_url`, and `slash_commands[].url`, and verify
requests with the signing secret (the exact pattern already proven pure and
unit-tested in `crates/agentbbs-web/src/slack_bridge.rs::verify_signature`).
HTTP mode fits Cloud Run scale-to-zero; socket mode would instead require a
long-lived WebSocket worker (`min-instances=1`, an `xapp-` app-level token) and
is only preferable if no public endpoint is wanted. Phases:

- **Phase B0 (decision + secrets):** choose HTTP vs socket; provision
  `SLACK_BOT_TOKEN` (`xoxb-`), `SLACK_SIGNING_SECRET` (+ `SLACK_APP_TOKEN`
  `xapp-` only if socket) as Secret Manager secrets in `cognitum-20260110`.
- **Phase B1 (service):** a Slack events handler that verifies, then routes to
  the meta-LLM (`apicompletions`, already consumed by `agentbbs-web` via
  `AGENTBBS_COGNITUM_KEY`) and posts replies via `chat.postMessage`.
- **Phase B2 (optional):** unlock protected MCP tools for Slack by putting an
  OAuth layer (DCR or manual) in front of `mcpsse`, or a dedicated
  Slack-facing MCP route on `apigateway` that injects the API key server-side.

**Non-goals now:** exposing `apimcptools` to Slack; building OAuth in front of
`mcpsse`; merging Cognitum One Agent with the AgentBBS channel bridge.

## Consequences

- Surface A ships immediately with no deploy, no code, no credentials — but
  only the **public, unauthenticated** tool surface. Anyone in the workspace
  gets the same capability set; nothing account-scoped works until Phase B2.
- `mcpsse` is already `allUsers`-invokable, so Slack adds no new exposure; the
  60-second Slackbot tool timeout bounds long calls (the SSE stream endpoint is
  irrelevant — Slack uses Streamable HTTP POSTs, which the service answers in
  plain JSON).
- If Phase B2 ever stores a Cognitum API key where Slack can send it, that key
  becomes a credential held by a third party — mint a dedicated least-privilege
  key, never reuse `COGNITUM_API_KEY`/`AGENTBBS_COGNITUM_KEY`.
- If the Phase B1 handler lands in the `cognitum-one/api` or `website` repos,
  the known deploy-path gotchas apply (api's CD is green-but-no-op without
  deploy credentials — api#39; website has no CD at all — website#101): merging
  ≠ deployed, validate the live endpoint after every merge.
- Flipping to HTTP events mode changes the manifest (socket fields → request
  URLs); the repo manifest must be updated in the same change or it will
  describe a world that no longer exists.
- Keeping Cognitum One Agent separate from the AgentBBS bridge app costs a
  second Slack app to administer but avoids event-subscription contention and
  keeps blast radii separate.

## Implementation

Surface A can be applied either through the App Settings dialog (checklist
below) or **API-first**: the repo manifest now carries the top-level
`mcp_servers:` block (`cognitum` → `mcpsse` URL, `auth_type: no_auth`) plus the
`mcp:connect` bot scope, so with an app configuration token (`xoxe.xoxp-…`)
one `apps.manifest.validate` + `apps.manifest.update` call applies the whole
setup; `apps.manifest.export` then confirms repo ↔ hosted parity. Config
tokens are generated at api.slack.com/apps ("Your App Configuration Tokens"),
expire after 12 hours, and rotate via `tooling.tokens.rotate`.

Surface A checklist (operator, ~5 minutes):

1. App settings → Features → MCP Servers → Add MCP Server; enter the Name /
   URL / Auth type values above and Save. If the workspace tier rejects the
   save, the failure is visible in the dialog itself.
2. Reinstall the app if prompted (the `mcp:connect` scope addition requires
   it).
3. In a DM to Cognitum One, ask something that should hit `docs_search`; check
   MCP Servers → Logs for the discovery + call records.
4. Re-export the manifest from app settings; overwrite
   `cognitumoneagentmanifest.yaml`; commit.

Phase B0 secret provisioning — **partially done 2026-08-17**: the full app
credential set from the operator's `.env` was created in Secret Manager
(`cognitum-20260110`): `SLACK_SIGNING_SECRET`, `SLACK_CLIENT_SECRET`,
`SLACK_VERIFICATION_TOKEN` (stored at operator request; deprecated by Slack —
new code must verify with the signing secret, never this token),
`SLACK_APP_ID`, `SLACK_CLIENT_ID`, and `SLACK_APP_TOKEN` (`xapp-1-…`,
verified live via `auth.test` + `apps.connections.open` — Socket Mode is
connectable). Outstanding: `SLACK_BOT_TOKEN` — the
`xoxb-` token only exists after the app's Install App step (OAuth &
Permissions page):

```bash
printf '%s' "$XOXB" | gcloud secrets create SLACK_BOT_TOKEN \
  --project=cognitum-20260110 --data-file=-
```

Phases B1/B2 get their own implementation notes when scheduled; this ADR fixes
the shape, not the code.
