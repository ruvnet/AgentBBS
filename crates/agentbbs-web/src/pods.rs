//! # Pods control plane (ADR-0035)
//!
//! AgentBBS is the **control plane** for MetaHarness domain agent pods; the
//! meta-llm Cognitum gateway is the **runtime**. This module owns the
//! AgentBBS-side glue: a [`PodController`] that spawns pods through the gateway's
//! `POST /v1/pods/spawn`, evaluates them via `POST /v1/bench/evaluate`, and the
//! `/api/pods` REST surface that sits beside `/api/boards`, `/api/arena`, etc.
//!
//! Three things make this Phase honest and `$0`:
//!
//! - **The HTTP transport is abstracted** ([`PodTransport`]) so tests mock the
//!   gateway and CI never makes a model call.
//! - **Scopes are explicit** ([`Scope`]/[`ScopeSet`]): `pods:spawn` and
//!   `bench:run` are *additive* to ADR-0034's `completions:{low,mid,high}`. The
//!   client refuses locally when the configured key lacks the scope (defense in
//!   depth) and forwards the scope set so the gateway is the final authority.
//! - **Tenancy is enforced** ([`PodController`] keys every pod by its owning
//!   AgentBBS identity): one tenant cannot list, inspect, evaluate, or stop
//!   another tenant's pod, and the `accountId` rides the spawn payload for the
//!   gateway's per-account billing/budget.
//!
//! When no pods-scoped key is configured the controller falls back to a
//! [`PlannedTransport`] that echoes the spawn payload as a *planned* (dry-run)
//! pod — mirroring ADR-0034's graceful scripted fallback — so the server runs
//! with no key and no spend.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentbbs_core::identity::Identity;
use agentbbs_core::pod::catalogue::catalogue;
use agentbbs_core::{MaxTier, PodTemplate};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::AppState;

/// A boxed, `Send` future — lets [`PodTransport`] stay object-safe without the
/// `async-trait` crate.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ---- scopes ------------------------------------------------------------------

/// A capability scope carried by a `cog_` gateway key.
///
/// `pods:spawn` and `bench:run` are the two new scopes ADR-0035 adds *on top of*
/// ADR-0034's `completions:{low,mid,high}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Chat-completion, cheap tier.
    CompletionsLow,
    /// Chat-completion, mid tier.
    CompletionsMid,
    /// Chat-completion, frontier tier.
    CompletionsHigh,
    /// Create / stop a pod (`POST /v1/pods/spawn`).
    PodsSpawn,
    /// Request an AgentiCow bench evaluation (`POST /v1/bench/evaluate`).
    BenchRun,
}

impl Scope {
    /// The wire string, e.g. `pods:spawn`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::CompletionsLow => "completions:low",
            Scope::CompletionsMid => "completions:mid",
            Scope::CompletionsHigh => "completions:high",
            Scope::PodsSpawn => "pods:spawn",
            Scope::BenchRun => "bench:run",
        }
    }

    /// Parse a single wire scope token (case-insensitive).
    pub fn parse(s: &str) -> Option<Scope> {
        match s.trim().to_ascii_lowercase().as_str() {
            "completions:low" => Some(Scope::CompletionsLow),
            "completions:mid" => Some(Scope::CompletionsMid),
            "completions:high" => Some(Scope::CompletionsHigh),
            "pods:spawn" => Some(Scope::PodsSpawn),
            "bench:run" => Some(Scope::BenchRun),
            _ => None,
        }
    }
}

/// A set of [`Scope`]s a key claims to hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeSet(BTreeSet<Scope>);

impl ScopeSet {
    /// Every scope — the default for a fully-provisioned pods key.
    pub fn full() -> Self {
        ScopeSet(BTreeSet::from([
            Scope::CompletionsLow,
            Scope::CompletionsMid,
            Scope::CompletionsHigh,
            Scope::PodsSpawn,
            Scope::BenchRun,
        ]))
    }

