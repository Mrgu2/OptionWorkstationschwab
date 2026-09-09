use std::{
    collections::{BTreeMap, HashMap},
    env,
    process::Command,
    sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}},
    time::Duration,
};

use anyhow::{Context, anyhow};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::{
    analytics::{ChainBuild, build_chain, build_surface},
    models::{Bar, ConnectionStatus, CredentialRequest, LiveFeedInfo, LiveSessionRequest, LiveSnapshot, OAuthStatus, RawOptionQuote},
    strategy::ExecutionLeg,
};

const API_VERSION: &str = "Schwab Market Data API v1";
const OAUTH_AUTHORIZE_URL: &str = "https://api.schwabapi.com/v1/oauth/authorize";
const OAUTH_TOKEN_URL: &str = "https://api.schwabapi.com/v1/oauth/token";
const MARKET_BASE: &str = "https://api.schwabapi.com/marketdata/v1";
const DEFAULT_REDIRECT_URI: &str = "https://127.0.0.1:5556";
const DEFAULT_REFRESH_MS: u64 = 3_000;
const COMPAT_CONTRACT_LIMIT: usize = 500;
const RETRY_MARKER: &str = "schwab_retry_after_ms=";

#[derive(Debug, Clone)]
struct TokenState {
    client_id: String,
    client_secret: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct PendingOAuth {
    flow_id: String,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

#[derive(Debug, Clone)]
struct ActiveUniverse {
    symbol: String,
    selected_expiration: NaiveDate,
    expirations: Vec<NaiveDate>,
    max_contracts: usize,
    moneyness_window: f64,
    pricing_mode: String,
    dealer_model: String,
}

#[derive(Default)]
struct ManagerState {
    active: Option<ActiveUniverse>,
    snapshot: Option<LiveSnapshot>,
    status: ConnectionStatus,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
}

fn default_expires_in() -> i64 { 1_800 }

pub struct LiveManager {
    state: RwLock<ManagerState>,
    auth: Mutex<Option<TokenState>>,
    auth_refresh: Mutex<()>,
    pending_oauth: Mutex<Option<PendingOAuth>>,
    oauth_status: Mutex<OAuthStatus>,
    events: broadcast::Sender<u64>,
    sequence: AtomicU64,
    refresh_started: AtomicBool,
    setup_guard: Mutex<()>,
    risk_free_rate: f64,
    refresh_ms: u64,
}

impl LiveManager {
    pub fn new(risk_free_rate: f64) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let refresh_ms = env::var("OPTION_WORKSTATION_SCHWAB_REFRESH_MS").ok().and_then(|value| value.parse::<u64>().ok()).unwrap_or(DEFAULT_REFRESH_MS).clamp(2_000, 30_000);
        Arc::new(Self {
            state: RwLock::new(ManagerState::default()), auth: Mutex::new(None), auth_refresh: Mutex::new(()), pending_oauth: Mutex::new(None), oauth_status: Mutex::new(OAuthStatus::default()), events,
            sequence: AtomicU64::new(0), refresh_started: AtomicBool::new(false), setup_guard: Mutex::new(()), risk_free_rate, refresh_ms,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<u64> { self.events.subscribe() }
    fn notify(&self) { let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1; let _ = self.events.send(sequence); }
    pub async fn status(&self) -> ConnectionStatus { self.state.read().await.status.clone() }
    pub async fn oauth_status(&self) -> OAuthStatus { self.oauth_status.lock().await.clone() }

    pub async fn start_oauth(self: &Arc<Self>, client_id: String) -> anyhow::Result<OAuthStatus> {
        let client_id = client_id.trim().to_string();
        anyhow::ensure!(!client_id.is_empty() && client_id.len() <= 256, "OAuth App Key 不能为空或长度异常");
        let client_secret = env::var("OPTION_WORKSTATION_SCHWAB_APP_SECRET").ok().filter(|value| !value.trim().is_empty()).ok_or_else(|| anyhow!("Schwab App Secret 未配置。请在 .env 设置 OPTION_WORKSTATION_SCHWAB_APP_SECRET"))?;
        let redirect_uri = env::var("OPTION_WORKSTATION_SCHWAB_REDIRECT_URI").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string());
        anyhow::ensure!(redirect_uri.starts_with("http://") || redirect_uri.starts_with("https://"), "Schwab Redirect URI 格式异常");
        let flow_id = format!("schwab-oauth-{}", Utc::now().timestamp());
        let authorization_url = format!("{OAUTH_AUTHORIZE_URL}?client_id={}&redirect_uri={}", percent_encode(&client_id), percent_encode(&redirect_uri));
        *self.pending_oauth.lock().await = Some(PendingOAuth { flow_id: flow_id.clone(), client_id: client_id.clone(), client_secret, redirect_uri });
        let status = OAuthStatus { status: "pending".into(), flow_id: Some(flow_id), client_id: Some(client_id), authorization_url: Some(authorization_url), error: None };
        *self.oauth_status.lock().await = status.clone();
        Ok(status)
    }

    pub async fn connect(self: &Arc<Self>, credentials: CredentialRequest) -> anyhow::Result<ConnectionStatus> {
        credentials.validate().map_err(|message| anyhow!(message))?;
        let supplied = credentials.access_token.trim();
        if supplied.starts_with("http://") || supplied.starts_with("https://") {
            let pending = self.pending_oauth.lock().await.clone().unwrap_or_else(|| PendingOAuth {
                flow_id: format!("schwab-manual-{}", Utc::now().timestamp()), client_id: credentials.app_key.trim().to_string(),
                client_secret: env::var("OPTION_WORKSTATION_SCHWAB_APP_SECRET").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| credentials.app_secret.trim().to_string()),
                redirect_uri: env::var("OPTION_WORKSTATION_SCHWAB_REDIRECT_URI").ok().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string()),
            });
            anyhow::ensure!(pending.client_id == credentials.app_key.trim(), "OAuth App Key 与待授权会话不一致");
            let code = extract_authorization_code(supplied)?;
            let token = oauth_token_request(&pending.client_id, &pending.client_secret, &[("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", pending.redirect_uri.as_str())]).await?;
            *self.auth.lock().await = Some(TokenState { client_id: pending.client_id.clone(), client_secret: pending.client_secret.clone(), access_token: token.access_token, refresh_token: token.refresh_token, expires_at: Utc::now() + chrono::Duration::seconds(token.expires_in.max(60)) });
            let status = self.validate_connection("oauth").await?;
            *self.pending_oauth.lock().await = None;
            *self.oauth_status.lock().await = OAuthStatus { status: "connected".into(), flow_id: Some(pending.flow_id), client_id: Some(pending.client_id), authorization_url: None, error: None };
            return Ok(status);
        }
        *self.pending_oauth.lock().await = None;
        *self.oauth_status.lock().await = OAuthStatus::default();
        *self.auth.lock().await = Some(TokenState { client_id: credentials.app_key.trim().to_string(), client_secret: credentials.app_secret.trim().to_string(), access_token: supplied.to_string(), refresh_token: None, expires_at: Utc::now() + chrono::Duration::minutes(25) });
        self.validate_connection("access_token").await
    }

    async fn validate_connection(&self, auth_method: &str) -> anyhow::Result<ConnectionStatus> {
        self.get_json(&format!("{MARKET_BASE}/quotes"), &[("symbols", "SPY")]).await.context("Schwab Market Data 权限验证失败")?;
        let status = ConnectionStatus {
            connected: true, state: "connected".into(), auth_method: auth_method.into(), account_hint: Some("Schwab Data".into()), quote_level: Some("Schwab Market Data".into()), packages: vec!["Market Data".into()], subscribed_contracts: 0,
            last_event_at: None, last_snapshot_at: None, last_snapshot_sequence: None, latency_ms: None, stale_after_ms: self.refresh_ms * 3, reconnect_count: 0, active_symbol: None, switch_state: "idle".into(), error: None,
            credential_storage: "process_memory_only", trade_connected: false, paper_account: false, account_type: None, buy_power: None, order_execution_enabled: false,
        };
        let mut state = self.state.write().await; state.active = None; state.snapshot = None; state.status = status.clone(); Ok(status)
    }

    pub async fn disconnect(&self) -> ConnectionStatus {
        *self.auth.lock().await = None; *self.pending_oauth.lock().await = None; *self.oauth_status.lock().await = OAuthStatus::default();
        let mut state = self.state.write().await; *state = ManagerState::default(); self.notify(); state.status.clone()
    }

    pub fn start_refresh_loop(self: &Arc<Self>) {
        if self.refresh_started.swap(true, Ordering::AcqRel) { return; }
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(manager.refresh_ms)); interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Some(active) = manager.state.read().await.active.clone() {
                    match manager.refresh_snapshot(&active, false).await {
                        Ok(snapshot) => { let mut state = manager.state.write().await; state.status.last_event_at = Some(snapshot.feed.as_of.clone()); state.status.error = None; state.snapshot = Some(snapshot); drop(state); manager.notify(); }
                        Err(error) => manager.state.write().await.status.error = Some(format!("{error:#}")),
                    }
                }
            }
        });
    }

    pub async fn setup_session(&self, request: LiveSessionRequest) -> anyhow::Result<LiveSnapshot> {
        let _guard = self.setup_guard.lock().await;
        anyhow::ensure!(self.auth.lock().await.is_some(), "请先连接 Schwab Market Data API");
        let symbol = normalize_us_symbol(&request.symbol)?;
        anyhow::ensure!(matches!(request.pricing_mode.as_str(), "mid" | "micro" | "ask"), "invalid pricing mode");
        anyhow::ensure!(matches!(request.dealer_model.as_str(), "classic" | "short_all" | "long_all"), "invalid dealer model");
        let available = self.expirations(&symbol).await?; let today = Utc::now().with_timezone(&New_York).date_naive();
        let available: Vec<_> = available.into_iter().filter(|expiration| *expiration >= today).collect(); anyhow::ensure!(!available.is_empty(), "{symbol} 没有未来期权到期日");
        let selected_expiration = match request.expiration.as_deref() { Some(value) => { let parsed = NaiveDate::parse_from_str(value, "%Y-%m-%d").context("invalid expiration")?; anyhow::ensure!(available.contains(&parsed), "expiration not available: {value}"); parsed }, None => available[0] };
        let expiry_count = request.surface_expiries.clamp(2, 6); let mut expirations: Vec<NaiveDate> = available.iter().copied().take(expiry_count).collect();
        if !expirations.contains(&selected_expiration) { if expirations.len() >= expiry_count { expirations.pop(); } expirations.push(selected_expiration); expirations.sort(); }
        let active = ActiveUniverse { symbol: symbol.clone(), selected_expiration, expirations, max_contracts: request.max_contracts.clamp(20, COMPAT_CONTRACT_LIMIT), moneyness_window: request.moneyness_window.clamp(0.04, 0.30), pricing_mode: request.pricing_mode, dealer_model: request.dealer_model };
        let snapshot = self.refresh_snapshot(&active, true).await?;
        { let mut state = self.state.write().await; state.active = Some(active); state.snapshot = Some(snapshot.clone()); state.status.state = "streaming".into(); state.status.switch_state = "ready".into(); state.status.active_symbol = Some(symbol); state.status.subscribed_contracts = snapshot.feed.subscribed_contracts; state.status.last_event_at = Some(snapshot.feed.as_of.clone()); state.status.error = None; }
        self.notify(); Ok(snapshot)
    }

    pub async fn snapshot(&self) -> anyhow::Result<LiveSnapshot> { self.state.read().await.snapshot.clone().ok_or_else(|| anyhow!("尚未建立实时期权会话")) }

    async fn refresh_snapshot(&self, active: &ActiveUniverse, include_bars: bool) -> anyhow::Result<LiveSnapshot> {
        let payload = self.fetch_chain(active).await?; let parsed = parse_chain_payload(active, &payload)?; let now = Utc::now(); let as_of = parsed.as_of.unwrap_or(now); let spot_age_ms = (now - as_of).num_milliseconds().max(0); let mut chains = Vec::new();
        for expiration in &active.expirations {
            let quotes = parsed.by_expiration.get(expiration).cloned().unwrap_or_default(); if quotes.is_empty() { continue; }
            let quote_contracts = quotes.iter().filter(|quote| quote.bid > 0.0 || quote.ask > 0.0).count(); let metadata_contracts = quotes.iter().filter(|quote| quote.open_interest > 0 || quote.sdk_iv.is_some()).count(); let total = quotes.len().max(1) as f64;
            chains.push(build_chain(ChainBuild { symbol: &active.symbol, spot: parsed.spot, as_of, expiration: *expiration, quotes: &quotes, pricing_mode: &active.pricing_mode, dealer_model: &active.dealer_model, risk_free_rate: self.risk_free_rate, source: "Schwab", quote_interval: "REST refresh", oi_frequency: "snapshot", prefer_sdk_greeks: true, quote_coverage: quote_contracts as f64 / total * 100.0, fresh_quote_coverage: quote_contracts as f64 / total * 100.0, metadata_coverage: metadata_contracts as f64 / total * 100.0, spot_age_ms: Some(spot_age_ms) })?);
        }
        let chain = chains.iter().find(|chain| chain.expiration == active.selected_expiration.to_string()).cloned().ok_or_else(|| anyhow!("selected expiration has no usable Schwab quotes"))?; let surface = build_surface(&active.symbol, &chains, as_of);
        let bars = if include_bars { self.minute_bars(&active.symbol).await.unwrap_or_default() } else { self.state.read().await.snapshot.as_ref().filter(|snapshot| snapshot.feed.symbol == active.symbol).map(|snapshot| snapshot.bars.clone()).unwrap_or_default() };
        let subscribed_contracts = parsed.by_expiration.values().map(Vec::len).sum::<usize>(); let quote_contracts = parsed.by_expiration.values().flatten().filter(|quote| quote.bid > 0.0 || quote.ask > 0.0).count(); let metadata_contracts = parsed.by_expiration.values().flatten().filter(|quote| quote.open_interest > 0 || quote.sdk_iv.is_some()).count(); let total = subscribed_contracts.max(1) as f64;
        let quote_coverage_pct = quote_contracts as f64 / total * 100.0; let metadata_coverage_pct = metadata_contracts as f64 / total * 100.0; let quality_state = if quote_coverage_pct < 80.0 { "degraded_quotes" } else if metadata_coverage_pct < 80.0 { "waiting_metadata" } else { "ready" };
        Ok(LiveSnapshot { kind: "live_snapshot", sequence: self.sequence.load(Ordering::Relaxed) + 1, feed: LiveFeedInfo { source: "Schwab", transport: "Schwab Market Data REST -> local WebSocket", sdk_version: API_VERSION, symbol: active.symbol.clone(), expiration: active.selected_expiration.to_string(), expirations: active.expirations.iter().map(ToString::to_string).collect(), subscribed_contracts, quote_contracts, metadata_contracts, quote_coverage_pct: round2(quote_coverage_pct), fresh_quote_coverage_pct: round2(quote_coverage_pct), metadata_coverage_pct: round2(metadata_coverage_pct), subscription_limit: COMPAT_CONTRACT_LIMIT, as_of: as_of.to_rfc3339(), stale_after_ms: self.refresh_ms * 3, latency_ms: spot_age_ms, quality_state: quality_state.into() }, bars, chain, surface })
    }

    async fn expirations(&self, symbol: &str) -> anyhow::Result<Vec<NaiveDate>> {
        let payload = self.get_json(&format!("{MARKET_BASE}/expirationchain"), &[("symbol", symbol)]).await.context("load Schwab option expiration chain")?;
        let mut dates: Vec<NaiveDate> = payload.get("expirationList").and_then(Value::as_array).into_iter().flatten().filter_map(|row| row.get("expirationDate").and_then(Value::as_str)).filter_map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()).collect(); dates.sort(); dates.dedup(); Ok(dates)
    }

    async fn fetch_chain(&self, active: &ActiveUniverse) -> anyhow::Result<Value> {
        let first = active.expirations.first().copied().unwrap_or(active.selected_expiration).to_string(); let last = active.expirations.last().copied().unwrap_or(active.selected_expiration).to_string(); let strikes = (active.max_contracts / active.expirations.len().max(1) / 2).clamp(10, 100).to_string();
        self.get_json(&format!("{MARKET_BASE}/chains"), &[("symbol", active.symbol.as_str()), ("contractType", "ALL"), ("strikeCount", strikes.as_str()), ("includeUnderlyingQuote", "true"), ("strategy", "SINGLE"), ("fromDate", first.as_str()), ("toDate", last.as_str())]).await.context("load Schwab option chain")
    }

    async fn minute_bars(&self, symbol: &str) -> anyhow::Result<Vec<Bar>> {
        let payload = self.get_json(&format!("{MARKET_BASE}/pricehistory"), &[("symbol", symbol), ("periodType", "day"), ("period", "1"), ("frequencyType", "minute"), ("frequency", "1"), ("needExtendedHoursData", "false"), ("needPreviousClose", "true")]).await.context("load Schwab one minute price history")?;
        let today_et = Utc::now().with_timezone(&New_York).date_naive();
        Ok(payload.get("candles").and_then(Value::as_array).into_iter().flatten().filter_map(|row| { let timestamp = epoch_ms(row.get("datetime")?)?; if timestamp.with_timezone(&New_York).date_naive() != today_et { return None; } let close = number(row.get("close")?)?; let et = timestamp.with_timezone(&New_York); Some(Bar { time: et.format("%H:%M").to_string(), timestamp: timestamp.to_rfc3339(), open: row.get("open").and_then(number).unwrap_or(close), high: row.get("high").and_then(number).unwrap_or(close), low: row.get("low").and_then(number).unwrap_or(close), close, volume: row.get("volume").and_then(integer).unwrap_or_default(), vwap: close }) }).collect())
    }

    pub async fn daily_closes(&self, count: usize) -> anyhow::Result<Vec<(String, f64)>> {
        let symbol = self.state.read().await.active.as_ref().map(|active| active.symbol.clone()).ok_or_else(|| anyhow!("尚未建立实时期权会话"))?;
        let payload = self.get_json(&format!("{MARKET_BASE}/pricehistory"), &[("symbol", symbol.as_str()), ("periodType", "year"), ("period", "1"), ("frequencyType", "daily"), ("frequency", "1"), ("needExtendedHoursData", "false")]).await.context("load Schwab daily price history")?;
        let today_et = Utc::now().with_timezone(&New_York).date_naive(); let mut rows: Vec<(String, f64)> = payload.get("candles").and_then(Value::as_array).into_iter().flatten().filter_map(|row| { let timestamp = epoch_ms(row.get("datetime")?)?; let date = timestamp.with_timezone(&New_York).date_naive(); if date == today_et { return None; } Some((date.to_string(), number(row.get("close")?)?)) }).collect(); rows.sort_by(|left, right| left.0.cmp(&right.0)); let keep = count.clamp(21, 120); if rows.len() > keep { rows.drain(0..rows.len() - keep); } Ok(rows)
    }

    pub async fn trade_account(&self) -> anyhow::Result<Value> { Err(anyhow!("Schwab adapter is market-data only; account APIs are disabled")) }
    pub async fn today_orders(&self) -> anyhow::Result<Value> { Err(anyhow!("Schwab adapter is market-data only; order APIs are disabled")) }
    pub async fn submit_paper_orders(&self, _orders: &[ExecutionLeg], _preview_id: &str, _confirmation: &str) -> anyhow::Result<Value> { Err(anyhow!("Schwab adapter is market-data only; order submission is disabled")) }
    pub async fn cancel_paper_order(&self, _order_id: &str) -> anyhow::Result<Value> { Err(anyhow!("Schwab adapter is market-data only; order cancellation is disabled")) }

    async fn get_json(&self, url: &str, query: &[(&str, &str)]) -> anyhow::Result<Value> {
        let token = self.access_token().await?; let mut full_url = url.to_string(); if !query.is_empty() { full_url.push('?'); full_url.push_str(&query.iter().map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value))).collect::<Vec<_>>().join("&")); }
        curl_json("GET", &full_url, Some(&token), None, &[]).await
    }

    async fn access_token(&self) -> anyhow::Result<String> {
        let refresh_needed = { let auth = self.auth.lock().await; let auth = auth.as_ref().ok_or_else(|| anyhow!("Schwab is not connected"))?; if auth.expires_at - Utc::now() > chrono::Duration::seconds(90) { return Ok(auth.access_token.clone()); } auth.refresh_token.is_some() };
        if !refresh_needed { return self.auth.lock().await.as_ref().map(|auth| auth.access_token.clone()).ok_or_else(|| anyhow!("Schwab access token 已过期，请重新授权")); }
        let _guard = self.auth_refresh.lock().await; let snapshot = self.auth.lock().await.as_ref().cloned().ok_or_else(|| anyhow!("Schwab is not connected"))?; if snapshot.expires_at - Utc::now() > chrono::Duration::seconds(90) { return Ok(snapshot.access_token); }
        let refresh_token = snapshot.refresh_token.clone().ok_or_else(|| anyhow!("Schwab access token 已过期，请重新授权"))?; let refreshed = oauth_token_request(&snapshot.client_id, &snapshot.client_secret, &[("grant_type", "refresh_token"), ("refresh_token", refresh_token.as_str())]).await?;
        let mut auth = self.auth.lock().await; let auth = auth.as_mut().ok_or_else(|| anyhow!("Schwab is not connected"))?; auth.access_token = refreshed.access_token; auth.refresh_token = refreshed.refresh_token.or(Some(refresh_token)); auth.expires_at = Utc::now() + chrono::Duration::seconds(refreshed.expires_in.max(60)); Ok(auth.access_token.clone())
    }
}

