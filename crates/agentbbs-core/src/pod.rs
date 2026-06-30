//! Domain agent-pod templates — the AgentBBS-side control-plane primitive for
//! ADR-0035 (MetaHarness domain agent pods).
//!
//! A [`PodTemplate`] is the declarative definition of a hosted autonomous
//! worker: a domain system prompt, its (firewalled) tools, the **behavioral
//! gate** (`bench_assertions`) that every loop must pass, and the governance
//! bounds that make it runaway-proof — a Reserve-and-Commit hard cap
//! (`per_agent_cap_usd`) and a tier ceiling ([`MaxTier`]). Pods report signed
//! step-results into `registered_room` (a board slug, ADR-0003). This module
//! owns only the *type + validation*; spawning/monitoring (the `PodController`)
//! lives in `agentbbs-web`.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The model-tier ceiling a pod may escalate to (cheap-by-default, frontier on
/// hard steps — the meta-llm `cognitum-auto` dial, ADR-0034). Ordered
/// `Low < Mid < High`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTier {
    /// Cheapest tier only.
    Low,
    /// Up to the mid tier.
    Mid,
    /// May escalate to the frontier tier.
    High,
}

/// The declarative definition of a domain agent pod (ADR-0035 `template_ref`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PodTemplate {
    /// Stable reference, `domain/name@version` (e.g. `research/competitive-intel@1`).
    pub template_ref: String,
    /// Vertical: `research` | `coding` | `security` | `trading` | `tasks` | `business-ops`.
    pub domain: String,
    /// Domain system prompt steering the pod.
    pub system_prompt: String,
    /// Domain tools the pod may call (AgentiCow-firewalled at runtime).
    pub tools: Vec<String>,
    /// The AgentiCow behavioral pass/fail set — the per-loop gate. The heart of
    /// each vertical; required (an ungated pod is not allowed).
    pub bench_assertions: String,
    /// Reserve-and-Commit hard spend cap, USD. Must be finite and > 0.
    pub per_agent_cap_usd: f64,
    /// Cron schedule for a recurring pod; `None` for a bounded long-horizon run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_schedule: Option<String>,
    /// Tier ceiling.
    pub max_tier: MaxTier,
    /// Board slug the pod posts signed step-results into (`rooms = boards`).
    pub registered_room: String,
}

impl PodTemplate {
    /// Validate the template's invariants (ADR-0035): non-empty identity fields,
    /// a positive finite spend cap (runaway-proof), a required behavioral gate,
    /// a slug-shaped room, and a 5-field cron when scheduled. Returns
    /// [`Error::malformed`] on the first violation.
    pub fn validate(&self) -> Result<()> {
        if self.template_ref.trim().is_empty() || !self.template_ref.contains('@') {
            return Err(Error::malformed(
                "pod",
                "template_ref must be non-empty and shaped domain/name@version",
            ));
        }
        for (field, val) in [
            ("domain", &self.domain),
            ("system_prompt", &self.system_prompt),
            ("bench_assertions", &self.bench_assertions),
        ] {
            if val.trim().is_empty() {
                return Err(Error::malformed(
                    "pod",
                    format!("{field} must not be empty"),
                ));
            }
        }
        if !self.per_agent_cap_usd.is_finite() || self.per_agent_cap_usd <= 0.0 {
            return Err(Error::malformed(
                "pod",
                "per_agent_cap_usd must be finite and > 0 (a pod must be bounded)",
            ));
        }
        if !is_slug(&self.registered_room) {
            return Err(Error::malformed(
                "pod",
                "registered_room must be a board slug ([a-z0-9-]+)",
            ));
        }
        if let Some(cron) = &self.cron_schedule {
            if cron.split_whitespace().count() != 5 {
                return Err(Error::malformed("pod", "cron_schedule must have 5 fields"));
            }
        }
        Ok(())
    }
}

/// A board-slug check: non-empty, lowercase alphanumerics and hyphens only.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn research_pod() -> PodTemplate {
        PodTemplate {
            template_ref: "research/competitive-intel@1".into(),
            domain: "research".into(),
            system_prompt: "You are a competitive-intelligence analyst.".into(),
            tools: vec!["web.search".into(), "rvf.memory".into()],
            bench_assertions: "every briefing claim has >=2 independent dated sources".into(),
            per_agent_cap_usd: 0.10,
            cron_schedule: Some("0 * * * *".into()),
            max_tier: MaxTier::Mid,
            registered_room: "research-intel".into(),
        }
    }

    #[test]
    fn valid_template_passes_and_roundtrips() {
        let p = research_pod();
        assert!(p.validate().is_ok());
        // Serde shape matches ADR-0035.
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["template_ref"], "research/competitive-intel@1");
        assert_eq!(v["max_tier"], "mid");
        let back: PodTemplate = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn long_horizon_pod_omits_cron() {
        let mut p = research_pod();
        p.cron_schedule = None;
        assert!(p.validate().is_ok());
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("cron_schedule").is_none()); // skipped when None
    }

    #[test]
    fn unbounded_cap_is_rejected() {
        let mut p = research_pod();
        p.per_agent_cap_usd = 0.0;
        assert!(p.validate().is_err());
        p.per_agent_cap_usd = -1.0;
        assert!(p.validate().is_err());
        p.per_agent_cap_usd = f64::INFINITY;
        assert!(p.validate().is_err());
    }

    #[test]
    fn ungated_or_malformed_fields_rejected() {
        let mut p = research_pod();
        p.bench_assertions = "  ".into();
        assert!(p.validate().is_err()); // the behavioral gate is required

        let mut p = research_pod();
        p.registered_room = "Research Intel".into();
        assert!(p.validate().is_err()); // not a slug

        let mut p = research_pod();
        p.template_ref = "no-version".into();
        assert!(p.validate().is_err());

        let mut p = research_pod();
        p.cron_schedule = Some("0 *".into());
        assert!(p.validate().is_err()); // not 5 fields
    }

    #[test]
    fn tier_ordering() {
        assert!(MaxTier::Low < MaxTier::Mid && MaxTier::Mid < MaxTier::High);
    }
}