    /// Parse a comma-separated scope list (unknown tokens are ignored).
    pub fn parse_csv(s: &str) -> Self {
        ScopeSet(s.split(',').filter_map(Scope::parse).collect())
    }

    /// Whether the set contains `scope`.
    pub fn has(&self, scope: Scope) -> bool {
        self.0.contains(&scope)
    }

    /// The canonical comma-separated wire form (sorted, stable).
    pub fn to_csv(&self) -> String {
        self.0
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }
}

// ---- transport ---------------------------------------------------------------

/// A gateway-call failure, surfaced to the controller and mapped to HTTP.
#[derive(Clone, Debug)]
pub enum GatewayError {
    /// No pods-scoped gateway key is configured.
    NotConfigured,
    /// The configured key lacks the scope this call requires (local pre-check).
    MissingScope(Scope),
    /// A transport-level failure (DNS, TLS, timeout, decode).
    Transport(String),
    /// The gateway returned a non-success status with a body.
    Upstream { status: u16, body: Value },
}

/// The meta-llm pods API, abstracted so tests can mock it (no model calls in CI).
///
/// Implementors must verify the required scope before any network egress:
/// `spawn` requires [`Scope::PodsSpawn`], `evaluate` requires [`Scope::BenchRun`].
pub trait PodTransport: Send + Sync {
    /// `POST /v1/pods/spawn` — `payload` is the flattened `template_ref` object
    /// plus `account_id`.
    fn spawn<'a>(&'a self, payload: &'a Value) -> BoxFuture<'a, Result<Value, GatewayError>>;

    /// `POST /v1/bench/evaluate` — `payload` carries `pod_id`, `template_ref`,
    /// `account_id`, and any caller-supplied task context.
    fn evaluate<'a>(&'a self, payload: &'a Value) -> BoxFuture<'a, Result<Value, GatewayError>>;
}

/// The live transport: real HTTPS to the meta-llm Cognitum gateway, reusing the
/// ADR-0034 `Bearer`-`cog_` convention and forwarding the scope set + account.
pub struct LiveTransport {
    base_url: String,
    key: String,
    account_id: String,
    scopes: ScopeSet,
    client: reqwest::Client,
}

impl LiveTransport {
    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    async fn post(&self, path: &str, payload: &Value) -> Result<Value, GatewayError> {
        let resp = self
            .client
            .post(self.url(path))
            .bearer_auth(&self.key)
            .header("X-Cognitum-Account", &self.account_id)
            .header("X-Cognitum-Scopes", self.scopes.to_csv())
            .json(payload)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| GatewayError::Transport(e.to_string()))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::Transport(format!("decode: {e}")))?;
        if !status.is_success() {
            return Err(GatewayError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        Ok(body)
    }
}

impl PodTransport for LiveTransport {
    fn spawn<'a>(&'a self, payload: &'a Value) -> BoxFuture<'a, Result<Value, GatewayError>> {
        Box::pin(async move {
            if !self.scopes.has(Scope::PodsSpawn) {
                return Err(GatewayError::MissingScope(Scope::PodsSpawn));
            }
            self.post("pods/spawn", payload).await
        })
    }

    fn evaluate<'a>(&'a self, payload: &'a Value) -> BoxFuture<'a, Result<Value, GatewayError>> {
        Box::pin(async move {
            if !self.scopes.has(Scope::BenchRun) {
                return Err(GatewayError::MissingScope(Scope::BenchRun));
            }
            self.post("bench/evaluate", payload).await
        })
    }
}

/// The no-key fallback: spawn echoes the payload as a *planned* (dry-run) pod;
/// evaluate is unavailable (needs the live gateway). Keeps the server `$0`.
pub struct PlannedTransport;

impl PodTransport for PlannedTransport {
    fn spawn<'a>(&'a self, payload: &'a Value) -> BoxFuture<'a, Result<Value, GatewayError>> {
        Box::pin(async move {
            Ok(json!({
                "status": "PLANNED",
                "planned": true,
                "spawn_request": payload,
            }))
        })
    }

