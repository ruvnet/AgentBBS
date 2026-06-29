//! Marketplace settlement — a signed, verifiable credits ledger.
//!
//! The [`crate::market`] layer establishes *authenticity* (who is selling
//! what). This layer adds *settlement*: an abstract credits balance and signed
//! [`Transfer`]s that move credits between anonymous [`AgentId`]s. A
//! [`Transfer`] is sender-signed and content-addressed exactly like a board
//! [`crate::board::Message`] or a [`crate::market::Listing`], so it is
//! tamper-evident and verifiable without a trusted server — and a [`Ledger`]
//! that applies it rejects forgeries, overdrafts, and double-spends.
//!
//! Credits here are deliberately *abstract* (not real money). The unit is
//! whatever an operator decides; issuance is via [`Ledger::mint`] (a faucet,
//! an airdrop, an off-chain bridge — out of scope here).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::{AgentId, Identity, SignatureBytes};
use crate::market::Listing;

/// Content-addressed transfer id (BLAKE3 hex of the signing bytes).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransferId(pub String);

impl std::fmt::Display for TransferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for TransferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TransferId({})", &self.0[..self.0.len().min(8)])
    }
}

/// The pre-signature content of a credits transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferBody {
    /// Sender (debited). Must equal the signer.
    pub from: AgentId,
    /// Recipient (credited).
    pub to: AgentId,
    /// Amount of credits to move.
    pub amount: u64,
    /// Per-sender monotonic nonce — makes otherwise-identical transfers unique
    /// (distinct ids) and anchors replay protection.
    pub nonce: u64,
    /// Free-form memo (e.g. `buy:<sku>`), bound by the signature.
    pub memo: String,
    /// Creation time.
    pub created_at: DateTime<Utc>,
}

