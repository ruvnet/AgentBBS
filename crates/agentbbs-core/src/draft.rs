//! Agent Inbox — human-confirmed agent-drafted replies (ADR-0049).
//!
//! A [`Draft`] is deliberately **not** a signed [`crate::Message`] — AgentBBS
//! already requires explicit client-side Ed25519 signing for anything to become
//! a real, federatable artifact (ADR-0003/0016), so a draft needs no new crypto
//! primitive: it is simply an **unsigned candidate body** sitting in a queue
//! until a human reviews, optionally edits, and explicitly sends it (signing
//! with their own key at that point, via the normal post path).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lifecycle of a draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    /// Freshly composed by an agent, not yet reviewed.
    Pending,
    /// A human edited the body; still awaiting send.
    Edited,
    /// A human sent it — signed and posted under their own key.
    Sent,
    /// A human discarded it.
    Discarded,
}

/// An agent-composed reply candidate awaiting human review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// Content-addressed (BLAKE3 of the canonical bytes at creation time);
    /// stable across edits — it identifies *this draft*, not its current text.
    pub id: String,
    /// Board slug or `dm:<peer>` the draft would post to.
    pub target: String,
    /// Parent message id this is a reply to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    /// Which agent composed this draft (a cosmetic handle — the human's own
    /// key signs on send, never the agent's).
    pub agent: String,
    /// Subject line.
    pub subject: String,
    /// The drafted reply text.
    pub body: String,
    /// When the agent composed it.
    pub created_at: DateTime<Utc>,
    /// Where it stands in the review lifecycle.
    pub status: DraftStatus,
    /// Whether the inbound content this was drafted from was flagged
    /// `Suspicious` by postguard at draft time (`Malicious` is refused before
    /// a `Draft` is ever created — see `tools::draft_reply`). Surfaced so the
    /// reviewing human gets extra scrutiny cues, never auto-discarded.
    #[serde(default)]
    pub flagged: bool,
}

impl Draft {
    fn content_bytes(target: &str, agent: &str, body: &str, created_at: &DateTime<Utc>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"agentbbs.draft.v1\n");
        for p in [
            target.as_bytes(),
            agent.as_bytes(),
            body.as_bytes(),
            created_at.to_rfc3339().as_bytes(),
        ] {
            out.extend_from_slice(format!("{}:", p.len()).as_bytes());
            out.extend_from_slice(p);
            out.push(b'\n');
        }
        out
    }

    /// Compose a new `Pending` draft, computing its content-addressed id.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: impl Into<String>,
        in_reply_to: Option<String>,
        agent: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
        created_at: DateTime<Utc>,
        flagged: bool,
    ) -> Self {
        let target = target.into();
        let agent = agent.into();
        let body = body.into();
        let id = blake3::hash(&Self::content_bytes(&target, &agent, &body, &created_at))
            .to_hex()
            .to_string();
        Draft {
            id,
            target,
            in_reply_to,
            agent,
            subject: subject.into(),
            body,
            created_at,
            status: DraftStatus::Pending,
            flagged,
        }
    }
}

/// A queue of drafts. AgentBBS has no server-side account concept (identities
/// are client-held keys, ADR-0016), so this is **not** partitioned per
/// recipient at the store level — `target` (board/DM) scoping plus the
/// viewer's own read access is what makes "my drafts" meaningful; the UI
/// shows drafts for boards/DMs the viewer can already see.
#[derive(Default, Debug)]
pub struct DraftQueue {
    drafts: Vec<Draft>,
}

impl DraftQueue {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a draft (idempotent on its content-addressed id).
    pub fn add(&mut self, draft: Draft) {
        if !self.drafts.iter().any(|d| d.id == draft.id) {
            self.drafts.push(draft);
        }
    }

    /// All drafts still awaiting a decision (`Pending` or `Edited`), newest first.
    pub fn pending(&self) -> Vec<&Draft> {
        let mut v: Vec<&Draft> = self
            .drafts
            .iter()
            .filter(|d| matches!(d.status, DraftStatus::Pending | DraftStatus::Edited))
            .collect();
        v.sort_by_key(|d| std::cmp::Reverse(d.created_at));
        v
    }

    /// Look up a draft by id regardless of status.
    pub fn get(&self, id: &str) -> Option<&Draft> {
        self.drafts.iter().find(|d| d.id == id)
    }

