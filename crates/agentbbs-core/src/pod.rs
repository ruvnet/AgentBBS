//! Domain agent pods — declarative templates for MetaHarness Darwin-Loop pods
//! (ADR-0035).
//!
//! A [`PodTemplate`] is the *data* definition of a domain pod: the system
//! prompt, the domain tools (AgentiCow-firewalled), the behavioural
//! [`BenchAssertion`]s that gate each loop, and the governance knobs
//! (`per_agent_cap_usd`, `max_tier`, optional `cron_schedule`). AgentBBS owns
//! and stores these; the meta-llm gateway *consumes* them on
//! `POST /v1/pods/spawn`.
//!
//! Two shapes live here, on purpose:
//!
//! - [`PodTemplate`] is the rich, AgentBBS-side catalogue type — tools and
//!   bench-assertions are structured so the TUI/web can render them and the
//!   firewall can reason about them.
//! - [`SpawnRequest`] is the flattened *wire* payload that exactly matches the
//!   `template_ref` object meta-llm's `/v1/pods/spawn` expects (tools as a
//!   string array, bench-assertions joined into one string, optional cron
//!   omitted when absent). [`PodTemplate::to_spawn_request`] converts.
//!
//! Adding a vertical is *data, not code*: a new [`PodTemplate`] in the
//! [`catalogue`], not a gateway change. The six curated starting points
//! (research, coding, security, trading, tasks, business-ops) live in
//! [`catalogue`].

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A domain-pod template reference, e.g. `research/competitive-intel@1`.
///
/// The canonical form is `<domain-segment>/<name>@<version>`. It is stored and
/// transmitted as a plain string (serde-transparent) so it round-trips byte for
/// byte with the gateway, but parses into its three segments for validation and
/// display.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TemplateRef(pub String);

impl TemplateRef {
    /// Build a [`TemplateRef`] from `domain`, `name`, and `version`.
    pub fn new(domain: &str, name: &str, version: u32) -> Self {
        TemplateRef(format!("{domain}/{name}@{version}"))
    }

    /// The raw string form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate the `<domain>/<name>@<version>` shape, returning the
    /// `(domain, name, version)` segments on success.
    pub fn parts(&self) -> Result<(&str, &str, &str)> {
        let (domain, rest) = self
            .0
            .split_once('/')
            .ok_or_else(|| Error::malformed("template_ref", "missing '/' domain separator"))?;
        let (name, version) = rest
            .split_once('@')
            .ok_or_else(|| Error::malformed("template_ref", "missing '@' version separator"))?;
        if domain.is_empty() || name.is_empty() || version.is_empty() {
            return Err(Error::malformed("template_ref", "empty segment"));
        }
        if name.contains('/') || version.contains('@') {
            return Err(Error::malformed("template_ref", "too many separators"));
        }
        Ok((domain, name, version))
    }

    /// Validate the reference shape.
    pub fn validate(&self) -> Result<()> {
        self.parts().map(|_| ())
    }
}

impl std::fmt::Display for TemplateRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The six first-class pod verticals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Domain {
    /// Competitive-intelligence / source-graded research.
    Research,
    /// Code generation gated on a green test suite.
    Coding,
    /// Dependency / CVE auditing with reproducer asserts.
    Security,
    /// Risk-gated trading-signal generation.
    Trading,
    /// Recurring task execution against an acceptance checklist.
    Tasks,
    /// Ledger / reconciliation business operations.
    BusinessOps,
}

/// The gateway tier ceiling a pod is allowed to escalate to.
///
/// Maps to the meta-llm `cognitum-{low,mid,high}` pins; `max_tier` caps how far
/// the `cognitum-auto` router may escalate on a hard step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaxTier {
    /// Cheap tier only.
    Low,
    /// Up to the mid tier.
    Mid,
    /// Up to the frontier tier.
    High,
}

/// A single domain tool the pod may call, behind the AgentiCow firewall.
///
/// The `name` is the firewalled tool id the gateway/runtime resolves (e.g.
/// `web.search`); `description` is human-facing catalogue/UI text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Firewalled tool id, e.g. `web.search`, `risk.check`.
    pub name: String,
    /// Human-readable description for the catalogue/UI.
    pub description: String,
}

impl ToolSpec {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        ToolSpec {
            name: name.into(),
            description: description.into(),
        }
    }
}

/// The behavioural family a [`BenchAssertion`] belongs to — the per-loop
/// pass/fail gate that makes a vertical a *domain* pod and not a generic agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BenchKind {
    /// research: each claim carries enough independent, dated sources.
    SourceGrading,
    /// coding: the produced patch compiles and the task's tests are green.
    TestsPass,
    /// security: a finding maps to a CVE/CWE and has a confirmed reproducer.
    ScanFinding,
    /// trading: position / drawdown / exposure limits hold before any order.
    RiskCheck,
    /// tasks: the acceptance checklist is satisfied and idempotent.
    Completion,
    /// business-ops: ledger debits = credits and tie out to source documents.
    ReconciliationBalance,
}

/// A behavioural pass/fail assertion (the AgentiCow gate) for one loop step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchAssertion {
    /// Stable id within the template, e.g. `tests-green`.
    pub id: String,
    /// Which behavioural family this assertion enforces.
    pub kind: BenchKind,
    /// Human/AgentiCow-readable predicate that must hold for the loop to pass.
    pub description: String,
}

