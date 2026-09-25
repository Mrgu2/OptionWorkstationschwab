use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    backtest::{BacktestReport, BacktestRequest, run_backtest, validate_request},
    manifest::{StrategyDefinition, freeze_manifest},
    replay::{ReplayDataFingerprint, ReplayStore},
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_fingerprint: Option<ReplayDataFingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_free_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_contract: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_definition: Option<StrategyDefinition>,
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
struct CommitmentMaterialV1<'a> {
    protocol_version: &'a str,
    strategy_id: &'a str,
    symbol: &'a str,
    development_start: &'a str,
    development_end: &'a str,
    holdout_start: &'a str,
    holdout_end: &'a str,
    holdout_sessions: usize,
}

#[derive(Serialize)]
struct CommitmentMaterialV2<'a> {
    protocol_version: &'a str,
    strategy_id: &'a str,
    symbol: &'a str,
    development_start: &'a str,
    development_end: &'a str,
    holdout_start: &'a str,
    holdout_end: &'a str,
    holdout_sessions: usize,
    data_fingerprint: &'a ReplayDataFingerprint,
    risk_free_rate: f64,
    engine_contract: &'a str,
}

const HOLDOUT_PROTOCOL_V2: &str = "untouched-holdout-v2";
const ENGINE_CONTRACT: &str = "point_in_time_v2|Rust-BSM+SVI-v2";

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

    let effective_start = reconcile_boundary(
        "start_date",
        request.start_date.as_deref(),
        request.strategy.start_date.as_deref(),
    )?;
    let effective_end = reconcile_boundary(
        "end_date",
        request.end_date.as_deref(),
        request.strategy.end_date.as_deref(),
    )?;
    let start = effective_start
        .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d"))
        .transpose()
        .map_err(|_| anyhow::anyhow!("start_date must use YYYY-MM-DD"))?;
    let end = effective_end
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
    dates.retain(|date| effective_start.is_none_or(|start| date.as_str() >= start));
    dates.retain(|date| effective_end.is_none_or(|end| date.as_str() <= end));

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

    let data_fingerprint = store.data_fingerprint(&symbol, &holdout_start, &holdout_end)?;
    let risk_free_rate = store.risk_free_rate();
    let material = CommitmentMaterialV2 {
        protocol_version: HOLDOUT_PROTOCOL_V2,
        strategy_id: &manifest.strategy_id,
        symbol: &symbol,
        development_start: &development_start,
        development_end: &development_end,
        holdout_start: &holdout_start,
        holdout_end: &holdout_end,
        holdout_sessions: request.holdout_sessions,
        data_fingerprint: &data_fingerprint,
        risk_free_rate,
        engine_contract: ENGINE_CONTRACT,
    };
    let commitment = hex::encode(Sha256::digest(serde_json::to_vec(&material)?));

    Ok(HoldoutPlan {
        protocol_version: HOLDOUT_PROTOCOL_V2.into(),
        commitment,
        strategy_id: manifest.strategy_id,
        symbol,
        development_start,
        development_end,
        holdout_start,
        holdout_end,
        holdout_sessions: request.holdout_sessions,
        data_fingerprint: Some(data_fingerprint),
        risk_free_rate: Some(risk_free_rate),
        engine_contract: Some(ENGINE_CONTRACT.into()),
        strategy_definition: Some(manifest.definition),
    })
}

pub fn open_sealed_holdout(
    store: &ReplayStore,
    request: &HoldoutOpenRequest,
    sealed_plan: &HoldoutPlan,
) -> anyhow::Result<HoldoutOpenReport> {
    validate_request(&request.plan.strategy)?;
    anyhow::ensure!(
        request.commitment == sealed_plan.commitment,
        "holdout commitment does not match the sealed audit record"
    );

    let manifest = freeze_manifest(&request.plan.strategy)?;
    anyhow::ensure!(
        manifest.strategy_id == sealed_plan.strategy_id,
        "strategy definition changed after holdout sealing"
    );
    let symbol = store.validate_symbol(&request.plan.strategy.symbol)?;
    anyhow::ensure!(
        symbol == sealed_plan.symbol,
        "strategy symbol does not match the sealed holdout"
    );

    verify_sealed_plan(store, sealed_plan)?;

    let mut holdout_request = request.plan.strategy.clone();
    holdout_request.start_date = Some(sealed_plan.holdout_start.clone());
    holdout_request.end_date = Some(sealed_plan.holdout_end.clone());
    let holdout = run_backtest(store, &holdout_request)?;

    if sealed_plan.protocol_version == HOLDOUT_PROTOCOL_V2 {
        let sealed_fingerprint = sealed_plan
            .data_fingerprint
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("v2 holdout is missing its data fingerprint"))?;
        let after =
            store.data_fingerprint(&sealed_plan.symbol, &sealed_plan.holdout_start, &sealed_plan.holdout_end)?;
        anyhow::ensure!(
            &after == sealed_fingerprint,
            "replay dataset changed while the holdout was being evaluated"
        );
    }

    let mut notes = vec![
        "the audit ledger is the source of truth for the resolved holdout dates, so later catalog growth does not move the sealed sample".into(),
        "opening the final holdout should be treated as a one-time research event; changing the strategy afterwards creates a new research hypothesis".into(),
    ];
    if sealed_plan.protocol_version == HOLDOUT_PROTOCOL_V2 {
        notes.push(
            "the v2 commitment binds strategy, resolved sample boundaries, replay file bytes, risk-free-rate configuration, and the research engine contract".into(),
        );
    } else {
        notes.push(
            "legacy v1 seals bind strategy and sample boundaries but cannot verify replay-file identity or engine configuration".into(),
        );
    }

    Ok(HoldoutOpenReport {
        engine: if sealed_plan.protocol_version == HOLDOUT_PROTOCOL_V2 {
            "untouched_holdout_v2"
        } else {
            "untouched_holdout_v1"
        },
        plan: sealed_plan.clone(),
        holdout,
        notes,
    })
}

