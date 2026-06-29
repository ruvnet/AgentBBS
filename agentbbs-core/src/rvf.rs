//! RVF — a minimal, clean-room implementation of a RuVector-style vector
//! memory format for AgentBBS agent memory and semantic board search.
//!
//! This is *not* a port of the `ruvnet/ruvector` crate; it is an
//! independent, self-contained store that reads and writes a documented
//! `.rvf` binary layout and supports cosine nearest-neighbour search. It
//! interoperates at the concept level (vectors + metadata + cosine search)
//! and is intended to be swappable for the full RuVector engine via the
//! `agentbbs-federation` AgentDB adapter when that engine is present.
//!
//! ## File layout (`agentbbs.rvf.v1`)
//!
//! ```text
//! magic:   8 bytes  = b"AGBBSRVF"
//! version: u16 LE   = 1
//! dim:     u32 LE   = vector dimensionality
//! count:   u32 LE   = number of records
//! records: count × {
//!     id_len:  u16 LE
//!     id:      id_len bytes (utf-8)
//!     meta_len:u32 LE
//!     meta:    meta_len bytes (utf-8 json)
//!     vector:  dim × f32 LE
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const MAGIC: &[u8; 8] = b"AGBBSRVF";
const VERSION: u16 = 1;

/// A single vector record: an id, an embedding, and arbitrary JSON metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Caller-chosen id (often a [`crate::board::MessageId`] or memory key).
    pub id: String,
    /// The embedding vector.
    pub vector: Vec<f32>,
    /// Free-form metadata (kept small).
    pub meta: serde_json::Value,
}

/// A scored search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The matched record's id.
    pub id: String,
    /// Cosine similarity in `[-1, 1]` (higher is closer).
    pub score: f32,
    /// The matched record's metadata.
    pub meta: serde_json::Value,
}

/// An in-memory vector store with a documented on-disk `.rvf` format.
#[derive(Clone, Debug, Default)]
pub struct RvfStore {
    dim: usize,
    records: Vec<Record>,
}

impl RvfStore {
    /// Create an empty store for vectors of dimensionality `dim`.
    pub fn new(dim: usize) -> Self {
        RvfStore {
            dim,
            records: Vec::new(),
        }
    }

    /// Vector dimensionality.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Number of stored records.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Insert or replace a record. Errors if the vector's length disagrees
    /// with the store dimensionality.
    pub fn upsert(&mut self, rec: Record) -> Result<()> {
        if rec.vector.len() != self.dim {
            return Err(Error::malformed(
                "vector",
                format!("expected dim {}, got {}", self.dim, rec.vector.len()),
            ));
        }
        if let Some(existing) = self.records.iter_mut().find(|r| r.id == rec.id) {
            *existing = rec;
        } else {
            self.records.push(rec);
        }
        Ok(())
    }

    /// Cosine `top_k` nearest neighbours to `query`.
    pub fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<Hit>> {
        if query.len() != self.dim {
            return Err(Error::malformed(
                "query",
                format!("expected dim {}, got {}", self.dim, query.len()),
            ));
        }
        let qn = norm(query);
        let mut hits: Vec<Hit> = self
            .records
            .iter()
            .map(|r| Hit {
                id: r.id.clone(),
                score: cosine(query, qn, &r.vector),
                meta: r.meta.clone(),
            })
            .collect();
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(top_k);
        Ok(hits)
    }

