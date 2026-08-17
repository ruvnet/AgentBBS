//! Teams inbound bridge (ADR-0055 Phase B + the pure Phase C JWKS slice — the
//! outbound mirror already ships in `agentbbs-bridge`. Phase C adds static-JWKS
//! validation (`parse_jwks` + `validate_jwt_jwks`); the production JWKS
//! *fetcher/cache/rotation* from the Bot Framework OpenID config, and the Azure
//! Bot Service registration, are the remaining Phase C follow-up).
//!
//! Like the Slack (`slack_bridge.rs`) and WhatsApp (`whatsapp_bridge.rs`)
//! bridges this is an Internet-facing webhook, so every POST is verified before
//! anything else happens — otherwise anyone could forge a Bot Framework Activity
//! and get a genuine, correctly bridge-signed board post falsely claiming to be
//! "from Teams". Teams inbound is the first bridge whose per-request auth is
//! asymmetric: instead of a shared-secret HMAC, each request carries an
//! `Authorization: Bearer <JWT>` issued by the Bot Framework / Azure AD, and
//! validation is RS256 signature + issuer + audience + expiry, fail-closed.
//!
//! Unlike Slack/WhatsApp there is **no GET handshake** — Teams has no
//! `hub.challenge` / `url_verification` echo — so this module has no
//! `verify_handshake` variant.
//!
//! Once verified, an inbound text Activity on an allowlisted conversation is
//! bridge-signed via the same `agentbbs_bridge::{BridgeIdentity, sign_inbound,
//! SeenSet}` primitives the Slack/WhatsApp/IRC bridges use — `platform:
//! "teams"` slots in exactly like `"slack"` did. Secrets (the bot App Id,
//! JWT issuer, decoding key PEM, bridge seed) live only in the server
//! environment, never logged or shipped.

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// A minimal, already-validated inbound Teams message.
#[derive(Debug, PartialEq)]
pub struct TeamsMessage {
    /// The channel/conversation id the message arrived on (allowlist key) —
    /// prefers `channelData.channel.id`, falling back to `conversation.id`.
    pub conversation: String,
    /// The sender's Teams id (`from.id`) — the per-source bridge subkey origin.
    pub user: String,
    /// The sender's display name (`from.name`), or empty if absent.
    pub name: String,
    pub text: String,
    /// The Activity id (`id`) — the loop-guard / dedupe key.
    pub activity_id: String,
}

/// What a parsed Bot Framework Activity means for the bridge.
#[derive(Debug, PartialEq)]
pub enum TeamsEvent {
    /// A real chat message to potentially bridge.
    Message(TeamsMessage),
    /// Anything else (non-`message` types, the bot's own echoes, activities with
    /// no text or missing required fields) — deliberately ignored, not an error.
    Ignored,
}

/// Parse a Bot Framework Activity JSON payload. The bot's own echoes
/// (`from.id == recipient.id`) and non-`message` activity types are dropped —
/// the parse-layer loop guard the Slack bridge uses, ahead of the downstream
/// `SeenSet` on the activity id.
pub fn parse_activity(payload: &serde_json::Value) -> TeamsEvent {
    if payload["type"] != "message" {
        return TeamsEvent::Ignored;
    }
    // Drop the bot's own echoes: when both are present and equal, this Activity
    // originated from the bot itself.
    if let (Some(from_id), Some(recipient_id)) = (
        payload["from"]["id"].as_str(),
        payload["recipient"]["id"].as_str(),
    ) {
        if from_id == recipient_id {
            return TeamsEvent::Ignored;
        }
    }
    // The allowlist key: prefer the channel id, fall back to the conversation id.
    let conversation = payload["channelData"]["channel"]["id"]
        .as_str()
        .or_else(|| payload["conversation"]["id"].as_str());
    let name = payload["from"]["name"].as_str().unwrap_or("");
    let (user, text, activity_id) = (
        payload["from"]["id"].as_str(),
        payload["text"].as_str(),
        payload["id"].as_str(),
    );
    match (conversation, user, text, activity_id) {
        (Some(conversation), Some(user), Some(text), Some(activity_id)) => {
            TeamsEvent::Message(TeamsMessage {
                conversation: conversation.to_string(),
                user: user.to_string(),
                name: name.to_string(),
                text: text.to_string(),
                activity_id: activity_id.to_string(),
            })
        }
        _ => TeamsEvent::Ignored,
    }
}

