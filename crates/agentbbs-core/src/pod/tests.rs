//! Unit tests for [`super`] — template-ref parsing, per-vertical template
//! shape, bench-assertion behaviour, the spawn-payload wire shape, and serde
//! round-trips. No network: this is the ADR-0035 Phase-0 ($0) gate.

use super::catalogue::*;
use super::*;

// ---- TemplateRef -------------------------------------------------------------

#[test]
fn template_ref_parses_three_segments() {
    let r = TemplateRef::new("research", "competitive-intel", 1);
    assert_eq!(r.as_str(), "research/competitive-intel@1");
    let (domain, name, version) = r.parts().unwrap();
    assert_eq!(
        (domain, name, version),
        ("research", "competitive-intel", "1")
    );
    r.validate().unwrap();
}

#[test]
fn template_ref_rejects_malformed() {
    assert!(TemplateRef("no-separators".into()).validate().is_err());
    assert!(TemplateRef("missing/version".into()).validate().is_err());
    assert!(TemplateRef("/empty-domain@1".into()).validate().is_err());
    assert!(TemplateRef("domain/@1".into()).validate().is_err());
    assert!(TemplateRef("domain/name@".into()).validate().is_err());
}

#[test]
fn template_ref_serialises_transparently() {
    let r = TemplateRef::new("security", "dep-cve-watch", 1);
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(json, "\"security/dep-cve-watch@1\"");
    let back: TemplateRef = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}

// ---- Catalogue completeness --------------------------------------------------

#[test]
fn catalogue_has_all_six_distinct_verticals() {
    let cat = catalogue();
    assert_eq!(cat.len(), 6);
    let domains: Vec<Domain> = cat.iter().map(|t| t.domain).collect();
    for d in [
        Domain::Research,
        Domain::Coding,
        Domain::Security,
        Domain::Trading,
        Domain::Tasks,
        Domain::BusinessOps,
    ] {
        assert!(domains.contains(&d), "missing vertical {d:?}");
    }
    // template_refs are unique and registered_rooms are unique.
    let mut refs: Vec<&str> = cat.iter().map(|t| t.id.as_str()).collect();
    refs.sort_unstable();
    refs.dedup();
    assert_eq!(refs.len(), 6, "duplicate template_ref");
}

#[test]
fn every_catalogue_template_validates() {
    for t in catalogue() {
        t.validate()
            .unwrap_or_else(|e| panic!("{} failed validation: {e}", t.id));
        // template_ref domain segment must equal the domain slug.
        let (seg, _, _) = t.id.parts().unwrap();
        assert_eq!(seg, t.domain_slug());
        // governance invariants hold.
        assert!(t.per_agent_cap_usd > 0.0);
        assert!(!t.tools.is_empty());
        assert!(!t.bench_assertions.is_empty());
    }
}

// ---- Per-vertical shape ------------------------------------------------------

#[test]
fn research_template_shape() {
    let t = research_competitive_intel();
    assert_eq!(t.domain, Domain::Research);
    assert_eq!(t.id.as_str(), "research/competitive-intel@1");
    assert_eq!(t.max_tier, MaxTier::Mid);
    assert_eq!(t.cron_schedule.as_deref(), Some("0 * * * *"));
    assert_eq!(t.registered_room, "research-intel");
    assert!((t.per_agent_cap_usd - 0.10).abs() < f64::EPSILON);
    let tool_names: Vec<&str> = t.tools.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(tool_names, ["web.search", "web.fetch", "rvf.memory"]);
}

#[test]
fn coding_template_shape() {
    let t = coding_patch_green();
    assert_eq!(t.domain, Domain::Coding);
    assert_eq!(t.max_tier, MaxTier::High);
    // coding is long-horizon (no cron) — gated by tests, not a schedule.
    assert!(t.cron_schedule.is_none());
}

#[test]
fn trading_template_shape() {
    let t = trading_signal_guarded();
    assert_eq!(t.domain, Domain::Trading);
    assert_eq!(t.max_tier, MaxTier::High);
    assert_eq!(t.cron_schedule.as_deref(), Some("*/5 * * * *"));
    assert!((t.per_agent_cap_usd - 1.00).abs() < f64::EPSILON);
}

#[test]
fn business_ops_domain_slug_is_kebab() {
    let t = business_ops_reconciliation();
    assert_eq!(t.domain_slug(), "business-ops");
    // Domain enum serialises kebab-case to match the template_ref segment.
    let json = serde_json::to_string(&t.domain).unwrap();
    assert_eq!(json, "\"business-ops\"");
}

// ---- Bench assertions (the behavioural gate) ---------------------------------

