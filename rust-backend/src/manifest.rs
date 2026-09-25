use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backtest::{BacktestLegRule, BacktestRequest, CostModel, ExitRules};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDefinition {
    pub symbol: String,
    pub entry_minute: String,
    pub exit_minute: String,
    pub hold_trading_days: usize,
    pub target_dte: i64,
    pub quantity: u32,
    pub pricing_mode: String,
    pub dealer_model: String,
    pub costs: CostModel,
    pub exits: ExitRules,
    pub legs: Vec<BacktestLegRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyManifest {
    pub manifest_version: String,
    pub strategy_id: String,
    pub definition: StrategyDefinition,
}

pub fn freeze_manifest(request: &BacktestRequest) -> anyhow::Result<StrategyManifest> {
    let definition = StrategyDefinition {
        symbol: request.symbol.trim().to_uppercase(),
        entry_minute: request.entry_minute.clone(),
        exit_minute: request.exit_minute.clone(),
        hold_trading_days: request.hold_trading_days,
        target_dte: request.target_dte,
        quantity: request.quantity,
        pricing_mode: request.pricing_mode.clone(),
        dealer_model: request.dealer_model.clone(),
        costs: request.costs.clone(),
        exits: request.exits.clone(),
        legs: request.legs.clone(),
    };
    let material = serde_json::to_vec(&definition)?;
    let strategy_id = hex::encode(Sha256::digest(material))[..20].to_string();
    Ok(StrategyManifest {
        manifest_version: "strategy-manifest-v1".into(),
        strategy_id,
        definition,
    })
}

pub fn unique_strategy_ids(requests: &[BacktestRequest]) -> anyhow::Result<Vec<String>> {
    let mut seen = HashSet::with_capacity(requests.len());
    let mut ids = Vec::with_capacity(requests.len());
    for request in requests {
        let id = freeze_manifest(request)?.strategy_id;
        anyhow::ensure!(
            seen.insert(id.clone()),
            "duplicate strategy definition in candidate family: {id}"
        );
        ids.push(id);
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(start: Option<&str>, end: Option<&str>, delta: f64) -> BacktestRequest {
        BacktestRequest {
            symbol: "spy".into(),
            start_date: start.map(str::to_string),
            end_date: end.map(str::to_string),
            entry_minute: "10:00".into(),
            exit_minute: "15:45".into(),
            hold_trading_days: 5,
            target_dte: 14,
            quantity: 1,
            pricing_mode: "micro".into(),
            dealer_model: "classic".into(),
            costs: CostModel::default(),
            exits: ExitRules::default(),
            legs: vec![BacktestLegRule {
                right: "PUT".into(),
                side: "BUY".into(),
                target_delta: delta,
                ratio: 1,
            }],
        }
    }

    #[test]
    fn duplicate_strategy_family_is_rejected() {
        let a = request(None, None, 0.30);
        let mut b = request(Some("2026-01-01"), Some("2026-02-01"), 0.30);
        b.symbol = "SPY".into();
        assert!(unique_strategy_ids(&[a, b]).is_err());
    }

    #[test]
    fn evaluation_window_does_not_change_strategy_id() {
        let a = freeze_manifest(&request(Some("2026-01-01"), Some("2026-03-01"), 0.30)).unwrap();
        let b = freeze_manifest(&request(Some("2026-04-01"), Some("2026-06-01"), 0.30)).unwrap();
        assert_eq!(a.strategy_id, b.strategy_id);
    }

    #[test]
    fn strategy_parameter_change_changes_strategy_id() {
        let a = freeze_manifest(&request(None, None, 0.30)).unwrap();
        let b = freeze_manifest(&request(None, None, 0.25)).unwrap();
        assert_ne!(a.strategy_id, b.strategy_id);
    }
}