fn verify_sealed_plan(store: &ReplayStore, plan: &HoldoutPlan) -> anyhow::Result<()> {
    match plan.protocol_version.as_str() {
        "untouched-holdout-v1" => {
            let material = CommitmentMaterialV1 {
                protocol_version: "untouched-holdout-v1",
                strategy_id: &plan.strategy_id,
                symbol: &plan.symbol,
                development_start: &plan.development_start,
                development_end: &plan.development_end,
                holdout_start: &plan.holdout_start,
                holdout_end: &plan.holdout_end,
                holdout_sessions: plan.holdout_sessions,
            };
            let expected = hex::encode(Sha256::digest(serde_json::to_vec(&material)?));
            anyhow::ensure!(
                expected == plan.commitment,
                "legacy holdout audit record has an invalid commitment"
            );
        }
        HOLDOUT_PROTOCOL_V2 => {
            let sealed_fingerprint = plan
                .data_fingerprint
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("v2 holdout is missing its data fingerprint"))?;
            let sealed_rate = plan
                .risk_free_rate
                .ok_or_else(|| anyhow::anyhow!("v2 holdout is missing its risk-free rate"))?;
            let sealed_engine = plan
                .engine_contract
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("v2 holdout is missing its engine contract"))?;

            anyhow::ensure!(
                sealed_rate.to_bits() == store.risk_free_rate().to_bits(),
                "risk-free-rate configuration changed after holdout sealing"
            );
            anyhow::ensure!(
                sealed_engine == ENGINE_CONTRACT,
                "research engine contract changed after holdout sealing"
            );

            store.clear_caches();
            let current_fingerprint =
                store.data_fingerprint(&plan.symbol, &plan.holdout_start, &plan.holdout_end)?;
            anyhow::ensure!(
                &current_fingerprint == sealed_fingerprint,
                "replay dataset changed after holdout sealing"
            );

            let material = CommitmentMaterialV2 {
                protocol_version: HOLDOUT_PROTOCOL_V2,
                strategy_id: &plan.strategy_id,
                symbol: &plan.symbol,
                development_start: &plan.development_start,
                development_end: &plan.development_end,
                holdout_start: &plan.holdout_start,
                holdout_end: &plan.holdout_end,
                holdout_sessions: plan.holdout_sessions,
                data_fingerprint: sealed_fingerprint,
                risk_free_rate: sealed_rate,
                engine_contract: sealed_engine,
            };
            let expected = hex::encode(Sha256::digest(serde_json::to_vec(&material)?));
            anyhow::ensure!(
                expected == plan.commitment,
                "v2 holdout audit record has an invalid commitment"
            );
        }
        other => anyhow::bail!("unsupported holdout protocol version: {other}"),
    }
    Ok(())
}

fn reconcile_boundary<'a>(
    field: &str,
    outer: Option<&'a str>,
    strategy: Option<&'a str>,
) -> anyhow::Result<Option<&'a str>> {
    if let (Some(outer), Some(strategy)) = (outer, strategy) {
        anyhow::ensure!(
            outer == strategy,
            "{field} conflicts between holdout plan and strategy request"
        );
    }
    Ok(outer.or(strategy))
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
    fn holdout_window_rejects_conflicting_outer_and_strategy_dates() {
        assert!(
            reconcile_boundary(
                "start_date",
                Some("2026-01-01"),
                Some("2026-01-02")
            )
            .is_err()
        );
        assert_eq!(
            reconcile_boundary("start_date", None, Some("2026-01-02")).unwrap(),
            Some("2026-01-02")
        );
    }

    #[test]
    fn v2_commitment_changes_when_dataset_changes() {
        let a = ReplayDataFingerprint {
            algorithm: "sha256-content-v1".into(),
            digest: "aaa".into(),
            files: 10,
            bytes: 100,
        };
        let b = ReplayDataFingerprint {
            digest: "bbb".into(),
            ..a.clone()
        };
        let material_a = CommitmentMaterialV2 {
            protocol_version: HOLDOUT_PROTOCOL_V2,
            strategy_id: "abc",
            symbol: "SPY",
            development_start: "2026-01-01",
            development_end: "2026-06-30",
            holdout_start: "2026-07-01",
            holdout_end: "2026-07-31",
            holdout_sessions: 20,
            data_fingerprint: &a,
            risk_free_rate: 0.04,
            engine_contract: ENGINE_CONTRACT,
        };
        let material_b = CommitmentMaterialV2 {
            data_fingerprint: &b,
            ..material_a
        };
        let hash_a = hex::encode(Sha256::digest(serde_json::to_vec(&material_a).unwrap()));
        let hash_b = hex::encode(Sha256::digest(serde_json::to_vec(&material_b).unwrap()));
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn commitment_changes_when_boundary_changes() {
        let a = CommitmentMaterialV1 {
            protocol_version: "untouched-holdout-v1",
            strategy_id: "abc",
            symbol: "SPY",
            development_start: "2026-01-01",
            development_end: "2026-06-30",
            holdout_start: "2026-07-01",
            holdout_end: "2026-07-31",
            holdout_sessions: 20,
        };
        let b = CommitmentMaterialV1 {
            holdout_sessions: 21,
            ..a
        };
        let hash_a = hex::encode(Sha256::digest(serde_json::to_vec(&a).unwrap()));
        let hash_b = hex::encode(Sha256::digest(serde_json::to_vec(&b).unwrap()));
        assert_ne!(hash_a, hash_b);
    }
}
