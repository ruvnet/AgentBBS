//! Route-scoped, single-use administration proofs for high-impact HTTP actions.
//!
//! `agentbbs.admin-action.v3` binds the proof to the node audience, the route
//! action, a short expiry, a single-use `jti`, and the exact request: its
//! method, path (which carries any path parameter such as a run id) and the
//! SHA-256 of the raw body. A proof minted for one pod, amount or run cannot be
//! replayed against another. v2 proofs (no request binding) are rejected.

use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const SPAWN: &str = "pods.spawn";
pub const TOPUP: &str = "budget.topup";
pub const PLAYBOOK_RUN: &str = "playbooks.run";
pub const RUN_ADVANCE: &str = "runs.advance";
const MAX_FUTURE_SECONDS: i64 = 300;

#[derive(Default)]
pub struct ReplayGuard {
    used: HashMap<String, i64>,
}

impl ReplayGuard {
    pub fn consume(&mut self, jti: &str, exp: i64, now: i64) -> Result<(), String> {
        self.used.retain(|_, expiry| *expiry > now);
        if self.used.contains_key(jti) {
            return Err("admin proof replayed".into());
        }
        self.used.insert(jti.to_owned(), exp);
        Ok(())
    }
}

pub struct VerifiedProof {
    pub jti: String,
    pub exp: i64,
}

/// The request a proof is bound to.
pub struct BoundRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub body: &'a [u8],
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("missing or invalid {name}"))
}

pub fn body_sha256(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

pub fn canonical(exp: i64, audience: &str, action: &str, jti: &str, req: &BoundRequest) -> String {
    format!(
        "agentbbs.admin-action.v3\nver=3\nrole=sysop\nexp={exp}\naud={audience}\naction={action}\njti={jti}\nmethod={}\npath={}\nbody_sha256={}",
        req.method,
        req.path,
        body_sha256(req.body)
    )
}

pub fn verify(
    headers: &HeaderMap,
    expected_action: &str,
    req: &BoundRequest,
    secret: &str,
    audience: &str,
    now: i64,
) -> Result<VerifiedProof, String> {
    if secret.is_empty() || audience.is_empty() {
        return Err("admin action verifier is not configured".into());
    }
    let version = header(headers, "x-agentbbs-admin-ver")?;
    let role = header(headers, "x-agentbbs-admin-role")?;
    let exp_text = header(headers, "x-agentbbs-admin-exp")?;
    let supplied_audience = header(headers, "x-agentbbs-admin-aud")?;
    let action = header(headers, "x-agentbbs-admin-action")?;
    let jti = header(headers, "x-agentbbs-admin-jti")?;
    let signature = header(headers, "x-agentbbs-admin-sig")?;

    if version != "3"
        || role != "sysop"
        || supplied_audience != audience
        || action != expected_action
    {
        return Err("admin proof claims do not match this action".into());
    }
    if jti.len() > 128
        || !jti
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return Err("invalid admin proof jti".into());
    }
    let exp: i64 = exp_text.parse().map_err(|_| "invalid admin proof exp")?;
    if exp <= now || exp > now + MAX_FUTURE_SECONDS {
        return Err("admin proof expired or exceeds maximum lifetime".into());
    }
    let supplied = hex::decode(signature).map_err(|_| "invalid admin proof signature")?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| "invalid admin key")?;
    mac.update(canonical(exp, audience, action, jti, req).as_bytes());
    mac.verify_slice(&supplied)
        .map_err(|_| "invalid admin proof signature")?;
    Ok(VerifiedProof {
        jti: jti.into(),
        exp,
    })
}

