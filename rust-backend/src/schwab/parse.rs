#[derive(Debug)]
struct ParsedChain {
    spot: f64,
    as_of: Option<DateTime<Utc>>,
    by_expiration: BTreeMap<NaiveDate, Vec<RawOptionQuote>>,
}

fn parse_chain_payload(active: &ActiveUniverse, payload: &Value) -> anyhow::Result<ParsedChain> {
    let spot = payload
        .get("underlyingPrice")
        .and_then(number)
        .or_else(|| payload.pointer("/underlying/last").and_then(number))
        .or_else(|| payload.pointer("/underlying/lastPrice").and_then(number))
        .or_else(|| payload.pointer("/underlying/quote/lastPrice").and_then(number))
        .ok_or_else(|| anyhow!("Schwab option chain is missing underlying price"))?;
    anyhow::ensure!(spot.is_finite() && spot > 0.0, "invalid Schwab underlying price");

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
        "Schwab option chain returned no contracts inside the configured window"
    );

    Ok(ParsedChain {
        spot,
        as_of,
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
            let Some(contract) = contracts.as_array().and_then(|rows| rows.first()) else {
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
                    sdk_delta: contract.get("delta").and_then(number),
                    sdk_gamma: contract.get("gamma").and_then(number),
                    sdk_theta: contract.get("theta").and_then(number),
                    sdk_vega: contract.get("vega").and_then(number),
                });
        }
    }
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
