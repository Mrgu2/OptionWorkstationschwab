impl LiveManager {
    pub async fn setup_session(&self, request: LiveSessionRequest) -> anyhow::Result<LiveSnapshot> {
        let _guard = self.setup_guard.lock().await;
        anyhow::ensure!(
            self.auth.lock().await.is_some(),
            "请先连接 Schwab Market Data API"
        );

        let symbol = normalize_us_symbol(&request.symbol)?;
        anyhow::ensure!(
            matches!(request.pricing_mode.as_str(), "mid" | "micro" | "ask"),
            "invalid pricing mode"
        );
        anyhow::ensure!(
            matches!(
                request.dealer_model.as_str(),
                "classic" | "short_all" | "long_all"
            ),
            "invalid dealer model"
        );

        let available = self.expirations(&symbol).await?;
        let today = Utc::now().with_timezone(&New_York).date_naive();
        let available: Vec<_> = available
            .into_iter()
            .filter(|expiration| *expiration >= today)
            .collect();
        anyhow::ensure!(!available.is_empty(), "{symbol} 没有未来期权到期日");

        let selected_expiration = match request.expiration.as_deref() {
            Some(value) => {
                let parsed =
                    NaiveDate::parse_from_str(value, "%Y-%m-%d").context("invalid expiration")?;
                anyhow::ensure!(
                    available.contains(&parsed),
                    "expiration not available: {value}"
                );
                parsed
            }
            None => available[0],
        };

        let expiry_count = request.surface_expiries.clamp(2, 6);
        let mut expirations: Vec<NaiveDate> =
            available.iter().copied().take(expiry_count).collect();
        if !expirations.contains(&selected_expiration) {
            if expirations.len() >= expiry_count {
                expirations.pop();
            }
            expirations.push(selected_expiration);
            expirations.sort();
        }

        let active = ActiveUniverse {
            symbol: symbol.clone(),
            selected_expiration,
            expirations,
            max_contracts: request.max_contracts.clamp(20, COMPAT_CONTRACT_LIMIT),
            moneyness_window: request.moneyness_window.clamp(0.04, 0.30),
            pricing_mode: request.pricing_mode,
            dealer_model: request.dealer_model,
        };

        let snapshot = self.refresh_snapshot(&active, true).await?;
        {
            let mut state = self.state.write().await;
            state.active = Some(active);
            state.snapshot = Some(snapshot.clone());
            state.status.state = "streaming".into();
            state.status.switch_state = "ready".into();
            state.status.active_symbol = Some(symbol);
            state.status.subscribed_contracts = snapshot.feed.subscribed_contracts;
            state.status.last_event_at = Some(snapshot.feed.as_of.clone());
            state.status.error = None;
        }
        self.notify();
        Ok(snapshot)
    }

    pub async fn snapshot(&self) -> anyhow::Result<LiveSnapshot> {
        self.state
            .read()
            .await
            .snapshot
            .clone()
            .ok_or_else(|| anyhow!("尚未建立实时期权会话"))
    }

    async fn refresh_snapshot(
        &self,
        active: &ActiveUniverse,
        include_bars: bool,
    ) -> anyhow::Result<LiveSnapshot> {
        let payload = self.fetch_chain(active).await?;
        let parsed = parse_chain_payload(active, &payload)?;
        let now = Utc::now();
        let as_of = parsed.as_of.unwrap_or(now);
        let spot_age_ms = (now - as_of).num_milliseconds().max(0);
        let mut chains = Vec::new();

        for expiration in &active.expirations {
            let quotes = parsed
                .by_expiration
                .get(expiration)
                .cloned()
                .unwrap_or_default();
            if quotes.is_empty() {
                continue;
            }

            let quote_contracts = quotes
                .iter()
                .filter(|quote| quote.bid > 0.0 || quote.ask > 0.0)
                .count();
            let metadata_contracts = quotes
                .iter()
                .filter(|quote| quote.open_interest > 0 || quote.sdk_iv.is_some())
                .count();
            let total = quotes.len().max(1) as f64;

            chains.push(build_chain(ChainBuild {
                symbol: &active.symbol,
                spot: parsed.spot,
                as_of,
                expiration: *expiration,
                quotes: &quotes,
                pricing_mode: &active.pricing_mode,
                dealer_model: &active.dealer_model,
                risk_free_rate: self.risk_free_rate,
                source: "Schwab",
                quote_interval: "REST refresh",
                oi_frequency: "snapshot",
                prefer_sdk_greeks: true,
                quote_coverage: quote_contracts as f64 / total * 100.0,
                fresh_quote_coverage: quote_contracts as f64 / total * 100.0,
                metadata_coverage: metadata_contracts as f64 / total * 100.0,
                spot_age_ms: Some(spot_age_ms),
            })?);
        }

        let chain = chains
            .iter()
            .find(|chain| chain.expiration == active.selected_expiration.to_string())
            .cloned()
            .ok_or_else(|| anyhow!("selected expiration has no usable Schwab quotes"))?;
        let surface = build_surface(&active.symbol, &chains, as_of);

        let bars = if include_bars {
            self.minute_bars(&active.symbol).await.unwrap_or_default()
        } else {
            self.state
                .read()
                .await
                .snapshot
                .as_ref()
                .filter(|snapshot| snapshot.feed.symbol == active.symbol)
                .map(|snapshot| snapshot.bars.clone())
                .unwrap_or_default()
        };

        let subscribed_contracts = parsed.by_expiration.values().map(Vec::len).sum::<usize>();
        let quote_contracts = parsed
            .by_expiration
            .values()
            .flatten()
            .filter(|quote| quote.bid > 0.0 || quote.ask > 0.0)
            .count();
        let metadata_contracts = parsed
            .by_expiration
            .values()
            .flatten()
            .filter(|quote| quote.open_interest > 0 || quote.sdk_iv.is_some())
            .count();
        let total = subscribed_contracts.max(1) as f64;
        let quote_coverage_pct = quote_contracts as f64 / total * 100.0;
        let metadata_coverage_pct = metadata_contracts as f64 / total * 100.0;
        let quality_state = if quote_coverage_pct < 80.0 {
            "degraded_quotes"
        } else if metadata_coverage_pct < 80.0 {
            "waiting_metadata"
        } else {
            "ready"
        };

        Ok(LiveSnapshot {
            kind: "live_snapshot",
            sequence: self.sequence.load(Ordering::Relaxed) + 1,
            feed: LiveFeedInfo {
                source: "Schwab",
                transport: "Schwab Market Data REST -> local WebSocket",
                sdk_version: API_VERSION,
                symbol: active.symbol.clone(),
                expiration: active.selected_expiration.to_string(),
                expirations: active.expirations.iter().map(ToString::to_string).collect(),
                subscribed_contracts,
                quote_contracts,
                metadata_contracts,
                quote_coverage_pct: round2(quote_coverage_pct),
                fresh_quote_coverage_pct: round2(quote_coverage_pct),
                metadata_coverage_pct: round2(metadata_coverage_pct),
                subscription_limit: COMPAT_CONTRACT_LIMIT,
                as_of: as_of.to_rfc3339(),
                stale_after_ms: self.refresh_ms * 3,
                latency_ms: spot_age_ms,
                quality_state: quality_state.into(),
            },
            bars,
            chain,
            surface,
        })
    }

    async fn expirations(&self, symbol: &str) -> anyhow::Result<Vec<NaiveDate>> {
        let payload = self
            .get_json(
                &format!("{MARKET_BASE}/expirationchain"),
                &[("symbol", symbol)],
            )
            .await
            .context("load Schwab option expiration chain")?;

        let mut dates: Vec<NaiveDate> = payload
            .get("expirationList")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("expirationDate").and_then(Value::as_str))
            .filter_map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
            .collect();
        dates.sort();
        dates.dedup();
        Ok(dates)
    }

    async fn fetch_chain(&self, active: &ActiveUniverse) -> anyhow::Result<Value> {
        let first = active
            .expirations
            .first()
            .copied()
            .unwrap_or(active.selected_expiration)
            .to_string();
        let last = active
            .expirations
            .last()
            .copied()
            .unwrap_or(active.selected_expiration)
            .to_string();
        let strikes = (active.max_contracts / active.expirations.len().max(1) / 2)
            .clamp(10, 100)
            .to_string();

        self.get_json(
            &format!("{MARKET_BASE}/chains"),
            &[
                ("symbol", active.symbol.as_str()),
                ("contractType", "ALL"),
                ("strikeCount", strikes.as_str()),
                ("includeUnderlyingQuote", "true"),
                ("strategy", "SINGLE"),
                ("fromDate", first.as_str()),
                ("toDate", last.as_str()),
            ],
        )
        .await
        .context("load Schwab option chain")
    }

    async fn minute_bars(&self, symbol: &str) -> anyhow::Result<Vec<Bar>> {
        let payload = self
            .get_json(
                &format!("{MARKET_BASE}/pricehistory"),
                &[
                    ("symbol", symbol),
                    ("periodType", "day"),
                    ("period", "1"),
                    ("frequencyType", "minute"),
                    ("frequency", "1"),
                    ("needExtendedHoursData", "false"),
                    ("needPreviousClose", "true"),
                ],
            )
            .await
            .context("load Schwab one minute price history")?;

        let today_et = Utc::now().with_timezone(&New_York).date_naive();
        Ok(payload
            .get("candles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let timestamp = epoch_ms(row.get("datetime")?)?;
                if timestamp.with_timezone(&New_York).date_naive() != today_et {
                    return None;
                }
                let close = number(row.get("close")?)?;
                let et = timestamp.with_timezone(&New_York);
                Some(Bar {
                    time: et.format("%H:%M").to_string(),
                    timestamp: timestamp.to_rfc3339(),
                    open: row.get("open").and_then(number).unwrap_or(close),
                    high: row.get("high").and_then(number).unwrap_or(close),
                    low: row.get("low").and_then(number).unwrap_or(close),
                    close,
                    volume: row.get("volume").and_then(integer).unwrap_or_default(),
                    vwap: close,
                })
            })
            .collect())
    }

    pub async fn daily_closes(&self, count: usize) -> anyhow::Result<Vec<(String, f64)>> {
        let symbol = self
            .state
            .read()
            .await
            .active
            .as_ref()
            .map(|active| active.symbol.clone())
            .ok_or_else(|| anyhow!("尚未建立实时期权会话"))?;

        let payload = self
            .get_json(
                &format!("{MARKET_BASE}/pricehistory"),
                &[
                    ("symbol", symbol.as_str()),
                    ("periodType", "year"),
                    ("period", "1"),
                    ("frequencyType", "daily"),
                    ("frequency", "1"),
                    ("needExtendedHoursData", "false"),
                ],
            )
            .await
            .context("load Schwab daily price history")?;

        let today_et = Utc::now().with_timezone(&New_York).date_naive();
        let mut rows: Vec<(String, f64)> = payload
            .get("candles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let timestamp = epoch_ms(row.get("datetime")?)?;
                let date = timestamp.with_timezone(&New_York).date_naive();
                if date == today_et {
                    return None;
                }
                Some((date.to_string(), number(row.get("close")?)?))
            })
            .collect();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        let keep = count.clamp(21, 120);
        if rows.len() > keep {
            rows.drain(0..rows.len() - keep);
        }
        Ok(rows)
    }

    pub async fn trade_account(&self) -> anyhow::Result<Value> {
        Err(anyhow!(
            "Schwab adapter is market-data only; account APIs are disabled"
        ))
    }

    pub async fn today_orders(&self) -> anyhow::Result<Value> {
        Err(anyhow!(
            "Schwab adapter is market-data only; order APIs are disabled"
        ))
    }

    pub async fn submit_paper_orders(
        &self,
        _orders: &[ExecutionLeg],
        _preview_id: &str,
        _confirmation: &str,
    ) -> anyhow::Result<Value> {
        Err(anyhow!(
            "Schwab adapter is market-data only; order submission is disabled"
        ))
    }

    pub async fn cancel_paper_order(&self, _order_id: &str) -> anyhow::Result<Value> {
        Err(anyhow!(
            "Schwab adapter is market-data only; order cancellation is disabled"
        ))
    }

    async fn get_json(&self, url: &str, query: &[(&str, &str)]) -> anyhow::Result<Value> {
        let token = self.access_token().await?;
        let mut full_url = url.to_string();
        if !query.is_empty() {
            full_url.push('?');
            full_url.push_str(
                &query
                    .iter()
                    .map(|(key, value)| {
                        format!("{}={}", percent_encode(key), percent_encode(value))
                    })
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        }
        curl_json("GET", &full_url, Some(&token), None, &[]).await
    }

    async fn access_token(&self) -> anyhow::Result<String> {
        {
            let auth = self.auth.lock().await;
            let auth = auth
                .as_ref()
                .ok_or_else(|| anyhow!("Schwab is not connected"))?;
            if auth.expires_at - Utc::now() > chrono::Duration::seconds(90) {
                return Ok(auth.access_token.clone());
            }
            if auth.refresh_token.is_none() {
                return Err(anyhow!("Schwab access token 已过期，请重新授权"));
            }
        }

        let _guard = self.auth_refresh.lock().await;
        let snapshot = self
            .auth
            .lock()
            .await
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("Schwab is not connected"))?;
        if snapshot.expires_at - Utc::now() > chrono::Duration::seconds(90) {
            return Ok(snapshot.access_token);
        }

        let refresh_token = snapshot
            .refresh_token
            .clone()
            .ok_or_else(|| anyhow!("Schwab access token 已过期，请重新授权"))?;
        let refreshed = oauth_token_request(
            &snapshot.client_id,
            &snapshot.client_secret,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.as_str()),
            ],
        )
        .await?;

        let mut auth = self.auth.lock().await;
        let auth = auth
            .as_mut()
            .ok_or_else(|| anyhow!("Schwab is not connected"))?;
        auth.access_token = refreshed.access_token;
        auth.refresh_token = refreshed.refresh_token.or(Some(refresh_token));
        auth.expires_at = Utc::now() + chrono::Duration::seconds(refreshed.expires_in.max(60));
        Ok(auth.access_token.clone())
    }
}
