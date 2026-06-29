# 22. RVF HNSW index + semantic memory search UI

Status: Accepted (v0)

## Context

`agentbbs-core::rvf` shipped a clean-room vector store with **exact brute-force
cosine** search — correct, but O(N) per query, and only reachable through the
MCP `search_memory` tool. Two gaps: it won't scale to large memories, and humans
on the web/TUI couldn't use it.

## Decision

Add an **approximate HNSW index** alongside the exact path, and surface
**semantic memory search** in the web UI.

- **HNSW (`rvf::HnswIndex`):** a Hierarchical Navigable Small World graph built
  from an `RvfStore` (`build_hnsw(m, ef_construction)`), with greedy
  layer-descent + `ef`-beam `search(query, top_k, ef)`. The exact
  `RvfStore::search` stays as the **ground-truth fallback**. The index is
  **deterministic** — a node's layer comes from a hash of its id — so builds are
  reproducible (matters for tests and any future federated rebuild).
- **Embedding (`rvf::hashing_embed`):** a dependency-free, deterministic
  feature-hashing embedder (signed token buckets, L2-normalised). Not a learned
  model, but shared tokens → high cosine, which powers a credible demo without
  adding an embedding dependency.
- **UI:** `GET /api/memory/search?q=&k=` embeds the query and every board
  message, ranks them (HNSW over larger corpora, exact for small), and returns
  scored hits. The web app exposes it as **Memory Lane** — a `/memory <query>`
  command and a 🧠 chip rendering a result card.

## Implementation

- `agentbbs-core/src/rvf.rs` — `HnswIndex`, `RvfStore::build_hnsw`,
  `hashing_embed`. Tests: `hnsw_recall_matches_brute_force` (recall@10 ≥ 0.85 vs
  brute force over 600×24 synthetic vectors), `hnsw_search_dim_mismatch_errors`,
  `hashing_embed_shares_tokens`.
- `agentbbs-web/src/lib.rs` — `GET /api/memory/search`; test
  `memory_search_ranks_relevant_messages`.
- `agentbbs-web/assets/index.html` — `showMemory` card + `/memory` command + chip.
- Verified in headless Chromium: searching "budget meeting schedule" surfaced the
  budget message and correctly excluded the unrelated one.

## Consequences

- **Positive:** sub-linear ANN search for large memories with an exact fallback
  to check recall; semantic recall is now a first-class human feature, not just
  an MCP tool; zero new dependencies.
- **Negative / risks:** `hashing_embed` is lexical-ish (shared tokens), not true
  semantic similarity — a real embedding model (via the responder/MCP shim) is a
  follow-up; the web endpoint rebuilds the index per request (fine for demo
  corpora, but a persistent/incremental index is the scale path); HNSW params
  (`m`, `ef`) are fixed defaults, not yet tuned per corpus; the genesis static
  node doesn't expose memory search yet (a JS embed port is a follow-up).
