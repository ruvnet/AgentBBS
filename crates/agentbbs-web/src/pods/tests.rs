//! Unit + HTTP tests for the pods control plane (ADR-0035). No model calls: the
//! gateway is mocked, so this whole suite is `$0` and CI-safe.

use std::sync::Mutex;

use super::*;
use agentbbs_core::pod::catalogue::research_competitive_intel;

// ---- a recording mock gateway (no network) -----------------------------------

struct MockTransport {
    spawn_result: Mutex<Result<Value, GatewayError>>,
    evaluate_result: Mutex<Result<Value, GatewayError>>,
    spawn_calls: Mutex<Vec<Value>>,
    evaluate_calls: Mutex<Vec<Value>>,
}

impl MockTransport {
    /// A mock whose spawn returns a fixed `pod_id`, and whose evaluate reports a
    /// `$0.0125` low-tier bench result.
    fn new() -> Arc<Self> {
        Arc::new(MockTransport {
            spawn_result: Mutex::new(Ok(json!({ "pod_id": "pod-mock-1", "status": "SPAWNED" }))),
            evaluate_result: Mutex::new(Ok(json!({
                "status": "EXECUTING",
                "passed": 3,
                "total": 3,
                "x_cognitum": { "price_usd": 0.0125, "resolved_tier": "low" },
            }))),
            spawn_calls: Mutex::new(Vec::new()),
            evaluate_calls: Mutex::new(Vec::new()),
        })
    }

    /// A mock whose spawn omits `pod_id` (so the controller mints unique ones —
    /// needed when a test spawns more than one pod).
    fn unique_ids() -> Arc<Self> {
        let m = MockTransport::new();
        *m.spawn_result.lock().unwrap() = Ok(json!({ "status": "SPAWNED" }));
        m
    }

    /// A mock whose spawn fails with `err`.
    fn failing_spawn(err: GatewayError) -> Arc<Self> {
        let m = MockTransport::new();
        *m.spawn_result.lock().unwrap() = Err(err);
        m
    }
}

impl PodTransport for MockTransport {
    fn spawn<'a>(&'a self, payload: &'a Value) -> BoxFuture<'a, Result<Value, GatewayError>> {
        self.spawn_calls.lock().unwrap().push(payload.clone());
        let r = self.spawn_result.lock().unwrap().clone();
        Box::pin(async move { r })
    }

    fn evaluate<'a>(&'a self, payload: &'a Value) -> BoxFuture<'a, Result<Value, GatewayError>> {
        self.evaluate_calls.lock().unwrap().push(payload.clone());
        let r = self.evaluate_result.lock().unwrap().clone();
        Box::pin(async move { r })
    }
}

// ---- scope unit tests --------------------------------------------------------

#[test]
fn scopes_are_additive_over_completions() {
    let full = ScopeSet::full();
    // The two new ADR-0035 scopes are present alongside the ADR-0034 ones.
    assert!(full.has(Scope::PodsSpawn));
    assert!(full.has(Scope::BenchRun));
    assert!(full.has(Scope::CompletionsLow));
    assert!(full.has(Scope::CompletionsMid));
    assert!(full.has(Scope::CompletionsHigh));
}

#[test]
fn scope_set_parses_csv_and_ignores_garbage() {
    let s = ScopeSet::parse_csv("pods:spawn, bench:run , completions:low, totally-bogus");
    assert!(s.has(Scope::PodsSpawn));
    assert!(s.has(Scope::BenchRun));
    assert!(s.has(Scope::CompletionsLow));
    assert!(!s.has(Scope::CompletionsHigh));
    // Canonical, sorted wire form.
    assert_eq!(s.to_csv(), "completions:low,pods:spawn,bench:run");
}

// ---- controller unit tests ---------------------------------------------------

