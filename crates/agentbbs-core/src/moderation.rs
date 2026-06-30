//! Moderation engine on the capability model (ADR-0032 / ADR-0029 L3).
//!
//! Mute / ban / timeout layered on `Caps` (ADR-0004): a moderator (a holder of
//! [`Caps::MODERATE`], enforced at the call site) issues an **Ed25519-signed**
//! [`ModAction`] against a target; the latest action per target decides their
//! standing, and [`ModerationLog::can_post`] enforces it. Signed → attributable
//! + tamper-evident (like a board post, ADR-0003); the log is the audit trail.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::identity::{AgentId, Identity, SignatureBytes};

/// A moderation sanction against an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "sanction", rename_all = "snake_case")]
pub enum Sanction {
    /// Cannot post (indefinite).
    Mute,
    /// Removed from the community (indefinite, stronger than mute).
    Ban,
    /// Cannot post until `until`.
    Timeout {
        /// When the timeout expires.
        until: DateTime<Utc>,
    },
    /// Clear any active sanction (un-mute / un-ban / end timeout).
    Lift,
}

/// A signed moderation action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModAction {
    /// The agent being moderated.
    pub target: AgentId,
    /// What is applied.
    pub sanction: Sanction,
    /// Human rationale (audited).
    pub reason: String,
    /// The moderator's identity.
    pub moderator: AgentId,
    /// When the action was taken.
    pub created_at: DateTime<Utc>,
    /// Ed25519 signature over the canonical bytes.
    pub signature: SignatureBytes,
}

impl ModAction {
    fn signing_bytes(
        target: &AgentId,
        sanction: &Sanction,
        reason: &str,
        moderator: &AgentId,
        created_at: &DateTime<Utc>,
    ) -> Vec<u8> {
        let s = match sanction {
            Sanction::Mute => "mute".to_string(),
            Sanction::Ban => "ban".to_string(),
            Sanction::Timeout { until } => format!("timeout:{}", until.to_rfc3339()),
            Sanction::Lift => "lift".to_string(),
        };
        let mut out = Vec::new();
        out.extend_from_slice(b"agentbbs.moderation.v1\n");
        for p in [
            target.to_hex().as_bytes(),
            s.as_bytes(),
            reason.as_bytes(),
            moderator.to_hex().as_bytes(),
            created_at.to_rfc3339().as_bytes(),
        ] {
            out.extend_from_slice(format!("{}:", p.len()).as_bytes());
            out.extend_from_slice(p);
            out.push(b'\n');
        }
        out
    }

    /// Sign a moderation action under the moderator's `identity`. The caller must
    /// have verified the moderator holds [`Caps::MODERATE`](crate::caps::Caps).
    pub fn sign(
        identity: &Identity,
        target: AgentId,
        sanction: Sanction,
        reason: impl Into<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        let reason = reason.into();
        let moderator = identity.id();
        let bytes = Self::signing_bytes(&target, &sanction, &reason, &moderator, &created_at);
        let signature = identity.sign(&bytes);
        ModAction {
            target,
            sanction,
            reason,
            moderator,
            created_at,
            signature,
        }
    }

    /// Verify the signature against the moderator's key.
    pub fn verify(&self) -> Result<()> {
        let bytes = Self::signing_bytes(
            &self.target,
            &self.sanction,
            &self.reason,
            &self.moderator,
            &self.created_at,
        );
        self.moderator.verify(&bytes, &self.signature)
    }
}

/// An agent's current moderation standing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModStatus {
    /// Banned (indefinite).
    pub banned: bool,
    /// Muted (indefinite).
    pub muted: bool,
    /// In a timeout until this instant (if any, and still in the future).
    pub timed_out_until: Option<DateTime<Utc>>,
}

impl ModStatus {
    /// Whether the agent may post right now.
    pub fn can_post(&self, now: DateTime<Utc>) -> bool {
        !self.banned && !self.muted && self.timed_out_until.is_none_or(|u| now >= u)
    }
}

