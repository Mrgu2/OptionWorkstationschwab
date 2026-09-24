use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::{
    models::{ChainRow, ChainSnapshot},
    replay::ReplayStore,
    strategy::StrategyLegInput,
};

#[derive(Debug, Clone, Deserialize)]
pub struct AttributionRequest {
    pub symbol: String,
    pub entry_date: String,
    pub entry_minute: String,
    pub exit_date: String,
    pub exit_minute: String,
    pub expiration: String,
    #[serde(default = "default_quantity")]
    pub quantity: u32,
    #[serde(default = "default_pricing_mode")]
    pub pricing_mode: String,
    #[serde(default = "default_dealer_model")]
    pub dealer_model: String,
    pub legs: Vec<StrategyLegInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LegAttribution {
    pub symbol: String,
    pub side: String,
    pub contracts: f64,
    pub realized_pnl: f64,
    pub delta_effect: f64,
    pub gamma_effect: f64,
    pub theta_effect: f64,
    pub vega_effect: f64,
    pub explained_pnl: f64,
    pub residual: f64,
    pub entry_iv: f64,
    pub exit_iv: f64,
    pub entry_delta: f64,
    pub entry_gamma: f64,
    pub entry_theta: f64,
    pub entry_vega: f64,
    pub entry_vanna: f64,
    pub entry_charm: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttributionReport {
    pub engine: &'static str,
    pub symbol: String,
    pub entry_snapshot_id: String,
    pub exit_snapshot_id: String,
    pub spot_change: f64,
    pub elapsed_calendar_days: f64,
    pub realized_pnl: f64,
    pub delta_effect: f64,
    pub gamma_effect: f64,
    pub theta_effect: f64,
    pub vega_effect: f64,
    pub explained_pnl: f64,
    pub residual: f64,
    pub legs: Vec<LegAttribution>,
    pub caveats: Vec<String>,
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

pub fn attribute(
    store: &ReplayStore,
    request: &AttributionRequest,
) -> anyhow::Result<AttributionReport> {
    anyhow::ensure!(
        !request.legs.is_empty() && request.legs.len() <= 8,
        "attribution requires 1-8 legs"
    );
    anyhow::ensure!(
        (1..=20).contains(&request.quantity),
        "quantity must be between 1 and 20"
    );
    let entry = store.chain(
        &request.symbol,
        &request.entry_date,
        &request.entry_minute,
        &request.expiration,
        &request.pricing_mode,
        &request.dealer_model,
    )?;
    let exit = store.chain(
        &request.symbol,
        &request.exit_date,
        &request.exit_minute,
        &request.expiration,
        &request.pricing_mode,
        &request.dealer_model,
    )?;
    let elapsed = elapsed_days(&request.entry_date, &request.exit_date)?;
    attribute_chains(&entry, &exit, &request.legs, request.quantity, elapsed)
}

fn attribute_chains(
    entry: &ChainSnapshot,
    exit: &ChainSnapshot,
    legs: &[StrategyLegInput],
    quantity: u32,
    elapsed_calendar_days: f64,
) -> anyhow::Result<AttributionReport> {
    let ds = exit.spot - entry.spot;
    let mut rows = Vec::new();
    for leg in legs {
        let entry_row = resolve_row(entry, leg)?;
        let exit_row = exit
            .rows
            .iter()
            .find(|row| row.symbol == entry_row.symbol)
            .ok_or_else(|| {
                anyhow::anyhow!("exit snapshot missing contract {}", entry_row.symbol)
            })?;
        let sign = if leg.side.eq_ignore_ascii_case("BUY") {
            1.0
        } else if leg.side.eq_ignore_ascii_case("SELL") {
            -1.0
        } else {
            anyhow::bail!("invalid side {}", leg.side)
        };
        let contracts = leg.ratio as f64 * quantity as f64;
        let multiplier = sign * contracts * 100.0;

        let entry_exec = if sign > 0.0 {
            entry_row.ask
        } else {
            entry_row.bid
        };
        let exit_exec = if sign > 0.0 {
            exit_row.bid
        } else {
            exit_row.ask
        };
        let realized_pnl = multiplier * (exit_exec - entry_exec);
        let delta_effect = multiplier * entry_row.delta * ds;
        let gamma_effect = multiplier * 0.5 * entry_row.gamma * ds * ds;
        let theta_effect = multiplier * entry_row.theta * elapsed_calendar_days;
        let iv_change_points = exit_row.iv - entry_row.iv;
        let vega_effect = multiplier * entry_row.vega * iv_change_points;
        let explained_pnl = delta_effect + gamma_effect + theta_effect + vega_effect;
        let residual = realized_pnl - explained_pnl;

        rows.push(LegAttribution {
            symbol: entry_row.symbol.clone(),
            side: leg.side.to_uppercase(),
            contracts,
            realized_pnl,
            delta_effect,
            gamma_effect,
            theta_effect,
            vega_effect,
            explained_pnl,
            residual,
            entry_iv: entry_row.iv,
            exit_iv: exit_row.iv,
            entry_delta: entry_row.delta,
            entry_gamma: entry_row.gamma,
            entry_theta: entry_row.theta,
            entry_vega: entry_row.vega,
            entry_vanna: entry_row.vanna,
            entry_charm: entry_row.charm,
        });
    }

    let sum = |f: fn(&LegAttribution) -> f64| rows.iter().map(f).sum::<f64>();
    let realized_pnl = sum(|x| x.realized_pnl);
    let delta_effect = sum(|x| x.delta_effect);
    let gamma_effect = sum(|x| x.gamma_effect);
    let theta_effect = sum(|x| x.theta_effect);
    let vega_effect = sum(|x| x.vega_effect);
    let explained_pnl = delta_effect + gamma_effect + theta_effect + vega_effect;

    Ok(AttributionReport {
        engine: "greek_attribution_v1",
        symbol: entry.symbol.clone(),
        entry_snapshot_id: entry.snapshot_id.clone(),
        exit_snapshot_id: exit.snapshot_id.clone(),
        spot_change: ds,
        elapsed_calendar_days,
        realized_pnl,
        delta_effect,
        gamma_effect,
        theta_effect,
        vega_effect,
        explained_pnl,
        residual: realized_pnl - explained_pnl,
        legs: rows,
        caveats: vec![
            "first and second order Taylor attribution uses entry Greeks".into(),
            "vega uses entry vega times the observed contract IV change in percentage points".into(),
            "vanna and charm are reported as diagnostics but are not added to explained P/L to avoid overlap".into(),
            "residual includes higher-order Greeks, smile dynamics, discrete repricing, execution spread changes, and model error".into(),
        ],
    })
}

fn resolve_row<'a>(
    chain: &'a ChainSnapshot,
    leg: &StrategyLegInput,
) -> anyhow::Result<&'a ChainRow> {
    if let Some(symbol) = leg.symbol.as_deref() {
        return chain
            .rows
            .iter()
            .find(|row| row.symbol == symbol)
            .ok_or_else(|| anyhow::anyhow!("contract not found: {symbol}"));
    }
    chain
        .rows
        .iter()
        .find(|row| {
            (row.strike - leg.strike).abs() < 1e-6 && row.right.eq_ignore_ascii_case(&leg.right)
        })
        .ok_or_else(|| anyhow::anyhow!("contract not found: {} {}", leg.right, leg.strike))
}

fn elapsed_days(entry: &str, exit: &str) -> anyhow::Result<f64> {
    let entry = NaiveDate::parse_from_str(entry, "%Y-%m-%d")?;
    let exit = NaiveDate::parse_from_str(exit, "%Y-%m-%d")?;
    anyhow::ensure!(exit >= entry, "exit date must not precede entry date");
    Ok((exit - entry).num_days() as f64)
}

#[cfg(test)]
mod tests {
    use super::elapsed_days;
    #[test]
    fn elapsed_days_is_calendar_based() {
        assert_eq!(elapsed_days("2026-09-18", "2026-09-21").unwrap(), 3.0);
    }
}