impl BenchAssertion {
    /// Convenience constructor.
    pub fn new(id: impl Into<String>, kind: BenchKind, description: impl Into<String>) -> Self {
        BenchAssertion {
            id: id.into(),
            kind,
            description: description.into(),
        }
    }
}

/// The declarative definition of a domain pod (ADR-0035 §3).
///
/// `id` is the [`TemplateRef`] (`template_ref` on the wire). The governance
/// fields — `per_agent_cap_usd` (Reserve-and-Commit hard cap) and `max_tier`
/// (escalation ceiling) — make a pod bounded and runaway-proof regardless of
/// loop count.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PodTemplate {
    /// The template reference / id (serialised as `template_ref`).
    #[serde(rename = "template_ref")]
    pub id: TemplateRef,
    /// The pod's domain vertical.
    pub domain: Domain,
    /// The domain system prompt.
    pub system_prompt: String,
    /// Domain tools, AgentiCow-firewalled.
    pub tools: Vec<ToolSpec>,
    /// The behavioural pass/fail gate evaluated each loop step.
    pub bench_assertions: Vec<BenchAssertion>,
    /// Per-pod Reserve-and-Commit inference hard cap, in USD.
    pub per_agent_cap_usd: f64,
    /// Optional cron schedule; `None` = long-horizon (bounded by budget/goal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_schedule: Option<String>,
    /// The tier ceiling the router may escalate to.
    pub max_tier: MaxTier,
    /// AgentBBS board slug the pod posts signed step-results into.
    pub registered_room: String,
}

impl PodTemplate {
    /// Validate the template's invariants:
    /// - the [`TemplateRef`] parses and its domain segment matches `domain`;
    /// - at least one tool and one bench assertion are declared;
    /// - the cap is finite and strictly positive;
    /// - the `registered_room` slug is non-empty.
    pub fn validate(&self) -> Result<()> {
        let (domain_seg, _, _) = self.id.parts()?;
        if domain_seg != self.domain_slug() {
            return Err(Error::malformed(
                "pod_template",
                format!(
                    "template_ref domain '{domain_seg}' != domain '{}'",
                    self.domain_slug()
                ),
            ));
        }
        if self.tools.is_empty() {
            return Err(Error::malformed("pod_template", "no tools declared"));
        }
        if self.bench_assertions.is_empty() {
            return Err(Error::malformed(
                "pod_template",
                "no bench_assertions declared",
            ));
        }
        if !self.per_agent_cap_usd.is_finite() || self.per_agent_cap_usd <= 0.0 {
            return Err(Error::malformed(
                "pod_template",
                "per_agent_cap_usd must be finite and > 0",
            ));
        }
        if self.system_prompt.trim().is_empty() {
            return Err(Error::malformed("pod_template", "empty system_prompt"));
        }
        if self.registered_room.trim().is_empty() {
            return Err(Error::malformed("pod_template", "empty registered_room"));
        }
        Ok(())
    }

    /// The kebab-case slug for this template's [`Domain`], matching the
    /// `template_ref` domain segment (e.g. `business-ops`).
    pub fn domain_slug(&self) -> &'static str {
        match self.domain {
            Domain::Research => "research",
            Domain::Coding => "coding",
            Domain::Security => "security",
            Domain::Trading => "trading",
            Domain::Tasks => "tasks",
            Domain::BusinessOps => "business-ops",
        }
    }

    /// Flatten into the [`SpawnRequest`] wire payload meta-llm consumes on
    /// `POST /v1/pods/spawn`: tools collapse to their firewalled ids, and the
    /// bench assertions join (with `; `) into the single `bench_assertions`
    /// string the gateway expects.
    pub fn to_spawn_request(&self) -> SpawnRequest {
        SpawnRequest {
            template_ref: self.id.clone(),
            domain: self.domain,
            system_prompt: self.system_prompt.clone(),
            tools: self.tools.iter().map(|t| t.name.clone()).collect(),
            bench_assertions: self
                .bench_assertions
                .iter()
                .map(|a| a.description.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            per_agent_cap_usd: self.per_agent_cap_usd,
            cron_schedule: self.cron_schedule.clone(),
            max_tier: self.max_tier,
            registered_room: self.registered_room.clone(),
        }
    }
}

/// The flattened `template_ref` payload sent to meta-llm `POST /v1/pods/spawn`.
///
/// Field-for-field with the ADR-0035 §3 JSON: `tools` is a string array,
/// `bench_assertions` is a single string, and `cron_schedule` is omitted when
/// absent (long-horizon pods).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// The template reference / id.
    pub template_ref: TemplateRef,
    /// The pod's domain vertical.
    pub domain: Domain,
    /// The domain system prompt.
    pub system_prompt: String,
    /// Firewalled tool ids.
    pub tools: Vec<String>,
    /// The behavioural pass/fail gate, joined into one predicate string.
    pub bench_assertions: String,
    /// Per-pod Reserve-and-Commit inference hard cap, in USD.
    pub per_agent_cap_usd: f64,
    /// Optional cron schedule; omitted for long-horizon pods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_schedule: Option<String>,
    /// The tier ceiling the router may escalate to.
    pub max_tier: MaxTier,
    /// AgentBBS board slug the pod posts signed step-results into.
    pub registered_room: String,
}

pub mod catalogue;

#[cfg(test)]
mod tests;