#[tokio::test]
async fn spawn_sends_template_ref_and_account_to_gateway() {
    let mock = MockTransport::new();
    let ctrl = PodController::new(mock.clone(), "acct-x".into());
    let tmpl = research_competitive_intel();

    let view = ctrl
        .spawn("owner-a", "author-1", "acct-x", &tmpl)
        .await
        .unwrap();

    assert_eq!(view.pod_id, "pod-mock-1");
    assert_eq!(view.status, "SPAWNED");
    assert_eq!(view.template_ref, "research/competitive-intel@1");
    assert_eq!(view.registered_room, "research-intel");
    assert_eq!(view.account_id, "acct-x");
    assert_eq!(view.max_tier, "mid");

    let calls = mock.spawn_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let p = &calls[0];
    assert_eq!(p["template_ref"], "research/competitive-intel@1");
    assert_eq!(p["account_id"], "acct-x");
    // Wire flattening (Phase 0): tools collapse to firewalled id strings.
    assert_eq!(p["tools"][0], "web.search");
    assert!(p["bench_assertions"].is_string());
}

#[tokio::test]
async fn pods_are_isolated_per_tenant() {
    let ctrl = PodController::new(MockTransport::unique_ids(), "a".into());
    let tmpl = research_competitive_intel();
    let a = ctrl.spawn("owner-a", "x", "a", &tmpl).await.unwrap();
    let _b = ctrl.spawn("owner-b", "x", "a", &tmpl).await.unwrap();

    assert_eq!(ctrl.list("owner-a").len(), 1);
    assert_eq!(ctrl.list("owner-b").len(), 1);
    assert_eq!(ctrl.list("owner-c").len(), 0);

    // owner-a's pod is invisible to owner-b (existence hidden as NotFound).
    assert!(ctrl.get("owner-a", &a.pod_id).is_ok());
    assert!(matches!(
        ctrl.get("owner-b", &a.pod_id),
        Err(PodError::NotFound)
    ));
}

#[tokio::test]
async fn evaluate_accumulates_measured_spend_and_isolates() {
    let mock = MockTransport::new();
    let ctrl = PodController::new(mock.clone(), "a".into());
    let tmpl = research_competitive_intel();
    let pod = ctrl.spawn("owner-a", "x", "acct-a", &tmpl).await.unwrap();
    let id = pod.pod_id.clone();

    // A different tenant cannot evaluate this pod.
    assert!(matches!(
        ctrl.evaluate("owner-b", &id, json!({})).await,
        Err(PodError::NotFound)
    ));

    let r1 = ctrl
        .evaluate("owner-a", &id, json!({ "task": "q1" }))
        .await
        .unwrap();
    assert!((r1.spend_usd - 0.0125).abs() < 1e-9);
    let r2 = ctrl.evaluate("owner-a", &id, json!({})).await.unwrap();
    assert!((r2.spend_usd - 0.0250).abs() < 1e-9);
    assert!(r2.last_bench.is_some());

    let calls = mock.evaluate_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["pod_id"], id);
    assert_eq!(calls[0]["account_id"], "acct-a");
    assert_eq!(calls[0]["template_ref"], "research/competitive-intel@1");
    assert_eq!(calls[0]["task"], "q1");
}

#[tokio::test]
async fn stopped_pod_rejects_evaluation() {
    let ctrl = PodController::new(MockTransport::new(), "a".into());
    let tmpl = research_competitive_intel();
    let pod = ctrl.spawn("owner-a", "x", "a", &tmpl).await.unwrap();
    let stopped = ctrl.stop("owner-a", &pod.pod_id).unwrap();
    assert_eq!(stopped.status, "STOPPED");
    assert!(matches!(
        ctrl.evaluate("owner-a", &pod.pod_id, json!({})).await,
        Err(PodError::Conflict(_))
    ));
}

#[tokio::test]
async fn missing_pods_spawn_scope_is_rejected() {
    let mock = MockTransport::failing_spawn(GatewayError::MissingScope(Scope::PodsSpawn));
    let ctrl = PodController::new(mock, "a".into());
    let tmpl = research_competitive_intel();
    let err = ctrl.spawn("owner-a", "x", "a", &tmpl).await.unwrap_err();
    assert!(matches!(
        err,
        PodError::Gateway(GatewayError::MissingScope(Scope::PodsSpawn))
    ));
}