impl TransferBody {
    /// Deterministic canonical signing bytes (domain-separated, length-prefixed).
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(160);
        out.extend_from_slice(b"agentbbs.transfer.v1\n");
        out.extend_from_slice(self.from.to_hex().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(self.to.to_hex().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(self.amount.to_string().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(self.nonce.to_string().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(format!("{}:", self.memo.len()).as_bytes());
        out.extend_from_slice(self.memo.as_bytes());
        out.push(b'\n');
        out.extend_from_slice(self.created_at.to_rfc3339().as_bytes());
        out
    }

    /// Content-addressed id.
    pub fn id(&self) -> TransferId {
        TransferId(blake3::hash(&self.signing_bytes()).to_hex().to_string())
    }
}

/// A signed, content-addressed credits transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    /// Content-addressed id.
    pub id: TransferId,
    /// Signed content.
    pub body: TransferBody,
    /// Sender's signature over `body.signing_bytes()`.
    pub signature: SignatureBytes,
}

impl Transfer {
    /// Sign a transfer with `identity` (must equal `body.from`).
    pub fn sign(identity: &Identity, body: TransferBody) -> Result<Self> {
        if identity.id() != body.from {
            return Err(Error::malformed("transfer", "signer is not the sender"));
        }
        let signature = identity.sign(&body.signing_bytes());
        Ok(Transfer {
            id: body.id(),
            body,
            signature,
        })
    }

    /// Verify the id and the sender's signature.
    pub fn verify(&self) -> Result<()> {
        let bytes = self.body.signing_bytes();
        if TransferId(blake3::hash(&bytes).to_hex().to_string()) != self.id {
            return Err(Error::malformed("transfer", "id does not match content"));
        }
        self.body.from.verify(&bytes, &self.signature)
    }
}

/// A credits ledger: balances plus the set of applied transfers (so applying a
/// transfer is idempotent — no double-spend) and per-account next-nonce (replay
/// protection across distinct transfers).
#[derive(Default)]
pub struct Ledger {
    balances: HashMap<AgentId, u64>,
    applied: HashSet<String>,
    next_nonce: HashMap<AgentId, u64>,
}

impl Ledger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue `amount` credits to `to` (faucet/airdrop/bridge). Not a transfer;
    /// no signature required — issuance policy is the operator's.
    pub fn mint(&mut self, to: AgentId, amount: u64) {
        *self.balances.entry(to).or_insert(0) += amount;
    }

    /// Balance of `id` (0 if unknown).
    pub fn balance(&self, id: &AgentId) -> u64 {
        self.balances.get(id).copied().unwrap_or(0)
    }

    /// The next acceptable nonce for `from` (0 for a new account).
    pub fn next_nonce(&self, from: &AgentId) -> u64 {
        self.next_nonce.get(from).copied().unwrap_or(0)
    }

    /// Total issued credits across all accounts (conserved by transfers).
    pub fn total_supply(&self) -> u64 {
        self.balances.values().sum()
    }

    /// Apply a signed transfer. Rejects forged signatures and overdrafts;
    /// duplicates (same content id) are a no-op (idempotent — safe to replay
    /// across federation). Enforces a strictly-increasing per-sender nonce.
    pub fn apply(&mut self, transfer: &Transfer) -> Result<()> {
        transfer.verify()?;
        if self.applied.contains(&transfer.id.0) {
            return Ok(()); // idempotent: already settled
        }
        let from = transfer.body.from;
        let amount = transfer.body.amount;

        let expected = self.next_nonce(&from);
        if transfer.body.nonce != expected {
            return Err(Error::Other(format!(
                "bad nonce: expected {expected}, got {}",
                transfer.body.nonce
            )));
        }
        if self.balance(&from) < amount {
            return Err(Error::Other("insufficient balance".into()));
        }

        *self.balances.entry(from).or_insert(0) -= amount;
        *self.balances.entry(transfer.body.to).or_insert(0) += amount;
        self.next_nonce.insert(from, expected + 1);
        self.applied.insert(transfer.id.0.clone());
        Ok(())
    }
}

/// A settlement receipt: the listing bought and the transfer that paid for it.
#[derive(Clone, Debug)]
pub struct Receipt {
    /// The listing SKU purchased.
    pub sku: String,
    /// The signed payment transfer.
    pub transfer: Transfer,
}

/// Buy `listing`: build, sign, and settle a payment of `listing.price` from
/// `buyer` to the listing's seller. The listing must verify (authentic) before
/// money moves. Returns a [`Receipt`] on success.
pub fn purchase(buyer: &Identity, listing: &Listing, ledger: &mut Ledger) -> Result<Receipt> {
    listing.verify()?; // never pay for a forged listing
    let body = TransferBody {
        from: buyer.id(),
        to: listing.body.seller,
        amount: listing.body.price,
        nonce: ledger.next_nonce(&buyer.id()),
        memo: format!("buy:{}", listing.body.sku),
        created_at: Utc::now(),
    };
    let transfer = Transfer::sign(buyer, body)?;
    ledger.apply(&transfer)?;
    Ok(Receipt {
        sku: listing.body.sku.clone(),
        transfer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::{artifact_hash, ListingBody, ListingKind};

    fn transfer(from: &Identity, to: AgentId, amount: u64, nonce: u64) -> Transfer {
        let body = TransferBody {
            from: from.id(),
            to,
            amount,
            nonce,
            memo: "test".into(),
            created_at: Utc::now(),
        };
        Transfer::sign(from, body).unwrap()
    }

    #[test]
    fn sign_and_verify() {
        let a = Identity::generate();
        let b = Identity::generate();
        let t = transfer(&a, b.id(), 10, 0);
        assert!(t.verify().is_ok());
    }

    #[test]
    fn tampered_amount_detected() {
        let a = Identity::generate();
        let b = Identity::generate();
        let mut t = transfer(&a, b.id(), 10, 0);
        t.body.amount = 1_000_000;
        assert!(t.verify().is_err());
    }

    #[test]
    fn signer_must_be_sender() {
        let a = Identity::generate();
        let b = Identity::generate();
        let body = TransferBody {
            from: b.id(), // not a
            to: b.id(),
            amount: 1,
            nonce: 0,
            memo: String::new(),
            created_at: Utc::now(),
        };
        assert!(Transfer::sign(&a, body).is_err());
    }

    #[test]
    fn apply_moves_credits() {
        let a = Identity::generate();
        let b = Identity::generate();
        let mut l = Ledger::new();
        l.mint(a.id(), 100);
        l.apply(&transfer(&a, b.id(), 30, 0)).unwrap();
        assert_eq!(l.balance(&a.id()), 70);
        assert_eq!(l.balance(&b.id()), 30);
        assert_eq!(l.total_supply(), 100); // conserved
    }

    #[test]
    fn overdraft_rejected() {
        let a = Identity::generate();
        let b = Identity::generate();
        let mut l = Ledger::new();
        l.mint(a.id(), 5);
        let err = l.apply(&transfer(&a, b.id(), 10, 0));
        assert!(err.is_err());
        assert_eq!(l.balance(&a.id()), 5); // unchanged
    }

    #[test]
    fn duplicate_is_idempotent() {
        let a = Identity::generate();
        let b = Identity::generate();
        let mut l = Ledger::new();
        l.mint(a.id(), 100);
        let t = transfer(&a, b.id(), 40, 0);
        l.apply(&t).unwrap();
        l.apply(&t).unwrap(); // replay — must not double-debit
        assert_eq!(l.balance(&a.id()), 60);
        assert_eq!(l.balance(&b.id()), 40);
    }

    #[test]
    fn nonce_must_be_in_order() {
        let a = Identity::generate();
        let b = Identity::generate();
        let mut l = Ledger::new();
        l.mint(a.id(), 100);
        // nonce 1 before 0 is rejected.
        assert!(l.apply(&transfer(&a, b.id(), 10, 1)).is_err());
        l.apply(&transfer(&a, b.id(), 10, 0)).unwrap();
        l.apply(&transfer(&a, b.id(), 10, 1)).unwrap();
        assert_eq!(l.balance(&b.id()), 20);
    }

    #[test]
    fn forged_signature_rejected_by_apply() {
        let a = Identity::generate();
        let b = Identity::generate();
        let mut l = Ledger::new();
        l.mint(a.id(), 100);
        let mut t = transfer(&a, b.id(), 10, 0);
        // Swap in a signature over different content.
        t.signature = transfer(&a, b.id(), 99, 7).signature;
        assert!(l.apply(&t).is_err());
        assert_eq!(l.balance(&a.id()), 100);
    }

    #[test]
    fn buy_flow_settles_against_a_listing() {
        let seller = Identity::generate();
        let buyer = Identity::generate();
        let body = ListingBody {
            sku: "echo-door".into(),
            kind: ListingKind::Plugin,
            title: "Echo Door".into(),
            description: "a plugin".into(),
            price: 25,
            seller: seller.id(),
            handle: "vendor".into(),
            artifact_hash: artifact_hash(b"wasm"),
            created_at: Utc::now(),
        };
        let listing = Listing::sign(&seller, body).unwrap();

        let mut l = Ledger::new();
        l.mint(buyer.id(), 100);
        let receipt = purchase(&buyer, &listing, &mut l).unwrap();

        assert_eq!(receipt.sku, "echo-door");
        assert!(receipt.transfer.verify().is_ok());
        assert_eq!(receipt.transfer.body.memo, "buy:echo-door");
        assert_eq!(l.balance(&buyer.id()), 75);
        assert_eq!(l.balance(&seller.id()), 25);

        // Can't buy again without funds for a 4th time etc.; buying with an
        // empty wallet overdrafts.
        let broke = Identity::generate();
        assert!(purchase(&broke, &listing, &mut l).is_err());
    }
}
