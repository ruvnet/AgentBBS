//! Decision records (ADR-0045) — the org's signed memory of material decisions.
//!
//! A [`DecisionRecord`] is a content-addressed (BLAKE3), Ed25519-signed record of
//! a decision and its rationale — the business equivalent of an ADR. It captures
//! the durable *why* behind autopilot actions (playbooks, approvals), is
//! tamper-evident, and is citable by `id`. A [`DecisionLog`] stores verified
//! records; one can also be posted as a normal signed board message.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::{AgentId, Identity, SignatureBytes};

/// A signed, content-addressed decision record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// BLAKE3 content hash of the record (its citation id).
    pub id: String,
    /// Short decision title.
    pub title: String,
    /// The decision taken.
    pub decision: String,
    /// Why — the rationale.
    pub rationale: String,
    /// Board the decision belongs to.
    pub board: String,
    /// Who decided (and signed).
    pub decided_by: AgentId,
    /// When.
    pub decided_at: DateTime<Utc>,
    /// Signature over the canonical bytes.
    pub signature: SignatureBytes,
}

fn content_bytes(
    title: &str,
    decision: &str,
    rationale: &str,
    board: &str,
    decided_by: &AgentId,
    decided_at: &DateTime<Utc>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"agentbbs.decision.v1\n");
    for p in [
        title.as_bytes(),
        decision.as_bytes(),
        rationale.as_bytes(),
        board.as_bytes(),
        decided_by.to_hex().as_bytes(),
        decided_at.to_rfc3339().as_bytes(),
    ] {
        out.extend_from_slice(format!("{}:", p.len()).as_bytes());
        out.extend_from_slice(p);
        out.push(b'\n');
    }
    out
}

impl DecisionRecord {
    /// Create + sign a decision record, computing its content-addressed `id`.
    pub fn new(
        decider: &Identity,
        title: impl Into<String>,
        decision: impl Into<String>,
        rationale: impl Into<String>,
        board: impl Into<String>,
        decided_at: DateTime<Utc>,
    ) -> Self {
        let title = title.into();
        let decision = decision.into();
        let rationale = rationale.into();
        let board = board.into();
        let decided_by = decider.id();
        let bytes = content_bytes(
            &title,
            &decision,
            &rationale,
            &board,
            &decided_by,
            &decided_at,
        );
        let id = blake3::hash(&bytes).to_hex().to_string();
        let signature = decider.sign(&bytes);
        DecisionRecord {
            id,
            title,
            decision,
            rationale,
            board,
            decided_by,
            decided_at,
            signature,
        }
    }

    /// Verify the content hash AND the signature.
    pub fn verify(&self) -> Result<()> {
        let bytes = content_bytes(
            &self.title,
            &self.decision,
            &self.rationale,
            &self.board,
            &self.decided_by,
            &self.decided_at,
        );
        if blake3::hash(&bytes).to_hex().to_string() != self.id {
            return Err(Error::malformed("decision", "id does not match content"));
        }
        self.decided_by.verify(&bytes, &self.signature)
    }
}

/// The org's decision log.
#[derive(Default, Debug)]
pub struct DecisionLog {
    records: Vec<DecisionRecord>,
}

impl DecisionLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a record after verifying it (forged/tampered → rejected). Idempotent
    /// on the content-addressed `id`.
    pub fn add(&mut self, record: DecisionRecord) -> Result<()> {
        record.verify()?;
        if !self.records.iter().any(|r| r.id == record.id) {
            self.records.push(record);
        }
        Ok(())
    }

    /// All records, newest first.
    pub fn all(&self) -> Vec<&DecisionRecord> {
        let mut v: Vec<&DecisionRecord> = self.records.iter().collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.decided_at));
        v
    }

    /// Records for one board, newest first.
    pub fn for_board(&self, board: &str) -> Vec<&DecisionRecord> {
        self.all()
            .into_iter()
            .filter(|r| r.board == board)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn content_addressed_sign_and_verify() {
        let d = Identity::generate();
        let r = DecisionRecord::new(
            &d,
            "Refund policy",
            "30-day no-questions refunds",
            "reduces support load; matches competitors",
            "ops",
            at("2026-06-30T05:00:00Z"),
        );
        assert_eq!(r.id.len(), 64);
        assert!(r.verify().is_ok());
        // same content → same id (deterministic)
        let r2 = DecisionRecord::new(
            &d,
            "Refund policy",
            "30-day no-questions refunds",
            "reduces support load; matches competitors",
            "ops",
            at("2026-06-30T05:00:00Z"),
        );
        assert_eq!(r.id, r2.id);
        // tamper → verify fails
        let mut bad = r.clone();
        bad.decision = "no refunds".into();
        assert!(bad.verify().is_err());
    }

    #[test]
    fn log_add_dedup_and_per_board() {
        let d = Identity::generate();
        let mut log = DecisionLog::new();
        let a = DecisionRecord::new(
            &d,
            "A",
            "do A",
            "because",
            "ops",
            at("2026-06-30T05:00:00Z"),
        );
        let b = DecisionRecord::new(
            &d,
            "B",
            "do B",
            "because",
            "eng",
            at("2026-06-30T06:00:00Z"),
        );
        log.add(a.clone()).unwrap();
        log.add(a.clone()).unwrap(); // idempotent on id
        log.add(b).unwrap();
        assert_eq!(log.all().len(), 2);
        assert_eq!(log.all()[0].title, "B"); // newest first
        assert_eq!(log.for_board("ops").len(), 1);
    }

    #[test]
    fn forged_record_not_added() {
        let d = Identity::generate();
        let mut r = DecisionRecord::new(&d, "T", "x", "y", "ops", at("2026-06-30T05:00:00Z"));
        r.rationale = "tampered".into(); // id no longer matches
        let mut log = DecisionLog::new();
        assert!(log.add(r).is_err());
    }
}
