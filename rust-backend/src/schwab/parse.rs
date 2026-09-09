#[derive(Debug)]
struct ParsedChain {
    spot: f64,
    as_of: Option<DateTime<Utc>>,
    spot_as_of: Option<DateTime<Utc>>,
    delayed: bool,
    truncated: bool,
    by_expiration: BTreeMap<NaiveDate, Vec<RawOptionQuote>>,
}

fn parse_chain_payload(active: &ActiveUniverse, payload: &Value) -> anyhow::Result<ParsedChain> {
    if let Some(status) = payload.get("status").and_then(Value::as_str) {
        anyhow::ensure!(
            status.eq_ignore_ascii_case("SUCCESS"),
            "Schwab option chain status: {status}"
        );
    }

    let spot = payload
        .get("underlyingPrice")
        .and_then(number)
        .or_else(|| payload.pointer("/underlying/last").and_then(number))
        .or_else(|| payload.pointer("/underlying/lastPrice").and_then(number))
        .or_else(|| payload.pointer("/underlying/quote/lastPrice").and_then(number))
        .ok_or_else(|| anyhow!("Schwab option chain is missing underlying price"))?;
    anyhow::ensure!(spot.is_finite() && spot > 0.0, "invalid Schwab underlying price");

    let spot_as_of = ["/underlying/quoteTime", "/underlying/tradeTime"]
        .into_iter()
        .filter_map(|pointer| payload.pointer(pointer).and_then(epoch_ms))
        .max();
    let delayed = payload
        .get("isDelayed")
        .and_then(Value::as_bool)
        .or_else(|| payload.pointer("/underlying/delayed").and_then(Value::as_bool))
        .unwrap_or(false);
    let truncated = payload
        .get("isChainTruncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let selected: HashMap<NaiveDate, ()> = active
        .expirations
        .iter()
        .copied()
        .map(|date| (date, ()))
        .collect();
    let mut by_expiration = BTreeMap::new();
    let mut as_of = None;

    collect_side(
        payload.get("callExpDateMap"),
        "CALL",
        spot,
        active.moneyness_window,
        &selected,
        &mut by_expiration,
        &mut as_of,
    );
    collect_side(
        payload.get("putExpDateMap"),
        "PUT",
        spot,
        active.moneyness_window,
        &selected,
        &mut by_expiration,
        &mut as_of,
    );

    let total: usize = by_expiration.values().map(Vec::len).sum();
    if total > active.max_contracts {
        let per_expiry = (active.max_contracts / active.expirations.len().max(1)).max(2);
        for rows in by_expiration.values_mut() {
            if rows.len() > per_expiry {
                trim_around_spot(rows, spot, per_expiry);
            }
        }
    }

    anyhow::ensure!(
        by_expiration.values().any(|rows| !rows.is_empty()),
        "Schwab option chain returned no standard contracts inside the configured window"
    );

    Ok(ParsedChain {
        spot,
        as_of,
        spot_as_of,
        delayed,
        truncated,
        by_expiration,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_side(
    map: Option<&Value>,
    right: &str,
    spot: f64,
    window: f64,
    selected: &HashMap<NaiveDate, ()>,
    target: &mut BTreeMap<NaiveDate, Vec<RawOptionQuote>>,
    as_of: &mut Option<DateTime<Utc>>,
) {
    let Some(expirations) = map.and_then(Value::as_object) else {
        return;
    };

    for (expiration_key, strikes) in expirations {
        let date_text = expiration_key.split(':').next().unwrap_or(expiration_key);
        let Ok(expiration) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
            continue;
        };
        if !selected.contains_key(&expiration) {
            continue;
        }

        let Some(strikes) = strikes.as_object() else {
            continue;
        };
        for (strike_key, contracts) in strikes {
            let Some(contract) = contracts
                .as_array()
                .and_then(|rows| rows.iter().find(|contract| standard_contract(contract)))
            else {
                continue;
            };
            let strike = contract
                .get("strikePrice")
                .and_then(number)
                .or_else(|| strike_key.parse::<f64>().ok())
                .unwrap_or_default();
            if strike <= 0.0 || (strike / spot - 1.0).abs() > window {
                continue;
            }

            let symbol = contract
                .get("symbol")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if symbol.is_empty() {
                continue;
            }

            let quote_time = ["quoteTimeInLong", "tradeTimeInLong"]
                .into_iter()
                .find_map(|key| contract.get(key).and_then(epoch_ms));
            if let Some(timestamp) = quote_time
                && (as_of.is_none() || as_of.is_some_and(|current| timestamp > current))
            {
                *as_of = Some(timestamp);
            }

            target
                .entry(expiration)
                .or_default()
                .push(RawOptionQuote {
                    symbol,
                    strike,
                    right: right.into(),
                    bid_size: contract
                        .get("bidSize")
                        .and_then(integer)
                        .unwrap_or_default(),
                    ask_size: contract
                        .get("askSize")
                        .and_then(integer)
                        .unwrap_or_default(),
                    bid: contract.get("bid").and_then(number).unwrap_or_default(),
                    ask: contract.get("ask").and_then(number).unwrap_or_default(),
                    last: contract.get("last").and_then(number),
                    volume: contract
                        .get("totalVolume")
                        .or_else(|| contract.get("volume"))
                        .and_then(integer)
                        .unwrap_or_default(),
                    open_interest: contract
                        .get("openInterest")
                        .and_then(integer)
                        .unwrap_or_default(),
                    sdk_iv: contract
                        .get("volatility")
                        .and_then(number)
                        .and_then(normalize_iv),
                    sdk_delta: contract
                        .get("delta")
                        .and_then(number)
                        .and_then(provider_delta),
                    sdk_gamma: contract
                        .get("gamma")
                        .and_then(number)
                        .and_then(provider_gamma),
                    sdk_theta: contract
                        .get("theta")
                        .and_then(number)
                        .and_then(provider_greek),
                    sdk_vega: contract
                        .get("vega")
                        .and_then(number)
                        .and_then(provider_vega),
                });
        }
    }
}

fn standard_contract(contract: &Value) -> bool {
    if contract
        .get("nonStandard")
        .or_else(|| contract.get("isNonStandard"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    if contract
        .get("mini")
        .or_else(|| contract.get("isMini"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    contract
        .get("multiplier")
        .and_then(number)
        .is_none_or(|multiplier| (multiplier - 100.0).abs() <= 0.01)
}

fn provider_greek(value: f64) -> Option<f64> {
    (-900.0..900.0).contains(&value).then_some(value)
}

fn provider_delta(value: f64) -> Option<f64> {
    provider_greek(value).filter(|value| (-1.001..=1.001).contains(value))
}

fn provider_gamma(value: f64) -> Option<f64> {
    provider_greek(value).filter(|value| *value >= 0.0)
}

fn provider_vega(value: f64) -> Option<f64> {
    provider_greek(value).filter(|value| *value >= 0.0)
}

fn trim_around_spot(rows: &mut Vec<RawOptionQuote>, spot: f64, limit: usize) {
    rows.sort_by(|left, right| {
        (left.strike - spot)
            .abs()
            .total_cmp(&(right.strike - spot).abs())
    });
    rows.truncate(limit);
    rows.sort_by(|left, right| {
        left.strike
            .total_cmp(&right.strike)
            .then_with(|| left.right.cmp(&right.right))
    });
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use serde_json::json;

    fn active() -> ActiveUniverse {
        let expiration = NaiveDate::from_ymd_opt(2030, 1, 18).unwrap();
        ActiveUniverse {
            symbol: "SPY".into(),
            selected_expiration: expiration,
            expirations: vec![expiration],
            max_contracts: 100,
            moneyness_window: 0.20,
            pricing_mode: "micro".into(),
            dealer_model: "classic".into(),
        }
    }

    #[test]
    fn selects_standard_contract_and_filters_provider_sentinels() {
        let payload = json!({
            "status": "SUCCESS",
            "underlyingPrice": 100.0,
            "isDelayed": true,
            "isChainTruncated": true,
            "underlying": {
                "quoteTime": 1_700_000_000_000_i64,
                "delayed": true
            },
            "callExpDateMap": {
                "2030-01-18:100": {
                    "100.0": [
                        {
                            "symbol": "SPY MINI",
                            "strikePrice": 100.0,
                            "mini": true,
                            "multiplier": 10.0,
                            "volatility": 50.0
                        },
                        {
                            "symbol": "SPY STD",
                            "strikePrice": 100.0,
                            "mini": false,
                            "nonStandard": false,
                            "multiplier": 100.0,
                            "bid": 1.0,
                            "ask": 1.2,
                            "openInterest": 42,
                            "volatility": 3.5,
                            "delta": -999.0,
                            "gamma": 0.012,
                            "theta": -0.04,
                            "vega": 0.08,
                            "quoteTimeInLong": 1_700_000_001_000_i64
                        }
                    ]
                }
            },
            "putExpDateMap": {}
        });

        let parsed = parse_chain_payload(&active(), &payload).unwrap();
        let rows = parsed.by_expiration.values().next().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol, "SPY STD");
        assert_eq!(rows[0].sdk_iv, Some(0.035));
        assert_eq!(rows[0].sdk_delta, None);
        assert_eq!(rows[0].sdk_gamma, Some(0.012));
        assert!(parsed.delayed);
        assert!(parsed.truncated);
        assert_eq!(
            parsed.spot_as_of.unwrap().timestamp_millis(),
            1_700_000_000_000
        );
    }

    #[test]
    fn rejects_non_success_chain_status() {
        let payload = json!({"status": "FAILED", "underlyingPrice": 100.0});
        assert!(parse_chain_payload(&active(), &payload).is_err());
    }
}