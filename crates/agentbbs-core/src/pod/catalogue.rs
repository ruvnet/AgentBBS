//! The curated six-vertical domain-pod catalogue (ADR-0035 §3).
//!
//! Each function returns a ready-to-spawn [`PodTemplate`] starting point;
//! [`catalogue`] returns all six. These are *data* — a self-hoster forks one,
//! tweaks the prompt/cap/cron, and stores it per-tenant. The three concrete
//! examples from ADR-0035 §3 (research, security, trading) are reproduced
//! faithfully; coding, tasks, and business-ops follow the same behavioural-gate
//! contract from the §3 table.

use super::{BenchAssertion, BenchKind, Domain, MaxTier, PodTemplate, TemplateRef, ToolSpec};

/// (a) Hourly research pod — cheap-tier scanning, frontier only on synthesis.
pub fn research_competitive_intel() -> PodTemplate {
    PodTemplate {
        id: TemplateRef::new("research", "competitive-intel", 1),
        domain: Domain::Research,
        system_prompt: "You are a competitive-intelligence analyst. Each cycle: \
            scan the configured sources, extract material changes, grade every \
            claim by source, and synthesize a briefing only when something \
            material changed."
            .to_string(),
        tools: vec![
            ToolSpec::new("web.search", "Search the open web for sources."),
            ToolSpec::new("web.fetch", "Fetch and read a source URL."),
            ToolSpec::new(
                "rvf.memory",
                "Recall/store prior briefings in vector memory.",
            ),
        ],
        bench_assertions: vec![
            BenchAssertion::new(
                "sources-graded",
                BenchKind::SourceGrading,
                "every briefing claim has >=2 independent dated sources",
            ),
            BenchAssertion::new(
                "no-contradiction",
                BenchKind::SourceGrading,
                "no claim contradicts a higher-graded source",
            ),
        ],
        per_agent_cap_usd: 0.10,
        cron_schedule: Some("0 * * * *".to_string()),
        max_tier: MaxTier::Mid,
        registered_room: "research-intel".to_string(),
    }
}

/// Coding pod — generate a patch and gate on a green test suite.
pub fn coding_patch_green() -> PodTemplate {
    PodTemplate {
        id: TemplateRef::new("coding", "patch-green", 1),
        domain: Domain::Coding,
        system_prompt: "You are a software engineer. Each task: produce a minimal \
            patch that resolves the issue, then run the task's test suite. Only \
            submit when the patch compiles and every test is green."
            .to_string(),
        tools: vec![
            ToolSpec::new("repo.read", "Read repository files."),
            ToolSpec::new("repo.patch", "Apply a patch to the working tree."),
            ToolSpec::new("sandbox.exec", "Run build/test commands in the sandbox."),
        ],
        bench_assertions: vec![
            BenchAssertion::new(
                "compiles",
                BenchKind::TestsPass,
                "the produced patch compiles cleanly",
            ),
            BenchAssertion::new(
                "tests-green",
                BenchKind::TestsPass,
                "the task's test suite is green; TOOLING vs GENUINE split is \
                 recorded per ADR-0023",
            ),
        ],
        per_agent_cap_usd: 0.75,
        cron_schedule: None,
        max_tier: MaxTier::High,
        registered_room: "coding-tasks".to_string(),
    }
}

/// (b) Nightly security pod — dependency + CVE scan with reproducer asserts.
pub fn security_dep_cve_watch() -> PodTemplate {
    PodTemplate {
        id: TemplateRef::new("security", "dep-cve-watch", 1),
        domain: Domain::Security,
        system_prompt: "You are an application-security auditor. Each night: \
            enumerate dependencies, match against CVE feeds, and for each \
            candidate finding produce a minimal reproducer or downgrade it to \
            informational."
            .to_string(),
        tools: vec![
            ToolSpec::new("repo.read", "Read repository files."),
            ToolSpec::new("deps.list", "Enumerate project dependencies."),
            ToolSpec::new("cve.lookup", "Match dependencies against CVE feeds."),
            ToolSpec::new("sandbox.exec", "Confirm a reproducer in the sandbox."),
        ],
        bench_assertions: vec![
            BenchAssertion::new(
                "cve-mapped",
                BenchKind::ScanFinding,
                "each HIGH/CRITICAL finding maps to a CVE/CWE id",
            ),
            BenchAssertion::new(
                "reproducer-confirmed",
                BenchKind::ScanFinding,
                "each HIGH/CRITICAL finding has a reproducer the sandbox \
                 confirms; unreproducible findings are demoted, never reported \
                 as confirmed",
            ),
        ],
        per_agent_cap_usd: 0.50,
        cron_schedule: Some("0 3 * * *".to_string()),
        max_tier: MaxTier::Mid,
        registered_room: "security-watch".to_string(),
    }
}