/// Mint proof headers. Used by the route tests; the production minter is the
/// operator's admin console, which must produce the same canonical bytes.
#[cfg(test)]
pub fn mint(
    action: &str,
    exp: i64,
    audience: &str,
    jti: &str,
    secret: &str,
    req: &BoundRequest,
) -> HeaderMap {
    use axum::http::HeaderValue;
    let mut h = HeaderMap::new();
    for (name, value) in [
        ("x-agentbbs-admin-ver", "3".to_string()),
        ("x-agentbbs-admin-role", "sysop".to_string()),
        ("x-agentbbs-admin-exp", exp.to_string()),
        ("x-agentbbs-admin-aud", audience.to_string()),
        ("x-agentbbs-admin-action", action.to_string()),
        ("x-agentbbs-admin-jti", jti.to_string()),
    ] {
        h.insert(name, HeaderValue::from_str(&value).unwrap());
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(canonical(exp, audience, action, jti, req).as_bytes());
    h.insert(
        "x-agentbbs-admin-sig",
        HeaderValue::from_str(&hex::encode(mac.finalize().into_bytes())).unwrap(),
    );
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQ: BoundRequest = BoundRequest {
        method: "POST",
        path: "/api/budget/topup",
        body: br#"{"pod_id":"pod-0000","amount":1.0}"#,
    };

    fn proof(action: &str, exp: i64, secret: &str) -> HeaderMap {
        mint(action, exp, "agentbbs-prod", "request-1", secret, &REQ)
    }

    #[test]
    fn route_scope_expiry_signature_and_replay_are_enforced() {
        let now = 1_700_000_000;
        let headers = proof(SPAWN, now + 60, "admin-key");
        let verified = verify(&headers, SPAWN, &REQ, "admin-key", "agentbbs-prod", now).unwrap();
        assert!(verify(&headers, TOPUP, &REQ, "admin-key", "agentbbs-prod", now).is_err());
        assert!(verify(&headers, SPAWN, &REQ, "results-key", "agentbbs-prod", now).is_err());
        assert!(verify(&headers, SPAWN, &REQ, "admin-key", "other", now).is_err());
        assert!(verify(
            &proof(SPAWN, now, "admin-key"),
            SPAWN,
            &REQ,
            "admin-key",
            "agentbbs-prod",
            now
        )
        .is_err());
        assert!(verify(
            &proof(SPAWN, now + 301, "admin-key"),
            SPAWN,
            &REQ,
            "admin-key",
            "agentbbs-prod",
            now
        )
        .is_err());
        let mut guard = ReplayGuard::default();
        assert!(guard.consume(&verified.jti, verified.exp, now).is_ok());
        assert!(guard.consume(&verified.jti, verified.exp, now).is_err());
    }

    #[test]
    fn proof_is_bound_to_body_path_and_method() {
        let now = 1_700_000_000;
        let headers = proof(TOPUP, now + 60, "admin-key");
        assert!(verify(&headers, TOPUP, &REQ, "admin-key", "agentbbs-prod", now).is_ok());
        let other_body = BoundRequest {
            body: br#"{"pod_id":"pod-0000","amount":500.0}"#,
            ..REQ
        };
        assert!(verify(
            &headers,
            TOPUP,
            &other_body,
            "admin-key",
            "agentbbs-prod",
            now
        )
        .is_err());
        let other_path = BoundRequest {
            path: "/api/runs/run-0001/advance",
            ..REQ
        };
        assert!(verify(
            &headers,
            TOPUP,
            &other_path,
            "admin-key",
            "agentbbs-prod",
            now
        )
        .is_err());
        let other_method = BoundRequest {
            method: "PUT",
            ..REQ
        };
        assert!(verify(
            &headers,
            TOPUP,
            &other_method,
            "admin-key",
            "agentbbs-prod",
            now
        )
        .is_err());
    }

    #[test]
    fn v2_proofs_are_rejected() {
        let now = 1_700_000_000;
        let mut headers = proof(TOPUP, now + 60, "admin-key");
        headers.insert("x-agentbbs-admin-ver", "2".parse().unwrap());
        assert!(verify(&headers, TOPUP, &REQ, "admin-key", "agentbbs-prod", now).is_err());
    }
}