#[derive(Debug)]
struct ParsedChain { spot: f64, as_of: Option<DateTime<Utc>>, by_expiration: BTreeMap<NaiveDate, Vec<RawOptionQuote>> }

fn parse_chain_payload(active: &ActiveUniverse, payload: &Value) -> anyhow::Result<ParsedChain> {
    let spot = payload.get("underlyingPrice").and_then(number).or_else(|| payload.pointer("/underlying/last").and_then(number)).or_else(|| payload.pointer("/underlying/lastPrice").and_then(number)).or_else(|| payload.pointer("/underlying/quote/lastPrice").and_then(number)).ok_or_else(|| anyhow!("Schwab option chain is missing underlying price"))?;
    anyhow::ensure!(spot.is_finite() && spot > 0.0, "invalid Schwab underlying price"); let selected: HashMap<NaiveDate, ()> = active.expirations.iter().copied().map(|date| (date, ())).collect(); let mut by_expiration = BTreeMap::new(); let mut as_of = None;
    collect_side(payload.get("callExpDateMap"), "CALL", spot, active.moneyness_window, &selected, &mut by_expiration, &mut as_of); collect_side(payload.get("putExpDateMap"), "PUT", spot, active.moneyness_window, &selected, &mut by_expiration, &mut as_of);
    let total: usize = by_expiration.values().map(Vec::len).sum(); if total > active.max_contracts { let per_expiry = (active.max_contracts / active.expirations.len().max(1)).max(2); for rows in by_expiration.values_mut() { if rows.len() > per_expiry { trim_around_spot(rows, spot, per_expiry); } } }
    anyhow::ensure!(by_expiration.values().any(|rows| !rows.is_empty()), "Schwab option chain returned no contracts inside the configured window"); Ok(ParsedChain { spot, as_of, by_expiration })
}

