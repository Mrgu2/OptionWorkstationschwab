use serde::{Deserialize, Serialize};

use crate::{
    backtest::{BacktestRequest, BacktestStats, run_backtest},
    manifest::{freeze_manifest, unique_strategy_ids},
    replay::ReplayStore,
};

#[derive(Debug, Clone, Deserialize)]
pub struct StabilityRequest {
    pub candidates: Vec<BacktestRequest>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    #[serde(default = "default_min_trades")]
    pub min_trades: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StabilityCandidate {
    pub strategy_id: String,
    pub eligible: bool,
    pub trades: usize,
    pub average_pnl: f64,
    pub total_pnl: f64,
    pub win_rate: f64,
    pub profit_factor: Option<f64>,
    pub max_drawdown: f64,
    pub average_pnl_vs_base: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StabilityReport {
    pub engine: &'static str,
    pub symbol: String,
    pub base_strategy_id: String,
    pub base_average_pnl: f64,
    pub candidates: Vec<StabilityCandidate>,
    pub eligible_candidates: usize,
    pub profitable_candidate_pct: f64,
    pub base_sign_survival_pct: f64,
    pub mean_average_pnl: f64,
    pub median_average_pnl: f64,
    pub average_pnl_stddev: f64,
    pub dispersion_to_mean: Option<f64>,
    pub worst_average_pnl: f64,
    pub best_average_pnl: f64,
    pub median_max_drawdown: f64,
    pub notes: Vec<String>,
}

fn default_min_trades() -> usize {
    10
}

pub fn analyze_stability(
    store: &ReplayStore,
    request: &StabilityRequest,
) -> anyhow::Result<StabilityReport> {
    anyhow::ensure!(
        !request.candidates.is_empty() && request.candidates.len() <= 100,
        "stability analysis requires 1-100 candidate strategies"
    );
    anyhow::ensure!(request.min_trades >= 1, "min_trades must be at least 1");
    unique_strategy_ids(&request.candidates)?;

    let symbol = store.validate_symbol(&request.candidates[0].symbol)?;
    let mut rows = Vec::with_capacity(request.candidates.len());

    for candidate in &request.candidates {
        anyhow::ensure!(
            store.validate_symbol(&candidate.symbol)? == symbol,
            "all stability candidates must use the same symbol"
        );
        let mut windowed = candidate.clone();
        windowed.start_date = request.start_date.clone();
        windowed.end_date = request.end_date.clone();
        let strategy_id = freeze_manifest(&windowed)?.strategy_id;
        let report = run_backtest(store, &windowed)?;
        rows.push((strategy_id, report.stats));
    }

    let (base_strategy_id, base_stats) = rows
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing base candidate"))?;
    anyhow::ensure!(
        base_stats.trades >= request.min_trades,
        "base stability candidate has only {} trades; at least {} are required",
        base_stats.trades,
        request.min_trades
    );
    let base_average_pnl = base_stats.average_pnl;

    let eligible_stats: Vec<BacktestStats> = rows
        .iter()
        .map(|(_, stats)| stats)
        .filter(|stats| stats.trades >= request.min_trades)
        .cloned()
        .collect();
    anyhow::ensure!(
        !eligible_stats.is_empty(),
        "no stability candidate has at least {} trades",
        request.min_trades
    );

    let values: Vec<f64> = eligible_stats
        .iter()
        .map(|stats| stats.average_pnl)
        .collect();
    let mean_average_pnl = values.iter().sum::<f64>() / values.len() as f64;
    let average_pnl_stddev = {
        let variance = values
            .iter()
            .map(|value| (value - mean_average_pnl).powi(2))
            .sum::<f64>()
            / values.len() as f64;
        variance.sqrt()
    };
    let median_average_pnl = median(values.clone());
    let profitable = eligible_stats
        .iter()
        .filter(|stats| stats.average_pnl > 0.0)
        .count();
    let same_sign = eligible_stats
        .iter()
        .filter(|stats| {
            if base_average_pnl > 0.0 {
                stats.average_pnl > 0.0
            } else if base_average_pnl < 0.0 {
                stats.average_pnl < 0.0
            } else {
                stats.average_pnl.abs() < 1e-9
            }
        })
        .count();
    let max_drawdowns = eligible_stats
        .iter()
        .map(|stats| stats.max_drawdown)
        .collect::<Vec<_>>();

    let candidates = rows
        .into_iter()
        .map(|(strategy_id, stats)| StabilityCandidate {
            strategy_id,
            eligible: stats.trades >= request.min_trades,
            trades: stats.trades,
            average_pnl: stats.average_pnl,
            total_pnl: stats.total_pnl,
            win_rate: stats.win_rate,
            profit_factor: stats.profit_factor,
            max_drawdown: stats.max_drawdown,
            average_pnl_vs_base: (base_average_pnl.abs() > 1e-9)
                .then_some(stats.average_pnl / base_average_pnl),
        })
        .collect();

    Ok(StabilityReport {
        engine: "parameter_stability_v1",
        symbol,
        base_strategy_id,
        base_average_pnl,
        candidates,
        eligible_candidates: eligible_stats.len(),
        profitable_candidate_pct: profitable as f64 / eligible_stats.len() as f64 * 100.0,
        base_sign_survival_pct: same_sign as f64 / eligible_stats.len() as f64 * 100.0,
        mean_average_pnl,
        median_average_pnl,
        average_pnl_stddev,
        dispersion_to_mean: (mean_average_pnl.abs() > 1e-9)
            .then_some(average_pnl_stddev / mean_average_pnl.abs()),
        worst_average_pnl: values.iter().copied().fold(f64::INFINITY, f64::min),
        best_average_pnl: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        median_max_drawdown: median(max_drawdowns),
        notes: vec![
            "the first candidate is treated as the base strategy; the remaining candidates should represent nearby parameter perturbations rather than unrelated strategies".into(),
            "a narrow optimum with weak neighboring results is a warning sign for parameter overfitting".into(),
            "stability analysis remains in-sample evidence and should be read together with walk-forward and final holdout results".into(),
        ],
    })
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    if values.len().is_multiple_of(2) {
        (values[values.len() / 2 - 1] + values[values.len() / 2]) / 2.0
    } else {
        values[values.len() / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::median;

    #[test]
    fn median_handles_even_and_odd_samples() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(vec![4.0, 1.0, 2.0, 3.0]), 2.5);
    }
}
