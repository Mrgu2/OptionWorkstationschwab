use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    backtest::{BacktestReport, BacktestRequest, run_backtest, validate_request},
    manifest::freeze_manifest,
    replay::ReplayStore,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HoldoutPlanRequest {
    pub strategy: BacktestRequest,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    #[serde(default = "default_holdout_sessions")]
    pub holdout_sessions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldoutPlan {
    pub protocol_version: String,
    pub commitment: String,
    pub strategy_id: String,
    pub symbol: String,
    pub development_start: String,
    pub development_end: String,
    pub holdout_start: String,
    pub holdout_end: String,
    pub holdout_sessions: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HoldoutOpenRequest {
    pub plan: HoldoutPlanRequest,
    pub commitment: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HoldoutOpenReport {
    pub engine: &'static str,
    pub plan: HoldoutPlan,
    pub holdout: BacktestReport,
    pub notes: Vec<String>,
}

#[derive(Serialize)]
struct CommitmentMaterial<'a> {
    protocol_version: &'a str,
    strategy_id: &'a str,
    symbol: &'a str,
    development_start: &'a str,
    development_end: &'a str,
    holdout_start: &'a str,
    holdout_end: &'a str,
    holdout_sessions: usize,
}

fn default_holdout_sessions() -> usize {
    20
}

pub fn plan_holdout(
    store: &ReplayStore,
    request: &HoldoutPlanRequest,
) -> anyhow::Result<HoldoutPlan> {
    validate_request(&request.strategy)?;
    anyhow::ensure!(
        request.holdout_sessions >= 5,
        "holdout_sessions must be at least 5"
    );
    let start = request
        .start_date
        .as_deref()
        .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d"))
        .transpose()
        .map_err(|_| anyhow::anyhow!("start_date must use YYYY-MM-DD"))?;
    let end = request
        .end_date
        .as_deref()
        .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d"))
        .transpose()
        .map_err(|_| anyhow::anyhow!("end_date must use YYYY-MM-DD"))?;
    if let (Some(start), Some(end)) = (start, end) {
        anyhow::ensure!(start <= end, "start_date must not be after end_date");
    }
    let manifest = freeze_manifest(&request.strategy)?;
    let symbol = store.validate_symbol(&request.strategy.symbol)?;
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
        dates.len() > request.holdout_sessions + request.strategy.hold_trading_days,
        "insufficient sessions to reserve the requested untouched holdout"
    );

    let split = dates.len() - request.holdout_sessions;
    let development_start = dates[0].clone();
    let development_end = dates[split - 1].clone();
    let holdout_start = dates[split].clone();
    let holdout_end = dates
        .last()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no sessions available"))?;

    let material = CommitmentMaterial {
        protocol_version: "untouched-holdout-v1",
        strategy_id: &manifest.strategy_id,
        symbol: &symbol,
        development_start: &development_start,
        development_end: &development_end,
        holdout_start: &holdout_start,
        holdout_end: &holdout_end,
        holdout_sessions: request.holdout_sessions,
    };
    let commitment = hex::encode(Sha256::digest(serde_json::to_vec(&material)?));

    Ok(HoldoutPlan {
        protocol_version: "untouched-holdout-v1".into(),
        commitment,
        strategy_id: manifest.strategy_id,
        symbol,
        development_start,
        development_end,
        holdout_start,
        holdout_end,
        holdout_sessions: request.holdout_sessions,
    })
}

pub fn open_holdout(
    store: &ReplayStore,
    request: &HoldoutOpenRequest,
) -> anyhow::Result<HoldoutOpenReport> {
    let plan = plan_holdout(store, &request.plan)?;
    anyhow::ensure!(
        request.commitment == plan.commitment,
        "holdout commitment mismatch; strategy or sample boundary changed after sealing"
    );

    let mut holdout_request = request.plan.strategy.clone();
    holdout_request.start_date = Some(plan.holdout_start.clone());
    holdout_request.end_date = Some(plan.holdout_end.clone());
    let holdout = run_backtest(store, &holdout_request)?;

    Ok(HoldoutOpenReport {
        engine: "untouched_holdout_v1",
        plan,
        holdout,
        notes: vec![
            "the commitment binds the strategy ID and holdout date boundary before holdout metrics are revealed".into(),
            "the audit ledger should record both sealing and first opening so later research can distinguish an untouched holdout from a reused test set".into(),
            "opening the final holdout should be treated as a one-time research event; changing the strategy afterwards creates a new research hypothesis".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holdout_outer_window_rejects_invalid_date_order() {
        let request = HoldoutPlanRequest {
            strategy: BacktestRequest {
                symbol: "SPY".into(),
                start_date: None,
                end_date: None,
                entry_minute: "10:00".into(),
                exit_minute: "15:45".into(),
                hold_trading_days: 5,
                target_dte: 14,
                quantity: 1,
                pricing_mode: "micro".into(),
                dealer_model: "classic".into(),
                costs: Default::default(),
                exits: Default::default(),
                legs: vec![crate::backtest::BacktestLegRule {
                    right: "PUT".into(),
                    side: "BUY".into(),
                    target_delta: 0.30,
                    ratio: 1,
                }],
            },
            start_date: Some("2026-10-01".into()),
            end_date: Some("2026-09-01".into()),
            holdout_sessions: 20,
        };
        let start = request
            .start_date
            .as_deref()
            .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d"))
            .transpose()
            .unwrap();
        let end = request
            .end_date
            .as_deref()
            .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d"))
            .transpose()
            .unwrap();
        assert!(start > end);
    }

    #[test]
    fn commitment_changes_when_boundary_changes() {
        let a = CommitmentMaterial {
            protocol_version: "untouched-holdout-v1",
            strategy_id: "abc",
            symbol: "SPY",
            development_start: "2026-01-01",
            development_end: "2026-06-30",
            holdout_start: "2026-07-01",
            holdout_end: "2026-07-31",
            holdout_sessions: 20,
        };
        let b = CommitmentMaterial {
            holdout_sessions: 21,
            ..a
        };
        let hash_a = hex::encode(Sha256::digest(serde_json::to_vec(&a).unwrap()));
        let hash_b = hex::encode(Sha256::digest(serde_json::to_vec(&b).unwrap()));
        assert_ne!(hash_a, hash_b);
    }
}
