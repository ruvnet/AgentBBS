//! Route-scoped, single-use administration proofs for high-impact HTTP actions.

use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;

pub const SPAWN: &str = "pods.spawn";
pub const TOPUP: &str = "budget.topup";
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

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("missing or invalid {name}"))
}

pub fn canonical(exp: i64, audience: &str, action: &str, jti: &str) -> String {
    format!(
        "agentbbs.admin-action.v2\nver=2\nrole=sysop\nexp={exp}\naud={audience}\naction={action}\njti={jti}"
    )
}

pub fn verify(
    headers: &HeaderMap,
    expected_action: &str,
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

    if version != "2"
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
    mac.update(canonical(exp, audience, action, jti).as_bytes());
    mac.verify_slice(&supplied)
        .map_err(|_| "invalid admin proof signature")?;
    Ok(VerifiedProof {
        jti: jti.into(),
        exp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn proof(action: &str, exp: i64, secret: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in [
            ("x-agentbbs-admin-ver", "2".to_string()),
            ("x-agentbbs-admin-role", "sysop".to_string()),
            ("x-agentbbs-admin-exp", exp.to_string()),
            ("x-agentbbs-admin-aud", "agentbbs-prod".to_string()),
            ("x-agentbbs-admin-action", action.to_string()),
            ("x-agentbbs-admin-jti", "request-1".to_string()),
        ] {
            h.insert(name, HeaderValue::from_str(&value).unwrap());
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(canonical(exp, "agentbbs-prod", action, "request-1").as_bytes());
        h.insert(
            "x-agentbbs-admin-sig",
            HeaderValue::from_str(&hex::encode(mac.finalize().into_bytes())).unwrap(),
        );
        h
    }

    #[test]
    fn route_scope_expiry_signature_and_replay_are_enforced() {
        let now = 1_700_000_000;
        let headers = proof(SPAWN, now + 60, "admin-key");
        let verified = verify(&headers, SPAWN, "admin-key", "agentbbs-prod", now).unwrap();
        assert!(verify(&headers, TOPUP, "admin-key", "agentbbs-prod", now).is_err());
        assert!(verify(&headers, SPAWN, "results-key", "agentbbs-prod", now).is_err());
        assert!(verify(&headers, SPAWN, "admin-key", "other", now).is_err());
        assert!(verify(
            &proof(SPAWN, now, "admin-key"),
            SPAWN,
            "admin-key",
            "agentbbs-prod",
            now
        )
        .is_err());
        assert!(verify(
            &proof(SPAWN, now + 301, "admin-key"),
            SPAWN,
            "admin-key",
            "agentbbs-prod",
            now
        )
        .is_err());
        let mut guard = ReplayGuard::default();
        assert!(guard.consume(&verified.jti, verified.exp, now).is_ok());
        assert!(guard.consume(&verified.jti, verified.exp, now).is_err());
    }
}