    fn evaluate<'a>(&'a self, _payload: &'a Value) -> BoxFuture<'a, Result<Value, GatewayError>> {
        Box::pin(async move { Err(GatewayError::NotConfigured) })
    }
}

/// Resolve the pods transport + default account from the environment.
///
/// A [`LiveTransport`] is built only when both a base URL
/// (`AGENTBBS_POD_BASE_URL`, else `AGENTBBS_LLM_BASE_URL`) and a key (the var
/// *named* by `AGENTBBS_POD_KEY_ENV`, else `AGENTBBS_LLM_KEY_ENV`'s var) are
/// present; otherwise the [`PlannedTransport`] is used. Scopes come from
/// `AGENTBBS_POD_SCOPES` (csv, default = all); the account from
/// `AGENTBBS_POD_ACCOUNT` (default `agentbbs-test`).
pub fn resolve_transport() -> (Arc<dyn PodTransport>, String) {
    fn env(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|s| !s.trim().is_empty())
    }
    let account = env("AGENTBBS_POD_ACCOUNT").unwrap_or_else(|| "agentbbs-test".to_string());
    let base_url = env("AGENTBBS_POD_BASE_URL").or_else(|| env("AGENTBBS_LLM_BASE_URL"));
    let key = env("AGENTBBS_POD_KEY_ENV")
        .or_else(|| env("AGENTBBS_LLM_KEY_ENV"))
        .and_then(|var| env(&var));
    match (base_url, key) {
        (Some(base_url), Some(key)) => {
            let scopes = env("AGENTBBS_POD_SCOPES")
                .map(|s| ScopeSet::parse_csv(&s))
                .unwrap_or_else(ScopeSet::full);
            let transport = LiveTransport {
                base_url,
                key,
                account_id: account.clone(),
                scopes,
                client: reqwest::Client::new(),
            };
            (Arc::new(transport), account)
        }
        _ => (Arc::new(PlannedTransport), account),
    }
}

// ---- pod state ---------------------------------------------------------------

/// The lifecycle state of a pod, mirroring the meta-llm Darwin-Loop states plus
/// the AgentBBS-local `Planned` (dry-run) and `Stopped`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PodStatus {
    /// Dry-run only — no gateway spawn happened (no pods key configured).
    Planned,
    /// The gateway accepted the spawn.
    Spawned,
    /// A loop step is running.
    Executing,
    /// A bench evaluation is in flight.
    Evaluating,
    /// Operator-stopped; no further work.
    Stopped,
    /// The pod failed.
    Failed,
}

impl PodStatus {
    fn as_str(&self) -> &'static str {
        match self {
            PodStatus::Planned => "PLANNED",
            PodStatus::Spawned => "SPAWNED",
            PodStatus::Executing => "EXECUTING",
            PodStatus::Evaluating => "EVALUATING",
            PodStatus::Stopped => "STOPPED",
            PodStatus::Failed => "FAILED",
        }
    }

    fn parse(s: &str) -> PodStatus {
        match s.trim().to_ascii_uppercase().as_str() {
            "PLANNED" => PodStatus::Planned,
            "EXECUTING" => PodStatus::Executing,
            "EVALUATING" => PodStatus::Evaluating,
            "STOPPED" => PodStatus::Stopped,
            "FAILED" => PodStatus::Failed,
            _ => PodStatus::Spawned,
        }
    }
}

fn max_tier_str(t: MaxTier) -> &'static str {
    match t {
        MaxTier::Low => "low",
        MaxTier::Mid => "mid",
        MaxTier::High => "high",
    }
}