#[tokio::test]
async fn planned_transport_is_a_dry_run_with_no_spend() {
    let ctrl = PodController::new(Arc::new(PlannedTransport), "a".into());
    let tmpl = research_competitive_intel();
    let pod = ctrl.spawn("owner-a", "x", "a", &tmpl).await.unwrap();
    assert_eq!(pod.status, "PLANNED");
    assert!(pod.planned);
    // Evaluation needs the live gateway; the dry-run cannot.
    assert!(matches!(
        ctrl.evaluate("owner-a", &pod.pod_id, json!({})).await,
        Err(PodError::Gateway(GatewayError::NotConfigured))
    ));
}

// ---- HTTP route tests --------------------------------------------------------

mod http {
    use super::*;
    use crate::{router, AppState};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn spawn_req(session: &str, body: Value) -> Request<Body> {
        Request::post("/api/pods")
            .header("content-type", "application/json")
            .header("x-session", session)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn spawn_lists_and_evaluates_over_http() {
        let state = AppState::in_memory_with_pods(MockTransport::new(), "acct-http");
        let app = router(state);

        // Spawn from a catalogue template_ref.
        let resp = app
            .clone()
            .oneshot(spawn_req(
                "tenant-a",
                json!({ "template_ref": "research/competitive-intel@1" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let id = v["pod_id"].as_str().unwrap().to_string();
        assert_eq!(v["template_ref"], "research/competitive-intel@1");
        assert_eq!(v["account_id"], "acct-http"); // controller default account
        assert_eq!(v["status"], "SPAWNED");

        // The pod's room (board) was auto-created.
        let board = app
            .clone()
            .oneshot(
                Request::get("/api/boards/research-intel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(board.status(), StatusCode::OK);

        // The owner lists their pod.
        let list = body_json(
            app.clone()
                .oneshot(
                    Request::get("/api/pods")
                        .header("x-session", "tenant-a")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(list["pods"].as_array().unwrap().len(), 1);

        // Evaluate updates measured spend.
        let ev = body_json(
            app.clone()
                .oneshot(
                    Request::post(format!("/api/pods/{id}/evaluate"))
                        .header("content-type", "application/json")
                        .header("x-session", "tenant-a")
                        .body(Body::from(json!({ "task": "scan" }).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert!((ev["spend_usd"].as_f64().unwrap() - 0.0125).abs() < 1e-9);
    }

    #[tokio::test]
    async fn cross_tenant_access_is_hidden_over_http() {
        let state = AppState::in_memory_with_pods(MockTransport::unique_ids(), "acct-http");
        let app = router(state);

        let v = body_json(
            app.clone()
                .oneshot(spawn_req(
                    "tenant-a",
                    json!({ "template_ref": "security/dep-cve-watch@1" }),
                ))
                .await
                .unwrap(),
        )
        .await;
        let id = v["pod_id"].as_str().unwrap().to_string();

        // tenant-b cannot see tenant-a's pod by id...
        let resp = app
            .clone()
            .oneshot(
                Request::get(format!("/api/pods/{id}"))
                    .header("x-session", "tenant-b")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // ...nor evaluate it...
        let resp = app
            .clone()
            .oneshot(
                Request::post(format!("/api/pods/{id}/evaluate"))
                    .header("x-session", "tenant-b")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // ...and tenant-b's list is empty.
        let list = body_json(
            app.clone()
                .oneshot(
                    Request::get("/api/pods")
                        .header("x-session", "tenant-b")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(list["pods"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn unknown_template_ref_is_404() {
        let state = AppState::in_memory_with_pods(MockTransport::new(), "acct-http");
        let app = router(state);
        let resp = app
            .oneshot(spawn_req("tenant-a", json!({ "template_ref": "no/such@1" })))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn spawn_without_template_is_400() {
        let state = AppState::in_memory_with_pods(MockTransport::new(), "acct-http");
        let app = router(state);
        let resp = app
            .oneshot(spawn_req("tenant-a", json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn templates_endpoint_lists_the_six_verticals() {
        let state = AppState::in_memory_with_pods(MockTransport::new(), "acct-http");
        let app = router(state);
        let json = body_json(
            app.oneshot(
                Request::get("/api/pods/templates")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(json["templates"].as_array().unwrap().len(), 6);
    }
}