#[allow(clippy::too_many_arguments)]
fn collect_side(map: Option<&Value>, right: &str, spot: f64, window: f64, selected: &HashMap<NaiveDate, ()>, target: &mut BTreeMap<NaiveDate, Vec<RawOptionQuote>>, as_of: &mut Option<DateTime<Utc>>) {
    let Some(expirations) = map.and_then(Value::as_object) else { return; }; for (expiration_key, strikes) in expirations { let date_text = expiration_key.split(':').next().unwrap_or(expiration_key); let Ok(expiration) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else { continue; }; if !selected.contains_key(&expiration) { continue; } let Some(strikes) = strikes.as_object() else { continue; };
        for (strike_key, contracts) in strikes { let Some(contract) = contracts.as_array().and_then(|rows| rows.first()) else { continue; }; let strike = contract.get("strikePrice").and_then(number).or_else(|| strike_key.parse::<f64>().ok()).unwrap_or_default(); if strike <= 0.0 || (strike / spot - 1.0).abs() > window { continue; } let symbol = contract.get("symbol").and_then(Value::as_str).unwrap_or_default().to_string(); if symbol.is_empty() { continue; }
            let quote_time = ["quoteTimeInLong", "tradeTimeInLong"].into_iter().find_map(|key| contract.get(key).and_then(epoch_ms)); if let Some(timestamp) = quote_time { if as_of.is_none() || as_of.is_some_and(|current| timestamp > current) { *as_of = Some(timestamp); } }
            target.entry(expiration).or_default().push(RawOptionQuote { symbol, strike, right: right.into(), bid_size: contract.get("bidSize").and_then(integer).unwrap_or_default(), ask_size: contract.get("askSize").and_then(integer).unwrap_or_default(), bid: contract.get("bid").and_then(number).unwrap_or_default(), ask: contract.get("ask").and_then(number).unwrap_or_default(), last: contract.get("last").and_then(number), volume: contract.get("totalVolume").or_else(|| contract.get("volume")).and_then(integer).unwrap_or_default(), open_interest: contract.get("openInterest").and_then(integer).unwrap_or_default(), sdk_iv: contract.get("volatility").and_then(number).and_then(normalize_iv), sdk_delta: contract.get("delta").and_then(number), sdk_gamma: contract.get("gamma").and_then(number), sdk_theta: contract.get("theta").and_then(number), sdk_vega: contract.get("vega").and_then(number) });
        }
    }
}