    /// Serialize to the `.rvf` byte layout.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.dim as u32).to_le_bytes());
        out.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        for r in &self.records {
            let id = r.id.as_bytes();
            out.extend_from_slice(&(id.len() as u16).to_le_bytes());
            out.extend_from_slice(id);
            let meta = serde_json::to_vec(&r.meta).unwrap_or_else(|_| b"null".to_vec());
            out.extend_from_slice(&(meta.len() as u32).to_le_bytes());
            out.extend_from_slice(&meta);
            for f in &r.vector {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        out
    }

    /// Parse a store from the `.rvf` byte layout.
    pub fn from_bytes(buf: &[u8]) -> Result<Self> {
        let mut c = Cursor::new(buf);
        let magic = c.take(8)?;
        if magic != MAGIC {
            return Err(Error::malformed("rvf", "bad magic"));
        }
        let version = u16::from_le_bytes(c.take(2)?.try_into().unwrap());
        if version != VERSION {
            return Err(Error::malformed("rvf", format!("unsupported version {version}")));
        }
        let dim = u32::from_le_bytes(c.take(4)?.try_into().unwrap()) as usize;
        let count = u32::from_le_bytes(c.take(4)?.try_into().unwrap()) as usize;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let id_len = u16::from_le_bytes(c.take(2)?.try_into().unwrap()) as usize;
            let id = String::from_utf8(c.take(id_len)?.to_vec())
                .map_err(|e| Error::malformed("rvf id", e))?;
            let meta_len = u32::from_le_bytes(c.take(4)?.try_into().unwrap()) as usize;
            let meta: serde_json::Value = serde_json::from_slice(c.take(meta_len)?)?;
            let mut vector = Vec::with_capacity(dim);
            for _ in 0..dim {
                vector.push(f32::from_le_bytes(c.take(4)?.try_into().unwrap()));
            }
            records.push(Record { id, vector, meta });
        }
        Ok(RvfStore { dim, records })
    }
}

/// A tiny bounds-checked byte cursor.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| Error::malformed("rvf", "length overflow"))?;
        if end > self.buf.len() {
            return Err(Error::malformed("rvf", "unexpected end of buffer"));
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn cosine(a: &[f32], a_norm: f32, b: &[f32]) -> f32 {
    let bn = norm(b);
    if a_norm == 0.0 || bn == 0.0 {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    dot / (a_norm * bn)
}

/// A deterministic, dependency-free **feature-hashing** embedding of `text`
/// into a `dim`-vector: tokens are hashed to coordinates with a signed bucket,
/// then L2-normalised. It is not a learned model, but shared tokens produce
/// high cosine similarity, which is enough to power semantic-ish recall in the
/// demo memory search without pulling in an embedding dependency.
pub fn hashing_embed(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0f32; dim.max(1)];
    let mut token = String::new();
    let flush = |tok: &mut String, v: &mut [f32]| {
        if tok.is_empty() {
            return;
        }
        let h = blake3::hash(tok.as_bytes());
        let b = h.as_bytes();
        let idx = (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) % v.len();
        let sign = if b[4] & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
        tok.clear();
    };
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            token.extend(ch.to_lowercase());
        } else {
            flush(&mut token, &mut v);
        }
    }
    flush(&mut token, &mut v);
    let n = norm(&v);
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}

// ---- HNSW approximate nearest-neighbour index ----

/// Cosine *distance* (lower is closer), used internally by the index.
fn cos_dist(a: &[f32], a_norm: f32, b: &[f32], b_norm: f32) -> f32 {
    if a_norm == 0.0 || b_norm == 0.0 {
        return 1.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    1.0 - dot / (a_norm * b_norm)
}

#[derive(Clone, Copy)]
struct Cand {
    dist: f32,
    node: usize,
}
impl PartialEq for Cand {
    fn eq(&self, o: &Self) -> bool {
        self.dist == o.dist && self.node == o.node
    }
}
impl Eq for Cand {}
impl Ord for Cand {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.dist.total_cmp(&o.dist).then(self.node.cmp(&o.node))
    }
}
impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// A Hierarchical Navigable Small World index over an [`RvfStore`]'s vectors —
/// approximate nearest-neighbour search that scales far better than the exact
/// brute-force [`RvfStore::search`] (which remains the ground-truth fallback).
///
/// The index is deterministic: a node's layer is derived from a hash of its id,
/// so builds are reproducible (important for tests and federation).
pub struct HnswIndex {
    dim: usize,
    m: usize,
    ef_construction: usize,
    ids: Vec<String>,
    vectors: Vec<Vec<f32>>,
    norms: Vec<f32>,
    meta: Vec<serde_json::Value>,
    /// `neighbors[node][layer]` — adjacency per layer.
    neighbors: Vec<Vec<Vec<usize>>>,
    entry: Option<usize>,
    max_level: usize,
    ml: f32,
}