    /// Edit a still-pending draft's body (marks it `Edited`). Returns `false`
    /// if the id is unknown or already `Sent`/`Discarded`.
    pub fn edit(&mut self, id: &str, new_body: impl Into<String>) -> bool {
        match self
            .drafts
            .iter_mut()
            .find(|d| d.id == id && matches!(d.status, DraftStatus::Pending | DraftStatus::Edited))
        {
            Some(d) => {
                d.body = new_body.into();
                d.status = DraftStatus::Edited;
                true
            }
            None => false,
        }
    }

    /// Mark a draft `Sent` (the caller is responsible for having actually
    /// signed and posted it first). Returns `false` if the id is unknown or
    /// already terminal.
    pub fn mark_sent(&mut self, id: &str) -> bool {
        self.transition(id, DraftStatus::Sent)
    }

    /// Discard a pending draft. Returns `false` if the id is unknown or
    /// already terminal.
    pub fn discard(&mut self, id: &str) -> bool {
        self.transition(id, DraftStatus::Discarded)
    }

    fn transition(&mut self, id: &str, to: DraftStatus) -> bool {
        match self
            .drafts
            .iter_mut()
            .find(|d| d.id == id && matches!(d.status, DraftStatus::Pending | DraftStatus::Edited))
        {
            Some(d) => {
                d.status = to;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn add_then_pending_then_get() {
        let mut q = DraftQueue::new();
        let d = Draft::new(
            "general",
            None,
            "claude",
            "re: dinner",
            "Thursday works for me.",
            at("2026-06-30T05:00:00Z"),
            false,
        );
        let id = d.id.clone();
        q.add(d);
        assert_eq!(q.pending().len(), 1);
        assert_eq!(q.get(&id).unwrap().status, DraftStatus::Pending);
    }

    #[test]
    fn add_is_idempotent_on_content_addressed_id() {
        let mut q = DraftQueue::new();
        let make = || {
            Draft::new(
                "general",
                None,
                "claude",
                "s",
                "same body",
                at("2026-06-30T05:00:00Z"),
                false,
            )
        };
        q.add(make());
        q.add(make()); // identical content -> identical id -> not duplicated
        assert_eq!(q.pending().len(), 1);
    }

    #[test]
    fn edit_marks_edited_and_changes_body_but_keeps_id() {
        let mut q = DraftQueue::new();
        let d = Draft::new(
            "general",
            None,
            "claude",
            "s",
            "original",
            at("2026-06-30T05:00:00Z"),
            false,
        );
        let id = d.id.clone();
        q.add(d);
        assert!(q.edit(&id, "revised wording"));
        let edited = q.get(&id).unwrap();
        assert_eq!(edited.status, DraftStatus::Edited);
        assert_eq!(edited.body, "revised wording");
        assert_eq!(edited.id, id, "id is stable across edits");
        assert_eq!(
            q.pending().len(),
            1,
            "Edited still counts as pending review"
        );
    }

    #[test]
    fn send_and_discard_remove_from_pending_and_are_terminal() {
        let mut q = DraftQueue::new();
        let d1 = Draft::new(
            "general",
            None,
            "claude",
            "s",
            "one",
            at("2026-06-30T05:00:00Z"),
            false,
        );
        let d2 = Draft::new(
            "general",
            None,
            "claude",
            "s",
            "two",
            at("2026-06-30T05:01:00Z"),
            false,
        );
        let (id1, id2) = (d1.id.clone(), d2.id.clone());
        q.add(d1);
        q.add(d2);
        assert!(q.mark_sent(&id1));
        assert!(q.discard(&id2));
        assert_eq!(q.pending().len(), 0);
        assert_eq!(q.get(&id1).unwrap().status, DraftStatus::Sent);
        assert_eq!(q.get(&id2).unwrap().status, DraftStatus::Discarded);
        // Terminal drafts can't be re-sent/re-discarded/re-edited.
        assert!(!q.mark_sent(&id1));
        assert!(!q.edit(&id2, "too late"));
    }

    #[test]
    fn unknown_id_operations_return_false() {
        let mut q = DraftQueue::new();
        assert!(!q.mark_sent("nope"));
        assert!(!q.discard("nope"));
        assert!(!q.edit("nope", "x"));
        assert!(q.get("nope").is_none());
    }
}
