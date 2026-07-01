//! G2 — bridge-signing identity (ADR-0025 Phase 1).
//!
//! Slack/Teams users hold no Ed25519 keys, so the bridge signs inbound messages
//! on their behalf. Following the Matrix appservice "ghost user" model:
//!
//! - The bridge derives a **stable per-source subkey** (one Ed25519 key per
//!   `platform:workspace`) from its root seed, so trust and revocation are
//!   scoped to a single source — revoking one workspace can't forge another.
//! - Each inbound message is wrapped as a normal **signed** AgentBBS message
//!   authored by that subkey and marked `bridged` (the reserved `bridge:` handle
//!   prefix, which [`crate::is_bridged`] uses as the outbound loop guard).
//! - Verifying nodes verify the **bridge** (the peer vouches the relay is
//!   faithful), **not** the human — who is explicitly un-authenticated.
//!
//! The Socket Mode / Events transport that *delivers* these inbound events is
//! thin glue layered on top of this (and needs a live Slack connection, so it
//! isn't unit-tested here); this module is the testable identity core.

use agentbbs_core::{Identity, Message, MessageBody, MessageKind};
use chrono::{DateTime, Utc};
use std::collections::HashSet;

/// The bridge's root identity. Never leaves the bridge; only per-source subkeys
/// ever author messages.
#[derive(Clone)]
pub struct BridgeIdentity {
    root_seed: [u8; 32],
}

impl BridgeIdentity {
    pub fn from_seed(root_seed: [u8; 32]) -> Self {
        Self { root_seed }
    }

    /// Deterministic per-source Ed25519 subkey:
    /// `blake3(domain || root_seed || source)`. Same source → same key forever;
    /// different sources → independent keys (scoped trust/revocation).
    pub fn subkey(&self, source: &str) -> Identity {
        let mut h = blake3::Hasher::new();
        h.update(b"agentbbs.bridge.subkey.v1\n");
        h.update(&self.root_seed);
        h.update(source.as_bytes());
        let seed: [u8; 32] = *h.finalize().as_bytes();
        Identity::from_seed(&seed)
    }
}

/// An inbound external message to mirror INTO an AgentBBS board.
#[derive(Clone, Debug)]
pub struct Inbound {
    pub platform: String,     // "slack" | "teams"
    pub workspace: String,    // workspace / team id
    pub user_id: String,      // external user id
    pub display_name: String, // external user display name
    pub text: String,
    pub external_msg_id: String,
    pub board: String, // target board slug
}

impl Inbound {
    /// The per-source key namespace (one subkey per platform+workspace).
    pub fn source(&self) -> String {
        format!("{}:{}", self.platform, self.workspace)
    }
}

/// Sign an inbound external message as a `bridged` AgentBBS message authored by
/// the per-source bridge subkey. `created_at` is passed in (not read from the
/// clock) so the result is deterministic and testable.
pub fn sign_inbound(id: &BridgeIdentity, inb: &Inbound, created_at: DateTime<Utc>) -> Message {
    let sub = id.subkey(&inb.source());
    let handle = format!("bridge:{}:{}", inb.platform, inb.display_name);
    // Origin identifiers, carried in the (signed) subject for audit.
    let subject = format!("{}@{}", inb.user_id, inb.workspace);
    let body = MessageBody {
        board: inb.board.clone(),
        parent: None,
        subject,
        body: inb.text.clone(),
        author: sub.id(),
        handle,
        created_at,
        kind: MessageKind::Post,
    };
    Message::sign(&sub, body).expect("the subkey is the declared author")
}

/// Loop guard: remembers external message ids so a relay is never mirrored
/// twice and the bridge's own posts are never re-ingested.
#[derive(Default)]
pub struct SeenSet {
    ids: HashSet<String>,
}

impl SeenSet {
    pub fn new() -> Self {
        Self::default()
    }
    /// Returns `true` if `external_id` was already seen (caller should SKIP);
    /// records it and returns `false` on first sight.
    pub fn seen_or_record(&mut self, external_id: &str) -> bool {
        !self.ids.insert(external_id.to_string())
    }
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::is_bridged;

    fn inbound(platform: &str, workspace: &str, text: &str) -> Inbound {
        Inbound {
            platform: platform.into(),
            workspace: workspace.into(),
            user_id: "U123".into(),
            display_name: "Alice".into(),
            text: text.into(),
            external_msg_id: "1699999999.0001".into(),
            board: "general".into(),
        }
    }

    #[test]
    fn subkey_is_deterministic_and_source_scoped() {
        let id = BridgeIdentity::from_seed([7u8; 32]);
        assert_eq!(id.subkey("slack:T1").id(), id.subkey("slack:T1").id());
        assert_ne!(id.subkey("slack:T1").id(), id.subkey("slack:T2").id());
        assert_ne!(id.subkey("slack:T1").id(), id.subkey("teams:T1").id());
        // a different root seed yields different keys for the same source
        let other = BridgeIdentity::from_seed([8u8; 32]);
        assert_ne!(id.subkey("slack:T1").id(), other.subkey("slack:T1").id());
    }

    #[test]
    fn signed_inbound_verifies_and_is_marked_bridged() {
        let id = BridgeIdentity::from_seed([7u8; 32]);
        let inb = inbound("slack", "T1", "hello from slack");
        let msg = sign_inbound(&id, &inb, Utc::now());
        assert!(msg.verify().is_ok(), "bridge-signed message must verify");
        assert!(msg.body.handle.starts_with("bridge:slack:"));
        assert!(is_bridged(&msg), "must be loop-guarded from re-mirroring");
        assert_eq!(msg.body.board, "general");
        assert_eq!(msg.body.body, "hello from slack");
        // authored by the per-source subkey, not any human key
        assert_eq!(msg.body.author, id.subkey("slack:T1").id());
        assert!(msg.body.subject.contains("@T1"));
    }

    #[test]
    fn different_workspaces_use_different_author_keys() {
        let id = BridgeIdentity::from_seed([7u8; 32]);
        let a = sign_inbound(&id, &inbound("slack", "T1", "x"), Utc::now());
        let b = sign_inbound(&id, &inbound("slack", "T2", "y"), Utc::now());
        assert_ne!(a.body.author, b.body.author);
    }

    #[test]
    fn bridged_inbound_is_not_re_mirrored_outbound() {
        use crate::{BoardMapping, Bridge, BridgeConfig};
        let id = BridgeIdentity::from_seed([7u8; 32]);
        let msg = sign_inbound(&id, &inbound("slack", "T1", "echo"), Utc::now());
        let bridge = Bridge::new(BridgeConfig {
            mappings: vec![BoardMapping {
                board: "general".into(),
                slack_webhook: Some("https://hooks.slack.com/x".into()),
                teams_webhook: None,
                discord_webhook: None,
                whatsapp: None,
            }],
        });
        // The full loop guard: an inbound message the bridge itself signed must
        // never be planned for outbound mirroring (no Slack→BBS→Slack echo).
        assert!(bridge.plan(&msg).is_empty());
    }

    #[test]
    fn seen_set_dedupes() {
        let mut seen = SeenSet::new();
        assert!(!seen.seen_or_record("ext-1")); // first sight
        assert!(seen.seen_or_record("ext-1")); // duplicate → skip
        assert!(!seen.seen_or_record("ext-2"));
        assert_eq!(seen.len(), 2);
    }
}
