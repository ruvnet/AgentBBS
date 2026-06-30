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
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
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
            return Err(Error::malformed(
                "rvf",
                format!("unsupported version {version}"),
            ));
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

/// An approximate-nearest-neighbour index over an [`RvfStore`] (G6 / ADR-0028).
///
/// Sign random-projection LSH: each vector gets a 64-bit signature from its sign
/// against 64 deterministic random hyperplanes. A query prunes to the
/// `max_candidates` records with the smallest Hamming distance to the query
/// signature, then those candidates are **exact-cosine re-ranked** — so results
/// are correct for the returned set and degrade gracefully:
/// `max_candidates >= len` is identical to the exact brute-force scan, and a
/// query equal to a stored vector always finds it (Hamming 0). This trades the
/// O(n·dim) scan for O(n) Hamming + O(max_candidates·dim) rerank.
#[derive(Clone, Debug)]
pub struct LshIndex {
    planes: Vec<Vec<f32>>, // BITS hyperplanes, each `dim` long
    sigs: Vec<u64>,        // one signature per store record (build-time order)
    dim: usize,
}

impl LshIndex {
    /// Number of LSH bits (hyperplanes). One u64 signature per record.
    pub const BITS: usize = 64;

    /// Build the index over a store's current records (O(n·BITS·dim) once).
    pub fn build(store: &RvfStore) -> Self {
        let planes = lsh_planes(store.dim, Self::BITS, 0xA9E5_2026_C0FF_EE01);
        let sigs = store
            .records
            .iter()
            .map(|r| lsh_sig(&planes, &r.vector))
            .collect();
        Self {
            planes,
            sigs,
            dim: store.dim,
        }
    }

    /// Approximate cosine `top_k` over `store`, considering at most
    /// `max_candidates` LSH-nearest records before exact re-ranking. Falls back
    /// to the exact scan if the index is stale (store mutated since `build`).
    pub fn search(
        &self,
        store: &RvfStore,
        query: &[f32],
        top_k: usize,
        max_candidates: usize,
    ) -> Result<Vec<Hit>> {
        if query.len() != self.dim {
            return Err(Error::malformed(
                "query",
                format!("expected dim {}, got {}", self.dim, query.len()),
            ));
        }
        if self.sigs.len() != store.records.len() {
            return store.search(query, top_k); // stale index — be correct, not fast
        }
        let qsig = lsh_sig(&self.planes, query);
        let mut idx: Vec<usize> = (0..store.records.len()).collect();
        idx.sort_by_key(|&i| (self.sigs[i] ^ qsig).count_ones());
        idx.truncate(max_candidates.max(top_k));
        let qn = norm(query);
        let mut hits: Vec<Hit> = idx
            .iter()
            .map(|&i| {
                let r = &store.records[i];
                Hit {
                    id: r.id.clone(),
                    score: cosine(query, qn, &r.vector),
                    meta: r.meta.clone(),
                }
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(top_k);
        Ok(hits)
    }
}

/// `bits` deterministic hyperplanes of length `dim`, components uniform in
/// [-1, 1) from a splitmix64 stream (no RNG dependency; reproducible).
fn lsh_planes(dim: usize, bits: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut s = seed;
    (0..bits)
        .map(|_| {
            (0..dim)
                .map(|_| {
                    let u = (splitmix64(&mut s) >> 11) as f32 / ((1u64 << 53) as f32); // [0,1)
                    u * 2.0 - 1.0
                })
                .collect()
        })
        .collect()
}

/// Sign random-projection signature: bit i = (v · plane_i >= 0).
fn lsh_sig(planes: &[Vec<f32>], v: &[f32]) -> u64 {
    let mut sig = 0u64;
    for (i, p) in planes.iter().enumerate() {
        let dot: f32 = p.iter().zip(v).map(|(a, b)| a * b).sum();
        if dot >= 0.0 {
            sig |= 1u64 << i;
        }
    }
    sig
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
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
    fn lsh_full_budget_matches_exact() {
        let mut s = RvfStore::new(3);
        s.upsert(rec("a", vec![1.0, 0.0, 0.0])).unwrap();
        s.upsert(rec("b", vec![0.0, 1.0, 0.0])).unwrap();
        s.upsert(rec("c", vec![0.9, 0.1, 0.0])).unwrap();
        s.upsert(rec("d", vec![0.0, 0.0, 1.0])).unwrap();
        let idx = LshIndex::build(&s);
        // With the candidate budget >= len, approx reranks the same full set with
        // exact cosine. The unambiguous top-2 (a, then c) must match exactly; the
        // two zero-score tails (b, d) tie and may order differently.
        let exact = s.search(&[1.0, 0.0, 0.0], 2).unwrap();
        let approx = idx.search(&s, &[1.0, 0.0, 0.0], 2, s.len()).unwrap();
        assert_eq!(
            exact.iter().map(|h| h.id.clone()).collect::<Vec<_>>(),
            approx.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );
        assert_eq!(approx[0].id, "a");
        assert_eq!(approx[1].id, "c");
        // approx considers the whole set, so it returns all 4 when asked.
        assert_eq!(
            idx.search(&s, &[1.0, 0.0, 0.0], 4, s.len()).unwrap().len(),
            4
        );
    }

    #[test]
    fn lsh_finds_exact_vector_query_with_tiny_budget() {
        let mut s = RvfStore::new(4);
        for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            let mut v = vec![0.0; 4];
            v[i % 4] = 1.0 + i as f32 * 0.01;
            s.upsert(rec(id, v)).unwrap();
        }
        let idx = LshIndex::build(&s);
        // Querying with a stored vector → its signature matches (Hamming 0) → it
        // is always a candidate, even with budget 1.
        let q = s.records[2].vector.clone();
        let hits = idx.search(&s, &q, 1, 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "c");
    }

    #[test]
    fn lsh_dim_mismatch_errors() {
        let s = RvfStore::new(3);
        let idx = LshIndex::build(&s);
        assert!(idx.search(&s, &[1.0, 0.0], 1, 4).is_err());
    }

    #[test]
    fn lsh_stale_index_falls_back_to_exact() {
        let mut s = RvfStore::new(2);
        s.upsert(rec("a", vec![1.0, 0.0])).unwrap();
        let idx = LshIndex::build(&s);
        s.upsert(rec("b", vec![0.0, 1.0])).unwrap(); // mutate after build → stale
                                                     // Falls back to exact scan, still returns the correct neighbour.
        let hits = idx.search(&s, &[0.0, 1.0], 1, 1).unwrap();
        assert_eq!(hits[0].id, "b");
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
}
