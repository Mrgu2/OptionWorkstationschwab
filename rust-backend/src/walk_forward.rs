use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    backtest::{BacktestRequest, BacktestStats, BacktestTrade, run_backtest, summarize},
    manifest::freeze_manifest,
    replay::ReplayStore,
};

#[derive(Debug, Clone, Deserialize)]
pub struct WalkForwardRequest {
    pub candidates: Vec<BacktestRequest>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    #[serde(default = "default_train_sessions")]
    pub train_sessions: usize,
    #[serde(default = "default_test_sessions")]
    pub test_sessions: usize,
    pub step_sessions: Option<usize>,
    #[serde(default = "default_min_train_trades")]
    pub min_train_trades: usize,
    #[serde(default = "default_anchored")]
    pub anchored: bool,
    #[serde(default = "default_selection_metric")]
    pub selection_metric: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateScore {
    pub strategy_id: String,
    pub eligible: bool,
    pub trades: usize,
    pub score: Option<f64>,
    pub average_pnl: f64,
    pub profit_factor: Option<f64>,
    pub max_drawdown: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalkForwardFold {
    pub fold: usize,
    pub train_start: String,
    pub train_end: String,
    pub test_start: String,
    pub test_end: String,
    pub selected_strategy_id: String,
    pub selected_score: f64,
    pub candidate_scores: Vec<CandidateScore>,
    pub train_stats: BacktestStats,
    pub test_stats: BacktestStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalkForwardReport {
    pub engine: &'static str,
    pub symbol: String,
    pub selection_metric: String,
    pub anchored: bool,
    pub train_sessions: usize,
    pub test_sessions: usize,
    pub step_sessions: usize,
    pub folds: Vec<WalkForwardFold>,
    pub out_of_sample_stats: BacktestStats,
    pub selected_strategy_frequency: BTreeMap<String, usize>,
    pub profitable_oos_folds_pct: f64,
    pub oos_to_selected_train_average_pnl_ratio: Option<f64>,
    pub notes: Vec<String>,
}

fn default_train_sessions() -> usize {
    60
}
fn default_test_sessions() -> usize {
    20
}
fn default_min_train_trades() -> usize {
    10
}
fn default_anchored() -> bool {
    true
}
fn default_selection_metric() -> String {
    "average_pnl".into()
}

pub fn run_walk_forward(
    store: &ReplayStore,
    request: &WalkForwardRequest,
) -> anyhow::Result<WalkForwardReport> {
    anyhow::ensure!(
        !request.candidates.is_empty() && request.candidates.len() <= 50,
        "walk-forward requires 1-50 candidate strategies"
    );
    anyhow::ensure!(
        request.train_sessions >= 10,
        "train_sessions must be at least 10"
    );
    anyhow::ensure!(
        request.test_sessions >= 1,
        "test_sessions must be at least 1"
    );
    let step_sessions = request.step_sessions.unwrap_or(request.test_sessions);
    anyhow::ensure!(
        step_sessions >= request.test_sessions,
        "step_sessions must be at least test_sessions to keep OOS folds non-overlapping"
    );
    anyhow::ensure!(
        request.min_train_trades >= 1,
        "min_train_trades must be at least 1"
    );
    anyhow::ensure!(
        matches!(
            request.selection_metric.as_str(),
            "average_pnl" | "profit_factor" | "total_pnl"
        ),
        "selection_metric must be average_pnl, profit_factor, or total_pnl"
    );

    let symbol = store.validate_symbol(&request.candidates[0].symbol)?;
    for candidate in &request.candidates {
        anyhow::ensure!(
            store.validate_symbol(&candidate.symbol)? == symbol,
            "all candidate strategies must use the same symbol"
        );
    }

    let mut dates = store.dates(&symbol);
    dates.sort();
    dates.retain(|date| {
        request
            .start_date
            .as_ref()
            .is_none_or(|start| date >= start)
    });
    dates.retain(|date| request.end_date.as_ref().is_none_or(|end| date <= end));
    anyhow::ensure!(
        dates.len() >= request.train_sessions + request.test_sessions,
        "insufficient sessions for one complete walk-forward fold"
    );

    let mut folds = Vec::new();
    let mut all_oos_trades: Vec<BacktestTrade> = Vec::new();
    let mut selected_strategy_frequency = BTreeMap::new();
    let mut offset = 0usize;

    loop {
        let train_start_index = if request.anchored { 0 } else { offset };
        let train_end_index = if request.anchored {
            request.train_sessions - 1 + offset
        } else {
            train_start_index + request.train_sessions - 1
        };
        let test_start_index = train_end_index + 1;
        let test_end_index = test_start_index + request.test_sessions - 1;
        if test_end_index >= dates.len() {
            break;
        }

        let train_start = dates[train_start_index].clone();
        let train_end = dates[train_end_index].clone();
        let test_start = dates[test_start_index].clone();
        let test_end = dates[test_end_index].clone();

        let mut scored = Vec::new();
        let mut selected: Option<(usize, f64, BacktestStats, String)> = None;

        for (candidate_index, candidate) in request.candidates.iter().enumerate() {
            let train_request = with_window(candidate, &train_start, &train_end);
            let manifest = freeze_manifest(&train_request)?;
            let report = run_backtest(store, &train_request)?;
            let eligible = report.stats.trades >= request.min_train_trades;
            let score = eligible
                .then(|| selection_score(&report.stats, &request.selection_metric))
                .filter(|value| value.is_finite());

            scored.push(CandidateScore {
                strategy_id: manifest.strategy_id.clone(),
                eligible,
                trades: report.stats.trades,
                score,
                average_pnl: report.stats.average_pnl,
                profit_factor: report.stats.profit_factor,
                max_drawdown: report.stats.max_drawdown,
            });

            if let Some(score) = score {
                let replace = selected
                    .as_ref()
                    .is_none_or(|(_, best_score, _, _)| score > *best_score);
                if replace {
                    selected = Some((
                        candidate_index,
                        score,
                        report.stats.clone(),
                        manifest.strategy_id,
                    ));
                }
            }
        }

        let Some((selected_index, selected_score, train_stats, selected_strategy_id)) = selected
        else {
            anyhow::bail!(
                "fold {} has no eligible candidate with at least {} training trades",
                folds.len() + 1,
                request.min_train_trades
            );
        };

        let test_request = with_window(&request.candidates[selected_index], &test_start, &test_end);
        let test_report = run_backtest(store, &test_request)?;
        all_oos_trades.extend(test_report.trades.iter().cloned());
        *selected_strategy_frequency
            .entry(selected_strategy_id.clone())
            .or_insert(0) += 1;

        folds.push(WalkForwardFold {
            fold: folds.len() + 1,
            train_start,
            train_end,
            test_start,
            test_end,
            selected_strategy_id,
            selected_score,
            candidate_scores: scored,
            train_stats,
            test_stats: test_report.stats,
        });

        offset += step_sessions;
        if request.anchored {
            if request.train_sessions + offset + request.test_sessions > dates.len() {
                break;
            }
        } else if offset + request.train_sessions + request.test_sessions > dates.len() {
            break;
        }
    }

    anyhow::ensure!(
        !folds.is_empty(),
        "no complete walk-forward folds were produced"
    );

    let out_of_sample_stats = summarize(&all_oos_trades);
    let profitable_folds = folds
        .iter()
        .filter(|fold| fold.test_stats.total_pnl > 0.0)
        .count();
    let selected_train_average = folds
        .iter()
        .map(|fold| fold.train_stats.average_pnl)
        .sum::<f64>()
        / folds.len() as f64;
    let ratio = (selected_train_average.abs() > 1e-9)
        .then_some(out_of_sample_stats.average_pnl / selected_train_average);

    let fold_count = folds.len();

    Ok(WalkForwardReport {
        engine: "walk_forward_v1",
        symbol,
        selection_metric: request.selection_metric.clone(),
        anchored: request.anchored,
        train_sessions: request.train_sessions,
        test_sessions: request.test_sessions,
        step_sessions,
        folds,
        out_of_sample_stats,
        selected_strategy_frequency,
        profitable_oos_folds_pct: profitable_folds as f64 / fold_count as f64 * 100.0,
        oos_to_selected_train_average_pnl_ratio: ratio,
        notes: vec![
            "candidate selection uses training data only; the selected manifest is frozen before the test window is evaluated".into(),
            "test windows are non-overlapping, so aggregate OOS statistics do not double-count the same session window".into(),
            "all exits are constrained to the active train or test window, preventing a trade from using prices beyond its fold".into(),
            "a strong in-sample score with weak OOS results is evidence of instability or overfitting, not proof of persistent edge".into(),
        ],
    })
}

fn with_window(candidate: &BacktestRequest, start: &str, end: &str) -> BacktestRequest {
    let mut request = candidate.clone();
    request.start_date = Some(start.into());
    request.end_date = Some(end.into());
    request
}

fn selection_score(stats: &BacktestStats, metric: &str) -> f64 {
    match metric {
        "profit_factor" => stats.profit_factor.unwrap_or_else(|| {
            if stats.wins > 0 && stats.losses == 0 {
                f64::MAX
            } else {
                f64::MIN
            }
        }),
        "total_pnl" => stats.total_pnl,
        _ => stats.average_pnl,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_metric_uses_requested_statistic() {
        let stats = BacktestStats {
            trades: 10,
            wins: 6,
            losses: 4,
            win_rate: 0.6,
            total_pnl: 500.0,
            average_pnl: 50.0,
            median_pnl: 20.0,
            profit_factor: Some(1.8),
            max_drawdown: 120.0,
            total_costs: 13.0,
        };
        assert_eq!(selection_score(&stats, "average_pnl"), 50.0);
        assert_eq!(selection_score(&stats, "total_pnl"), 500.0);
        assert_eq!(selection_score(&stats, "profit_factor"), 1.8);
    }
}
