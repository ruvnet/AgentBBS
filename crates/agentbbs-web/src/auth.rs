//! Opt-in caller authentication for the AgentBBS HTTP API (SEC-8).
//!
//! # Why this exists
//!
//! AgentBBS's native model is *anonymous, browser-signed, single-node*: every
//! board is world-readable and world-writable, and the only integrity guarantee
//! is client-side Ed25519 signing (ADR-0016) — you cannot forge another
//! author's post, but nothing stops an arbitrary caller from *reading* or
//! *posting to* any board. That is correct and safe for the public
//! static-genesis node.
//!
//! It is **not** safe for a commercial multi-tenant deployment where boards
//! carry per-tenant data behind predictable names (`harnessaas-{accountId}`,
//! `collab-{accountId}`): any internet caller can read or post cross-tenant.
//! That is SEC-8 (`cognitum/docs/release/ISSUES.md`).
//!
//! # What this module does (Phase 1)
//!
//! An **opt-in, default-OFF** request gate on the JSON API surface (`/api/*`).
//! When `AGENTBBS_REQUIRE_AUTH` is unset/false the gate is a no-op and the node
//! behaves exactly as it always has — the OSS / genesis-static deployment is
//! unaffected. When enabled, every `/api/*` request must present a valid caller
//! credential (`Authorization: Bearer <token>`) matched constant-time against
//! the configured key set (`AGENTBBS_API_KEYS`), or receive `401`. This is the
//! lockable primitive that lets a shared/commercial instance stop anonymous
//! internet access and be pinned to a known caller (e.g. the meta-llm collab
//! gateway or the comms control-plane proxy).
//!
//! The static shell (`/`, `/vendor/*`, `/manifest.webmanifest`, health) is never
//! gated, so an authenticated front-end still boots and then presents its key on
//! the data calls. `AGENTBBS_AUTH_READONLY_PUBLIC=1` keeps GETs public while
//! still gating writes, for deployments that want a public read-only board with
//! authenticated posting.
//!
//! # What is deferred (Phase 2 — ADR-0056)
//!
//! Per-*board* authorization: a verified key carries a board allowlist claim and
//! the board/approvals/state handlers filter to it, plugged into the existing
//! `Caps` + `require` enforcement seam (ADR-0054 build item 1). Phase 1 gates the
//! *door*; Phase 2 scopes *which boards* an authenticated caller may touch.

use std::env;

use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Resolved auth configuration, read once from the environment.
#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    /// Master switch. When false the gate is a no-op (OSS/genesis default).
    pub enabled: bool,
    /// When true, unauthenticated GET/HEAD is allowed; writes are still gated.
    pub readonly_public: bool,
    /// Accepted opaque bearer tokens (from `AGENTBBS_API_KEYS`, comma-separated).
    pub keys: Vec<String>,
}

