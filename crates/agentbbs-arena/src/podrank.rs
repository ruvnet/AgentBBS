//! Pareto ranking of pod **configurations** (ADR-0035 / ADR-0023).
//!
//! A domain agent pod runs as `{domain × host × tier}`. Given observed results
//! per config — accuracy (bench pass-rate / requirement coverage, maximize) and
//! cost ($/task, minimize) — this ranks configs on the same accuracy-vs-cost
//! **Pareto frontier** the Retort track uses, so the cheapest config that still
//! clears the bar for a vertical is surfaced (the "most cost-effective config
//! per domain"). Reuses [`crate::pareto`] for the dominance relation, so a pod
//! config and a Retort stack are ranked by identical rules.

use serde::{Deserialize, Serialize};

use crate::pareto::{nondominated_tiers, ParetoPoint};

/// The identity of a pod configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodConfig {
    /// Vertical (research, coding, security, …).
    pub domain: String,
    /// Host runtime (claude-code, codex, native, …).
    pub host: String,
    /// Tier the pod ran at (low | mid | high).
    pub tier: String,
}

impl PodConfig {
    /// A stable `domain×host×tier` label.
    pub fn label(&self) -> String {
        format!("{}×{}×{}", self.domain, self.host, self.tier)
    }
}

/// An observed result for one pod config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PodConfigResult {
    /// Which config.
    pub config: PodConfig,
    /// Accuracy in `[0,1]` (bench pass-rate / requirement coverage), maximize.
    pub accuracy: f64,
    /// Cost in $/task, minimize.
    pub cost_usd: f64,
    /// How many runs this aggregate is over (for context; not ranked on).
    #[serde(default)]
    pub runs: u32,
}

/// A ranked pod config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PodConfigStanding {
    /// The config.
    pub config: PodConfig,
    /// Accuracy (maximize).
    pub accuracy: f64,
    /// Cost $/task (minimize).
    pub cost_usd: f64,
    /// Pareto tier (1 = frontier).
    pub pareto_tier: u32,
    /// Whether this config is on the accuracy-vs-cost frontier.
    pub on_frontier: bool,
    /// 1-based overall rank (frontier first, then by accuracy desc, cost asc).
    pub rank: usize,
}

/// Rank pod configs by Pareto position. Frontier (tier 1) configs come first;
/// within a tier, higher accuracy then lower cost. Deterministic.
pub fn rank_pod_configs(results: &[PodConfigResult]) -> Vec<PodConfigStanding> {
    let points: Vec<ParetoPoint> = results
        .iter()
        .map(|r| ParetoPoint {
            coverage: r.accuracy,
            cost: r.cost_usd,
        })
        .collect();
    let tiers = nondominated_tiers(&points);

    let mut standings: Vec<PodConfigStanding> = results
        .iter()
        .zip(tiers.iter())
        .map(|(r, &t)| PodConfigStanding {
            config: r.config.clone(),
            accuracy: r.accuracy,
            cost_usd: r.cost_usd,
            pareto_tier: t,
            on_frontier: t == 1,
            rank: 0,
        })
        .collect();

    standings.sort_by(|a, b| {
        a.pareto_tier
            .cmp(&b.pareto_tier)
            .then(
                b.accuracy
                    .partial_cmp(&a.accuracy)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                a.cost_usd
                    .partial_cmp(&b.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.config.label().cmp(&b.config.label()))
    });
    for (i, s) in standings.iter_mut().enumerate() {
        s.rank = i + 1;
    }
    standings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(domain: &str, host: &str, tier: &str) -> PodConfig {
        PodConfig {
            domain: domain.into(),
            host: host.into(),
            tier: tier.into(),
        }
    }
    fn res(c: PodConfig, accuracy: f64, cost_usd: f64) -> PodConfigResult {
        PodConfigResult {
            config: c,
            accuracy,
            cost_usd,
            runs: 10,
        }
    }

    #[test]
    fn frontier_beats_dominated_and_ranks_cheap_first() {
        let results = vec![
            // expensive frontier-accuracy
            res(cfg("research", "claude-code", "high"), 0.92, 0.020),
            // cheaper, nearly-as-accurate → also on frontier (a tradeoff point)
            res(cfg("research", "native", "low"), 0.88, 0.002),
            // dominated: less accurate AND pricier than the low-tier native
            res(cfg("research", "codex", "mid"), 0.80, 0.010),
        ];
        let r = rank_pod_configs(&results);
        // The dominated config is last and not on the frontier.
        let dominated = r.iter().find(|s| s.config.host == "codex").unwrap();
        assert!(!dominated.on_frontier);
        assert_eq!(dominated.rank, 3);
        // The two tradeoff points are both on the frontier (tier 1).
        assert!(r.iter().filter(|s| s.on_frontier).count() == 2);
        // Highest accuracy frontier point ranks first.
        assert_eq!(r[0].config.tier, "high");
        assert_eq!(r[0].rank, 1);
    }

    #[test]
    fn single_config_is_trivially_frontier() {
        let r = rank_pod_configs(&[res(cfg("coding", "native", "mid"), 0.7, 0.005)]);
        assert_eq!(r.len(), 1);
        assert!(r[0].on_frontier && r[0].pareto_tier == 1 && r[0].rank == 1);
        assert_eq!(r[0].config.label(), "coding×native×mid");
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(rank_pod_configs(&[]).is_empty());
    }
}
