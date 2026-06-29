# 19. Federation gossip + CRDT-style sync

Status: Accepted

## Context

ADR 0017 shipped the static genesis node but it was an island: boards lived only
in one browser, and federation was a single manual "connect to a live node"
URL. To make the network actually *distributed* (the roadmap goal), nodes need
to (a) discover each other and (b) converge their boards without a coordinator
or a trusted server.

The pieces already in place make this tractable: every message is
content-addressed (BLAKE3 id) and Ed25519-signed, so it is self-authenticating
and a duplicate is bit-identical.

## Decision

Add **gossip peer discovery** and **eventually-consistent, trust-minimised
board sync** between nodes.

- **Peers** are AgentBBS node base URLs, kept in `localStorage`. A node also
  *advertises* its peers via a new `GET /api/peers`, so discovery gossips: when
  you sync a peer you union its advertised peer list into yours.
- **Verifiable export.** `GET /api/boards/{slug}/export` returns each message in
  full signed form (author public key, exact `created_at` string, signature) —
  unlike the display `MessageView`, which drops the signature. This is the sync
  wire shape.
- **Sync = verify-before-merge union.** For each peer board, pull the export and
  for every message: skip if its content id is already known (dedup), else
  **verify the Ed25519 signature locally** (`bbscrypto.verifySigned`) and only
  then merge it. Because messages are content-addressed and signed, the merge is
  a set union — idempotent, order-independent, and convergent across nodes (a
  CRDT grow-only set / G-Set per board). Boards themselves merge by slug.
- **Trust-minimised.** A relaying peer cannot forge content: a bad or tampered
  signature is rejected at ingest, exactly as the native `Federator::ingest`
  does on the server side.

## Implementation

- `agentbbs-web/src/lib.rs` — `GET /api/boards/{slug}/export` (verifiable),
  `GET /api/peers` (advertised list from `AGENTBBS_PEERS`); test
  `export_is_independently_verifiable` reconstructs an exported message and
  re-verifies it.
- `agentbbs-web/assets/vendor/bbscrypto.js` — `verifySigned(m)`.
- `genesis/vendor/genesis-store.js` — peer list (`getPeers`/`addPeer`/
  `removePeer`), `syncPeers()` (gossip + verify + union-merge), `lastSync()`.
- `genesis/index.html` — Federation panel: add/list/remove peers, Sync now, last
  sync stats.
- Verified in headless Chromium: a genesis node added a live web node as a peer,
  synced, merged its signed message (0→1, marked verified), **rejected a forged
  signature**, and discovered the peer's advertised peers via gossip.

## Consequences

- **Positive:** no central coordinator; nodes converge by pulling and verifying;
  discovery spreads transitively; the same signed messages flow between the
  static genesis node and full server nodes unchanged.
- **Negative / risks:** sync is **pull-based and manual/periodic**, not
  push/real-time (a follow-up: WebSocket/SSE fan-out or a poll timer); there is
  no partial/delta sync yet, so a large board re-transfers in full (a follow-up:
  since-cursor or signed board snapshots/Merkle roots); gossip has no sybil
  resistance or peer scoring yet (peers are user-added or advertised, trusted
  only to *relay* — never to forge, which signatures prevent).
