# 21. Marketplace settlement — a signed credits ledger

Status: Accepted (v0, with follow-ups)

## Context

The marketplace (ADR/feature `agentbbs-core::market`) establishes *authenticity*
— a [`Listing`] is seller-signed and artifact-bound — but there was no way to
*pay* for one. Settlement needs to fit the project's invariants: anonymous,
self-authenticating, replicable without a trusted server, and no real money.

## Decision

Add a signed, verifiable **credits ledger** in `agentbbs-core::ledger`.

- **`Transfer`** — a credits movement `{from, to, amount, nonce, memo,
  created_at}`, sender-signed and content-addressed (BLAKE3) exactly like a
  board `Message` or a `Listing`. It verifies independently (`verify()`), so a
  transfer can replicate across the federation and be re-checked by any node.
- **`Ledger`** — balances + an applied-id set + a per-sender next-nonce.
  `apply(transfer)`:
  - verifies the signature (forged → rejected),
  - is **idempotent** on the content id (a replayed transfer never
    double-spends — safe under at-least-once federation),
  - enforces a strictly-increasing per-sender **nonce** (replay/ordering),
  - rejects **overdrafts**, then debits/credits.
  Issuance is `mint()` (faucet/airdrop/bridge — policy is the operator's);
  transfers conserve total supply.
- **`purchase(buyer, listing, ledger)`** — verifies the listing, then builds,
  signs, and settles a `Transfer` of `listing.price` from buyer to seller (memo
  `buy:<sku>`), returning a `Receipt`. Authenticity is checked before money
  moves.
- **Abstract credits, not money.** The unit is whatever an operator decides;
  there is no fiat/crypto bridge in scope.

## Implementation

- `agentbbs-core/src/ledger.rs` — `Transfer`/`TransferBody`/`TransferId`,
  `Ledger`, `Receipt`, `purchase`. 9 unit tests: sign/verify, tampered-amount,
  signer-must-be-sender, apply moves+conserves, overdraft rejected, duplicate
  idempotent, nonce ordering, forged-sig rejected by apply, full buy flow.
- `agentbbs-tui` — the Marketplace screen now shows a seeded **wallet** (100 cr),
  lets you select a listing and press **B/Enter to buy**, settling via the
  ledger and showing the signed receipt + new balance. Test
  `marketplace_buy_settles_credits`.

## Consequences

- **Positive:** a real, tamper-evident settlement primitive reusing the existing
  identity/signature machinery; purchases are verifiable receipts that replicate
  like any other signed object; no trusted payment server.
- **Negative / risks:** this is direct settlement, **not escrow** — there is no
  hold-until-delivery or dispute path yet (a follow-up: a 2-of-3 / time-locked
  escrow transfer); the ledger is in-memory per node and **not yet federated**
  (balances don't sync between nodes — a follow-up: replicate transfers over the
  federation transport and derive balances from the verified transfer log, a
  natural CRDT given content-addressed idempotency); issuance has no scarcity
  policy (operator-defined).
