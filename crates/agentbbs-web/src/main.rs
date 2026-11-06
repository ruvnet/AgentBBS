//! AgentBBS mobile web server.

use std::sync::Arc;

use agentbbs_core::store::{MemoryStore, RedbStore, Store};
use agentbbs_web::{router, AppState};

/// What `main` should do about store selection, decided from environment
/// alone so the decision itself is unit-testable without touching disk.
#[derive(Debug, PartialEq, Eq)]
enum StoreMode {
    /// Open a durable `RedbStore` at this path.
    Durable(String),
    /// Use the ephemeral in-memory store (only ever correct outside production).
    Ephemeral,
    /// Refuse to start. Carries the operator-facing reason.
    FailClosed(&'static str),
}

/// ADR-0054 Q4: with `AGENTBBS_DB_PATH` set, use the durable single-file
/// `RedbStore` so board state survives restarts — the persistence half of the
/// single-instance + persistent-volume Cloud Run recipe (pair with
/// `min-instances=1` and a mounted volume; redb is single-writer, so this is a
/// single-instance durability story, not multi-instance HA — see ADR-0054
/// backlog item 2(b) for the still-unbuilt shared/HA store).
///
/// `AGENTBBS_ENV=production` makes this fail closed: a production deployment
/// with no durable path configured, or one whose `RedbStore::open` fails,
/// refuses to boot rather than silently degrading to `MemoryStore` and losing
/// data on the next restart. Outside production (local dev, tests, demos),
/// the same failures fall back to `MemoryStore` with a loud warning.
fn store_mode(db_path: Option<&str>, production: bool) -> StoreMode {
    match db_path {
        Some(path) if !path.is_empty() => StoreMode::Durable(path.to_string()),
        _ if production => StoreMode::FailClosed(
            "AGENTBBS_ENV=production requires AGENTBBS_DB_PATH (a durable store) — \
             refusing to start with an ephemeral MemoryStore",
        ),
        _ => StoreMode::Ephemeral,
    }
}

fn is_production() -> bool {
    std::env::var("AGENTBBS_ENV").as_deref() == Ok("production")
}

#[allow(clippy::too_many_arguments)] // Flat env projection keeps table-driven startup tests explicit.
fn security_config_is_valid(
    admin_secret: &str,
    admin_audience: &str,
    results_kid: &str,
    results_secret: &str,
    results_account: &str,
    previous_kid: &str,
    previous_secret: &str,
    live_pods: bool,
) -> Result<(), &'static str> {
    if admin_secret.is_empty() || admin_audience.is_empty() {
        return Err("production requires admin-action verifier configuration");
    }
    if !live_pods {
        return Ok(());
    }
    if [results_kid, results_secret, results_account]
        .iter()
        .any(|v| v.is_empty())
    {
        return Err("live pod integration requires pod-result verifier configuration");
    }
    if previous_kid.is_empty() != previous_secret.is_empty() {
        return Err("previous pod-result kid and secret must be configured together");
    }
    if !previous_kid.is_empty() && previous_kid == results_kid {
        return Err("current and previous pod-result kid must differ");
    }
    Ok(())
}

fn validate_production_security_config() {
    if !is_production() {
        return;
    }
    let env = |key: &str| std::env::var(key).unwrap_or_default();
    security_config_is_valid(
        &env("AGENTBBS_ADMIN_ACTION_SECRET"),
        &env("AGENTBBS_ADMIN_ACTION_AUDIENCE"),
        &env("AGENTBBS_RESULTS_SIGNING_KEY_ID"),
        &env("AGENTBBS_RESULTS_SIGNING_SECRET"),
        &env("AGENTBBS_RESULTS_ACCOUNT_ID"),
        &env("AGENTBBS_RESULTS_PREVIOUS_SIGNING_KEY_ID"),
        &env("AGENTBBS_RESULTS_PREVIOUS_SIGNING_SECRET"),
        !env("AGENTBBS_PODS_BASE_URL").is_empty(),
    )
    .unwrap_or_else(|reason| panic!("invalid production security configuration: {reason}"));
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    validate_production_security_config();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8088);

    let db_path = std::env::var("AGENTBBS_DB_PATH").ok();
    let store: Arc<dyn Store> = match store_mode(db_path.as_deref(), is_production()) {
        StoreMode::Durable(path) => match RedbStore::open(&path) {
            Ok(s) => {
                tracing::info!("AgentBBS durable store: RedbStore at {path}");
                Arc::new(s) as Arc<dyn Store>
            }
            Err(e) if is_production() => {
                panic!(
                    "production requires a durable store; failed to open RedbStore at \
                     {path}: {e}"
                );
            }
            Err(e) => {
                tracing::error!(
                    "failed to open RedbStore at {path}: {e}; falling back to \
                     in-memory store (NOT durable — data is lost on restart)"
                );
                Arc::new(MemoryStore::new()) as Arc<dyn Store>
            }
        },
        StoreMode::Ephemeral => {
            tracing::info!(
                "AgentBBS in-memory store (ephemeral); set AGENTBBS_DB_PATH for persistence"
            );
            Arc::new(MemoryStore::new()) as Arc<dyn Store>
        }
        StoreMode::FailClosed(reason) => panic!("{reason}"),
    };
    let state = AppState::new(store);
    let app = router(state);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    tracing::info!("AgentBBS mobile web on http://{addr}");
    axum::serve(listener, app).await.expect("serve");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_path_selects_redb_regardless_of_environment() {
        assert_eq!(
            store_mode(Some("/data/bbs.redb"), false),
            StoreMode::Durable("/data/bbs.redb".to_string())
        );
        assert_eq!(
            store_mode(Some("/data/bbs.redb"), true),
            StoreMode::Durable("/data/bbs.redb".to_string())
        );
    }

    #[test]
    fn missing_path_is_ephemeral_outside_production() {
        assert_eq!(store_mode(None, false), StoreMode::Ephemeral);
        assert_eq!(store_mode(Some(""), false), StoreMode::Ephemeral);
    }

    #[test]
    fn missing_path_fails_closed_in_production() {
        assert!(matches!(store_mode(None, true), StoreMode::FailClosed(_)));
        assert!(matches!(
            store_mode(Some(""), true),
            StoreMode::FailClosed(_)
        ));
    }

    #[test]
    fn production_security_configuration_is_complete_and_rotation_is_paired() {
        assert!(security_config_is_valid("a", "aud", "", "", "", "", "", false).is_ok());
        assert!(security_config_is_valid("a", "aud", "current", "r", "acct", "", "", true).is_ok());
        for invalid in [
            ("", "aud", "current", "r", "acct", "", ""),
            ("a", "", "current", "r", "acct", "", ""),
            ("a", "aud", "", "r", "acct", "", ""),
            ("a", "aud", "current", "", "acct", "", ""),
            ("a", "aud", "current", "r", "", "", ""),
            ("a", "aud", "current", "r", "acct", "old", ""),
            ("a", "aud", "current", "r", "acct", "", "old-key"),
            ("a", "aud", "current", "r", "acct", "current", "old-key"),
        ] {
            assert!(security_config_is_valid(
                invalid.0, invalid.1, invalid.2, invalid.3, invalid.4, invalid.5, invalid.6, true
            )
            .is_err());
        }
        assert!(security_config_is_valid(
            "a", "aud", "current", "r", "acct", "previous", "old-key", true
        )
        .is_ok());
    }
}