fn env_flag(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

impl AuthConfig {
    /// Read the config from process env. `AGENTBBS_API_KEYS` is comma-separated;
    /// blanks are ignored.
    pub fn from_env() -> Self {
        AuthConfig {
            enabled: env_flag("AGENTBBS_REQUIRE_AUTH"),
            readonly_public: env_flag("AGENTBBS_AUTH_READONLY_PUBLIC"),
            keys: env::var("AGENTBBS_API_KEYS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }
}

/// The decision the gate reaches for a given request. Kept as a pure,
/// side-effect-free function so it is unit-tested without touching env or the
/// HTTP stack.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not an API path, or auth disabled, or a permitted public read → pass.
    Allow,
    /// Auth required but the presented credential is missing/invalid → 401.
    Unauthorized(&'static str),
    /// Auth required but the deployment configured no keys → 401 (fail-closed:
    /// "required but unconfigured" is a misconfiguration, never an open door).
    Misconfigured(&'static str),
}

/// Pure authorization decision. `token` is the already-extracted bearer value
/// (None if absent/malformed).
pub fn authorize(cfg: &AuthConfig, method: &Method, path: &str, token: Option<&str>) -> Outcome {
    if !cfg.enabled || !path.starts_with("/api/") {
        return Outcome::Allow;
    }
    // CORS preflight carries no credentials by design — never gate it, or the
    // browser's preflight fails before the real (authenticated) request is sent.
    if *method == Method::OPTIONS {
        return Outcome::Allow;
    }
    let is_read = matches!(*method, Method::GET | Method::HEAD);
    if cfg.readonly_public && is_read {
        return Outcome::Allow;
    }
    if cfg.keys.is_empty() {
        return Outcome::Misconfigured("api auth required but no keys configured");
    }
    match token {
        Some(t) if cfg.keys.iter().any(|k| ct_eq(k.as_bytes(), t.as_bytes())) => Outcome::Allow,
        _ => Outcome::Unauthorized("missing or invalid API credential"),
    }
}

/// Constant-time byte equality — avoids leaking key length/prefix via early
/// return timing. (Length inequality short-circuits, which is acceptable: the
/// keys are high-entropy tokens, not passwords whose length is secret.)
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

/// Axum middleware: enforce [`authorize`] against the live config. Applied to
/// the whole router; it self-limits to `/api/*` so the static shell is never
/// gated. Config is read per request so an operator can flip the env without a
/// code change (cheap: a few `env::var` reads).
pub async fn require_api_auth(req: Request, next: Next) -> Response {
    let cfg = AuthConfig::from_env();
    let token = bearer(req.headers());
    match authorize(&cfg, req.method(), req.uri().path(), token.as_deref()) {
        Outcome::Allow => next.run(req).await,
        Outcome::Unauthorized(msg) | Outcome::Misconfigured(msg) => {
            (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": msg }))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, readonly_public: bool, keys: &[&str]) -> AuthConfig {
        AuthConfig {
            enabled,
            readonly_public,
            keys: keys.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn disabled_allows_everything() {
        let c = cfg(false, false, &[]);
        assert_eq!(
            authorize(&c, &Method::POST, "/api/boards/x", None),
            Outcome::Allow
        );
    }

    #[test]
    fn non_api_paths_are_never_gated() {
        let c = cfg(true, false, &["k1"]);
        assert_eq!(authorize(&c, &Method::GET, "/", None), Outcome::Allow);
        assert_eq!(
            authorize(&c, &Method::GET, "/vendor/blake3.js", None),
            Outcome::Allow
        );
    }

    #[test]
    fn enabled_without_token_is_unauthorized() {
        let c = cfg(true, false, &["k1"]);
        assert!(matches!(
            authorize(&c, &Method::GET, "/api/approvals", None),
            Outcome::Unauthorized(_)
        ));
    }

    #[test]
    fn enabled_with_valid_token_allows() {
        let c = cfg(true, false, &["k1", "k2"]);
        assert_eq!(
            authorize(&c, &Method::POST, "/api/approvals", Some("k2")),
            Outcome::Allow
        );
    }

    #[test]
    fn enabled_with_wrong_token_is_unauthorized() {
        let c = cfg(true, false, &["k1"]);
        assert!(matches!(
            authorize(&c, &Method::POST, "/api/approvals", Some("nope")),
            Outcome::Unauthorized(_)
        ));
    }

    #[test]
    fn readonly_public_allows_get_but_gates_write() {
        let c = cfg(true, true, &["k1"]);
        assert_eq!(
            authorize(&c, &Method::GET, "/api/boards/x", None),
            Outcome::Allow
        );
        assert!(matches!(
            authorize(&c, &Method::POST, "/api/boards/x", None),
            Outcome::Unauthorized(_)
        ));
    }

    #[test]
    fn options_preflight_always_allowed() {
        let c = cfg(true, false, &["k1"]);
        assert_eq!(
            authorize(&c, &Method::OPTIONS, "/api/boards/x", None),
            Outcome::Allow
        );
    }

    #[test]
    fn enabled_but_no_keys_fails_closed() {
        let c = cfg(true, false, &[]);
        assert!(matches!(
            authorize(&c, &Method::POST, "/api/boards/x", Some("anything")),
            Outcome::Misconfigured(_)
        ));
    }

    #[test]
    fn ct_eq_matches_std_eq() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b""));
    }
}