/// A pod's server-side record. `owner` is the tenant-isolation key (the spawning
/// AgentBBS identity); `account_id` is the gateway billing attribution.
#[derive(Clone, Debug)]
struct PodRecord {
    pod_id: String,
    owner: String,
    pod_author: String,
    account_id: String,
    template_ref: String,
    domain: String,
    registered_room: String,
    cap_usd: f64,
    spend_usd: f64,
    max_tier: String,
    status: PodStatus,
    planned: bool,
    created_at: DateTime<Utc>,
    last_bench: Option<Value>,
}

impl PodRecord {
    fn view(&self) -> PodView {
        PodView {
            pod_id: self.pod_id.clone(),
            template_ref: self.template_ref.clone(),
            domain: self.domain.clone(),
            status: self.status.as_str(),
            registered_room: self.registered_room.clone(),
            account_id: self.account_id.clone(),
            owner: short(&self.owner),
            pod_author: self.pod_author.clone(),
            cap_usd: self.cap_usd,
            spend_usd: self.spend_usd,
            max_tier: self.max_tier.clone(),
            created_at: self.created_at.to_rfc3339(),
            planned: self.planned,
            last_bench: self.last_bench.clone(),
        }
    }
}

/// A short display form for an identity hex.
fn short(hex: &str) -> String {
    hex.chars().take(12).collect()
}

/// The JSON view returned by `/api/pods` handlers.
#[derive(Debug, Serialize)]
pub struct PodView {
    pod_id: String,
    template_ref: String,
    domain: String,
    status: &'static str,
    registered_room: String,
    account_id: String,
    owner: String,
    pod_author: String,
    cap_usd: f64,
    spend_usd: f64,
    max_tier: String,
    created_at: String,
    planned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_bench: Option<Value>,
}

/// Controller-level error, mapped to an HTTP status by the handlers.
#[derive(Debug)]
pub enum PodError {
    /// Bad input (template validation, missing fields).
    Validation(String),
    /// No such pod for this tenant (also used to hide cross-tenant pods).
    NotFound,
    /// The pod is in a state that forbids the operation.
    Conflict(String),
    /// A gateway-call failure.
    Gateway(GatewayError),
}

impl From<GatewayError> for PodError {
    fn from(e: GatewayError) -> Self {
        PodError::Gateway(e)
    }
}

// ---- controller --------------------------------------------------------------

/// The pods control plane: owns the transport and the per-tenant pod registry.
pub struct PodController {
    transport: Arc<dyn PodTransport>,
    account: String,
    pods: Mutex<HashMap<String, PodRecord>>,
}

impl PodController {
    /// Build a controller over a transport and a default billing account.
    pub fn new(transport: Arc<dyn PodTransport>, account: String) -> Self {
        PodController {
            transport,
            account,
            pods: Mutex::new(HashMap::new()),
        }
    }

    /// Build a controller from the environment (live key → [`LiveTransport`],
    /// else [`PlannedTransport`]).
    pub fn from_env() -> Self {
        let (transport, account) = resolve_transport();
        PodController::new(transport, account)
    }

    /// The default billing account for spawns that don't override it.
    pub fn default_account(&self) -> &str {
        &self.account
    }