/// The audited moderation log; latest verified action per target decides status.
#[derive(Default, Debug)]
pub struct ModerationLog {
    actions: Vec<ModAction>,
}

impl ModerationLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an action after verifying its signature (forged → rejected).
    pub fn record(&mut self, action: ModAction) -> Result<()> {
        action.verify()?;
        self.actions.push(action);
        Ok(())
    }

    /// The standing of `target`: the most recent action wins (`Lift` clears).
    pub fn status(&self, target: &AgentId) -> ModStatus {
        let latest = self
            .actions
            .iter()
            .filter(|a| a.target == *target)
            .max_by_key(|a| a.created_at);
        match latest.map(|a| a.sanction) {
            Some(Sanction::Ban) => ModStatus {
                banned: true,
                muted: false,
                timed_out_until: None,
            },
            Some(Sanction::Mute) => ModStatus {
                banned: false,
                muted: true,
                timed_out_until: None,
            },
            Some(Sanction::Timeout { until }) => ModStatus {
                banned: false,
                muted: false,
                timed_out_until: Some(until),
            },
            _ => ModStatus {
                banned: false,
                muted: false,
                timed_out_until: None,
            },
        }
    }

    /// Whether `target` may post at `now` given their standing.
    pub fn can_post(&self, target: &AgentId, now: DateTime<Utc>) -> bool {
        self.status(target).can_post(now)
    }

    /// Distinct agents that have at least one recorded action (in first-seen order).
    pub fn targets(&self) -> Vec<AgentId> {
        let mut out: Vec<AgentId> = Vec::new();
        for a in &self.actions {
            if !out.contains(&a.target) {
                out.push(a.target);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn sign_verify_and_tamper() {
        let m = Identity::generate();
        let t = Identity::generate().id();
        let a = ModAction::sign(&m, t, Sanction::Ban, "spam", at("2026-06-30T05:00:00Z"));
        assert!(a.verify().is_ok());
        let mut forged = a.clone();
        forged.reason = "nope".into();
        assert!(forged.verify().is_err());
    }

    #[test]
    fn ban_and_lift() {
        let m = Identity::generate();
        let t = Identity::generate().id();
        let mut log = ModerationLog::new();
        assert!(log.can_post(&t, at("2026-06-30T05:00:00Z"))); // clean by default

        log.record(ModAction::sign(
            &m,
            t,
            Sanction::Ban,
            "spam",
            at("2026-06-30T05:00:00Z"),
        ))
        .unwrap();
        assert!(log.status(&t).banned);
        assert!(!log.can_post(&t, at("2026-06-30T06:00:00Z")));

        // A later Lift restores posting (latest action wins).
        log.record(ModAction::sign(
            &m,
            t,
            Sanction::Lift,
            "appeal granted",
            at("2026-06-30T07:00:00Z"),
        ))
        .unwrap();
        assert!(!log.status(&t).banned);
        assert!(log.can_post(&t, at("2026-06-30T08:00:00Z")));
    }

    #[test]
    fn timeout_expires() {
        let m = Identity::generate();
        let t = Identity::generate().id();
        let mut log = ModerationLog::new();
        log.record(ModAction::sign(
            &m,
            t,
            Sanction::Timeout {
                until: at("2026-06-30T06:00:00Z"),
            },
            "cool off",
            at("2026-06-30T05:00:00Z"),
        ))
        .unwrap();
        assert!(!log.can_post(&t, at("2026-06-30T05:30:00Z"))); // during
        assert!(log.can_post(&t, at("2026-06-30T06:30:00Z"))); // after expiry
    }

    #[test]
    fn forged_action_not_recorded() {
        let m = Identity::generate();
        let t = Identity::generate().id();
        let mut a = ModAction::sign(&m, t, Sanction::Mute, "", at("2026-06-30T05:00:00Z"));
        a.target = Identity::generate().id(); // change target after signing
        let mut log = ModerationLog::new();
        assert!(log.record(a).is_err());
    }
}