fn trim_around_spot(rows: &mut Vec<RawOptionQuote>, spot: f64, limit: usize) { rows.sort_by(|left, right| (left.strike - spot).abs().total_cmp(&(right.strike - spot).abs())); rows.truncate(limit); rows.sort_by(|left, right| left.strike.total_cmp(&right.strike).then_with(|| left.right.cmp(&right.right))); }

async fn oauth_token_request(client_id: &str, client_secret: &str, form: &[(&str, &str)]) -> anyhow::Result<TokenResponse> { let basic = format!("{client_id}:{client_secret}"); let value = curl_json("POST", OAUTH_TOKEN_URL, None, Some(&basic), form).await?; serde_json::from_value(value).context("decode Schwab OAuth token response") }

async fn curl_json(method: &str, url: &str, bearer: Option<&str>, basic: Option<&str>, form: &[(&str, &str)]) -> anyhow::Result<Value> {
    let method = method.to_string(); let url = url.to_string(); let bearer = bearer.map(str::to_string); let basic = basic.map(str::to_string); let form: Vec<(String, String)> = form.iter().map(|(key, value)| ((*key).to_string(), (*value).to_string())).collect();
    tokio::task::spawn_blocking(move || { let mut command = Command::new("curl"); command.args(["--silent", "--show-error", "--location", "--connect-timeout", "5", "--max-time", "15", "--request", &method]); if let Some(token) = bearer { command.args(["--header", &format!("Authorization: Bearer {token}")]); } if let Some(credentials) = basic { command.args(["--user", &credentials]); } for (key, value) in form { command.args(["--data-urlencode", &format!("{key}={value}")]); } command.args(["--write-out", "\n%{http_code}", &url]); let output = command.output().context("run curl for Schwab API")?; anyhow::ensure!(output.status.success(), "curl failed: {}", String::from_utf8_lossy(&output.stderr).trim()); let stdout = String::from_utf8(output.stdout).context("Schwab API returned non UTF-8 response")?; let (body, status) = stdout.rsplit_once('\n').ok_or_else(|| anyhow!("Schwab API response missing HTTP status"))?; let status: u16 = status.trim().parse().context("parse Schwab HTTP status")?; if status == 429 { return Err(anyhow!("{RETRY_MARKER}5000; Schwab API rate limit reached")); } anyhow::ensure!((200..300).contains(&status), "Schwab API {status}: {}", compact_error(body)); if body.trim().is_empty() { Ok(Value::Null) } else { serde_json::from_str(body).context("decode Schwab JSON response") } }).await.context("join Schwab curl task")?
}

