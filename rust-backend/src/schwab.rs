use std::{
    collections::{BTreeMap, HashMap},
    env,
    io::Write,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{anyhow, Context};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::{
    analytics::{build_chain, build_surface, ChainBuild},
    models::{
        Bar, ConnectionStatus, CredentialRequest, LiveFeedInfo, LiveSessionRequest, LiveSnapshot,
        OAuthStatus, RawOptionQuote,
    },
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

#[derive(Debug, Clone, PartialEq)]
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

fn default_expires_in() -> i64 {
    1_800
}

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

include!("schwab/manager.rs");
include!("schwab/market.rs");
include!("schwab/parse.rs");
include!("schwab/http.rs");