impl HnswIndex {
    /// Vector dimensionality.
    pub fn dim(&self) -> usize {
        self.dim
    }
    /// Number of indexed vectors.
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Deterministic layer for a node id (geometric distribution via `ml`).
    fn level_for(id: &str, ml: f32) -> usize {
        let b = blake3::hash(id.as_bytes());
        let raw = u64::from_le_bytes(b.as_bytes()[..8].try_into().unwrap());
        // u in (0, 1]
        let u = ((raw >> 11) as f64 / (1u64 << 53) as f64).max(f64::MIN_POSITIVE);
        (-(u.ln()) * ml as f64).floor() as usize
    }

    fn dist(&self, node: usize, q: &[f32], q_norm: f32) -> f32 {
        cos_dist(&self.vectors[node], self.norms[node], q, q_norm)
    }

    /// Greedy best-first search within one layer; returns up to `ef` closest
    /// nodes to `q`, sorted nearest-first.
    fn search_layer(&self, q: &[f32], q_norm: f32, entry_points: &[usize], ef: usize, layer: usize) -> Vec<Cand> {
        use std::collections::BinaryHeap;
        let mut visited = std::collections::HashSet::new();
        // candidates: min-heap by dist (explore nearest first)
        let mut candidates: BinaryHeap<std::cmp::Reverse<Cand>> = BinaryHeap::new();
        // results: max-heap by dist (so we can pop the farthest when over ef)
        let mut results: BinaryHeap<Cand> = BinaryHeap::new();
        for &ep in entry_points {
            let d = self.dist(ep, q, q_norm);
            visited.insert(ep);
            candidates.push(std::cmp::Reverse(Cand { dist: d, node: ep }));
            results.push(Cand { dist: d, node: ep });
        }
        while let Some(std::cmp::Reverse(c)) = candidates.pop() {
            let worst = results.peek().map(|r| r.dist).unwrap_or(f32::INFINITY);
            if c.dist > worst && results.len() >= ef {
                break;
            }
            let neigh = &self.neighbors[c.node];
            if let Some(adj) = neigh.get(layer) {
                for &n in adj {
                    if visited.insert(n) {
                        let d = self.dist(n, q, q_norm);
                        let worst = results.peek().map(|r| r.dist).unwrap_or(f32::INFINITY);
                        if d < worst || results.len() < ef {
                            candidates.push(std::cmp::Reverse(Cand { dist: d, node: n }));
                            results.push(Cand { dist: d, node: n });
                            if results.len() > ef {
                                results.pop();
                            }
                        }
                    }
                }
            }
        }
        let mut out: Vec<Cand> = results.into_sorted_vec();
        out.truncate(ef);
        out
    }

    fn connect(&mut self, node: usize, layer: usize, mut selected: Vec<usize>) {
        let max = if layer == 0 { self.m * 2 } else { self.m };
        selected.retain(|&n| n != node);
        self.neighbors[node][layer] = selected.clone();
        self.neighbors[node][layer].truncate(max);
        for &n in &selected {
            let adj = &mut self.neighbors[n][layer];
            if !adj.contains(&node) {
                adj.push(node);
            }
            if adj.len() > max {
                // Prune to the `max` closest to n.
                let nv = self.vectors[n].clone();
                let nn = self.norms[n];
                let mut scored: Vec<(f32, usize)> =
                    adj.iter().map(|&x| (cos_dist(&self.vectors[x], self.norms[x], &nv, nn), x)).collect();
                scored.sort_by(|a, b| a.0.total_cmp(&b.0));
                *adj = scored.into_iter().take(max).map(|(_, x)| x).collect();
            }
        }
    }

