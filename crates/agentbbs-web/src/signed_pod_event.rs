//! Exact receiver for meta-llm ADR-208 `SignedPodEvent` callbacks.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::Sha256;

pub const MAX_CLOCK_SKEW_MS: i64 = 300_000;

#[derive(Debug, Deserialize, Serialize)]
pub struct SignedPodEvent {
    pub pod_id: String,
    pub account_id: String,
    pub event: String,
    pub status: String,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub step: Option<Step>,
    #[serde(default)]
    pub conformance: Option<Conformance>,
    pub committed_usd: f64,
    #[serde(default)]
    pub pause_reason: Option<String>,
    #[serde(default)]
    pub proposal_id: Option<String>,
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub mock: Option<bool>,
    pub event_id: String,
    pub ts: i64,
    pub signature: String,
    pub signing_key_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Step {
    pub summary: String,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Conformance {
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub status: Option<String>,
}

fn scalar_string(v: &Value) -> Result<String, String> {
    match v {
        // JavaScript's JSON.stringify number rendering is part of ADR-208's
        // wire contract. `serde_json` preserves `1.0`, while JS emits `1`;
        // ryu-js reproduces ECMAScript's shortest representation exactly.
        Value::Number(n) if n.is_f64() => n
            .as_f64()
            .map(|v| ryu_js::Buffer::new().format(v).to_owned())
            .ok_or_else(|| "cannot canonicalize event number".into()),
        _ => serde_json::to_string(v).map_err(|_| "cannot canonicalize event".into()),
    }
}

pub fn canonical(event: &SignedPodEvent) -> Result<String, String> {
    let mut fields = Map::new();
    fields.insert("pod_id".into(), Value::String(event.pod_id.clone()));
    fields.insert("account_id".into(), Value::String(event.account_id.clone()));
    fields.insert("event".into(), Value::String(event.event.clone()));
    fields.insert("status".into(), Value::String(event.status.clone()));
    fields.insert(
        "tier".into(),
        event.tier.clone().map(Value::String).unwrap_or(Value::Null),
    );
    fields.insert(
        "committed_usd".into(),
        serde_json::to_value(event.committed_usd).map_err(|_| "invalid committed_usd")?,
    );
    fields.insert(
        "pause_reason".into(),
        event
            .pause_reason
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "proposal_id".into(),
        event
            .proposal_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "verdict".into(),
        event
            .verdict
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "mock".into(),
        event.mock.map(Value::Bool).unwrap_or(Value::Null),
    );
    fields.insert(
        "step_summary".into(),
        event
            .step
            .as_ref()
            .map(|v| Value::String(v.summary.clone()))
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "step_tokens".into(),
        event
            .step
            .as_ref()
            .map(|v| Value::from(v.tokens))
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "step_cost_usd".into(),
        event
            .step
            .as_ref()
            .map(|v| serde_json::to_value(v.cost_usd).unwrap())
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "conformance_status".into(),
        event
            .conformance
            .as_ref()
            .and_then(|v| v.status.clone())
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "conformance_score".into(),
        event
            .conformance
            .as_ref()
            .and_then(|v| v.score)
            .map(|v| serde_json::to_value(v).unwrap())
            .unwrap_or(Value::Null),
    );
    fields.insert("event_id".into(), Value::String(event.event_id.clone()));
    fields.insert("ts".into(), Value::from(event.ts));
    let mut keys: Vec<_> = fields.keys().collect();
    keys.sort();
    let parts: Result<Vec<String>, String> = keys
        .iter()
        .map(|k| {
            Ok(format!(
                "{}:{}",
                scalar_string(&Value::String((*k).clone()))?,
                scalar_string(&fields[*k])?
            ))
        })
        .collect();
    Ok(format!("{{{}}}", parts?.join(",")))
}

pub fn verify(
    event: &SignedPodEvent,
    secret: &str,
    expected_kid: &str,
    expected_account: &str,
    route_pod: &str,
    now_ms: i64,
) -> Result<(), String> {
    if secret.is_empty() || expected_kid.is_empty() || expected_account.is_empty() {
        return Err("pod result verifier is not configured".into());
    }
    if event.signing_key_id != expected_kid
        || event.account_id != expected_account
        || event.pod_id != route_pod
    {
        return Err("pod result binding mismatch".into());
    }
    if event.event_id.is_empty()
        || event.event_id.len() > 160
        || event.ts.abs_diff(now_ms) > MAX_CLOCK_SKEW_MS as u64
    {
        return Err("pod result event id or timestamp invalid".into());
    }
    if !event.committed_usd.is_finite() || event.committed_usd < 0.0 {
        return Err("invalid committed_usd".into());
    }
    if let Some(step) = &event.step {
        if !step.cost_usd.is_finite() || step.cost_usd < 0.0 {
            return Err("invalid step cost".into());
        }
    }
    if let Some(score) = event.conformance.as_ref().and_then(|v| v.score) {
        if !score.is_finite() || !(0.0..=1.0).contains(&score) {
            return Err("invalid conformance score".into());
        }
    }
    let supplied = hex::decode(&event.signature).map_err(|_| "invalid pod result signature")?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| "invalid pod result key")?;
    mac.update(canonical(event)?.as_bytes());
    mac.verify_slice(&supplied)
        .map_err(|_| "invalid pod result signature".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> SignedPodEvent {
        SignedPodEvent {
            pod_id: "pod-1".into(),
            account_id: "acct-1".into(),
            event: "step".into(),
            status: "EXECUTING".into(),
            tier: Some("low".into()),
            step: Some(Step {
                summary: "safe".into(),
                tokens: 12,
                cost_usd: 0.25,
            }),
            conformance: Some(Conformance {
                score: Some(1.0),
                status: Some("CONFORMANT".into()),
            }),
            committed_usd: 0.25,
            pause_reason: None,
            proposal_id: None,
            verdict: None,
            mock: Some(false),
            event_id: "evt-1".into(),
            ts: 1_700_000_000_000,
            signature: String::new(),
            signing_key_id: "kid-current".into(),
        }
    }

    fn sign(mut event: SignedPodEvent, secret: &str) -> SignedPodEvent {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(canonical(&event).unwrap().as_bytes());
        event.signature = hex::encode(mac.finalize().into_bytes());
        event
    }

    #[test]
    fn verifies_exact_adr_208_event_and_rejects_every_binding_failure() {
        let base = sign(event(), "results-key");
        // Generated independently with meta-llm's TypeScript canonicalize +
        // createHmac implementation. This catches cross-language drift.
        assert_eq!(canonical(&base).unwrap(), "{\"account_id\":\"acct-1\",\"committed_usd\":0.25,\"conformance_score\":1,\"conformance_status\":\"CONFORMANT\",\"event\":\"step\",\"event_id\":\"evt-1\",\"mock\":false,\"pause_reason\":null,\"pod_id\":\"pod-1\",\"proposal_id\":null,\"status\":\"EXECUTING\",\"step_cost_usd\":0.25,\"step_summary\":\"safe\",\"step_tokens\":12,\"tier\":\"low\",\"ts\":1700000000000,\"verdict\":null}");
        assert_eq!(
            base.signature,
            "dbb247985ad872b72bc9409f034847f63bff74479e5a01503c58feece8347c56"
        );
        assert!(verify(
            &base,
            "results-key",
            "kid-current",
            "acct-1",
            "pod-1",
            base.ts
        )
        .is_ok());

        let cases = [
            (
                "wrong secret",
                verify(
                    &base,
                    "admin-key",
                    "kid-current",
                    "acct-1",
                    "pod-1",
                    base.ts,
                ),
            ),
            (
                "wrong kid",
                verify(&base, "results-key", "kid-old", "acct-1", "pod-1", base.ts),
            ),
            (
                "wrong account",
                verify(
                    &base,
                    "results-key",
                    "kid-current",
                    "acct-2",
                    "pod-1",
                    base.ts,
                ),
            ),
            (
                "wrong pod",
                verify(
                    &base,
                    "results-key",
                    "kid-current",
                    "acct-1",
                    "pod-2",
                    base.ts,
                ),
            ),
            (
                "stale",
                verify(
                    &base,
                    "results-key",
                    "kid-current",
                    "acct-1",
                    "pod-1",
                    base.ts + MAX_CLOCK_SKEW_MS + 1,
                ),
            ),
            (
                "future",
                verify(
                    &base,
                    "results-key",
                    "kid-current",
                    "acct-1",
                    "pod-1",
                    base.ts - MAX_CLOCK_SKEW_MS - 1,
                ),
            ),
        ];
        for (name, result) in cases {
            assert!(result.is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_tampering_and_non_finite_or_negative_telemetry() {
        let mut tampered = sign(event(), "results-key");
        tampered.step.as_mut().unwrap().summary = "forged".into();
        assert!(verify(
            &tampered,
            "results-key",
            "kid-current",
            "acct-1",
            "pod-1",
            tampered.ts
        )
        .is_err());

        for bad in [-1.0, f64::INFINITY, f64::NAN] {
            let mut candidate = event();
            candidate.committed_usd = bad;
            let candidate = sign(candidate, "results-key");
            assert!(verify(
                &candidate,
                "results-key",
                "kid-current",
                "acct-1",
                "pod-1",
                candidate.ts
            )
            .is_err());
        }
    }
}
