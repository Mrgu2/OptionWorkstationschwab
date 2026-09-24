use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::{
    manifest::{StrategyManifest, freeze_manifest},
    replay::ReplayStore,
    strategy::{StrategyLegInput, analyze_strategy},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BacktestLegRule {
    pub right: String,
    pub side: String,
    pub target_delta: f64,
    #[serde(default = "default_ratio")]
    pub ratio: u32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CostModel {
    #[serde(default)]
    pub commission_per_contract: f64,
    #[serde(default)]
    pub slippage_per_contract: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExitRules {
    pub take_profit_pct_of_risk: Option<f64>,
    pub stop_loss_pct_of_risk: Option<f64>,
    pub exit_dte_lte: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BacktestRequest {
    pub symbol: String,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    #[serde(default = "default_entry_minute")]
    pub entry_minute: String,
    #[serde(default = "default_exit_minute")]
    pub exit_minute: String,
    #[serde(default = "default_hold_days")]
    pub hold_trading_days: usize,
    #[serde(default = "default_target_dte")]
    pub target_dte: i64,
    #[serde(default = "default_quantity")]
    pub quantity: u32,
    #[serde(default = "default_pricing_mode")]
    pub pricing_mode: String,
    #[serde(default = "default_dealer_model")]
    pub dealer_model: String,
    #[serde(default)]
    pub costs: CostModel,
    #[serde(default)]
    pub exits: ExitRules,
    pub legs: Vec<BacktestLegRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub entry_date: String,
    pub exit_date: String,
    pub expiration: String,
    pub entry_minute: String,
    pub exit_minute: String,
    pub entry_spot: f64,
    pub exit_spot: f64,
    pub entry_cash_flow: f64,
    pub exit_liquidation_value: f64,
    pub gross_pnl: f64,
    pub entry_costs: f64,
    pub exit_costs: f64,
    pub total_costs: f64,
    pub pnl: f64,
    pub risk_basis: f64,
    pub bounded_risk: bool,
    pub exit_reason: String,
    pub holding_sessions: usize,
    pub return_on_debit: Option<f64>,
    pub entry_atm_iv: Option<f64>,
    pub entry_net_gex: Option<f64>,
    pub entry_gamma_flip: Option<f64>,
    pub spot_vs_gamma_flip: Option<String>,
    pub rr25: Option<f64>,
    pub bf25: Option<f64>,
    pub min_quote_quality: f64,
    pub legs: Vec<StrategyLegInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestStats {
    pub trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub average_pnl: f64,
    pub median_pnl: f64,
    pub profit_factor: Option<f64>,
    pub max_drawdown: f64,
    pub total_costs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub date: String,
    pub cumulative_pnl: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestReport {
    pub engine: &'static str,
    pub symbol: String,
    pub request_fingerprint: String,
    pub manifest: StrategyManifest,
    pub assumptions: Vec<String>,
    pub skipped: Vec<String>,
    pub stats: BacktestStats,
    pub equity_curve: Vec<EquityPoint>,
    pub trades: Vec<BacktestTrade>,
}

fn default_ratio() -> u32 {
    1
}
fn default_entry_minute() -> String {
    "10:00".into()
}
fn default_exit_minute() -> String {
    "15:45".into()
}
fn default_hold_days() -> usize {
    1
}
fn default_target_dte() -> i64 {
    14
}
fn default_quantity() -> u32 {
    1
}
fn default_pricing_mode() -> String {
    "micro".into()
}
fn default_dealer_model() -> String {
    "classic".into()
}

pub fn run_backtest(
    store: &ReplayStore,
    request: &BacktestRequest,
) -> anyhow::Result<BacktestReport> {
    validate_request(request)?;

    let symbol = store.validate_symbol(&request.symbol)?;
    let mut dates = store.dates(&symbol);
    dates.sort();
    dates.retain(|date| {
        request
            .start_date
            .as_ref()
            .is_none_or(|start| date >= start)
    });
    dates.retain(|date| request.end_date.as_ref().is_none_or(|end| date <= end));

    let mut trades = Vec::new();
    let mut skipped = Vec::new();

    for (index, entry_date) in dates.iter().enumerate() {
        if index + 1 >= dates.len() {
            break;
        }

        let expiration = match select_expiration(store, &symbol, entry_date, request.target_dte) {
            Some(value) => value,
            None => {
                skipped.push(format!(
                    "{entry_date}: no usable expiration near target DTE"
                ));
                continue;
            }
        };

        let entry_chain = match store.chain(
            &symbol,
            entry_date,
            &request.entry_minute,
            &expiration,
            &request.pricing_mode,
            &request.dealer_model,
        ) {
            Ok(value) => value,
            Err(error) => {
                skipped.push(format!("{entry_date}: entry chain unavailable: {error}"));
                continue;
            }
        };

        let selected_legs = match select_legs(&entry_chain, &request.legs) {
            Ok(value) => value,
            Err(error) => {
                skipped.push(format!("{entry_date}: leg selection failed: {error}"));
                continue;
            }
        };
        let entry_analysis = match analyze_strategy(&entry_chain, &selected_legs, request.quantity)
        {
            Ok(value) => value,
            Err(error) => {
                skipped.push(format!("{entry_date}: entry analysis failed: {error}"));
                continue;
            }
        };

        let entry_costs = execution_cost(&selected_legs, request.quantity, &request.costs);
        let debit = (-entry_analysis.entry_cash_flow).max(0.0);
        let bounded_risk = entry_analysis.max_loss.is_some();
        let risk_basis = entry_analysis
            .max_loss
            .map(f64::abs)
            .filter(|value| *value > 0.0)
            .or_else(|| (debit > 0.0).then_some(debit))
            .unwrap_or_else(|| entry_analysis.entry_cash_flow.abs().max(1.0));

        let mut exit_result = None;
        let last_session = request.hold_trading_days.min(dates.len() - index - 1);
        for holding_sessions in 1..=last_session {
            let exit_date = &dates[index + holding_sessions];
            let exit_chain = match store.chain(
                &symbol,
                exit_date,
                &request.exit_minute,
                &expiration,
                &request.pricing_mode,
                &request.dealer_model,
            ) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let exit_analysis =
                match analyze_strategy(&exit_chain, &selected_legs, request.quantity) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
            let exit_costs = execution_cost(&selected_legs, request.quantity, &request.costs);
            let gross_pnl = entry_analysis.entry_cash_flow + exit_analysis.liquidation_value;
            let pnl = gross_pnl - entry_costs - exit_costs;
            let exit_reason =
                choose_exit_reason(pnl, risk_basis, exit_chain.dte, holding_sessions, request);
            if let Some(exit_reason) = exit_reason {
                exit_result = Some((
                    exit_date.clone(),
                    exit_chain,
                    exit_analysis,
                    gross_pnl,
                    exit_costs,
                    pnl,
                    exit_reason,
                    holding_sessions,
                ));
                break;
            }
        }

        let Some((
            exit_date,
            exit_chain,
            exit_analysis,
            gross_pnl,
            exit_costs,
            pnl,
            exit_reason,
            holding_sessions,
        )) = exit_result
        else {
            skipped.push(format!(
                "{entry_date}: no executable exit found within {} sessions",
                request.hold_trading_days
            ));
            continue;
        };

        let denominator = (debit + entry_costs).max(0.0);
        trades.push(BacktestTrade {
            entry_date: entry_date.clone(),
            exit_date,
            expiration,
            entry_minute: request.entry_minute.clone(),
            exit_minute: request.exit_minute.clone(),
            entry_spot: entry_chain.spot,
            exit_spot: exit_chain.spot,
            entry_cash_flow: entry_analysis.entry_cash_flow,
            exit_liquidation_value: exit_analysis.liquidation_value,
            gross_pnl,
            entry_costs,
            exit_costs,
            total_costs: entry_costs + exit_costs,
            pnl,
            risk_basis,
            bounded_risk,
            exit_reason,
            holding_sessions,
            return_on_debit: (denominator > 0.0).then_some(pnl / denominator),
            entry_atm_iv: entry_chain.metrics.atm_iv,
            entry_net_gex: entry_chain.metrics.net_gex,
            entry_gamma_flip: entry_chain.metrics.gamma_flip,
            spot_vs_gamma_flip: entry_chain.metrics.gamma_flip.map(|flip| {
                if entry_chain.spot >= flip {
                    "above".into()
                } else {
                    "below".into()
                }
            }),
            rr25: entry_chain.metrics.rr25,
            bf25: entry_chain.metrics.bf25,
            min_quote_quality: entry_analysis.min_quote_quality,
            legs: selected_legs,
        });
    }

    let stats = summarize(&trades);
    let mut cumulative = 0.0;
    let equity_curve = trades
        .iter()
        .map(|trade| {
            cumulative += trade.pnl;
            EquityPoint {
                date: trade.exit_date.clone(),
                cumulative_pnl: cumulative,
            }
        })
        .collect();

    let manifest = freeze_manifest(request)?;
    let fingerprint = format!(
        "{}:{}:{}",
        manifest.strategy_id,
        request.start_date.as_deref().unwrap_or("first"),
        request.end_date.as_deref().unwrap_or("last"),
    );

    Ok(BacktestReport {
        engine: "point_in_time_v2",
        symbol,
        request_fingerprint: fingerprint,
        manifest,
        assumptions: vec![
            "entry contracts are selected only from the entry snapshot".into(),
            "buys execute at ask and sells execute at bid".into(),
            "exit liquidation uses the opposite executable side".into(),
            "configured commission and additional slippage are charged on entry and exit".into(),
            "take-profit and stop-loss thresholds use net P/L after configured execution costs".into(),
            "hold period is measured in available trading sessions".into(),
            "early assignment, dividends, taxes, partial fills, and market impact beyond configured slippage are not modeled".into(),
        ],
        skipped,
        stats,
        equity_curve,
        trades,
    })
}

fn validate_request(request: &BacktestRequest) -> anyhow::Result<()> {
    anyhow::ensure!(
        !request.legs.is_empty() && request.legs.len() <= 8,
        "backtest requires 1-8 legs"
    );
    anyhow::ensure!(
        (1..=20).contains(&request.quantity),
        "quantity must be between 1 and 20"
    );
    anyhow::ensure!(
        request.hold_trading_days >= 1 && request.hold_trading_days <= 120,
        "hold_trading_days must be between 1 and 120"
    );
    anyhow::ensure!(
        (0..=1000).contains(&request.target_dte),
        "target_dte must be between 0 and 1000"
    );
    anyhow::ensure!(
        request.costs.commission_per_contract >= 0.0 && request.costs.slippage_per_contract >= 0.0,
        "execution costs must be non-negative"
    );
    if let Some(value) = request.exits.take_profit_pct_of_risk {
        anyhow::ensure!(
            value > 0.0 && value <= 10.0,
            "invalid take-profit threshold"
        );
    }
    if let Some(value) = request.exits.stop_loss_pct_of_risk {
        anyhow::ensure!(value > 0.0 && value <= 10.0, "invalid stop-loss threshold");
    }
    if let Some(value) = request.exits.exit_dte_lte {
        anyhow::ensure!((0..=1000).contains(&value), "invalid exit DTE threshold");
    }
    for leg in &request.legs {
        anyhow::ensure!(
            matches!(leg.right.to_uppercase().as_str(), "CALL" | "PUT"),
            "invalid leg right"
        );
        anyhow::ensure!(
            matches!(leg.side.to_uppercase().as_str(), "BUY" | "SELL"),
            "invalid leg side"
        );
        anyhow::ensure!(
            (0.01..=0.99).contains(&leg.target_delta.abs()),
            "target_delta must be between 0.01 and 0.99"
        );
        anyhow::ensure!(
            (1..=20).contains(&leg.ratio),
            "leg ratio must be between 1 and 20"
        );
    }
    Ok(())
}

fn choose_exit_reason(
    pnl: f64,
    risk_basis: f64,
    dte: i64,
    holding_sessions: usize,
    request: &BacktestRequest,
) -> Option<String> {
    if request
        .exits
        .take_profit_pct_of_risk
        .is_some_and(|threshold| pnl >= risk_basis * threshold)
    {
        return Some("take_profit".into());
    }
    if request
        .exits
        .stop_loss_pct_of_risk
        .is_some_and(|threshold| pnl <= -risk_basis * threshold)
    {
        return Some("stop_loss".into());
    }
    if request
        .exits
        .exit_dte_lte
        .is_some_and(|threshold| dte <= threshold)
    {
        return Some("dte_exit".into());
    }
    (holding_sessions >= request.hold_trading_days).then(|| "max_hold".into())
}

fn execution_cost(legs: &[StrategyLegInput], quantity: u32, costs: &CostModel) -> f64 {
    let contracts = legs
        .iter()
        .map(|leg| leg.ratio as f64 * quantity as f64)
        .sum::<f64>();
    contracts * (costs.commission_per_contract + costs.slippage_per_contract)
}

fn select_expiration(
    store: &ReplayStore,
    symbol: &str,
    trading_date: &str,
    target_dte: i64,
) -> Option<String> {
    let day = NaiveDate::parse_from_str(trading_date, "%Y-%m-%d").ok()?;
    store
        .expirations(symbol, trading_date)
        .into_iter()
        .filter_map(|expiry| {
            let parsed = NaiveDate::parse_from_str(&expiry, "%Y-%m-%d").ok()?;
            let dte = (parsed - day).num_days();
            (dte >= 0).then_some((expiry, (dte - target_dte).abs()))
        })
        .min_by_key(|(_, distance)| *distance)
        .map(|(expiry, _)| expiry)
}

fn select_legs(
    chain: &crate::models::ChainSnapshot,
    rules: &[BacktestLegRule],
) -> anyhow::Result<Vec<StrategyLegInput>> {
    let mut selected = Vec::with_capacity(rules.len());
    let mut used = std::collections::HashSet::new();
    for rule in rules {
        let right = rule.right.to_uppercase();
        let target = if right == "PUT" {
            -rule.target_delta.abs()
        } else {
            rule.target_delta.abs()
        };
        let row = chain
            .rows
            .iter()
            .filter(|row| {
                row.right == right && row.bid >= 0.0 && row.ask > 0.0 && row.quality_score >= 40.0
            })
            .filter(|row| !used.contains(&row.symbol))
            .min_by(|a, b| {
                (a.delta - target)
                    .abs()
                    .total_cmp(&(b.delta - target).abs())
            })
            .ok_or_else(|| anyhow::anyhow!("no contract for {right} target delta {target:.2}"))?;
        used.insert(row.symbol.clone());
        selected.push(StrategyLegInput {
            symbol: Some(row.symbol.clone()),
            strike: row.strike,
            right,
            side: rule.side.to_uppercase(),
            ratio: rule.ratio,
        });
    }
    Ok(selected)
}

pub(crate) fn summarize(trades: &[BacktestTrade]) -> BacktestStats {
    if trades.is_empty() {
        return BacktestStats {
            trades: 0,
            wins: 0,
            losses: 0,
            win_rate: 0.0,
            total_pnl: 0.0,
            average_pnl: 0.0,
            median_pnl: 0.0,
            profit_factor: None,
            max_drawdown: 0.0,
            total_costs: 0.0,
        };
    }
    let wins = trades.iter().filter(|trade| trade.pnl > 0.0).count();
    let losses = trades.iter().filter(|trade| trade.pnl < 0.0).count();
    let total_pnl = trades.iter().map(|trade| trade.pnl).sum::<f64>();
    let total_costs = trades.iter().map(|trade| trade.total_costs).sum::<f64>();
    let gross_profit = trades
        .iter()
        .filter(|trade| trade.pnl > 0.0)
        .map(|trade| trade.pnl)
        .sum::<f64>();
    let gross_loss = -trades
        .iter()
        .filter(|trade| trade.pnl < 0.0)
        .map(|trade| trade.pnl)
        .sum::<f64>();
    let mut pnls: Vec<f64> = trades.iter().map(|trade| trade.pnl).collect();
    pnls.sort_by(f64::total_cmp);
    let median_pnl = if pnls.len().is_multiple_of(2) {
        (pnls[pnls.len() / 2 - 1] + pnls[pnls.len() / 2]) / 2.0
    } else {
        pnls[pnls.len() / 2]
    };
    let mut equity: f64 = 0.0;
    let mut peak: f64 = 0.0;
    let mut max_drawdown: f64 = 0.0;
    for trade in trades {
        equity += trade.pnl;
        peak = peak.max(equity);
        max_drawdown = max_drawdown.max(peak - equity);
    }
    BacktestStats {
        trades: trades.len(),
        wins,
        losses,
        win_rate: wins as f64 / trades.len() as f64,
        total_pnl,
        average_pnl: total_pnl / trades.len() as f64,
        median_pnl,
        profit_factor: (gross_loss > 0.0).then_some(gross_profit / gross_loss),
        max_drawdown,
        total_costs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_request() -> BacktestRequest {
        BacktestRequest {
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
            costs: CostModel {
                commission_per_contract: 0.65,
                slippage_per_contract: 0.05,
            },
            exits: ExitRules {
                take_profit_pct_of_risk: Some(0.5),
                stop_loss_pct_of_risk: Some(0.4),
                exit_dte_lte: Some(3),
            },
            legs: vec![],
        }
    }

    fn mk_trade(pnl: f64) -> BacktestTrade {
        BacktestTrade {
            entry_date: "2026-01-01".into(),
            exit_date: "2026-01-02".into(),
            expiration: "2026-02-01".into(),
            entry_minute: "10:00".into(),
            exit_minute: "15:45".into(),
            entry_spot: 100.0,
            exit_spot: 100.0,
            entry_cash_flow: -100.0,
            exit_liquidation_value: 100.0 + pnl,
            gross_pnl: pnl,
            entry_costs: 1.0,
            exit_costs: 1.0,
            total_costs: 2.0,
            pnl,
            risk_basis: 100.0,
            bounded_risk: true,
            exit_reason: "max_hold".into(),
            holding_sessions: 1,
            return_on_debit: Some(pnl / 101.0),
            entry_atm_iv: None,
            entry_net_gex: None,
            entry_gamma_flip: None,
            spot_vs_gamma_flip: None,
            rr25: None,
            bf25: None,
            min_quote_quality: 100.0,
            legs: vec![],
        }
    }

    #[test]
    fn summary_tracks_drawdown_profit_factor_and_costs() {
        let stats = summarize(&[mk_trade(100.0), mk_trade(-50.0), mk_trade(-25.0)]);
        assert_eq!(stats.trades, 3);
        assert_eq!(stats.wins, 1);
        assert_eq!(stats.max_drawdown, 75.0);
        assert_eq!(stats.total_costs, 6.0);
        assert!((stats.profit_factor.unwrap() - 1.3333333).abs() < 1e-5);
    }

    #[test]
    fn exit_rules_have_deterministic_priority() {
        let request = test_request();
        assert_eq!(
            choose_exit_reason(60.0, 100.0, 2, 5, &request).as_deref(),
            Some("take_profit")
        );
        assert_eq!(
            choose_exit_reason(-50.0, 100.0, 2, 5, &request).as_deref(),
            Some("stop_loss")
        );
        assert_eq!(
            choose_exit_reason(0.0, 100.0, 2, 2, &request).as_deref(),
            Some("dte_exit")
        );
    }

    #[test]
    fn execution_cost_is_per_contract_per_execution() {
        let legs = vec![
            StrategyLegInput {
                symbol: None,
                strike: 100.0,
                right: "CALL".into(),
                side: "BUY".into(),
                ratio: 1,
            },
            StrategyLegInput {
                symbol: None,
                strike: 105.0,
                right: "CALL".into(),
                side: "SELL".into(),
                ratio: 2,
            },
        ];
        let cost = execution_cost(
            &legs,
            2,
            &CostModel {
                commission_per_contract: 0.65,
                slippage_per_contract: 0.05,
            },
        );
        assert!((cost - 4.2).abs() < 1e-9);
    }
}