/// The subset of JWT claims we deserialize. Signature, issuer, audience, and
/// expiry are all validated by `jsonwebtoken` against the supplied key + config;
/// this struct just proves the payload deserializes.
#[derive(Debug, Deserialize)]
struct Claims {
    #[allow(dead_code)]
    aud: String,
    #[allow(dead_code)]
    iss: String,
}

/// Validate a Bot Framework Bearer JWT against a supplied RS256 decoding key.
/// Checks, fail-closed, all of: signature (RS256), issuer, audience, and expiry.
///
/// Returns `true` iff the token decodes and validates; `false` on any error
/// (bad key, bad signature, wrong issuer/audience, expired). This is the
/// *pure*, testable half of the auth flow — unlike the HMAC bridges we do NOT
/// inject `now`; `jsonwebtoken` owns the `exp`/`nbf` check against the real
/// clock. The *I/O* half — fetching, caching, and rotating the Bot Framework
/// JWKS to obtain `pubkey_pem` — is Phase C and lives outside this function.
pub fn validate_jwt(token: &str, pubkey_pem: &[u8], issuer: &str, audience: &str) -> bool {
    let Ok(key) = DecodingKey::from_rsa_pem(pubkey_pem) else {
        return false;
    };
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    jsonwebtoken::decode::<Claims>(token, &key, &validation).is_ok()
}

/// Parse a Bot Framework JWKS (JSON Web Key Set — `{"keys":[{kty,kid,n,e,...}]}`)
/// into a `kid -> (n, e)` map of base64url modulus/exponent strings. Only RSA
/// keys are kept: entries whose `kty` isn't `"RSA"`, or that are missing any of
/// `kid`/`n`/`e`, are silently skipped (a real JWKS may carry other key types).
/// This is the *pure* companion to `validate_jwt_jwks` — obtaining the JWKS by
/// fetching/caching/rotating it from the Bot Framework OpenID config is the
/// remaining Phase C I/O slice and lives outside this function.
pub fn parse_jwks(jwks: &serde_json::Value) -> std::collections::HashMap<String, (String, String)> {
    let mut map = std::collections::HashMap::new();
    let Some(keys) = jwks["keys"].as_array() else {
        return map;
    };
    for key in keys {
        if key["kty"].as_str() != Some("RSA") {
            continue;
        }
        if let (Some(kid), Some(n), Some(e)) =
            (key["kid"].as_str(), key["n"].as_str(), key["e"].as_str())
        {
            map.insert(kid.to_string(), (n.to_string(), e.to_string()));
        }
    }
    map
}

/// Validate a Bot Framework Bearer JWT against a supplied JWKS map (as produced
/// by `parse_jwks`), resolving the signing key by the token's `kid` header. Same
/// fail-closed RS256 signature + issuer + audience + expiry checks as
/// `validate_jwt`, just keyed by `kid` against a set of RSA components instead of
/// a single PEM.
///
/// Returns `true` iff the token's `kid` resolves to a key in the map and the
/// token decodes and validates; `false` on any error (no/absent `kid`, unknown
/// `kid`, bad components, bad signature, wrong issuer/audience, expired). As with
/// `validate_jwt` this is the *pure* half — `jsonwebtoken` owns the `exp`/`nbf`
/// check against the real clock, and the JWKS *fetch/cache/rotation* is the next
/// slice; this validates against an already-supplied JWKS.
pub fn validate_jwt_jwks(
    token: &str,
    jwks: &std::collections::HashMap<String, (String, String)>,
    issuer: &str,
    audience: &str,
) -> bool {
    let Ok(header) = jsonwebtoken::decode_header(token) else {
        return false;
    };
    let Some(kid) = header.kid else {
        return false;
    };
    let Some((n, e)) = jwks.get(&kid) else {
        return false;
    };
    let Ok(key) = DecodingKey::from_rsa_components(n, e) else {
        return false;
    };
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    jsonwebtoken::decode::<Claims>(token, &key, &validation).is_ok()
}

