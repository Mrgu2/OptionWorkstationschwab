use serde::{Deserialize, Serialize};

use crate::{
    backtest::{BacktestRequest, BacktestTrade, run_backtest},
    replay::ReplayStore,
};

#[derive(Debug, Clone, Deserialize)]
pub struct RegimeScanRequest {
    #[serde(flatten)]
    pub backtest: BacktestRequest,
    #[serde(default = "default_low_iv")]
    pub low_iv: f64,
    #[serde(default = "default_high_iv")]
    pub high_iv: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegimeBucket {
    pub dimension: String,
    pub bucket: String,
    pub trades: usize,
    pub win_rate: f64,
    pub average_pnl: f64,
    pub total_pnl: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegimeScanReport {
    pub engine: &'static str,
    pub symbol: String,
    pub total_trades: usize,
    pub buckets: Vec<RegimeBucket>,
    pub notes: Vec<String>,
}

fn default_low_iv() -> f64 {
    20.0
}
fn default_high_iv() -> f64 {
    40.0
}

pub fn scan(store: &ReplayStore, request: &RegimeScanRequest) -> anyhow::Result<RegimeScanReport> {
    anyhow::ensure!(
        request.low_iv > 0.0 && request.high_iv > request.low_iv,
        "invalid IV regime thresholds"
    );
    let report = run_backtest(store, &request.backtest)?;
    let mut buckets = Vec::new();

    for (label, predicate) in [
        (
            "low",
            Box::new(|t: &BacktestTrade| t.entry_atm_iv.is_some_and(|iv| iv < request.low_iv))
                as Box<dyn Fn(&BacktestTrade) -> bool>,
        ),
        (
            "mid",
            Box::new(|t: &BacktestTrade| {
                t.entry_atm_iv
                    .is_some_and(|iv| iv >= request.low_iv && iv < request.high_iv)
            }),
        ),
        (
            "high",
            Box::new(|t: &BacktestTrade| t.entry_atm_iv.is_some_and(|iv| iv >= request.high_iv)),
        ),
    ] {
        buckets.push(bucket(
            "atm_iv",
            label,
            report.trades.iter().filter(|t| predicate(t)).collect(),
        ));
    }

    for (label, predicate) in [
        (
            "negative",
            Box::new(|t: &BacktestTrade| t.entry_net_gex.is_some_and(|g| g < 0.0))
                as Box<dyn Fn(&BacktestTrade) -> bool>,
        ),
        (
            "positive",
            Box::new(|t: &BacktestTrade| t.entry_net_gex.is_some_and(|g| g >= 0.0)),
        ),
    ] {
        buckets.push(bucket(
            "net_gex",
            label,
            report.trades.iter().filter(|t| predicate(t)).collect(),
        ));
    }

    for label in ["above", "below"] {
        buckets.push(bucket(
            "gamma_flip",
            label,
            report
                .trades
                .iter()
                .filter(|t| t.spot_vs_gamma_flip.as_deref() == Some(label))
                .collect(),
        ));
    }

    Ok(RegimeScanReport {
        engine: "strategy_regime_scanner_v1",
        symbol: report.symbol,
        total_trades: report.trades.len(),
        buckets,
        notes: vec![
            "regime buckets are descriptive slices of the same backtest sample".into(),
            "small buckets can be unstable and must not be treated as independent validation".into(),
            "use holdout dates or walk-forward testing before interpreting a regime as persistent edge".into(),
        ],
    })
}

fn bucket(dimension: &str, name: &str, trades: Vec<&BacktestTrade>) -> RegimeBucket {
    let count = trades.len();
    let wins = trades.iter().filter(|trade| trade.pnl > 0.0).count();
    let total_pnl = trades.iter().map(|trade| trade.pnl).sum::<f64>();
    RegimeBucket {
        dimension: dimension.into(),
        bucket: name.into(),
        trades: count,
        win_rate: if count == 0 {
            0.0
        } else {
            wins as f64 / count as f64
        },
        average_pnl: if count == 0 {
            0.0
        } else {
            total_pnl / count as f64
        },
        total_pnl,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_bucket_is_well_defined() {
        let b = bucket("iv", "low", vec![]);
        assert_eq!(b.trades, 0);
        assert_eq!(b.win_rate, 0.0);
    }
}