    /// Spawn a pod from `template`, owned by `owner` (tenant) and billed to
    /// `account_id`. Calls the gateway's `pods/spawn` (or the planned fallback).
    pub async fn spawn(
        &self,
        owner: &str,
        pod_author: &str,
        account_id: &str,
        template: &PodTemplate,
    ) -> Result<PodView, PodError> {
        template
            .validate()
            .map_err(|e| PodError::Validation(e.to_string()))?;
        let wire = template.to_spawn_request();
        let mut payload =
            serde_json::to_value(&wire).map_err(|e| PodError::Validation(e.to_string()))?;
        payload["account_id"] = json!(account_id);

        let resp = self.transport.spawn(&payload).await?;
        let pod_id = resp
            .get("pod_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(new_pod_id);
        let planned = resp.get("planned").and_then(Value::as_bool).unwrap_or(false);
        let status = resp
            .get("status")
            .and_then(Value::as_str)
            .map(PodStatus::parse)
            .unwrap_or(PodStatus::Spawned);

        let record = PodRecord {
            pod_id: pod_id.clone(),
            owner: owner.to_string(),
            pod_author: pod_author.to_string(),
            account_id: account_id.to_string(),
            template_ref: template.id.0.clone(),
            domain: template.domain_slug().to_string(),
            registered_room: template.registered_room.clone(),
            cap_usd: template.per_agent_cap_usd,
            spend_usd: 0.0,
            max_tier: max_tier_str(template.max_tier).to_string(),
            status,
            planned,
            created_at: Utc::now(),
            last_bench: None,
        };
        let view = record.view();
        self.pods.lock().unwrap().insert(pod_id, record);
        Ok(view)
    }

    /// List the pods owned by `owner`, newest first.
    pub fn list(&self, owner: &str) -> Vec<PodView> {
        let pods = self.pods.lock().unwrap();
        let mut mine: Vec<&PodRecord> = pods.values().filter(|p| p.owner == owner).collect();
        mine.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        mine.into_iter().map(PodRecord::view).collect()
    }

    /// Get a single pod owned by `owner`. Cross-tenant access returns
    /// [`PodError::NotFound`] (existence is not leaked).
    pub fn get(&self, owner: &str, pod_id: &str) -> Result<PodView, PodError> {
        let pods = self.pods.lock().unwrap();
        match pods.get(pod_id) {
            Some(p) if p.owner == owner => Ok(p.view()),
            _ => Err(PodError::NotFound),
        }
    }

    /// Request an AgentiCow bench evaluation for `pod_id` (must be `owner`'s).
    /// Updates the pod's measured spend from `x_cognitum.price_usd`.
    pub async fn evaluate(
        &self,
        owner: &str,
        pod_id: &str,
        body: Value,
    ) -> Result<PodView, PodError> {
        let (account_id, template_ref) = {
            let pods = self.pods.lock().unwrap();
            let rec = match pods.get(pod_id) {
                Some(p) if p.owner == owner => p,
                _ => return Err(PodError::NotFound),
            };
            if rec.status == PodStatus::Stopped {
                return Err(PodError::Conflict("pod is stopped".into()));
            }
            (rec.account_id.clone(), rec.template_ref.clone())
        };

        let mut payload = json!({
            "pod_id": pod_id,
            "template_ref": template_ref,
            "account_id": account_id,
        });
        if let Value::Object(extra) = body {
            for (k, v) in extra {
                payload[k] = v;
            }
        }
        let resp = self.transport.evaluate(&payload).await?;

        let mut pods = self.pods.lock().unwrap();
        let rec = pods.get_mut(pod_id).ok_or(PodError::NotFound)?;
        if let Some(price) = resp
            .get("x_cognitum")
            .and_then(|x| x.get("price_usd"))
            .and_then(Value::as_f64)
        {
            rec.spend_usd += price;
        }
        rec.status = resp
            .get("status")
            .and_then(Value::as_str)
            .map(PodStatus::parse)
            .unwrap_or(PodStatus::Executing);
        rec.last_bench = Some(resp);
        Ok(rec.view())
    }

    /// Stop a pod (must be `owner`'s). Marks it `STOPPED`; no further evaluation
    /// is accepted. (Gateway-side stop lands with the live executor phase.)
    pub fn stop(&self, owner: &str, pod_id: &str) -> Result<PodView, PodError> {
        let mut pods = self.pods.lock().unwrap();
        match pods.get_mut(pod_id) {
            Some(p) if p.owner == owner => {
                p.status = PodStatus::Stopped;
                Ok(p.view())
            }
            _ => Err(PodError::NotFound),
        }
    }
}

/// Mint a fresh, unguessable pod id from a throwaway identity's public key.
fn new_pod_id() -> String {
    format!("pod-{}", &Identity::generate().id().to_hex()[..16])
}

// ---- HTTP handlers -----------------------------------------------------------

/// Read a non-empty header value as a string slice.
fn header_str<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    headers
        .get(key)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
}

