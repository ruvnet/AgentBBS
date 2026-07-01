//! Slack inbound bridge (ADR-0025 Phase 1 — completes the inbound half; the
//! outbound mirror already ships in `agentbbs-bridge`).
//!
//! A public Slack Events API webhook is Internet-facing, so unlike the IRC
//! bridge (a private/internal TCP listener) this endpoint MUST verify Slack's
//! request signature — otherwise anyone could POST a forged event and get a
//! genuine, correctly bridge-signed board post that falsely claims to be
//! "from Slack". Verification follows Slack's documented v0 scheme exactly:
//! `HMAC-SHA256("v0:{timestamp}:{raw_body}", signing_secret)` must equal the
//! `X-Slack-Signature` header, and the timestamp must be within a 5-minute
//! window (replay protection). The signing secret lives only in the server's
//! environment (`AGENTBBS_SLACK_SIGNING_SECRET`) — never logged, never
//! shipped to the Pages site, same boundary as every other cog_/API key in
//! this codebase.
//!
//! Once verified, an inbound `message` event on an allowlisted channel is
//! bridge-signed via the same `agentbbs_bridge::{BridgeIdentity, sign_inbound,
//! SeenSet}` primitives the IRC bridge already uses — `platform: "slack"`
//! slots in exactly like `"irc"` did.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The replay window Slack recommends: reject requests whose timestamp is
/// more than 5 minutes away from "now".
const REPLAY_WINDOW_SECS: i64 = 300;

/// Verify a Slack Events API request signature. `now` is passed in (not read
/// from the clock) so this is deterministic and testable.
pub fn verify_signature(
    signing_secret: &str,
    timestamp: &str,
    body: &str,
    signature: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Ok(ts) = timestamp.parse::<i64>() else {
        return false;
    };
    if (now.timestamp() - ts).abs() > REPLAY_WINDOW_SECS {
        return false;
    }
    let Some(sig_hex) = signature.strip_prefix("v0=") else {
        return false;
    };
    let Ok(expected) = hex::decode(sig_hex) else {
        return false;
    };
    let base = format!("v0:{timestamp}:{body}");
    let Ok(mut mac) = HmacSha256::new_from_slice(signing_secret.as_bytes()) else {
        return false;
    };
    mac.update(base.as_bytes());
    mac.verify_slice(&expected).is_ok()
}

/// A minimal, already-validated inbound Slack message event.
#[derive(Debug, PartialEq)]
pub struct SlackMessage {
    pub team_id: String,
    pub channel: String,
    pub user: String,
    pub text: String,
    pub ts: String,
}

/// What a parsed Slack Events API payload means for the bridge.
#[derive(Debug, PartialEq)]
pub enum SlackEvent {
    /// The one-time endpoint-ownership handshake — echo `challenge` back.
    UrlVerification { challenge: String },
    /// A real chat message to potentially bridge.
    Message(SlackMessage),
    /// Anything else (reactions, bot's own echoed messages, edits, etc.) —
    /// deliberately ignored, not an error.
    Ignored,
}

/// Parse a Slack Events API JSON payload. Bot-authored messages (`bot_id`
/// present) are ignored so the bridge never re-ingests its own outbound
/// mirror or another bot's traffic — the loop-guard equivalent of the IRC
/// bridge's `SeenSet`, at the parse layer instead of after signing.
pub fn parse_event(payload: &serde_json::Value) -> SlackEvent {
    if payload["type"] == "url_verification" {
        return SlackEvent::UrlVerification {
            challenge: payload["challenge"].as_str().unwrap_or("").to_string(),
        };
    }
    if payload["type"] != "event_callback" {
        return SlackEvent::Ignored;
    }
    let event = &payload["event"];
    if event["type"] != "message" || !event["bot_id"].is_null() {
        return SlackEvent::Ignored;
    }
    let (team_id, channel, user, text, ts) = (
        payload["team_id"].as_str(),
        event["channel"].as_str(),
        event["user"].as_str(),
        event["text"].as_str(),
        event["ts"].as_str(),
    );
    match (team_id, channel, user, text, ts) {
        (Some(team_id), Some(channel), Some(user), Some(text), Some(ts)) => {
            SlackEvent::Message(SlackMessage {
                team_id: team_id.to_string(),
                channel: channel.to_string(),
                user: user.to_string(),
                text: text.to_string(),
                ts: ts.to_string(),
            })
        }
        _ => SlackEvent::Ignored,
    }
}

