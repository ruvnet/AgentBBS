//! Message boards — the heart of a BBS.
//!
//! A [`Board`] is a named message base (a "conference" in Wildcat! parlance).
//! A [`Message`] is a signed post within a board, optionally threaded under a
//! parent. Every message carries the author's [`AgentId`] and an Ed25519
//! signature over its canonical bytes, so a post is independently verifiable
//! and survives replication across a federation without a trusted server.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::{AgentId, Identity, SignatureBytes};

/// A content-addressed message id: the BLAKE3 hash of the message's canonical
/// signing bytes, rendered as hex. Stable across nodes.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub String);

impl MessageId {
    /// Short id for retro screens.
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(8)]
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MessageId({})", self.short())
    }
}

/// A board (message base / conference).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Board {
    /// Stable slug, e.g. `general`, `agents.dev`, `marketplace`.
    pub slug: String,
    /// Human title shown in the menu.
    pub title: String,
    /// One-line description.
    pub description: String,
    /// Whether new posts are allowed.
    pub locked: bool,
    /// The agent who created the board.
    pub founder: AgentId,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Whether this board replicates to federated peers.
    pub federated: bool,
}

impl Board {
    /// Create a new, unlocked board founded by `founder`.
    pub fn new(slug: impl Into<String>, title: impl Into<String>, founder: AgentId) -> Self {
        Board {
            slug: slug.into(),
            title: title.into(),
            description: String::new(),
            locked: false,
            founder,
            created_at: Utc::now(),
            federated: true,
        }
    }
}

/// How a message participates in a long-running agent process (ADR-0052).
///
/// `Post` is the default and reproduces the exact pre-ADR-0052 signing bytes,
/// so every historical message and every ordinary post/reply hashes and
/// verifies byte-for-byte as before. `Milestone`/`Step` classify a message as a
/// major, always-visible update vs. a granular sub-step that nests (via
/// [`MessageBody::parent`]) under its milestone and is collapsed by default in
/// both frontends.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageKind {
    /// Ordinary post or reply — the only kind before ADR-0052.
    #[default]
    Post,
    /// A major, always-visible update in an agent process.
    Milestone,
    /// A granular sub-step, nested and collapsed under its milestone by default.
    Step,
}

impl MessageKind {
    /// Stable discriminant folded into the v2 signing bytes. Never renumber —
    /// these values are part of the content-addressed hash for non-`Post` kinds.
    fn discriminant(self) -> &'static str {
        match self {
            MessageKind::Post => "post",
            MessageKind::Milestone => "milestone",
            MessageKind::Step => "step",
        }
    }
}

/// The author-supplied, pre-signature body of a message. Kept separate from
/// the signed [`Message`] so the canonical signing bytes are unambiguous.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageBody {
    /// Board slug this message belongs to.
    pub board: String,
    /// Optional parent message id (for threaded replies).
    pub parent: Option<MessageId>,
    /// Optional subject line.
    pub subject: String,
    /// Message text (markdown/ANSI-safe plain text).
    pub body: String,
    /// Author identity (public key).
    pub author: AgentId,
    /// Author's chosen cosmetic handle (unauthenticated, may be empty).
    pub handle: String,
    /// Authoring timestamp (author's clock).
    pub created_at: DateTime<Utc>,
    /// Agent-process classification (ADR-0052). Defaults to [`MessageKind::Post`];
    /// absent from old stored/wire JSON, which deserializes to `Post`.
    #[serde(default)]
    pub kind: MessageKind,
}

