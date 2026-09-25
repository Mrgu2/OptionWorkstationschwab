use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::{
    backtest::{BacktestRequest, BacktestTrade, run_backtest},
    manifest::{freeze_manifest, unique_strategy_ids},
    replay::ReplayStore,
    strategy::analyze_strategy,
};

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioRequest {
    pub strategies: Vec<BacktestRequest>,
    #[serde(default = "default_initial_capital")]
    pub initial_capital: f64,
    #[serde(default = "default_max_open_positions")]
    pub max_open_positions: usize,
    #[serde(default = "default_max_risk_per_trade")]
    pub max_risk_pct_per_trade: f64,
    #[serde(default = "default_max_total_open_risk")]
    pub max_total_open_risk_pct: f64,
    #[serde(default = "default_require_bounded_risk")]
    pub require_bounded_risk: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortfolioTrade {
    pub strategy_id: String,
    pub accepted: bool,
    pub rejection_reason: Option<String>,
    pub capital_at_risk: f64,
    pub equity_at_entry: f64,
    pub open_risk_before_entry: f64,
    pub trade: BacktestTrade,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortfolioEquityPoint {
    pub timestamp: String,
    pub equity: f64,
    pub realized_pnl: f64,
    pub open_positions: usize,
    pub open_risk: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortfolioMtmPoint {
    pub date: String,
    pub equity: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub open_positions: usize,
    pub open_risk: f64,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortfolioReport {
    pub engine: &'static str,
    pub initial_capital: f64,
    pub ending_capital: f64,
    pub net_pnl: f64,
    pub return_pct: f64,
    pub accepted_trades: usize,
    pub rejected_trades: usize,
    pub max_open_positions_observed: usize,
    pub peak_open_risk: f64,
    pub peak_open_risk_pct_of_equity: f64,
    pub max_realized_drawdown: f64,
    pub max_realized_drawdown_pct: f64,
    pub max_mtm_drawdown: f64,
    pub max_mtm_drawdown_pct: f64,
    pub mtm_complete_points: usize,
    pub mtm_missing_marks: usize,
    pub total_modeled_costs: f64,
    pub rejection_reasons: BTreeMap<String, usize>,
    pub strategy_contributions: BTreeMap<String, f64>,
    pub equity_curve: Vec<PortfolioEquityPoint>,
    pub mtm_equity_curve: Vec<PortfolioMtmPoint>,
    pub trades: Vec<PortfolioTrade>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
struct CandidateTrade {
    strategy_id: String,
    trade: BacktestTrade,
}

#[derive(Debug, Clone)]
struct OpenPosition {
    risk: f64,
    strategy_id: String,
}

#[derive(Debug, Clone)]
struct Event {
    timestamp: String,
    priority: u8,
    candidate_index: usize,
    is_entry: bool,
}

fn default_initial_capital() -> f64 {
    100_000.0
}
fn default_max_open_positions() -> usize {
    10
}
fn default_max_risk_per_trade() -> f64 {
    0.05
}
fn default_max_total_open_risk() -> f64 {
    0.25
}
fn default_require_bounded_risk() -> bool {
    true
}

pub fn run_portfolio(
    store: &ReplayStore,
    request: &PortfolioRequest,
) -> anyhow::Result<PortfolioReport> {
    anyhow::ensure!(
        !request.strategies.is_empty() && request.strategies.len() <= 20,
        "portfolio requires 1-20 strategies"
    );
    anyhow::ensure!(
        request.initial_capital > 0.0,
        "initial_capital must be positive"
    );
    anyhow::ensure!(
        (1..=100).contains(&request.max_open_positions),
        "max_open_positions must be between 1 and 100"
    );
    anyhow::ensure!(
        request.max_risk_pct_per_trade > 0.0 && request.max_risk_pct_per_trade <= 1.0,
        "max_risk_pct_per_trade must be in (0, 1]"
    );
    anyhow::ensure!(
        request.max_total_open_risk_pct > 0.0 && request.max_total_open_risk_pct <= 1.0,
        "max_total_open_risk_pct must be in (0, 1]"
    );
    anyhow::ensure!(
        request.max_risk_pct_per_trade <= request.max_total_open_risk_pct,
        "per-trade risk cap cannot exceed total open-risk cap"
    );
    unique_strategy_ids(&request.strategies)?;

    let mut candidates = Vec::new();
    for strategy in &request.strategies {
        let report = run_backtest(store, strategy)?;
        let strategy_id = freeze_manifest(strategy)?.strategy_id;
        candidates.extend(report.trades.into_iter().map(|trade| CandidateTrade {
            strategy_id: strategy_id.clone(),
            trade,
        }));
    }

    candidates.sort_by(|a, b| {
        entry_timestamp(&a.trade)
            .cmp(&entry_timestamp(&b.trade))
            .then_with(|| a.strategy_id.cmp(&b.strategy_id))
    });

    let mut events = Vec::with_capacity(candidates.len() * 2);
    for (index, candidate) in candidates.iter().enumerate() {
        events.push(Event {
            timestamp: entry_timestamp(&candidate.trade),
            priority: 1,
            candidate_index: index,
            is_entry: true,
        });
        events.push(Event {
            timestamp: exit_timestamp(&candidate.trade),
            priority: 0,
            candidate_index: index,
            is_entry: false,
        });
    }
    events.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.candidate_index.cmp(&b.candidate_index))
    });

    let mut equity = request.initial_capital;
    let mut peak_equity = equity;
    let mut max_realized_drawdown: f64 = 0.0;
    let mut max_realized_drawdown_pct: f64 = 0.0;
    let mut open: HashMap<usize, OpenPosition> = HashMap::new();
    let mut accepted = vec![false; candidates.len()];
    let mut records: Vec<Option<PortfolioTrade>> = vec![None; candidates.len()];
    let mut rejection_reasons = BTreeMap::new();
    let mut strategy_contributions = BTreeMap::new();
    let mut equity_curve = Vec::new();
    let mut max_open_positions_observed = 0usize;
    let mut peak_open_risk: f64 = 0.0;
    let mut peak_open_risk_pct_of_equity: f64 = 0.0;
    let mut total_modeled_costs = 0.0;

    for event in events {
        let candidate = &candidates[event.candidate_index];
        if event.is_entry {
            let risk = (candidate.trade.risk_basis + candidate.trade.total_costs).max(0.0);
            let open_risk_before = open.values().map(|position| position.risk).sum::<f64>();
            let capacity_equity = equity.max(0.0);
            let per_trade_cap = capacity_equity * request.max_risk_pct_per_trade;
            let total_risk_cap = capacity_equity * request.max_total_open_risk_pct;

            let rejection = if request.require_bounded_risk && !candidate.trade.bounded_risk {
                Some("unbounded_risk")
            } else if capacity_equity <= 0.0 {
                Some("capital_depleted")
            } else if open.len() >= request.max_open_positions {
                Some("max_open_positions")
            } else if risk > per_trade_cap {
                Some("per_trade_risk_cap")
            } else if open_risk_before + risk > total_risk_cap {
                Some("total_open_risk_cap")
            } else {
                None
            };

            if let Some(reason) = rejection {
                *rejection_reasons.entry(reason.to_string()).or_insert(0) += 1;
                records[event.candidate_index] = Some(PortfolioTrade {
                    strategy_id: candidate.strategy_id.clone(),
                    accepted: false,
                    rejection_reason: Some(reason.into()),
                    capital_at_risk: risk,
                    equity_at_entry: equity,
                    open_risk_before_entry: open_risk_before,
                    trade: candidate.trade.clone(),
                });
            } else {
                accepted[event.candidate_index] = true;
                open.insert(
                    event.candidate_index,
                    OpenPosition {
                        risk,
                        strategy_id: candidate.strategy_id.clone(),
                    },
                );
                records[event.candidate_index] = Some(PortfolioTrade {
                    strategy_id: candidate.strategy_id.clone(),
                    accepted: true,
                    rejection_reason: None,
                    capital_at_risk: risk,
                    equity_at_entry: equity,
                    open_risk_before_entry: open_risk_before,
                    trade: candidate.trade.clone(),
                });
                max_open_positions_observed = max_open_positions_observed.max(open.len());
                let open_risk = open.values().map(|position| position.risk).sum::<f64>();
                peak_open_risk = peak_open_risk.max(open_risk);
                if equity > 0.0 {
                    peak_open_risk_pct_of_equity =
                        peak_open_risk_pct_of_equity.max(open_risk / equity);
                }
            }
        } else if accepted[event.candidate_index]
            && let Some(position) = open.remove(&event.candidate_index)
        {
            equity += candidate.trade.pnl;
            total_modeled_costs += candidate.trade.total_costs;
            *strategy_contributions
                .entry(position.strategy_id)
                .or_insert(0.0) += candidate.trade.pnl;

            peak_equity = peak_equity.max(equity);
            let drawdown = (peak_equity - equity).max(0.0);
            max_realized_drawdown = max_realized_drawdown.max(drawdown);
            if peak_equity > 0.0 {
                max_realized_drawdown_pct = max_realized_drawdown_pct.max(drawdown / peak_equity);
            }
            let open_risk = open.values().map(|position| position.risk).sum::<f64>();
            equity_curve.push(PortfolioEquityPoint {
                timestamp: event.timestamp,
                equity,
                realized_pnl: equity - request.initial_capital,
                open_positions: open.len(),
                open_risk,
            });
        }
    }

    let trades: Vec<_> = records.into_iter().flatten().collect();
    let accepted_trades = trades.iter().filter(|trade| trade.accepted).count();
    let rejected_trades = trades.len().saturating_sub(accepted_trades);
    let net_pnl = equity - request.initial_capital;
    let mtm = build_mtm_curve(store, request.initial_capital, &trades);

    Ok(PortfolioReport {
        engine: "portfolio_capital_v1",
        initial_capital: request.initial_capital,
        ending_capital: equity,
        net_pnl,
        return_pct: net_pnl / request.initial_capital * 100.0,
        accepted_trades,
        rejected_trades,
        max_open_positions_observed,
        peak_open_risk,
        peak_open_risk_pct_of_equity: peak_open_risk_pct_of_equity * 100.0,
        max_realized_drawdown,
        max_realized_drawdown_pct: max_realized_drawdown_pct * 100.0,
        max_mtm_drawdown: mtm.max_drawdown,
        max_mtm_drawdown_pct: mtm.max_drawdown_pct * 100.0,
        mtm_complete_points: mtm.complete_points,
        mtm_missing_marks: mtm.missing_marks,
        total_modeled_costs,
        rejection_reasons,
        strategy_contributions,
        equity_curve,
        mtm_equity_curve: mtm.points,
        trades,
        notes: vec![
            "capital admission is evaluated chronologically using realized equity available at each entry".into(),
            "defined-risk trades use backtest max-loss risk basis plus modeled execution costs".into(),
            "by default, trades without a finite max-loss estimate are rejected from portfolio simulation".into(),
            "daily mark-to-market equity uses executable-side liquidation values and includes hypothetical exit costs for open positions".into(),
            "MTM drawdown metrics use only complete daily marks; missing option-chain marks remain explicit instead of being forward-filled".into(),
            "individual strategy backtests can generate overlapping signals; portfolio limits decide which trades receive capital".into(),
        ],
    })
}

#[derive(Debug)]
struct MtmSummary {
    points: Vec<PortfolioMtmPoint>,
    max_drawdown: f64,
    max_drawdown_pct: f64,
    complete_points: usize,
    missing_marks: usize,
}

fn build_mtm_curve(
    store: &ReplayStore,
    initial_capital: f64,
    trades: &[PortfolioTrade],
) -> MtmSummary {
    let accepted: Vec<&PortfolioTrade> = trades.iter().filter(|trade| trade.accepted).collect();
    if accepted.is_empty() {
        return MtmSummary {
            points: Vec::new(),
            max_drawdown: 0.0,
            max_drawdown_pct: 0.0,
            complete_points: 0,
            missing_marks: 0,
        };
    }

    let min_date = accepted
        .iter()
        .map(|trade| trade.trade.entry_date.as_str())
        .min()
        .unwrap_or("");
    let max_date = accepted
        .iter()
        .map(|trade| trade.trade.exit_date.as_str())
        .max()
        .unwrap_or("");

    let mut dates = BTreeSet::new();
    let symbols: BTreeSet<String> = accepted
        .iter()
        .map(|trade| trade.trade.symbol.clone())
        .collect();
    for symbol in symbols {
        for date in store.dates(&symbol) {
            if date.as_str() >= min_date && date.as_str() <= max_date {
                dates.insert(date);
            }
        }
    }

    let mut points = Vec::new();
    let mut peak = initial_capital;
    let mut max_drawdown: f64 = 0.0;
    let mut max_drawdown_pct: f64 = 0.0;
    let mut complete_points = 0usize;
    let mut missing_marks = 0usize;

    for date in dates {
        let realized_pnl = accepted
            .iter()
            .filter(|record| record.trade.exit_date.as_str() <= date.as_str())
            .map(|record| record.trade.pnl)
            .sum::<f64>();

        let active: Vec<&PortfolioTrade> = accepted
            .iter()
            .copied()
            .filter(|record| {
                record.trade.entry_date.as_str() <= date.as_str()
                    && record.trade.exit_date.as_str() > date.as_str()
            })
            .collect();

        let mut unrealized_pnl = 0.0;
        let mut open_risk = 0.0;
        let mut complete = true;

        for record in &active {
            open_risk += record.capital_at_risk;
            let trade = &record.trade;
            let mark_minute = if date == trade.entry_date && trade.exit_minute < trade.entry_minute
            {
                &trade.entry_minute
            } else {
                &trade.exit_minute
            };
            let marked = store
                .chain(
                    &trade.symbol,
                    &date,
                    mark_minute,
                    &trade.expiration,
                    &trade.pricing_mode,
                    &trade.dealer_model,
                )
                .and_then(|chain| analyze_strategy(&chain, &trade.legs, trade.quantity))
                .and_then(|analysis| {
                    anyhow::ensure!(analysis.executable, "mark is not executable");
                    Ok(analysis)
                });
            match marked {
                Ok(analysis) => {
                    unrealized_pnl += trade.entry_cash_flow + analysis.liquidation_value
                        - trade.entry_costs
                        - trade.exit_costs;
                }
                Err(_) => {
                    complete = false;
                    missing_marks += 1;
                }
            }
        }

        let equity = initial_capital + realized_pnl + unrealized_pnl;
        if complete {
            complete_points += 1;
            peak = peak.max(equity);
            let drawdown = (peak - equity).max(0.0);
            max_drawdown = max_drawdown.max(drawdown);
            if peak > 0.0 {
                max_drawdown_pct = max_drawdown_pct.max(drawdown / peak);
            }
        }

        points.push(PortfolioMtmPoint {
            date,
            equity,
            realized_pnl,
            unrealized_pnl,
            open_positions: active.len(),
            open_risk,
            complete,
        });
    }

    MtmSummary {
        points,
        max_drawdown,
        max_drawdown_pct,
        complete_points,
        missing_marks,
    }
}

fn entry_timestamp(trade: &BacktestTrade) -> String {
    format!("{}T{}:00", trade.entry_date, trade.entry_minute)
}

fn exit_timestamp(trade: &BacktestTrade) -> String {
    format!("{}T{}:00", trade.exit_date, trade.exit_minute)
}

#[cfg(test)]
mod tests {
    #[test]
    fn entry_day_mark_never_uses_a_pre_entry_time() {
        let entry = "15:00".to_string();
        let exit = "09:45".to_string();
        let date = "2026-09-24".to_string();
        let entry_date = "2026-09-24".to_string();
        let mark = if date == entry_date && exit < entry {
            entry
        } else {
            exit
        };
        assert_eq!(mark, "15:00");
    }

    #[test]
    fn timestamps_sort_iso_dates_and_minutes() {
        let mut values = [
            "2026-09-24T15:45:00".to_string(),
            "2026-09-23T15:45:00".to_string(),
            "2026-09-24T10:00:00".to_string(),
        ];
        values.sort();
        assert_eq!(values[0], "2026-09-23T15:45:00");
        assert_eq!(values[1], "2026-09-24T10:00:00");
        assert_eq!(values[2], "2026-09-24T15:45:00");
    }
}