    /// Approximate `top_k` nearest neighbours to `query`, scored by cosine
    /// similarity (higher is closer), matching [`RvfStore::search`]'s output.
    pub fn search(&self, query: &[f32], top_k: usize, ef: usize) -> Result<Vec<Hit>> {
        if query.len() != self.dim {
            return Err(Error::malformed(
                "query",
                format!("expected dim {}, got {}", self.dim, query.len()),
            ));
        }
        let Some(entry) = self.entry else {
            return Ok(vec![]);
        };
        let q_norm = norm(query);
        let mut ep = entry;
        for layer in (1..=self.max_level).rev() {
            let r = self.search_layer(query, q_norm, &[ep], 1, layer);
            if let Some(best) = r.first() {
                ep = best.node;
            }
        }
        let found = self.search_layer(query, q_norm, &[ep], ef.max(top_k), 0);
        Ok(found
            .into_iter()
            .take(top_k)
            .map(|c| Hit {
                id: self.ids[c.node].clone(),
                score: 1.0 - c.dist, // cosine similarity
                meta: self.meta[c.node].clone(),
            })
            .collect())
    }
}

impl RvfStore {
    /// Build an [`HnswIndex`] over this store's vectors. `m` is the neighbour
    /// degree (typical 8–16) and `ef_construction` the build-time beam width
    /// (typical 64–200).
    pub fn build_hnsw(&self, m: usize, ef_construction: usize) -> HnswIndex {
        let m = m.max(2);
        let ml = 1.0 / (m as f32).ln();
        let mut index = HnswIndex {
            dim: self.dim,
            m,
            ef_construction: ef_construction.max(m),
            ids: Vec::new(),
            vectors: Vec::new(),
            norms: Vec::new(),
            meta: Vec::new(),
            neighbors: Vec::new(),
            entry: None,
            max_level: 0,
            ml,
        };
        for rec in &self.records {
            index.insert(rec);
        }
        index
    }
}