impl MessageBody {
    /// Canonical, deterministic bytes used for hashing and signing. Field
    /// order is fixed; this is *not* arbitrary JSON. Two nodes computing this
    /// for the same logical message must get identical bytes.
    pub fn signing_bytes(&self) -> Vec<u8> {
        // A compact, explicit canonical form. Newlines separate fields; the
        // body is length-prefixed so embedded newlines can't forge fields.
        //
        // Versioning (ADR-0052): a `Post`-kind message emits the exact v1 byte
        // sequence — no `kind` field at all — so every message authored before
        // ADR-0052 (and every ordinary post/reply after it) hashes and verifies
        // byte-for-byte unchanged. Only a non-`Post` kind switches to the v2
        // tag and appends the kind discriminant after `parent`. Verifiers must
        // branch on the leading tag, never assume v1 unconditionally.
        let is_v2 = self.kind != MessageKind::Post;
        let mut out = Vec::with_capacity(self.body.len() + 128);
        out.extend_from_slice(if is_v2 {
            b"agentbbs.msg.v2\n"
        } else {
            b"agentbbs.msg.v1\n"
        });
        out.extend_from_slice(self.board.as_bytes());
        out.push(b'\n');
        out.extend_from_slice(
            self.parent
                .as_ref()
                .map(|p| p.0.as_str())
                .unwrap_or("-")
                .as_bytes(),
        );
        out.push(b'\n');
        if is_v2 {
            out.extend_from_slice(self.kind.discriminant().as_bytes());
            out.push(b'\n');
        }
        out.extend_from_slice(self.subject.as_bytes());
        out.push(b'\n');
        out.extend_from_slice(self.author.to_hex().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(self.handle.as_bytes());
        out.push(b'\n');
        out.extend_from_slice(self.created_at.to_rfc3339().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(format!("{}:", self.body.len()).as_bytes());
        out.extend_from_slice(self.body.as_bytes());
        out
    }

    /// The content-addressed id for this body.
    pub fn id(&self) -> MessageId {
        let hash = blake3::hash(&self.signing_bytes());
        MessageId(hash.to_hex().to_string())
    }
}

/// A fully-formed, signed, content-addressed message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    /// Content-addressed id (BLAKE3 of `body.signing_bytes()`).
    pub id: MessageId,
    /// The signed body.
    pub body: MessageBody,
    /// Author's detached signature over `body.signing_bytes()`.
    pub signature: SignatureBytes,
}

impl Message {
    /// Sign `body` with `identity`, producing a verifiable message. The
    /// identity must match `body.author`.
    pub fn sign(identity: &Identity, body: MessageBody) -> Result<Self> {
        if identity.id() != body.author {
            return Err(Error::malformed(
                "message",
                "signing identity does not match author",
            ));
        }
        let bytes = body.signing_bytes();
        let signature = identity.sign(&bytes);
        Ok(Message {
            id: body.id(),
            body,
            signature,
        })
    }

