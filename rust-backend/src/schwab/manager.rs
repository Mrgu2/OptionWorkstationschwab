impl LiveManager {
    pub fn new(risk_free_rate: f64) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let refresh_ms = env::var("OPTION_WORKSTATION_SCHWAB_REFRESH_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_REFRESH_MS)
            .clamp(2_000, 30_000);

        Arc::new(Self {
            state: RwLock::new(ManagerState::default()),
            auth: Mutex::new(None),
            auth_refresh: Mutex::new(()),
            pending_oauth: Mutex::new(None),
            oauth_status: Mutex::new(OAuthStatus::default()),
            events,
            sequence: AtomicU64::new(0),
            refresh_started: AtomicBool::new(false),
            setup_guard: Mutex::new(()),
            risk_free_rate,
            refresh_ms,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.events.subscribe()
    }

    fn notify(&self) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.events.send(sequence);
    }

    pub async fn status(&self) -> ConnectionStatus {
        self.state.read().await.status.clone()
    }

    pub async fn oauth_status(&self) -> OAuthStatus {
        self.oauth_status.lock().await.clone()
    }

    pub async fn start_oauth(
        self: &Arc<Self>,
        client_id: String,
    ) -> anyhow::Result<OAuthStatus> {
        let client_id = client_id.trim().to_string();
        anyhow::ensure!(
            !client_id.is_empty() && client_id.len() <= 256,
            "OAuth App Key 不能为空或长度异常"
        );

        let client_secret = env::var("OPTION_WORKSTATION_SCHWAB_APP_SECRET")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "Schwab App Secret 未配置。请在 .env 设置 OPTION_WORKSTATION_SCHWAB_APP_SECRET"
                )
            })?;
        let redirect_uri = env::var("OPTION_WORKSTATION_SCHWAB_REDIRECT_URI")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string());
        anyhow::ensure!(
            redirect_uri.starts_with("http://") || redirect_uri.starts_with("https://"),
            "Schwab Redirect URI 格式异常"
        );

        let flow_id = format!("schwab-oauth-{}", Utc::now().timestamp_millis());
        let authorization_url = format!(
            "{OAUTH_AUTHORIZE_URL}?client_id={}&redirect_uri={}",
            percent_encode(&client_id),
            percent_encode(&redirect_uri)
        );

        *self.pending_oauth.lock().await = Some(PendingOAuth {
            flow_id: flow_id.clone(),
            client_id: client_id.clone(),
            client_secret,
            redirect_uri,
        });

        let status = OAuthStatus {
            status: "pending".into(),
            flow_id: Some(flow_id),
            client_id: Some(client_id),
            authorization_url: Some(authorization_url),
            error: None,
        };
        *self.oauth_status.lock().await = status.clone();
        Ok(status)
    }

    pub async fn connect(
        self: &Arc<Self>,
        credentials: CredentialRequest,
    ) -> anyhow::Result<ConnectionStatus> {
        credentials.validate().map_err(|message| anyhow!(message))?;
        let supplied = credentials.access_token.trim();

        if supplied.starts_with("http://") || supplied.starts_with("https://") {
            let pending = self
                .pending_oauth
                .lock()
                .await
                .clone()
                .ok_or_else(|| anyhow!("Schwab OAuth 会话已丢失，请重新点击开始授权"))?;

            anyhow::ensure!(
                pending.client_id == credentials.app_key.trim(),
                "OAuth App Key 与待授权会话不一致"
            );
            let code = extract_authorization_code(supplied)?;
            let token = oauth_token_request(
                &pending.client_id,
                &pending.client_secret,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code.as_str()),
                    ("redirect_uri", pending.redirect_uri.as_str()),
                ],
            )
            .await?;

            let candidate = TokenState {
                client_id: pending.client_id.clone(),
                client_secret: pending.client_secret.clone(),
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                expires_at: Utc::now() + chrono::Duration::seconds(token.expires_in.max(60)),
            };

            match self.install_auth_and_validate(candidate, "oauth").await {
                Ok(status) => {
                    *self.pending_oauth.lock().await = None;
                    *self.oauth_status.lock().await = OAuthStatus {
                        status: "connected".into(),
                        flow_id: Some(pending.flow_id),
                        client_id: Some(pending.client_id),
                        authorization_url: None,
                        error: None,
                    };
                    return Ok(status);
                }
                Err(error) => {
                    let detail = format!("{error:#}");
                    *self.pending_oauth.lock().await = None;
                    *self.oauth_status.lock().await = OAuthStatus {
                        status: "error".into(),
                        flow_id: Some(pending.flow_id),
                        client_id: Some(pending.client_id),
                        authorization_url: None,
                        error: Some(detail.clone()),
                    };
                    return Err(anyhow!(detail));
                }
            }
        }

        *self.pending_oauth.lock().await = None;
        *self.oauth_status.lock().await = OAuthStatus::default();
        let candidate = TokenState {
            client_id: credentials.app_key.trim().to_string(),
            client_secret: credentials.app_secret.trim().to_string(),
            access_token: supplied.to_string(),
            refresh_token: None,
            expires_at: Utc::now() + chrono::Duration::minutes(25),
        };
        self.install_auth_and_validate(candidate, "access_token")
            .await
    }

    async fn install_auth_and_validate(
        &self,
        candidate: TokenState,
        auth_method: &str,
    ) -> anyhow::Result<ConnectionStatus> {
        let previous = self.auth.lock().await.replace(candidate);
        match self.validate_connection(auth_method).await {
            Ok(status) => Ok(status),
            Err(error) => {
                *self.auth.lock().await = previous;
                Err(error)
            }
        }
    }

    async fn validate_connection(&self, auth_method: &str) -> anyhow::Result<ConnectionStatus> {
        self.get_json(
            &format!("{MARKET_BASE}/quotes"),
            &[("symbols", "SPY")],
        )
        .await
        .context("Schwab Market Data 权限验证失败")?;

        let status = ConnectionStatus {
            connected: true,
            state: "connected".into(),
            auth_method: auth_method.into(),
            account_hint: Some("Schwab Data".into()),
            quote_level: Some("Schwab Market Data".into()),
            packages: vec!["Market Data".into()],
            subscribed_contracts: 0,
            last_event_at: None,
            last_snapshot_at: None,
            last_snapshot_sequence: None,
            latency_ms: None,
            stale_after_ms: self.refresh_ms * 3,
            reconnect_count: 0,
            active_symbol: None,
            switch_state: "idle".into(),
            error: None,
            credential_storage: "process_memory_only",
            trade_connected: false,
            paper_account: false,
            account_type: None,
            buy_power: None,
            order_execution_enabled: false,
        };

        let mut state = self.state.write().await;
        state.active = None;
        state.snapshot = None;
        state.status = status.clone();
        Ok(status)
    }

    pub async fn disconnect(&self) -> ConnectionStatus {
        *self.auth.lock().await = None;
        *self.pending_oauth.lock().await = None;
        *self.oauth_status.lock().await = OAuthStatus::default();

        let mut state = self.state.write().await;
        *state = ManagerState::default();
        self.notify();
        state.status.clone()
    }

    pub fn start_refresh_loop(self: &Arc<Self>) {
        if self.refresh_started.swap(true, Ordering::AcqRel) {
            return;
        }

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(manager.refresh_ms));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;
                let active = { manager.state.read().await.active.clone() };
                if let Some(active) = active {
                    match manager.refresh_snapshot(&active, false).await {
                        Ok(snapshot) => {
                            let mut state = manager.state.write().await;
                            if state.active.as_ref() != Some(&active) {
                                continue;
                            }
                            state.status.last_event_at = Some(snapshot.feed.as_of.clone());
                            state.status.last_snapshot_at = Some(snapshot.chain.timestamp.clone());
                            state.status.last_snapshot_sequence = Some(snapshot.sequence);
                            state.status.latency_ms = Some(snapshot.feed.latency_ms);
                            state.status.error = None;
                            state.snapshot = Some(snapshot);
                            drop(state);
                            manager.notify();
                        }
                        Err(error) => {
                            let mut state = manager.state.write().await;
                            if state.active.as_ref() == Some(&active) {
                                state.status.error = Some(format!("{error:#}"));
                            }
                        }
                    }
                }
            }
        });
    }
}