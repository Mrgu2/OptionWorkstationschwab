use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    backtest::{BacktestRequest, execution_cost, select_expiration, select_legs, validate_request},
    manifest::freeze_manifest,
    replay::ReplayStore,
    strategy::{StrategyLegInput, analyze_strategy},
};

#[derive(Debug, Clone, Deserialize)]
pub struct RollingRequest {
    pub base: BacktestRequest,
    pub roll_dte_lte: i64,
    pub roll_target_dte: i64,
    #[serde(default = "default_max_rolls")]
    pub max_rolls: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct RollEvent {
    pub date: String,
    pub from_expiration: String,
    pub to_expiration: String,
    pub from_legs: Vec<StrategyLegInput>,
    pub to_legs: Vec<StrategyLegInput>,
    pub close_liquidation_value: f64,
    pub next_entry_cash_flow: f64,
    pub segment_net_pnl: f64,
    pub added_costs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RollingCampaign {
    pub entry_date: String,
    pub exit_date: String,
    pub initial_expiration: String,
    pub final_expiration: String,
    pub initial_spot: f64,
    pub final_spot: f64,
    pub roll_count: usize,
    pub exit_reason: String,
    pub gross_pnl: f64,
    pub total_costs: f64,
    pub pnl: f64,
    pub risk_basis: f64,
    pub bounded_risk: bool,
    pub rolls: Vec<RollEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RollingStats {
    pub campaigns: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub average_pnl: f64,
    pub median_pnl: f64,
    pub profit_factor: Option<f64>,
    pub max_drawdown: f64,
    pub total_costs: f64,
    pub total_rolls: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RollingReport {
    pub engine: &'static str,
    pub rolling_strategy_id: String,
    pub base_strategy_id: String,
    pub symbol: String,
    pub roll_dte_lte: i64,
    pub roll_target_dte: i64,
    pub max_rolls: u32,
    pub stats: RollingStats,
    pub skipped: Vec<String>,
    pub campaigns: Vec<RollingCampaign>,
    pub notes: Vec<String>,
}

#[derive(Serialize)]
struct RollingIdentity<'a> {
    version: &'a str,
    base_strategy_id: &'a str,
    roll_dte_lte: i64,
    roll_target_dte: i64,
    max_rolls: u32,
}

fn default_max_rolls() -> u32 {
    2
}

pub fn run_rolling_backtest(
    store: &ReplayStore,
    request: &RollingRequest,
) -> anyhow::Result<RollingReport> {
    validate_request(&request.base)?;
    anyhow::ensure!(
        request.roll_dte_lte >= 0 && request.roll_dte_lte <= 365,
        "roll_dte_lte must be between 0 and 365"
    );
    anyhow::ensure!(
        request.roll_target_dte > request.roll_dte_lte && request.roll_target_dte <= 1000,
        "roll_target_dte must be greater than roll_dte_lte and no more than 1000"
    );
    anyhow::ensure!(
        (1..=12).contains(&request.max_rolls),
        "max_rolls must be between 1 and 12"
    );

    let base_manifest = freeze_manifest(&request.base)?;
    let identity = RollingIdentity {
        version: "rolling-strategy-v1",
        base_strategy_id: &base_manifest.strategy_id,
        roll_dte_lte: request.roll_dte_lte,
        roll_target_dte: request.roll_target_dte,
        max_rolls: request.max_rolls,
    };
    let rolling_strategy_id =
        hex::encode(Sha256::digest(serde_json::to_vec(&identity)?))[..20].to_string();

    let symbol = store.validate_symbol(&request.base.symbol)?;
    let mut dates = store.dates(&symbol);
    dates.sort();
    dates.retain(|date| {
        request
            .base
            .start_date
            .as_ref()
            .is_none_or(|start| date >= start)
    });
    dates.retain(|date| request.base.end_date.as_ref().is_none_or(|end| date <= end));

    let mut campaigns = Vec::new();
    let mut skipped = Vec::new();

    for (index, entry_date) in dates.iter().enumerate() {
        if index + 1 >= dates.len() {
            break;
        }
        let Some(initial_expiration) =
            select_expiration(store, &symbol, entry_date, request.base.target_dte)
        else {
            skipped.push(format!("{entry_date}: no initial expiration"));
            continue;
        };
        let entry_chain = match store.chain(
            &symbol,
            entry_date,
            &request.base.entry_minute,
            &initial_expiration,
            &request.base.pricing_mode,
            &request.base.dealer_model,
        ) {
            Ok(value) => value,
            Err(error) => {
                skipped.push(format!("{entry_date}: initial chain unavailable: {error}"));
                continue;
            }
        };
        let initial_legs = match select_legs(&entry_chain, &request.base.legs) {
            Ok(value) => value,
            Err(error) => {
                skipped.push(format!(
                    "{entry_date}: initial leg selection failed: {error}"
                ));
                continue;
            }
        };
        let initial_analysis =
            match analyze_strategy(&entry_chain, &initial_legs, request.base.quantity) {
                Ok(value) => value,
                Err(error) => {
                    skipped.push(format!("{entry_date}: initial analysis failed: {error}"));
                    continue;
                }
            };

        let initial_open_cost =
            execution_cost(&initial_legs, request.base.quantity, &request.base.costs);
        let debit = (-initial_analysis.entry_cash_flow).max(0.0);
        let mut campaign_risk_basis = initial_analysis
            .max_loss
            .map(f64::abs)
            .filter(|value| *value > 0.0)
            .or_else(|| (debit > 0.0).then_some(debit))
            .unwrap_or_else(|| initial_analysis.entry_cash_flow.abs().max(1.0));
        let mut bounded_risk = initial_analysis.max_loss.is_some();

        let mut current_expiration = initial_expiration.clone();
        let mut current_legs = initial_legs;
        let mut current_segment_entry_cash_flow = initial_analysis.entry_cash_flow;
        let mut current_segment_open_cost = initial_open_cost;
        let mut gross_cash_flow = initial_analysis.entry_cash_flow;
        let mut open_costs = initial_open_cost;
        let mut close_costs = 0.0;
        let mut rolls = Vec::new();
        let mut terminal = None;

        let last_session = request
            .base
            .hold_trading_days
            .min(dates.len().saturating_sub(index + 1));

        for holding_sessions in 1..=last_session {
            let exit_date = &dates[index + holding_sessions];
            let chain = match store.chain(
                &symbol,
                exit_date,
                &request.base.exit_minute,
                &current_expiration,
                &request.base.pricing_mode,
                &request.base.dealer_model,
            ) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let analysis = match analyze_strategy(&chain, &current_legs, request.base.quantity) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let current_close_cost =
                execution_cost(&current_legs, request.base.quantity, &request.base.costs);
            let hypothetical_gross = gross_cash_flow + analysis.liquidation_value;
            let hypothetical_costs = open_costs + close_costs + current_close_cost;
            let hypothetical_pnl = hypothetical_gross - hypothetical_costs;

            if request
                .base
                .exits
                .take_profit_pct_of_risk
                .is_some_and(|threshold| hypothetical_pnl >= campaign_risk_basis * threshold)
            {
                terminal = Some((
                    exit_date.clone(),
                    chain.spot,
                    analysis.liquidation_value,
                    current_close_cost,
                    "take_profit".to_string(),
                ));
                break;
            }
            if request
                .base
                .exits
                .stop_loss_pct_of_risk
                .is_some_and(|threshold| hypothetical_pnl <= -campaign_risk_basis * threshold)
            {
                terminal = Some((
                    exit_date.clone(),
                    chain.spot,
                    analysis.liquidation_value,
                    current_close_cost,
                    "stop_loss".to_string(),
                ));
                break;
            }

            let can_roll =
                chain.dte <= request.roll_dte_lte && rolls.len() < request.max_rolls as usize;
            if can_roll {
                let next_expiration =
                    select_expiration(store, &symbol, exit_date, request.roll_target_dte);
                if let Some(next_expiration) = next_expiration
                    && next_expiration != current_expiration
                    && let Ok(next_chain) = store.chain(
                        &symbol,
                        exit_date,
                        &request.base.exit_minute,
                        &next_expiration,
                        &request.base.pricing_mode,
                        &request.base.dealer_model,
                    )
                    && let Ok(next_legs) = select_legs(&next_chain, &request.base.legs)
                    && let Ok(next_analysis) =
                        analyze_strategy(&next_chain, &next_legs, request.base.quantity)
                {
                    let next_open_cost =
                        execution_cost(&next_legs, request.base.quantity, &request.base.costs);
                    let next_debit = (-next_analysis.entry_cash_flow).max(0.0);
                    let next_risk = next_analysis
                        .max_loss
                        .map(f64::abs)
                        .filter(|value| *value > 0.0)
                        .or_else(|| (next_debit > 0.0).then_some(next_debit))
                        .unwrap_or_else(|| next_analysis.entry_cash_flow.abs().max(1.0));

                    let segment_net_pnl = current_segment_entry_cash_flow
                        + analysis.liquidation_value
                        - current_segment_open_cost
                        - current_close_cost;

                    rolls.push(RollEvent {
                        date: exit_date.clone(),
                        from_expiration: current_expiration.clone(),
                        to_expiration: next_expiration.clone(),
                        from_legs: current_legs.clone(),
                        to_legs: next_legs.clone(),
                        close_liquidation_value: analysis.liquidation_value,
                        next_entry_cash_flow: next_analysis.entry_cash_flow,
                        segment_net_pnl,
                        added_costs: current_close_cost + next_open_cost,
                    });

                    gross_cash_flow += analysis.liquidation_value + next_analysis.entry_cash_flow;
                    close_costs += current_close_cost;
                    open_costs += next_open_cost;
                    campaign_risk_basis = campaign_risk_basis.max(next_risk);
                    bounded_risk &= next_analysis.max_loss.is_some();
                    current_expiration = next_expiration;
                    current_legs = next_legs;
                    current_segment_entry_cash_flow = next_analysis.entry_cash_flow;
                    current_segment_open_cost = next_open_cost;
                    continue;
                }

                terminal = Some((
                    exit_date.clone(),
                    chain.spot,
                    analysis.liquidation_value,
                    current_close_cost,
                    "roll_unavailable_exit".to_string(),
                ));
                break;
            }

            if request
                .base
                .exits
                .exit_dte_lte
                .is_some_and(|threshold| chain.dte <= threshold)
            {
                terminal = Some((
                    exit_date.clone(),
                    chain.spot,
                    analysis.liquidation_value,
                    current_close_cost,
                    "dte_exit".to_string(),
                ));
                break;
            }

            if holding_sessions >= request.base.hold_trading_days {
                terminal = Some((
                    exit_date.clone(),
                    chain.spot,
                    analysis.liquidation_value,
                    current_close_cost,
                    "max_hold".to_string(),
                ));
                break;
            }
        }

        let Some((exit_date, final_spot, final_liquidation, final_close_cost, exit_reason)) =
            terminal
        else {
            skipped.push(format!(
                "{entry_date}: no executable rolling exit within {} sessions",
                request.base.hold_trading_days
            ));
            continue;
        };

        let gross_pnl = gross_cash_flow + final_liquidation;
        let total_costs = open_costs + close_costs + final_close_cost;
        let pnl = gross_pnl - total_costs;

        campaigns.push(RollingCampaign {
            entry_date: entry_date.clone(),
            exit_date,
            initial_expiration,
            final_expiration: current_expiration,
            initial_spot: entry_chain.spot,
            final_spot,
            roll_count: rolls.len(),
            exit_reason,
            gross_pnl,
            total_costs,
            pnl,
            risk_basis: campaign_risk_basis,
            bounded_risk,
            rolls,
        });
    }

    let stats = summarize(&campaigns);
    Ok(RollingReport {
        engine: "rolling_backtest_v1",
        rolling_strategy_id,
        base_strategy_id: base_manifest.strategy_id,
        symbol,
        roll_dte_lte: request.roll_dte_lte,
        roll_target_dte: request.roll_target_dte,
        max_rolls: request.max_rolls,
        stats,
        skipped,
        campaigns,
        notes: vec![
            "take-profit and stop-loss checks occur before a roll decision on each session".into(),
            "a roll closes the current contracts and opens newly delta-selected contracts at the same configured exit minute".into(),
            "each roll pays both close and new-entry execution costs; campaign P/L is the sum of all cash flows less all modeled costs".into(),
            "campaign risk basis is the maximum finite risk basis observed across its segments; an unbounded segment marks the whole campaign unbounded".into(),
        ],
    })
}

fn summarize(campaigns: &[RollingCampaign]) -> RollingStats {
    if campaigns.is_empty() {
        return RollingStats {
            campaigns: 0,
            wins: 0,
            losses: 0,
            win_rate: 0.0,
            total_pnl: 0.0,
            average_pnl: 0.0,
            median_pnl: 0.0,
            profit_factor: None,
            max_drawdown: 0.0,
            total_costs: 0.0,
            total_rolls: 0,
        };
    }
    let wins = campaigns
        .iter()
        .filter(|campaign| campaign.pnl > 0.0)
        .count();
    let losses = campaigns
        .iter()
        .filter(|campaign| campaign.pnl < 0.0)
        .count();
    let total_pnl = campaigns.iter().map(|campaign| campaign.pnl).sum::<f64>();
    let total_costs = campaigns
        .iter()
        .map(|campaign| campaign.total_costs)
        .sum::<f64>();
    let total_rolls = campaigns.iter().map(|campaign| campaign.roll_count).sum();
    let gross_profit = campaigns
        .iter()
        .filter(|campaign| campaign.pnl > 0.0)
        .map(|campaign| campaign.pnl)
        .sum::<f64>();
    let gross_loss = -campaigns
        .iter()
        .filter(|campaign| campaign.pnl < 0.0)
        .map(|campaign| campaign.pnl)
        .sum::<f64>();
    let mut pnls = campaigns
        .iter()
        .map(|campaign| campaign.pnl)
        .collect::<Vec<_>>();
    pnls.sort_by(f64::total_cmp);
    let median_pnl = if pnls.len().is_multiple_of(2) {
        (pnls[pnls.len() / 2 - 1] + pnls[pnls.len() / 2]) / 2.0
    } else {
        pnls[pnls.len() / 2]
    };

    let mut equity: f64 = 0.0;
    let mut peak: f64 = 0.0;
    let mut max_drawdown: f64 = 0.0;
    for campaign in campaigns {
        equity += campaign.pnl;
        peak = peak.max(equity);
        max_drawdown = max_drawdown.max(peak - equity);
    }

    RollingStats {
        campaigns: campaigns.len(),
        wins,
        losses,
        win_rate: wins as f64 / campaigns.len() as f64,
        total_pnl,
        average_pnl: total_pnl / campaigns.len() as f64,
        median_pnl,
        profit_factor: (gross_loss > 0.0).then_some(gross_profit / gross_loss),
        max_drawdown,
        total_costs,
        total_rolls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_identity_changes_with_roll_rule() {
        let a = RollingIdentity {
            version: "rolling-strategy-v1",
            base_strategy_id: "base",
            roll_dte_lte: 3,
            roll_target_dte: 30,
            max_rolls: 2,
        };
        let b = RollingIdentity {
            roll_target_dte: 45,
            ..a
        };
        let hash_a = hex::encode(Sha256::digest(serde_json::to_vec(&a).unwrap()));
        let hash_b = hex::encode(Sha256::digest(serde_json::to_vec(&b).unwrap()));
        assert_ne!(hash_a, hash_b);
    }
}