    /// Verify the message: its id must match its content, and its signature
    /// must validate under the author's key. Returns `Ok(())` if authentic.
    pub fn verify(&self) -> Result<()> {
        let bytes = self.body.signing_bytes();
        let recomputed = MessageId(blake3::hash(&bytes).to_hex().to_string());
        if recomputed != self.id {
            return Err(Error::malformed("message", "id does not match content"));
        }
        self.body.author.verify(&bytes, &self.signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(author: AgentId) -> MessageBody {
        MessageBody {
            board: "general".into(),
            parent: None,
            subject: "hello".into(),
            body: "first post from an agent".into(),
            author,
            handle: "wildcat".into(),
            created_at: Utc::now(),
            kind: MessageKind::Post,
        }
    }

    #[test]
    fn sign_and_verify() {
        let id = Identity::generate();
        let msg = Message::sign(&id, body(id.id())).unwrap();
        assert!(msg.verify().is_ok());
    }

    #[test]
    fn tampered_body_detected() {
        let id = Identity::generate();
        let mut msg = Message::sign(&id, body(id.id())).unwrap();
        msg.body.body = "edited after signing".into();
        assert!(msg.verify().is_err());
    }

    #[test]
    fn author_mismatch_rejected_at_sign() {
        let signer = Identity::generate();
        let other = Identity::generate();
        let err = Message::sign(&signer, body(other.id()));
        assert!(err.is_err());
    }

    #[test]
    fn id_is_content_addressed() {
        let id = Identity::generate();
        let b = body(id.id());
        assert_eq!(b.id(), b.id());
    }

    // ---- ADR-0052: MessageKind + v1/v2 signing bytes ----

    #[test]
    fn post_kind_signs_byte_identical_to_v1() {
        // A Post-kind body must reproduce the *exact* pre-ADR-0052 v1 bytes, so
        // every historical message's hash/signature is unchanged. Rebuild the
        // historical v1 layout by hand and assert byte equality — a stronger
        // guarantee than a tag check (and immune to the body text happening to
        // contain a word like "post").
        let id = Identity::generate();
        let b = body(id.id());
        assert_eq!(b.kind, MessageKind::Post);

        let mut expected = Vec::new();
        expected.extend_from_slice(b"agentbbs.msg.v1\n");
        expected.extend_from_slice(b.board.as_bytes());
        expected.push(b'\n');
        expected.extend_from_slice(b"-"); // parent: None
        expected.push(b'\n');
        expected.extend_from_slice(b.subject.as_bytes());
        expected.push(b'\n');
        expected.extend_from_slice(b.author.to_hex().as_bytes());
        expected.push(b'\n');
        expected.extend_from_slice(b.handle.as_bytes());
        expected.push(b'\n');
        expected.extend_from_slice(b.created_at.to_rfc3339().as_bytes());
        expected.push(b'\n');
        expected.extend_from_slice(format!("{}:", b.body.len()).as_bytes());
        expected.extend_from_slice(b.body.as_bytes());

        assert_eq!(b.signing_bytes(), expected);
    }

    #[test]
    fn milestone_and_step_use_v2_tag_with_discriminant() {
        let id = Identity::generate();
        for (kind, disc) in [
            (MessageKind::Milestone, "milestone"),
            (MessageKind::Step, "step"),
        ] {
            let mut b = body(id.id());
            b.kind = kind;
            let bytes = b.signing_bytes();
            assert!(bytes.starts_with(b"agentbbs.msg.v2\n"));
            assert!(
                String::from_utf8_lossy(&bytes).contains(disc),
                "v2 bytes must carry the {disc} discriminant"
            );
        }
    }

    #[test]
    fn kind_changes_the_content_id() {
        // kind is part of the content-addressed hash for non-Post kinds, so
        // flipping it must change the id (and thus the signature domain).
        let id = Identity::generate();
        let post = body(id.id());
        let mut milestone = body(id.id());
        milestone.kind = MessageKind::Milestone;
        assert_ne!(post.id(), milestone.id());
    }

    #[test]
    fn milestone_and_step_messages_sign_and_verify() {
        let id = Identity::generate();
        for kind in [MessageKind::Milestone, MessageKind::Step] {
            let mut b = body(id.id());
            b.kind = kind;
            let msg = Message::sign(&id, b).unwrap();
            assert!(msg.verify().is_ok());
        }
    }

    #[test]
    fn milestone_with_step_children_round_trips_via_parent() {
        // A Milestone anchors a process; its Steps point back via `parent`,
        // exactly like reply threading. Verify the whole thread signs/verifies
        // and each step's parent resolves to the milestone id.
        let id = Identity::generate();
        let mut m = body(id.id());
        m.kind = MessageKind::Milestone;
        m.subject = "process started".into();
        let milestone = Message::sign(&id, m).unwrap();

        let mut steps = Vec::new();
        for n in 0..3 {
            let mut s = body(id.id());
            s.kind = MessageKind::Step;
            s.parent = Some(milestone.id.clone());
            s.subject = format!("step {n}");
            steps.push(Message::sign(&id, s).unwrap());
        }

        assert!(milestone.verify().is_ok());
        for step in &steps {
            assert!(step.verify().is_ok());
            assert_eq!(step.body.parent.as_ref(), Some(&milestone.id));
        }
    }

    #[test]
    fn v1_json_without_kind_deserializes_to_post() {
        // Old stored/wire JSON predates the `kind` field; serde(default) must
        // fill it as Post so historical bodies still hash to the v1 bytes.
        let id = Identity::generate();
        let json = format!(
            r#"{{"board":"general","parent":null,"subject":"hello","body":"hi","author":"{}","handle":"w","created_at":"2020-01-01T00:00:00Z"}}"#,
            id.id().to_hex()
        );
        let b: MessageBody = serde_json::from_str(&json).unwrap();
        assert_eq!(b.kind, MessageKind::Post);
        assert!(b.signing_bytes().starts_with(b"agentbbs.msg.v1\n"));
    }
}