/// (c) Per-tick trading pod — risk checks gate every signal; frontier only on
/// escalation.
pub fn trading_signal_guarded() -> PodTemplate {
    PodTemplate {
        id: TemplateRef::new("trading", "signal-guarded", 1),
        domain: Domain::Trading,
        system_prompt: "You are a disciplined trading-signal generator. Each \
            tick: evaluate the strategy, and emit a signal ONLY if every risk \
            and position constraint holds. Escalate to frontier reasoning only \
            when the cheap pass is ambiguous."
            .to_string(),
        tools: vec![
            ToolSpec::new("market.ohlcv", "Fetch OHLCV market data."),
            ToolSpec::new("portfolio.state", "Read current positions and exposure."),
            ToolSpec::new("risk.check", "Evaluate risk/position constraints."),
        ],
        bench_assertions: vec![
            BenchAssertion::new(
                "position-size",
                BenchKind::RiskCheck,
                "position_size <= max_position",
            ),
            BenchAssertion::new(
                "drawdown",
                BenchKind::RiskCheck,
                "projected_drawdown <= max_drawdown",
            ),
            BenchAssertion::new(
                "exposure",
                BenchKind::RiskCheck,
                "aggregate_exposure <= exposure_limit; a breach fails the loop \
                 BEFORE any order intent is produced",
            ),
        ],
        per_agent_cap_usd: 1.00,
        cron_schedule: Some("*/5 * * * *".to_string()),
        max_tier: MaxTier::High,
        registered_room: "trading-signals".to_string(),
    }
}

/// Tasks pod — recurring task execution against an idempotent acceptance
/// checklist.
pub fn tasks_checklist_runner() -> PodTemplate {
    PodTemplate {
        id: TemplateRef::new("tasks", "checklist-runner", 1),
        domain: Domain::Tasks,
        system_prompt: "You are a reliable task runner. Each cycle: execute the \
            configured task and verify its acceptance checklist. Re-running must \
            be idempotent — already-satisfied items are left untouched."
            .to_string(),
        tools: vec![
            ToolSpec::new("task.read", "Read the task spec and acceptance checklist."),
            ToolSpec::new("task.exec", "Perform the task's actions."),
            ToolSpec::new("rvf.memory", "Recall prior runs to stay idempotent."),
        ],
        bench_assertions: vec![
            BenchAssertion::new(
                "checklist-satisfied",
                BenchKind::Completion,
                "every item on the task's acceptance checklist is satisfied",
            ),
            BenchAssertion::new(
                "idempotent",
                BenchKind::Completion,
                "re-running produces no further changes (idempotent)",
            ),
        ],
        per_agent_cap_usd: 0.05,
        cron_schedule: Some("*/30 * * * *".to_string()),
        max_tier: MaxTier::Low,
        registered_room: "tasks-runner".to_string(),
    }
}

/// Business-ops pod — ledger reconciliation that must balance to source
/// documents.
pub fn business_ops_reconciliation() -> PodTemplate {
    PodTemplate {
        id: TemplateRef::new("business-ops", "reconciliation", 1),
        domain: Domain::BusinessOps,
        system_prompt: "You are a meticulous bookkeeping assistant. Each cycle: \
            reconcile the ledger against its source documents. Report only when \
            debits and credits balance and every total ties out; flag any \
            discrepancy instead of forcing a balance."
            .to_string(),
        tools: vec![
            ToolSpec::new("ledger.read", "Read ledger entries."),
            ToolSpec::new(
                "docs.fetch",
                "Fetch source documents (invoices, statements).",
            ),
            ToolSpec::new("recon.match", "Match ledger lines to source documents."),
        ],
        bench_assertions: vec![
            BenchAssertion::new(
                "debits-equal-credits",
                BenchKind::ReconciliationBalance,
                "ledger debits == credits",
            ),
            BenchAssertion::new(
                "totals-tie-out",
                BenchKind::ReconciliationBalance,
                "totals tie out to source documents; an unbalanced \
                 reconciliation fails the loop",
            ),
        ],
        per_agent_cap_usd: 0.20,
        cron_schedule: Some("0 6 * * *".to_string()),
        max_tier: MaxTier::Mid,
        registered_room: "business-ops".to_string(),
    }
}

/// The full curated catalogue: one [`PodTemplate`] per vertical.
pub fn catalogue() -> Vec<PodTemplate> {
    vec![
        research_competitive_intel(),
        coding_patch_green(),
        security_dep_cve_watch(),
        trading_signal_guarded(),
        tasks_checklist_runner(),
        business_ops_reconciliation(),
    ]
}