impl HnswIndex {
    fn insert(&mut self, rec: &Record) {
        let node = self.ids.len();
        let level = Self::level_for(&rec.id, self.ml);
        self.ids.push(rec.id.clone());
        self.vectors.push(rec.vector.clone());
        self.norms.push(norm(&rec.vector));
        self.meta.push(rec.meta.clone());
        self.neighbors.push(vec![Vec::new(); level + 1]);

        let Some(entry) = self.entry else {
            self.entry = Some(node);
            self.max_level = level;
            return;
        };

        let qv = self.vectors[node].clone();
        let qn = self.norms[node];
        let mut ep = entry;
        // Descend from the top to just above the node's level.
        for layer in ((level + 1)..=self.max_level).rev() {
            let r = self.search_layer(&qv, qn, &[ep], 1, layer);
            if let Some(best) = r.first() {
                ep = best.node;
            }
        }
        // Connect at each layer from min(level, max_level) down to 0.
        let start = level.min(self.max_level);
        let mut eps = vec![ep];
        for layer in (0..=start).rev() {
            let found = self.search_layer(&qv, qn, &eps, self.ef_construction, layer);
            let selected: Vec<usize> = found.iter().take(self.m).map(|c| c.node).collect();
            self.connect(node, layer, selected);
            eps = found.into_iter().map(|c| c.node).collect();
            if eps.is_empty() {
                eps = vec![ep];
            }
        }
        if level > self.max_level {
            self.max_level = level;
            self.entry = Some(node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, v: Vec<f32>) -> Record {
        Record {
            id: id.into(),
            vector: v,
            meta: serde_json::json!({ "k": id }),
        }
    }

    #[test]
    fn search_ranks_by_cosine() {
        let mut s = RvfStore::new(3);
        s.upsert(rec("a", vec![1.0, 0.0, 0.0])).unwrap();
        s.upsert(rec("b", vec![0.0, 1.0, 0.0])).unwrap();
        s.upsert(rec("c", vec![0.9, 0.1, 0.0])).unwrap();
        let hits = s.search(&[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(hits[0].id, "a");
        assert_eq!(hits[1].id, "c");
    }

    #[test]
    fn upsert_replaces() {
        let mut s = RvfStore::new(2);
        s.upsert(rec("x", vec![1.0, 0.0])).unwrap();
        s.upsert(rec("x", vec![0.0, 1.0])).unwrap();
        assert_eq!(s.len(), 1);
        let hits = s.search(&[0.0, 1.0], 1).unwrap();
        assert!(hits[0].score > 0.99);
    }

    #[test]
    fn dim_mismatch_errors() {
        let mut s = RvfStore::new(3);
        assert!(s.upsert(rec("x", vec![1.0, 0.0])).is_err());
        assert!(s.search(&[1.0, 0.0], 1).is_err());
    }

    #[test]
    fn bytes_roundtrip() {
        let mut s = RvfStore::new(4);
        s.upsert(rec("a", vec![1.0, 2.0, 3.0, 4.0])).unwrap();
        s.upsert(rec("b", vec![-1.0, 0.5, 0.0, 2.0])).unwrap();
        let bytes = s.to_bytes();
        let back = RvfStore::from_bytes(&bytes).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.dim(), 4);
        let hits = back.search(&[1.0, 2.0, 3.0, 4.0], 1).unwrap();
        assert_eq!(hits[0].id, "a");
    }

    #[test]
    fn truncated_buffer_errors() {
        let mut s = RvfStore::new(4);
        s.upsert(rec("a", vec![1.0, 2.0, 3.0, 4.0])).unwrap();
        let bytes = s.to_bytes();
        assert!(RvfStore::from_bytes(&bytes[..bytes.len() - 3]).is_err());
    }

    // Deterministic pseudo-random vectors (no rng dep): hash(i,d) -> f32.
    fn synth_vec(i: usize, dim: usize) -> Vec<f32> {
        (0..dim)
            .map(|d| {
                let h = blake3::hash(format!("{i}:{d}").as_bytes());
                let b = h.as_bytes();
                (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / u32::MAX as f32) - 0.5
            })
            .collect()
    }

    #[test]
    fn hnsw_recall_matches_brute_force() {
        let dim = 24;
        let n = 600;
        let mut store = RvfStore::new(dim);
        for i in 0..n {
            store.upsert(rec(&format!("v{i}"), synth_vec(i, dim))).unwrap();
        }
        let index = store.build_hnsw(12, 100);
        assert_eq!(index.len(), n);

        let k = 10;
        let queries = 25;
        let mut hits = 0usize;
        let mut total = 0usize;
        for q in 0..queries {
            let query = synth_vec(10_000 + q, dim);
            let exact: std::collections::HashSet<String> = store
                .search(&query, k)
                .unwrap()
                .into_iter()
                .map(|h| h.id)
                .collect();
            let approx = index.search(&query, k, 64).unwrap();
            for h in &approx {
                if exact.contains(&h.id) {
                    hits += 1;
                }
            }
            total += k;
        }
        let recall = hits as f32 / total as f32;
        assert!(recall >= 0.85, "HNSW recall@{k} too low: {recall}");
    }

    #[test]
    fn hnsw_search_dim_mismatch_errors() {
        let mut store = RvfStore::new(4);
        store.upsert(rec("a", vec![1.0, 0.0, 0.0, 0.0])).unwrap();
        let index = store.build_hnsw(8, 32);
        assert!(index.search(&[1.0, 0.0], 1, 16).is_err());
    }

    #[test]
    fn hashing_embed_shares_tokens() {
        let dim = 64;
        let a = hashing_embed("schedule a dinner meeting", dim);
        let b = hashing_embed("dinner schedule please", dim);
        let c = hashing_embed("benchmark cve exploit sandbox", dim);
        let an = norm(&a);
        // Same shared tokens -> high cosine; unrelated -> low.
        assert!(cosine(&a, an, &b) > cosine(&a, an, &c));
        assert!((norm(&a) - 1.0).abs() < 1e-5); // normalized
    }
}
