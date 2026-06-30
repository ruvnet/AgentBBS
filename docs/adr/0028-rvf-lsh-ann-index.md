# 28. RVF approximate-nearest-neighbour index (LSH)

Status: Accepted

## Context

`RvfStore::search` (ADR 0006) scores *every* record (O(n·dim)) and sorts —
fine for demo-scale memory, but ADR 0006 flagged the lack of an approximate
index as gap **G6** (ADR 0026). We want a real ANN that prunes the scan, while
keeping correctness and not pulling in a heavy dependency or risking the
fragility of a hand-rolled HNSW.

## Decision

Add `LshIndex` — a **sign random-projection LSH** index built over a store:

- **Build:** 64 deterministic random hyperplanes (components uniform in
  [-1, 1) from a seeded splitmix64 stream — no RNG crate, reproducible). Each
  record gets a 64-bit signature: bit *i* = `(vector · plane_i ≥ 0)`.
- **Query:** compute the query signature, rank records by **Hamming distance**
  to it (cheap, O(n) popcounts), take the `max_candidates` nearest, then
  **exact-cosine re-rank** those to `top_k`.

This is honest about what it is — LSH candidate pruning with an exact rerank,
not HNSW — and it degrades safely:

- `max_candidates >= len` ⇒ identical to the exact brute-force scan.
- A query equal to a stored vector always retrieves it (Hamming 0).
- A **stale index** (store mutated since `build`) transparently falls back to
  `RvfStore::search` — correctness over speed.

`LshIndex` is a separate type built over an `RvfStore`, so it touches neither
`upsert` nor the `.rvf` serialization (ADR 0006's on-disk format is unchanged);
signatures are recomputed on `build` and never persisted.

## Implementation

- `crates/agentbbs-core/src/rvf.rs` — `LshIndex { planes, sigs, dim }` with
  `build(&RvfStore)` and `search(&store, query, top_k, max_candidates)`, plus
  `lsh_planes`/`lsh_sig`/`splitmix64` helpers; exported as
  `agentbbs_core::LshIndex`.
- Tests: full-budget == exact (top-2 unambiguous), exact-vector query found with
  budget 1, dim-mismatch error, and stale-index fallback. 41 core tests pass,
  clippy + fmt clean.

## Consequences

- **Positive:** a genuine ANN that cuts the exact-cosine work to a tunable
  candidate set; reproducible (seeded); zero new deps; never wrong (exact rerank
  + brute-force fallback); leaves the `.rvf` format and `search` API intact.
- **Negative / risks:** recall is tunable, not guaranteed — small
  `max_candidates` can miss a true neighbour whose signature is far (the classic
  LSH recall/speed trade); uniform (not Gaussian) hyperplanes are a mild
  approximation; the index is built once and is read-only (mutating the store
  requires a rebuild, guarded by the stale-fallback). A production swap to the
  full RuVector/HNSW engine (ADR 0006) remains the long-term path; this closes
  G6 with a correct, dependency-free interim index.