#[test]
fn coding_bench_asserts_tests_pass() {
    let t = coding_patch_green();
    assert!(t
        .bench_assertions
        .iter()
        .all(|a| a.kind == BenchKind::TestsPass));
    assert!(t
        .bench_assertions
        .iter()
        .any(|a| a.id == "tests-green" && a.description.contains("test suite is green")));
}

#[test]
fn security_bench_asserts_scan_finding_with_reproducer() {
    let t = security_dep_cve_watch();
    assert!(t
        .bench_assertions
        .iter()
        .all(|a| a.kind == BenchKind::ScanFinding));
    let repro = t
        .bench_assertions
        .iter()
        .find(|a| a.id == "reproducer-confirmed")
        .expect("reproducer assertion present");
    assert!(repro.description.contains("demoted"));
    assert!(repro.description.contains("never reported"));
}

#[test]
fn trading_bench_asserts_risk_before_order() {
    let t = trading_signal_guarded();
    assert!(t
        .bench_assertions
        .iter()
        .all(|a| a.kind == BenchKind::RiskCheck));
    let exposure = t
        .bench_assertions
        .iter()
        .find(|a| a.id == "exposure")
        .unwrap();
    assert!(exposure
        .description
        .contains("BEFORE any order intent is produced"));
}

#[test]
fn research_bench_asserts_source_grading() {
    let t = research_competitive_intel();
    assert!(t
        .bench_assertions
        .iter()
        .all(|a| a.kind == BenchKind::SourceGrading));
}

#[test]
fn business_ops_bench_asserts_reconciliation_balance() {
    let t = business_ops_reconciliation();
    assert!(t
        .bench_assertions
        .iter()
        .all(|a| a.kind == BenchKind::ReconciliationBalance));
    assert!(t
        .bench_assertions
        .iter()
        .any(|a| a.description.contains("debits == credits")));
}

// ---- Validation rejects bad templates ----------------------------------------

#[test]
fn validate_rejects_mismatched_domain_segment() {
    let mut t = research_competitive_intel();
    t.domain = Domain::Coding; // id still says research/...
    assert!(t.validate().is_err());
}

#[test]
fn validate_rejects_nonpositive_cap() {
    let mut t = tasks_checklist_runner();
    t.per_agent_cap_usd = 0.0;
    assert!(t.validate().is_err());
    t.per_agent_cap_usd = f64::NAN;
    assert!(t.validate().is_err());
}

#[test]
fn validate_rejects_empty_tools_or_asserts() {
    let mut t = tasks_checklist_runner();
    t.tools.clear();
    assert!(t.validate().is_err());
    let mut t2 = tasks_checklist_runner();
    t2.bench_assertions.clear();
    assert!(t2.validate().is_err());
}

// ---- Spawn payload (wire shape consumed by meta-llm /v1/pods/spawn) ----------

#[test]
fn spawn_request_flattens_tools_and_asserts() {
    let t = research_competitive_intel();
    let req = t.to_spawn_request();
    assert_eq!(req.template_ref, t.id);
    assert_eq!(req.tools, ["web.search", "web.fetch", "rvf.memory"]);
    // bench_assertions collapse to one ';'-joined predicate string.
    assert!(req
        .bench_assertions
        .contains(">=2 independent dated sources"));
    assert!(req.bench_assertions.contains("; "));
    assert_eq!(req.cron_schedule.as_deref(), Some("0 * * * *"));
    assert_eq!(req.max_tier, MaxTier::Mid);
}

#[test]
fn spawn_request_json_matches_wire_contract() {
    let req = research_competitive_intel().to_spawn_request();
    let v: serde_json::Value = serde_json::to_value(&req).unwrap();
    // Field names + types match the ADR-0035 §3 template_ref payload.
    assert_eq!(v["template_ref"], "research/competitive-intel@1");
    assert_eq!(v["domain"], "research");
    assert!(v["tools"].is_array());
    assert!(v["tools"][0].is_string());
    assert!(v["bench_assertions"].is_string());
    assert!(v["per_agent_cap_usd"].is_number());
    assert_eq!(v["max_tier"], "mid");
    assert_eq!(v["registered_room"], "research-intel");
}

#[test]
fn spawn_request_omits_cron_for_long_horizon() {
    // coding is long-horizon (cron_schedule = None) → key absent on the wire.
    let req = coding_patch_green().to_spawn_request();
    let v: serde_json::Value = serde_json::to_value(&req).unwrap();
    assert!(v.get("cron_schedule").is_none());
}

// ---- Full PodTemplate serde round-trip ---------------------------------------

#[test]
fn pod_template_round_trips_through_json() {
    for t in catalogue() {
        let json = serde_json::to_string(&t).unwrap();
        let back: PodTemplate = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
        // `id` serialises under the `template_ref` key.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("template_ref").is_some());
        assert!(v.get("id").is_none());
    }
}
