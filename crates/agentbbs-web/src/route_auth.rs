//! Caller authentication for the approval and agent-reply routes.
//!
//! * Approval decisions count only when the decider is one of the operator's
//!   configured approvers (`AGENTBBS_APPROVER_IDS`). A key the caller generated
//!   for themselves is not an approver. An empty set authorizes nothing.
//! * `POST /api/agent-reply` must be signed by the caller's own Ed25519
//!   identity, over a domain-separated canonical form of the request, with a
//!   short freshness window. The server rate-limits and moderates by that
//!   identity before any model call is made.

use agentbbs_core::identity::{AgentId, SignatureBytes};
use chrono::{DateTime, Utc};
use serde::Deserialize;

/// How far in the past a signed agent-reply request may be.
pub const AGENT_REPLY_MAX_AGE_SECONDS: i64 = 300;
/// Clock skew tolerated for a request dated in the future.
pub const AGENT_REPLY_MAX_SKEW_SECONDS: i64 = 60;

/// Parse a comma/whitespace separated list of hex agent ids. Entries that do
/// not parse are dropped, so a typo narrows the set rather than widening it.
pub fn parse_approver_ids(raw: &str) -> Vec<AgentId> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .filter_map(|s| AgentId::from_hex(s).ok())
        .collect()
}

/// The approver set configured for this node.
pub fn approver_ids() -> Vec<AgentId> {
    parse_approver_ids(&std::env::var("AGENTBBS_APPROVER_IDS").unwrap_or_default())
}

/// Whether `decider` may record an approval decision.
pub fn check_approver(decider: &AgentId, approvers: &[AgentId]) -> Result<(), &'static str> {
    if approvers.is_empty() {
        return Err("approvals are not configured on this node");
    }
    if approvers.iter().any(|a| a == decider) {
        Ok(())
    } else {
        Err("decider is not an approver on this node")
    }
}

/// A signed `POST /api/agent-reply` request.
#[derive(Deserialize)]
pub struct SignedAgentReply {
    pub agent: String,
    pub text: String,
    /// Hex public key of the caller. Missing auth fields default to empty and
    /// then fail verification, so an unsigned request is a 401, not a 422.
    #[serde(default)]
    pub requester: String,
    #[serde(default)]
    /// RFC3339 creation time.
    pub created_at: String,
    #[serde(default)]
    /// Hex Ed25519 signature over [`agent_reply_bytes`].
    pub signature: String,
}

/// Canonical signed bytes. Must match `agentReplyBytes` in `bbscrypto.js`.
pub fn agent_reply_bytes(agent: &str, text: &str, requester: &str, created_at: &str) -> Vec<u8> {
    let mut out = b"agentbbs.agent-reply.v1\n".to_vec();
    for field in [agent, text, requester, created_at] {
        out.extend_from_slice(format!("{}:", field.len()).as_bytes());
        out.extend_from_slice(field.as_bytes());
        out.push(b'\n');
    }
    out
}

/// A verified agent-reply request.
pub struct VerifiedAgentReply {
    pub requester: AgentId,
    /// Signature hex; single-use within the freshness window.
    pub nonce: String,
    /// When the nonce may be forgotten.
    pub expires: i64,
}

/// Verify the caller's signature and freshness. Every failure is an
/// authentication failure (401) at the route.
pub fn verify_agent_reply(
    req: &SignedAgentReply,
    now: DateTime<Utc>,
) -> Result<VerifiedAgentReply, &'static str> {
    let requester = AgentId::from_hex(&req.requester).map_err(|_| "invalid requester")?;
    let signature = SignatureBytes::from_hex(&req.signature).map_err(|_| "invalid signature")?;
    let created = DateTime::parse_from_rfc3339(&req.created_at)
        .map_err(|_| "invalid created_at")?
        .with_timezone(&Utc);
    let age = now.signed_duration_since(created).num_seconds();
    if !(-AGENT_REPLY_MAX_SKEW_SECONDS..=AGENT_REPLY_MAX_AGE_SECONDS).contains(&age) {
        return Err("request is stale or dated in the future");
    }
    let bytes = agent_reply_bytes(&req.agent, &req.text, &req.requester, &req.created_at);
    requester
        .verify(&bytes, &signature)
        .map_err(|_| "signature verification failed")?;
    Ok(VerifiedAgentReply {
        requester,
        nonce: req.signature.to_ascii_lowercase(),
        expires: created.timestamp() + AGENT_REPLY_MAX_AGE_SECONDS + AGENT_REPLY_MAX_SKEW_SECONDS,
    })
}

#[cfg(test)]
pub fn sign_agent_reply(
    identity: &agentbbs_core::Identity,
    agent: &str,
    text: &str,
    created_at: &str,
) -> serde_json::Value {
    let requester = identity.id().to_hex();
    let sig = identity.sign(&agent_reply_bytes(agent, text, &requester, created_at));
    serde_json::json!({
        "agent": agent, "text": text, "requester": requester,
        "created_at": created_at, "signature": sig.to_hex(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentbbs_core::Identity;

    fn parse(v: serde_json::Value) -> SignedAgentReply {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn approver_set_is_exact_and_empty_means_none() {
        let a = Identity::generate().id();
        let b = Identity::generate().id();
        let set = parse_approver_ids(&format!("{}, not-hex {}", a.to_hex(), ""));
        assert_eq!(set.len(), 1);
        assert!(check_approver(&a, &set).is_ok());
        assert!(check_approver(&b, &set).is_err());
        assert!(check_approver(&a, &[]).is_err());
    }

    #[test]
    fn agent_reply_signature_freshness_and_binding() {
        let me = Identity::generate();
        let now = Utc::now();
        let at = now.to_rfc3339();
        let ok = parse(sign_agent_reply(&me, "claude", "hi", &at));
        assert!(verify_agent_reply(&ok, now).is_ok());

        let mut tampered = sign_agent_reply(&me, "claude", "hi", &at);
        tampered["text"] = "spend more".into();
        assert!(verify_agent_reply(&parse(tampered), now).is_err());

        let mut other_key = sign_agent_reply(&me, "claude", "hi", &at);
        other_key["requester"] = Identity::generate().id().to_hex().into();
        assert!(verify_agent_reply(&parse(other_key), now).is_err());

        let old = (now - chrono::Duration::seconds(AGENT_REPLY_MAX_AGE_SECONDS + 1)).to_rfc3339();
        assert!(
            verify_agent_reply(&parse(sign_agent_reply(&me, "claude", "hi", &old)), now).is_err()
        );
        let future =
            (now + chrono::Duration::seconds(AGENT_REPLY_MAX_SKEW_SECONDS + 5)).to_rfc3339();
        assert!(
            verify_agent_reply(&parse(sign_agent_reply(&me, "claude", "hi", &future)), now)
                .is_err()
        );
    }
}
