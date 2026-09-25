use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    backtest::{BacktestRequest, run_backtest},
    manifest::{freeze_manifest, unique_strategy_ids},
    replay::ReplayStore,
};

#[derive(Debug, Clone, Deserialize)]
pub struct InferenceRequest {
    pub candidates: Vec<BacktestRequest>,
    #[serde(default = "default_iterations")]
    pub bootstrap_iterations: usize,
    #[serde(default = "default_alpha")]
    pub alpha: f64,
    #[serde(default = "default_min_trades")]
    pub min_trades: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateInference {
    pub strategy_id: String,
    pub trades: usize,
    pub average_pnl: f64,
    pub bootstrap_ci_lower: Option<f64>,
    pub bootstrap_ci_upper: Option<f64>,
    pub raw_one_sided_p: Option<f64>,
    pub holm_adjusted_p: Option<f64>,
    pub bh_fdr_adjusted_p: Option<f64>,
    pub passes_holm: bool,
    pub passes_bh_fdr: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct InferenceReport {
    pub engine: &'static str,
    pub alpha: f64,
    pub bootstrap_iterations: usize,
    pub tested_candidates: usize,
    pub eligible_candidates: usize,
    pub holm_discoveries: usize,
    pub bh_fdr_discoveries: usize,
    pub candidates: Vec<CandidateInference>,
    pub notes: Vec<String>,
}

fn default_iterations() -> usize {
    2_000
}

fn default_alpha() -> f64 {
    0.05
}

fn default_min_trades() -> usize {
    10
}

pub fn run_inference(
    store: &ReplayStore,
    request: &InferenceRequest,
) -> anyhow::Result<InferenceReport> {
    anyhow::ensure!(
        !request.candidates.is_empty() && request.candidates.len() <= 100,
        "inference requires 1-100 candidate strategies"
    );
    anyhow::ensure!(
        (200..=20_000).contains(&request.bootstrap_iterations),
        "bootstrap_iterations must be between 200 and 20000"
    );
    anyhow::ensure!(
        request.alpha > 0.0 && request.alpha < 0.5,
        "alpha must be in (0, 0.5)"
    );
    anyhow::ensure!(request.min_trades >= 3, "min_trades must be at least 3");

    let mut candidates = Vec::with_capacity(request.candidates.len());
    let mut eligible_indices = Vec::new();
    let mut raw_p_values = Vec::new();

    for (index, candidate) in request.candidates.iter().enumerate() {
        let report = run_backtest(store, candidate)?;
        let strategy_id = freeze_manifest(candidate)?.strategy_id;
        let pnls: Vec<f64> = report.trades.iter().map(|trade| trade.pnl).collect();
        let average_pnl = if pnls.is_empty() {
            0.0
        } else {
            pnls.iter().sum::<f64>() / pnls.len() as f64
        };

        if pnls.len() >= request.min_trades {
            let seed = seed_from_strategy(&strategy_id);
            let (lower, upper) =
                bootstrap_mean_ci(&pnls, request.bootstrap_iterations, request.alpha, seed);
            let p_value = centered_bootstrap_positive_mean_p(
                &pnls,
                request.bootstrap_iterations,
                seed ^ 0x9e3779b97f4a7c15,
            );
            eligible_indices.push(index);
            raw_p_values.push(p_value);
            candidates.push(CandidateInference {
                strategy_id,
                trades: pnls.len(),
                average_pnl,
                bootstrap_ci_lower: Some(lower),
                bootstrap_ci_upper: Some(upper),
                raw_one_sided_p: Some(p_value),
                holm_adjusted_p: None,
                bh_fdr_adjusted_p: None,
                passes_holm: false,
                passes_bh_fdr: false,
            });
        } else {
            candidates.push(CandidateInference {
                strategy_id,
                trades: pnls.len(),
                average_pnl,
                bootstrap_ci_lower: None,
                bootstrap_ci_upper: None,
                raw_one_sided_p: None,
                holm_adjusted_p: None,
                bh_fdr_adjusted_p: None,
                passes_holm: false,
                passes_bh_fdr: false,
            });
        }
    }

    let holm = holm_adjusted(&raw_p_values);
    let bh = benjamini_hochberg_adjusted(&raw_p_values);
    for (position, candidate_index) in eligible_indices.iter().copied().enumerate() {
        candidates[candidate_index].holm_adjusted_p = Some(holm[position]);
        candidates[candidate_index].bh_fdr_adjusted_p = Some(bh[position]);
        candidates[candidate_index].passes_holm = holm[position] <= request.alpha;
        candidates[candidate_index].passes_bh_fdr = bh[position] <= request.alpha;
    }

    let holm_discoveries = candidates
        .iter()
        .filter(|candidate| candidate.passes_holm)
        .count();
    let bh_fdr_discoveries = candidates
        .iter()
        .filter(|candidate| candidate.passes_bh_fdr)
        .count();

    Ok(InferenceReport {
        engine: "research_inference_v1",
        alpha: request.alpha,
        bootstrap_iterations: request.bootstrap_iterations,
        tested_candidates: request.candidates.len(),
        eligible_candidates: eligible_indices.len(),
        holm_discoveries,
        bh_fdr_discoveries,
        candidates,
        notes: vec![
            "confidence intervals are percentile bootstraps of per-trade mean P/L with deterministic resampling".into(),
            "one-sided p-values use a mean-centered bootstrap null and test whether observed average P/L is greater than zero".into(),
            "Holm adjustment controls family-wise error rate across the submitted candidate family".into(),
            "Benjamini-Hochberg adjustment controls false discovery rate across the submitted candidate family under its standard assumptions".into(),
            "trade-level bootstrap treats observed trades as the resampling unit; serial dependence and regime clustering can make intervals too narrow".into(),
            "these diagnostics quantify sampling uncertainty and multiple testing; they do not replace walk-forward validation or the sealed final holdout".into(),
        ],
    })
}

fn seed_from_strategy(strategy_id: &str) -> u64 {
    let digest = Sha256::digest(strategy_id.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes).max(1)
}

fn bootstrap_mean_ci(values: &[f64], iterations: usize, alpha: f64, seed: u64) -> (f64, f64) {
    let mut rng = XorShift64::new(seed);
    let mut means = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let mut total = 0.0;
        for _ in 0..values.len() {
            total += values[rng.index(values.len())];
        }
        means.push(total / values.len() as f64);
    }
    means.sort_by(f64::total_cmp);
    (
        quantile_sorted(&means, alpha / 2.0),
        quantile_sorted(&means, 1.0 - alpha / 2.0),
    )
}

fn centered_bootstrap_positive_mean_p(values: &[f64], iterations: usize, seed: u64) -> f64 {
    let observed = values.iter().sum::<f64>() / values.len() as f64;
    let centered: Vec<f64> = values.iter().map(|value| value - observed).collect();
    let mut rng = XorShift64::new(seed);
    let mut at_least_observed = 0usize;
    for _ in 0..iterations {
        let mut total = 0.0;
        for _ in 0..centered.len() {
            total += centered[rng.index(centered.len())];
        }
        let null_mean = total / centered.len() as f64;
        if null_mean >= observed {
            at_least_observed += 1;
        }
    }
    (at_least_observed as f64 + 1.0) / (iterations as f64 + 1.0)
}

fn holm_adjusted(p_values: &[f64]) -> Vec<f64> {
    let mut indexed: Vec<(usize, f64)> = p_values.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| a.1.total_cmp(&b.1));
    let m = indexed.len();
    let mut adjusted_sorted = vec![0.0; m];
    let mut running: f64 = 0.0;
    for (rank, (_, p)) in indexed.iter().enumerate() {
        let value = ((m - rank) as f64 * *p).min(1.0);
        running = running.max(value);
        adjusted_sorted[rank] = running;
    }
    let mut adjusted = vec![0.0; m];
    for (rank, (original, _)) in indexed.iter().enumerate() {
        adjusted[*original] = adjusted_sorted[rank];
    }
    adjusted
}

