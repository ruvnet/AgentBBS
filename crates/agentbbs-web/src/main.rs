//! AgentBBS mobile web server.

use std::sync::Arc;

use agentbbs_core::store::{MemoryStore, RedbStore, Store};
use agentbbs_web::{router, AppState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8088);

    // Store selection (ADR-0054 Q4): with AGENTBBS_DB_PATH set, use the durable
    // single-file RedbStore so board state survives restarts — the persistence
    // half of the single-instance + persistent-volume Cloud Run recipe (pair
    // with min-instances=1 and a mounted volume; redb is single-writer, so this
    // is a single-instance durability story, not multi-instance HA). Without it,
    // the in-memory store (fast, ephemeral). A failed open falls back to memory
    // with a loud warning rather than refusing to boot.
    let store: Arc<dyn Store> = match std::env::var("AGENTBBS_DB_PATH") {
        Ok(path) if !path.is_empty() => match RedbStore::open(&path) {
            Ok(s) => {
                tracing::info!("AgentBBS durable store: RedbStore at {path}");
                Arc::new(s) as Arc<dyn Store>
            }
            Err(e) => {
                tracing::error!(
                    "failed to open RedbStore at {path}: {e}; falling back to \
                     in-memory store (NOT durable — data is lost on restart)"
                );
                Arc::new(MemoryStore::new()) as Arc<dyn Store>
            }
        },
        _ => {
            tracing::info!(
                "AgentBBS in-memory store (ephemeral); set AGENTBBS_DB_PATH for persistence"
            );
            Arc::new(MemoryStore::new()) as Arc<dyn Store>
        }
    };
    let state = AppState::new(store);
    let app = router(state);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    tracing::info!("AgentBBS mobile web on http://{addr}");
    axum::serve(listener, app).await.expect("serve");
}