/// Map a [`PodError`] to an HTTP `(status, json)` response.
fn pod_err(e: PodError) -> (StatusCode, Json<Value>) {
    match e {
        PodError::Validation(m) => (StatusCode::BAD_REQUEST, Json(json!({ "error": m }))),
        PodError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "pod not found" })),
        ),
        PodError::Conflict(m) => (StatusCode::CONFLICT, Json(json!({ "error": m }))),
        PodError::Gateway(g) => match g {
            GatewayError::MissingScope(s) => (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": format!("gateway key missing scope {}", s.as_str()) })),
            ),
            GatewayError::NotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "no pods-scoped gateway key configured" })),
            ),
            GatewayError::Transport(m) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("gateway transport: {m}") })),
            ),
            GatewayError::Upstream { status, body } => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": "gateway error", "status": status, "body": body })),
            ),
        },
    }
}

/// The spawn request body: a `template_ref` resolved against the curated
/// catalogue, or a full inline `template` override.
#[derive(Deserialize, Default)]
pub struct SpawnBody {
    #[serde(default)]
    template_ref: Option<String>,
    #[serde(default)]
    template: Option<PodTemplate>,
}

/// `GET /api/pods/templates` — the curated six-vertical catalogue.
pub async fn api_pods_templates() -> impl IntoResponse {
    Json(json!({ "templates": catalogue() }))
}

/// `POST /api/pods` — spawn a pod from a `template_ref` (or inline `template`).
pub async fn api_pods_spawn(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<SpawnBody>,
) -> Result<Json<PodView>, (StatusCode, Json<Value>)> {
    let template = if let Some(t) = req.template {
        t
    } else if let Some(r) = &req.template_ref {
        catalogue()
            .into_iter()
            .find(|t| &t.id.0 == r)
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": format!("unknown template_ref '{r}'") })),
                )
            })?
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "provide template_ref or template" })),
        ));
    };

    let owner = state.tenant_owner(&headers);
    let pod_author = Identity::generate().id().short();
    let account = header_str(&headers, "x-cog-account")
        .map(str::to_string)
        .unwrap_or_else(|| state.pods.default_account().to_string());

    // Ensure the pod's room (board) exists so signed step-results have a home.
    state.ensure_board(&template.registered_room);

    state
        .pods
        .spawn(&owner, &pod_author, &account, &template)
        .await
        .map(Json)
        .map_err(pod_err)
}

/// `GET /api/pods` — list the caller's pods.
pub async fn api_pods_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let owner = state.tenant_owner(&headers);
    Json(json!({ "pods": state.pods.list(&owner).iter().map(|v| serde_json::to_value(v).unwrap()).collect::<Vec<_>>() }))
}

/// `GET /api/pods/{id}` — a single pod the caller owns.
pub async fn api_pods_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<PodView>, (StatusCode, Json<Value>)> {
    let owner = state.tenant_owner(&headers);
    state.pods.get(&owner, &id).map(Json).map_err(pod_err)
}

/// `POST /api/pods/{id}/evaluate` — request an AgentiCow bench evaluation.
pub async fn api_pods_evaluate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<PodView>, (StatusCode, Json<Value>)> {
    let owner = state.tenant_owner(&headers);
    let body = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    state
        .pods
        .evaluate(&owner, &id, body)
        .await
        .map(Json)
        .map_err(pod_err)
}

/// `POST /api/pods/{id}/stop` — stop a pod the caller owns.
pub async fn api_pods_stop(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<PodView>, (StatusCode, Json<Value>)> {
    let owner = state.tenant_owner(&headers);
    state.pods.stop(&owner, &id).map(Json).map_err(pod_err)
}

#[cfg(test)]
mod tests;