fn benjamini_hochberg_adjusted(p_values: &[f64]) -> Vec<f64> {
    let mut indexed: Vec<(usize, f64)> = p_values.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| a.1.total_cmp(&b.1));
    let m = indexed.len();
    let mut adjusted_sorted = vec![0.0; m];
    let mut running: f64 = 1.0;
    for rank in (0..m).rev() {
        let p = indexed[rank].1;
        let value = (p * m as f64 / (rank + 1) as f64).min(1.0);
        running = running.min(value);
        adjusted_sorted[rank] = running;
    }
    let mut adjusted = vec![0.0; m];
    for (rank, (original, _)) in indexed.iter().enumerate() {
        adjusted[*original] = adjusted_sorted[rank];
    }
    adjusted
}

fn quantile_sorted(values: &[f64], probability: f64) -> f64 {
    if values.len() == 1 {
        return values[0];
    }
    let position = probability.clamp(0.0, 1.0) * (values.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        values[lower]
    } else {
        let weight = position - lower as f64;
        values[lower] * (1.0 - weight) + values[upper] * weight
    }
}

#[derive(Debug, Clone)]
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }

    fn index(&mut self, length: usize) -> usize {
        (self.next_u64() as usize) % length
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_testing_adjustments_are_monotone_and_mapped_back() {
        let values = vec![0.01, 0.04, 0.02];
        let holm = holm_adjusted(&values);
        let bh = benjamini_hochberg_adjusted(&values);
        assert!(holm[0] <= holm[2] && holm[2] <= holm[1]);
        assert!(bh[0] <= bh[2] && bh[2] <= bh[1]);
        assert!((holm[0] - 0.03).abs() < 1e-12);
        assert!((bh[0] - 0.03).abs() < 1e-12);
    }

    #[test]
    fn bootstrap_is_deterministic() {
        let values = [10.0, -5.0, 8.0, 12.0, 3.0];
        let a = bootstrap_mean_ci(&values, 500, 0.05, 42);
        let b = bootstrap_mean_ci(&values, 500, 0.05, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn positive_constant_sample_has_small_null_p_value() {
        let values = vec![10.0; 20];
        let p = centered_bootstrap_positive_mean_p(&values, 500, 7);
        assert!(p < 0.01);
    }
}