/// Parse `"19:abc@thread.tacv2:general,19:def@thread.tacv2:agents.dev"` into a
/// conversation-id→board allowlist. Opt-in, same comma-separated shape as the
/// Slack/WhatsApp bridges — but split on the **last** colon, not the first:
/// Teams conversation ids themselves contain colons (e.g.
/// `19:conv@thread.tacv2`), so the board is the final `:board` suffix and the
/// conversation id is everything before it.
pub fn parse_channel_map(spec: &str) -> std::collections::HashMap<String, String> {
    spec.split(',')
        .filter_map(|pair| {
            // rsplitn yields the trailing segment first: [board, conversation].
            let mut it = pair.rsplitn(2, ':');
            let board = it.next()?.trim();
            let conv = it.next()?.trim();
            if conv.is_empty() || board.is_empty() {
                return None;
            }
            Some((conv.to_string(), board.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use rsa::pkcs8::{DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding};
    use rsa::traits::PublicKeyParts;
    use rsa::{RsaPrivateKey, RsaPublicKey};

    /// Generate an RS256 keypair and return `(private_pem, public_pem)`.
    /// 2048-bit is the floor; the keys never leave the test process.
    fn gen_keypair() -> (String, String) {
        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);
        let private_pem = private_key
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap()
            .to_string();
        let public_pem = public_key.to_public_key_pem(LineEnding::LF).unwrap();
        (private_pem, public_pem)
    }

    /// Build a single-key JWKS JSON from a public PEM, tagging it with `kid`.
    /// Extracts the RSA modulus/exponent and base64url-encodes them the same way
    /// a real Bot Framework JWKS entry does.
    fn jwks_from_pem(public_pem: &str, kid: &str) -> serde_json::Value {
        let public_key = RsaPublicKey::from_public_key_pem(public_pem).unwrap();
        let n =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public_key.n().to_bytes_be());
        let e =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public_key.e().to_bytes_be());
        serde_json::json!({ "keys": [ { "kty": "RSA", "kid": kid, "n": n, "e": e } ] })
    }

    /// Sign a token carrying an explicit `kid` header — the JWKS-resolution key.
    fn sign_with_kid(private_pem: &str, iss: &str, aud: &str, kid: &str) -> String {
        let claims = TestClaims {
            aud: aud.to_string(),
            iss: iss.to_string(),
            exp: chrono::Utc::now().timestamp() + 3600,
        };
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_pem.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    #[derive(serde::Serialize)]
    struct TestClaims {
        aud: String,
        iss: String,
        exp: i64,
    }

    fn sign(private_pem: &str, iss: &str, aud: &str) -> String {
        let claims = TestClaims {
            aud: aud.to_string(),
            iss: iss.to_string(),
            exp: chrono::Utc::now().timestamp() + 3600,
        };
        let header = jsonwebtoken::Header::new(Algorithm::RS256);
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_pem.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    #[test]
    fn parses_a_real_message_activity() {
        let payload = serde_json::json!({
            "type": "message",
            "id": "1690000000000",
            "text": "hi board",
            "from": { "id": "29:user-aad-id", "name": "Ada Lovelace" },
            "recipient": { "id": "28:bot-app-id" },
            "conversation": { "id": "19:conv@thread.tacv2" }
        });
        assert_eq!(
            parse_activity(&payload),
            TeamsEvent::Message(TeamsMessage {
                conversation: "19:conv@thread.tacv2".into(),
                user: "29:user-aad-id".into(),
                name: "Ada Lovelace".into(),
                text: "hi board".into(),
                activity_id: "1690000000000".into(),
            })
        );
    }

    #[test]
    fn ignores_non_message_activity_types() {
        let payload = serde_json::json!({
            "type": "conversationUpdate",
            "id": "1",
            "from": { "id": "u" },
            "conversation": { "id": "c" }
        });
        assert_eq!(parse_activity(&payload), TeamsEvent::Ignored);
    }

    #[test]
    fn ignores_bot_own_echoes() {
        let payload = serde_json::json!({
            "type": "message",
            "id": "1",
            "text": "echo",
            "from": { "id": "28:bot-app-id" },
            "recipient": { "id": "28:bot-app-id" },
            "conversation": { "id": "19:conv@thread.tacv2" }
        });
        assert_eq!(parse_activity(&payload), TeamsEvent::Ignored);
    }

    #[test]
    fn ignores_missing_required_fields_and_textless_activities() {
        // No text at all (e.g. an attachment-only activity).
        let no_text = serde_json::json!({
            "type": "message",
            "id": "1",
            "from": { "id": "u" },
            "conversation": { "id": "c" }
        });
        assert_eq!(parse_activity(&no_text), TeamsEvent::Ignored);

        // No conversation/channel id.
        let no_conv = serde_json::json!({
            "type": "message",
            "id": "1",
            "text": "hi",
            "from": { "id": "u" }
        });
        assert_eq!(parse_activity(&no_conv), TeamsEvent::Ignored);

        // No from.id, and no id.
        let no_from = serde_json::json!({
            "type": "message",
            "id": "1",
            "text": "hi",
            "conversation": { "id": "c" }
        });
        assert_eq!(parse_activity(&no_from), TeamsEvent::Ignored);
        let no_id = serde_json::json!({
            "type": "message",
            "text": "hi",
            "from": { "id": "u" },
            "conversation": { "id": "c" }
        });
        assert_eq!(parse_activity(&no_id), TeamsEvent::Ignored);
    }

    #[test]
    fn prefers_channel_id_over_conversation_id() {
        let payload = serde_json::json!({
            "type": "message",
            "id": "1",
            "text": "hi",
            "from": { "id": "u", "name": "N" },
            "conversation": { "id": "19:conv@thread.tacv2" },
            "channelData": { "channel": { "id": "19:chan@thread.tacv2" } }
        });
        match parse_activity(&payload) {
            TeamsEvent::Message(m) => assert_eq!(m.conversation, "19:chan@thread.tacv2"),
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn channel_map_parses_and_skips_malformed_pairs() {
        let m = parse_channel_map("conv-a:general, conv-b:agents.dev,bad,:x,y:");
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("conv-a"), Some(&"general".to_string()));
        assert_eq!(m.get("conv-b"), Some(&"agents.dev".to_string()));
    }

    #[test]
    fn channel_map_handles_real_teams_ids_with_colons() {
        // Real Teams conversation ids contain colons; split on the LAST colon so
        // the board is the suffix and the full id is the key.
        let m = parse_channel_map("19:conv@thread.tacv2:general,19:other@thread.tacv2:agents.dev");
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("19:conv@thread.tacv2"), Some(&"general".to_string()));
        assert_eq!(
            m.get("19:other@thread.tacv2"),
            Some(&"agents.dev".to_string())
        );
    }

    #[test]
    fn validate_jwt_accepts_a_correctly_signed_token() {
        let (private_pem, public_pem) = gen_keypair();
        let token = sign(&private_pem, "https://api.botframework.com", "app-id-123");
        assert!(validate_jwt(
            &token,
            public_pem.as_bytes(),
            "https://api.botframework.com",
            "app-id-123"
        ));
    }

    #[test]
    fn validate_jwt_rejects_wrong_audience_and_issuer() {
        let (private_pem, public_pem) = gen_keypair();
        let token = sign(&private_pem, "https://api.botframework.com", "app-id-123");
        // Wrong audience.
        assert!(!validate_jwt(
            &token,
            public_pem.as_bytes(),
            "https://api.botframework.com",
            "other-app-id"
        ));
        // Wrong issuer.
        assert!(!validate_jwt(
            &token,
            public_pem.as_bytes(),
            "https://evil.example",
            "app-id-123"
        ));
    }

    #[test]
    fn validate_jwt_rejects_a_token_signed_by_a_different_key() {
        let (private_pem, _public_pem) = gen_keypair();
        let (_other_private, other_public) = gen_keypair();
        let token = sign(&private_pem, "https://api.botframework.com", "app-id-123");
        // Correct iss/aud, but the token was signed by a different keypair.
        assert!(!validate_jwt(
            &token,
            other_public.as_bytes(),
            "https://api.botframework.com",
            "app-id-123"
        ));
    }

    #[test]
    fn parse_jwks_extracts_rsa_and_skips_non_rsa_or_malformed() {
        let (_priv, public_pem) = gen_keypair();
        let good = jwks_from_pem(&public_pem, "kid-1");
        let good_key = &good["keys"][0];
        // A set mixing the good RSA key with an EC key and RSA entries missing
        // required fields — only the well-formed RSA key survives.
        let jwks = serde_json::json!({
            "keys": [
                good_key,
                { "kty": "EC", "kid": "ec-1", "crv": "P-256", "x": "aa", "y": "bb" },
                { "kty": "RSA", "kid": "no-n", "e": "AQAB" },
                { "kty": "RSA", "n": "abc", "e": "AQAB" }
            ]
        });
        let map = parse_jwks(&jwks);
        assert_eq!(map.len(), 1);
        let (n, e) = map.get("kid-1").unwrap();
        assert_eq!(n, good_key["n"].as_str().unwrap());
        assert_eq!(e, good_key["e"].as_str().unwrap());
    }

    #[test]
    fn validate_jwt_jwks_accepts_token_with_matching_kid() {
        let (private_pem, public_pem) = gen_keypair();
        let jwks = parse_jwks(&jwks_from_pem(&public_pem, "test-kid"));
        let token = sign_with_kid(
            &private_pem,
            "https://api.botframework.com",
            "app-id-123",
            "test-kid",
        );
        assert!(validate_jwt_jwks(
            &token,
            &jwks,
            "https://api.botframework.com",
            "app-id-123"
        ));
    }

    #[test]
    fn validate_jwt_jwks_rejects_unknown_or_absent_kid() {
        let (private_pem, public_pem) = gen_keypair();
        let jwks = parse_jwks(&jwks_from_pem(&public_pem, "known-kid"));
        // Token's kid isn't in the JWKS.
        let unknown = sign_with_kid(
            &private_pem,
            "https://api.botframework.com",
            "app-id-123",
            "other-kid",
        );
        assert!(!validate_jwt_jwks(
            &unknown,
            &jwks,
            "https://api.botframework.com",
            "app-id-123"
        ));
        // Token has no kid header at all.
        let no_kid = sign(&private_pem, "https://api.botframework.com", "app-id-123");
        assert!(!validate_jwt_jwks(
            &no_kid,
            &jwks,
            "https://api.botframework.com",
            "app-id-123"
        ));
    }

    #[test]
    fn validate_jwt_jwks_rejects_token_signed_by_a_different_key() {
        let (private_pem, _public_pem) = gen_keypair();
        let (_other_private, other_public) = gen_keypair();
        // JWKS carries a DIFFERENT key under the same kid the token advertises.
        let jwks = parse_jwks(&jwks_from_pem(&other_public, "test-kid"));
        let token = sign_with_kid(
            &private_pem,
            "https://api.botframework.com",
            "app-id-123",
            "test-kid",
        );
        assert!(!validate_jwt_jwks(
            &token,
            &jwks,
            "https://api.botframework.com",
            "app-id-123"
        ));
    }

    #[test]
    fn validate_jwt_jwks_rejects_wrong_audience_and_issuer() {
        let (private_pem, public_pem) = gen_keypair();
        let jwks = parse_jwks(&jwks_from_pem(&public_pem, "test-kid"));
        let token = sign_with_kid(
            &private_pem,
            "https://api.botframework.com",
            "app-id-123",
            "test-kid",
        );
        // Wrong audience.
        assert!(!validate_jwt_jwks(
            &token,
            &jwks,
            "https://api.botframework.com",
            "other-app-id"
        ));
        // Wrong issuer.
        assert!(!validate_jwt_jwks(
            &token,
            &jwks,
            "https://evil.example",
            "app-id-123"
        ));
    }
}
