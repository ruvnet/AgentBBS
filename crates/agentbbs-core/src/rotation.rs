//! Key-rotation continuity (ADR-0044).
//!
//! Anonymous identities are throwaway (ADR-0002/0016), but rotating one orphans
//! its reputation, credentials, and trust. A [`RotationLink`] is a **dual-signed**
//! statement that the owner of `old` now uses `new` — signed by *both* keys, so
//! neither can forge a link to a key it doesn't control. A [`RotationChain`]
//! follows links so callers can `resolve` a retired key to its successor and let
//! standing carry over, all while the keys stay opaque (anonymity preserved).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::{AgentId, Identity, SignatureBytes};

/// A dual-signed "old → new" rotation statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationLink {
    /// The retired identity.
    pub old: AgentId,
    /// The successor identity.
    pub new: AgentId,
    /// When the rotation was declared.
    pub created_at: DateTime<Utc>,
    /// Signature by the OLD key over the canonical bytes.
    pub old_sig: SignatureBytes,
    /// Signature by the NEW key over the canonical bytes.
    pub new_sig: SignatureBytes,
}

impl RotationLink {
    fn signing_bytes(old: &AgentId, new: &AgentId, created_at: &DateTime<Utc>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"agentbbs.rotation.v1\n");
        for p in [
            old.to_hex().as_bytes(),
            new.to_hex().as_bytes(),
            created_at.to_rfc3339().as_bytes(),
        ] {
            out.extend_from_slice(format!("{}:", p.len()).as_bytes());
            out.extend_from_slice(p);
            out.push(b'\n');
        }
        out
    }

    /// Forge a rotation link, signed by both the old and new identities.
    pub fn link(old: &Identity, new: &Identity, created_at: DateTime<Utc>) -> Self {
        let bytes = Self::signing_bytes(&old.id(), &new.id(), &created_at);
        RotationLink {
            old: old.id(),
            new: new.id(),
            created_at,
            old_sig: old.sign(&bytes),
            new_sig: new.sign(&bytes),
        }
    }

    /// Verify BOTH signatures. Either missing/forged → [`Error::BadSignature`];
    /// a self-link (old == new) is rejected as malformed.
    pub fn verify(&self) -> Result<()> {
        if self.old == self.new {
            return Err(Error::malformed("rotation", "old and new are the same key"));
        }
        let bytes = Self::signing_bytes(&self.old, &self.new, &self.created_at);
        self.old.verify(&bytes, &self.old_sig)?;
        self.new.verify(&bytes, &self.new_sig)
    }
}

/// A set of verified rotation links; resolves a key to its current successor.
#[derive(Default, Debug)]
pub struct RotationChain {
    // old_hex -> new
    next: std::collections::HashMap<String, AgentId>,
}

impl RotationChain {
    /// An empty chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a link after verifying both signatures (forged → rejected). One
    /// successor per old key (first wins; a later different link is ignored).
    pub fn add(&mut self, link: RotationLink) -> Result<()> {
        link.verify()?;
        self.next.entry(link.old.to_hex()).or_insert(link.new);
        Ok(())
    }

    /// Follow `old → new` edges to the current identity (cycle- and
    /// depth-guarded). Returns `id` itself if it has not been rotated.
    pub fn resolve(&self, id: &AgentId) -> AgentId {
        let mut cur = *id;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            let hex = cur.to_hex();
            if !seen.insert(hex.clone()) {
                break; // cycle guard
            }
            match self.next.get(&hex) {
                Some(n) => cur = *n,
                None => break,
            }
        }
        cur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn dual_sign_and_verify() {
        let old = Identity::generate();
        let new = Identity::generate();
        let link = RotationLink::link(&old, &new, at("2026-06-30T05:00:00Z"));
        assert!(link.verify().is_ok());
        // Tampering the `new` target breaks both checks.
        let mut forged = link.clone();
        forged.new = Identity::generate().id();
        assert!(forged.verify().is_err());
    }

    #[test]
    fn single_signature_is_rejected() {
        let old = Identity::generate();
        let attacker = Identity::generate();
        // Attacker forges a link old→attacker but only the attacker (new) signs;
        // the old_sig is the attacker signing (wrong key for `old`).
        let bytes =
            RotationLink::signing_bytes(&old.id(), &attacker.id(), &at("2026-06-30T05:00:00Z"));
        let bad = RotationLink {
            old: old.id(),
            new: attacker.id(),
            created_at: at("2026-06-30T05:00:00Z"),
            old_sig: attacker.sign(&bytes), // not old's signature
            new_sig: attacker.sign(&bytes),
        };
        assert!(bad.verify().is_err());
    }

    #[test]
    fn resolve_follows_multi_hop() {
        let k1 = Identity::generate();
        let k2 = Identity::generate();
        let k3 = Identity::generate();
        let mut chain = RotationChain::new();
        chain
            .add(RotationLink::link(&k1, &k2, at("2026-06-30T05:00:00Z")))
            .unwrap();
        chain
            .add(RotationLink::link(&k2, &k3, at("2026-06-30T06:00:00Z")))
            .unwrap();
        assert_eq!(chain.resolve(&k1.id()), k3.id()); // k1 → k2 → k3
        assert_eq!(chain.resolve(&k3.id()), k3.id()); // current resolves to self
        let unknown = Identity::generate().id();
        assert_eq!(chain.resolve(&unknown), unknown); // never rotated
    }

    #[test]
    fn cycle_is_safe() {
        // A→B then B→A (both validly dual-signed) must not loop forever.
        let a = Identity::generate();
        let b = Identity::generate();
        let mut chain = RotationChain::new();
        chain
            .add(RotationLink::link(&a, &b, at("2026-06-30T05:00:00Z")))
            .unwrap();
        chain
            .add(RotationLink::link(&b, &a, at("2026-06-30T06:00:00Z")))
            .unwrap();
        let _ = chain.resolve(&a.id()); // terminates (depth/cycle guard)
    }
}