fn extract_authorization_code(value: &str) -> anyhow::Result<String> { let clean = value.trim(); if !(clean.starts_with("http://") || clean.starts_with("https://")) { anyhow::ensure!(!clean.is_empty() && clean.len() <= 4096, "authorization code 长度异常"); return Ok(clean.to_string()); } let query = clean.split_once('?').map(|(_, query)| query).unwrap_or_default().split('#').next().unwrap_or_default(); for pair in query.split('&') { let (key, value) = pair.split_once('=').unwrap_or((pair, "")); if key == "error" { return Err(anyhow!("Schwab OAuth 返回错误: {}", percent_decode(value))); } if key == "code" { return Ok(percent_decode(value)); } } Err(anyhow!("回调 URL 中没有 code 参数")) }
fn normalize_us_symbol(value: &str) -> anyhow::Result<String> { let clean = value.trim().to_uppercase().trim_end_matches(".US").to_string(); anyhow::ensure!(!clean.is_empty() && clean.len() <= 20 && clean.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-')), "invalid symbol"); Ok(clean) }
fn normalize_iv(value: f64) -> Option<f64> { let normalized = if value > 4.0 { value / 100.0 } else { value }; (0.001..=4.0).contains(&normalized).then_some(normalized) }
fn number(value: &Value) -> Option<f64> { value.as_f64().or_else(|| value.as_i64().map(|value| value as f64)).or_else(|| value.as_u64().map(|value| value as f64)).or_else(|| value.as_str().and_then(|value| value.parse().ok())).filter(|value| value.is_finite()) }
fn integer(value: &Value) -> Option<i64> { value.as_i64().or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok())).or_else(|| value.as_f64().map(|value| value.round() as i64)).or_else(|| value.as_str().and_then(|value| value.parse().ok())) }
fn epoch_ms(value: &Value) -> Option<DateTime<Utc>> { Utc.timestamp_millis_opt(integer(value)?).single() }
fn round2(value: f64) -> f64 { (value * 100.0).round() / 100.0 }
fn compact_error(value: &str) -> String { let compact = value.split_whitespace().collect::<Vec<_>>().join(" "); if compact.len() > 500 { format!("{}…", &compact[..500]) } else if compact.is_empty() { "empty response".into() } else { compact } }
fn percent_encode(value: &str) -> String { value.bytes().map(|byte| if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') { (byte as char).to_string() } else { format!("%{byte:02X}") }).collect() }
fn percent_decode(value: &str) -> String { let bytes = value.as_bytes(); let mut output = Vec::with_capacity(bytes.len()); let mut index = 0; while index < bytes.len() { if bytes[index] == b'%' && index + 2 < bytes.len() { if let Ok(hex) = u8::from_str_radix(&value[index + 1..index + 3], 16) { output.push(hex); index += 3; continue; } } output.push(if bytes[index] == b'+' { b' ' } else { bytes[index] }); index += 1; } String::from_utf8_lossy(&output).into_owned() }
pub fn option_retry_after_ms(detail: &str) -> Option<u64> { let start = detail.find(RETRY_MARKER)? + RETRY_MARKER.len(); let digits: String = detail[start..].chars().take_while(char::is_ascii_digit).collect(); digits.parse().ok() }

#[cfg(test)]
mod tests {
    use super::{extract_authorization_code, normalize_iv, normalize_us_symbol, percent_encode};
    #[test] fn extracts_code_from_callback_url() { assert_eq!(extract_authorization_code("https://127.0.0.1:5556/?code=abc%20123").unwrap(), "abc 123"); }
    #[test] fn normalizes_symbols_for_schwab() { assert_eq!(normalize_us_symbol("spy.us").unwrap(), "SPY"); }
    #[test] fn normalizes_percent_iv() { assert_eq!(normalize_iv(25.0), Some(0.25)); }
    #[test] fn encodes_redirect_uri() { assert!(percent_encode("https://127.0.0.1:5556").contains("%3A%2F%2F")); }
}