/// Parse `"C0123:general,C0456:agents.dev"` into a channel→board map —
/// same opt-in-allowlist shape and parser style as the IRC bridge's
/// `parse_channel_map`.
pub fn parse_channel_map(spec: &str) -> std::collections::HashMap<String, String> {
    spec.split(',')
        .filter_map(|pair| {
            let mut it = pair.splitn(2, ':');
            let ch = it.next()?.trim();
            let board = it.next()?.trim();
            if ch.is_empty() || board.is_empty() {
                return None;
            }
            Some((ch.to_string(), board.to_string()))
        })
        .collect()
}

/// Parse a 64-hex-char seed into 32 bytes, or `None` on any malformed input.
pub fn parse_seed_hex(hex_str: &str) -> Option<[u8; 32]> {
    hex::decode(hex_str.trim()).ok()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, timestamp: &str, body: &str) -> String {
        let base = format!("v0:{timestamp}:{body}");
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn verifies_a_correctly_signed_request() {
        let now = chrono::Utc::now();
        let ts = now.timestamp().to_string();
        let body = r#"{"type":"event_callback"}"#;
        let sig = sign("shh-its-secret", &ts, body);
        assert!(verify_signature("shh-its-secret", &ts, body, &sig, now));
    }

    #[test]
    fn rejects_a_signature_from_the_wrong_secret() {
        let now = chrono::Utc::now();
        let ts = now.timestamp().to_string();
        let body = r#"{"type":"event_callback"}"#;
        let sig = sign("the-real-secret", &ts, body);
        assert!(!verify_signature(
            "a-different-secret",
            &ts,
            body,
            &sig,
            now
        ));
    }

    #[test]
    fn rejects_a_tampered_body() {
        let now = chrono::Utc::now();
        let ts = now.timestamp().to_string();
        let sig = sign("shh-its-secret", &ts, r#"{"type":"event_callback"}"#);
        assert!(!verify_signature(
            "shh-its-secret",
            &ts,
            r#"{"type":"something_else"}"#,
            &sig,
            now
        ));
    }

    #[test]
    fn rejects_a_replayed_old_timestamp() {
        let now = chrono::Utc::now();
        let old_ts = (now - chrono::Duration::seconds(600))
            .timestamp()
            .to_string();
        let body = r#"{"type":"event_callback"}"#;
        let sig = sign("shh-its-secret", &old_ts, body);
        assert!(!verify_signature(
            "shh-its-secret",
            &old_ts,
            body,
            &sig,
            now
        ));
    }

    #[test]
    fn parses_url_verification_challenge() {
        let payload = serde_json::json!({"type": "url_verification", "challenge": "abc123"});
        assert_eq!(
            parse_event(&payload),
            SlackEvent::UrlVerification {
                challenge: "abc123".into()
            }
        );
    }

    #[test]
    fn parses_a_real_message_event() {
        let payload = serde_json::json!({
            "type": "event_callback",
            "team_id": "T1",
            "event": {"type": "message", "channel": "C1", "user": "U1", "text": "hi board", "ts": "1699999999.0001"}
        });
        assert_eq!(
            parse_event(&payload),
            SlackEvent::Message(SlackMessage {
                team_id: "T1".into(),
                channel: "C1".into(),
                user: "U1".into(),
                text: "hi board".into(),
                ts: "1699999999.0001".into(),
            })
        );
    }

    #[test]
    fn ignores_bot_authored_messages() {
        let payload = serde_json::json!({
            "type": "event_callback",
            "team_id": "T1",
            "event": {"type": "message", "channel": "C1", "user": "U1", "text": "echo", "ts": "1", "bot_id": "B1"}
        });
        assert_eq!(parse_event(&payload), SlackEvent::Ignored);
    }

    #[test]
    fn ignores_non_message_events() {
        let payload = serde_json::json!({
            "type": "event_callback",
            "team_id": "T1",
            "event": {"type": "reaction_added"}
        });
        assert_eq!(parse_event(&payload), SlackEvent::Ignored);
    }

    #[test]
    fn channel_map_parses_and_skips_malformed_pairs() {
        let m = parse_channel_map("C0123:general, C0456:agents.dev,bad,:x,y:");
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("C0123"), Some(&"general".to_string()));
        assert_eq!(m.get("C0456"), Some(&"agents.dev".to_string()));
    }

    #[test]
    fn seed_hex_round_trips() {
        let hex_str = "43ee46a3b62cc120a0fdb63523aed147245e18b24bca232ceedbea5de6a278bc";
        let seed = parse_seed_hex(hex_str).unwrap();
        assert_eq!(seed.len(), 32);
        assert_eq!(hex::encode(seed), hex_str);
    }

    #[test]
    fn seed_hex_rejects_wrong_length() {
        assert!(parse_seed_hex("deadbeef").is_none());
    }
}
