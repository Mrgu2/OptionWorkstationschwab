use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Deserialize)]
pub struct AuditCaptureRequest {
    pub kind: String,
    pub mode: String,
    pub symbol: String,
    pub snapshot_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: String,
    pub created_at: String,
    pub kind: String,
    pub mode: String,
    pub symbol: String,
    pub snapshot_id: Option<String>,
    pub previous_hash: Option<String>,
    pub record_hash: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditSummary {
    pub id: String,
    pub created_at: String,
    pub kind: String,
    pub mode: String,
    pub symbol: String,
    pub snapshot_id: Option<String>,
    pub record_hash: String,
}

pub struct AuditStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl AuditStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn append(&self, request: AuditCaptureRequest) -> anyhow::Result<AuditRecord> {
        validate_request(&request)?;
        let _guard = self.lock.lock().await;
        self.append_unlocked(request)
    }

    pub async fn append_holdout_seal_once(
        &self,
        request: AuditCaptureRequest,
        commitment: &str,
    ) -> anyhow::Result<AuditRecord> {
        validate_request(&request)?;
        anyhow::ensure!(
            request.kind == "holdout_seal",
            "expected holdout_seal audit kind"
        );
        let _guard = self.lock.lock().await;
        let records = read_records(&self.path)?;
        let opened = opened_holdout_commitments(&records);
        let active_for_symbol = records.iter().find(|record| {
            record.kind == "holdout_seal"
                && record.symbol.eq_ignore_ascii_case(&request.symbol)
                && record
                    .payload
                    .get("commitment")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !opened.contains(value))
        });
        if let Some(record) = active_for_symbol {
            let existing = record
                .payload
                .get("commitment")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            anyhow::bail!(
                "an untouched holdout is already sealed for {} ({:.12}…); open it before sealing another one",
                request.symbol.to_uppercase(),
                existing
            );
        }
        anyhow::ensure!(
            request.payload.get("commitment").and_then(Value::as_str) == Some(commitment),
            "holdout seal commitment payload mismatch"
        );
        self.append_unlocked(request)
    }

    pub async fn append_holdout_open_once(
        &self,
        request: AuditCaptureRequest,
        commitment: &str,
    ) -> anyhow::Result<AuditRecord> {
        validate_request(&request)?;
        anyhow::ensure!(
            request.kind == "holdout_open",
            "expected holdout_open audit kind"
        );
        let _guard = self.lock.lock().await;
        let records = read_records(&self.path)?;
        let opened = opened_holdout_commitments(&records);
        anyhow::ensure!(
            !opened.contains(commitment),
            "this holdout commitment has already been opened"
        );
        anyhow::ensure!(
            request.payload.get("commitment").and_then(Value::as_str) == Some(commitment),
            "holdout open commitment payload mismatch"
        );
        anyhow::ensure!(
            records.iter().any(|record| {
                record.kind == "holdout_seal"
                    && record.symbol.eq_ignore_ascii_case(&request.symbol)
                    && record.payload.get("commitment").and_then(Value::as_str) == Some(commitment)
            }),
            "no matching sealed holdout exists in the audit ledger"
        );
        self.append_unlocked(request)
    }

    fn append_unlocked(&self, request: AuditCaptureRequest) -> anyhow::Result<AuditRecord> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("create audit directory")?;
        }
        let previous_hash = read_records(&self.path)?
            .last()
            .map(|record| record.record_hash.clone());
        let created_at = Utc::now().to_rfc3339();
        let symbol = request.symbol.to_uppercase();
        let record_hash = calculate_hash(
            &created_at,
            &request.kind,
            &request.mode,
            &symbol,
            request.snapshot_id.as_deref(),
            previous_hash.as_deref(),
            &request.payload,
        )?;
        let record = AuditRecord {
            id: record_hash[..20].into(),
            created_at,
            kind: request.kind,
            mode: request.mode,
            symbol,
            snapshot_id: request.snapshot_id,
            previous_hash,
            record_hash,
            payload: request.payload,
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .context("open audit ledger")?;
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(record)
    }

    pub async fn list(&self, limit: usize) -> anyhow::Result<Vec<AuditSummary>> {
        let _guard = self.lock.lock().await;
        Ok(read_records(&self.path)?
            .into_iter()
            .rev()
            .take(limit.clamp(1, 200))
            .map(|record| AuditSummary {
                id: record.id,
                created_at: record.created_at,
                kind: record.kind,
                mode: record.mode,
                symbol: record.symbol,
                snapshot_id: record.snapshot_id,
                record_hash: record.record_hash,
            })
            .collect())
    }

    pub async fn get(&self, id: &str) -> anyhow::Result<AuditRecord> {
        let _guard = self.lock.lock().await;
        read_records(&self.path)?
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| anyhow!("audit record not found"))
    }

    pub async fn recent_records(&self, limit: usize) -> anyhow::Result<Vec<AuditRecord>> {
        let _guard = self.lock.lock().await;
        let mut records: Vec<_> = read_records(&self.path)?
            .into_iter()
            .rev()
            .take(limit.clamp(1, 500))
            .collect();
        records.reverse();
        Ok(records)
    }
    pub async fn active_holdout_seals_for_symbol(
        &self,
        symbol: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let _guard = self.lock.lock().await;
        let records = read_records(&self.path)?;
        let opened = opened_holdout_commitments(&records);

        Ok(records
            .iter()
            .filter(|record| {
                record.kind == "holdout_seal"
                    && record.symbol.eq_ignore_ascii_case(symbol)
                    && record
                        .payload
                        .get("commitment")
                        .and_then(Value::as_str)
                        .is_some_and(|commitment| !opened.contains(commitment))
            })
            .map(|record| record.payload.clone())
            .collect())
    }
}

fn opened_holdout_commitments(records: &[AuditRecord]) -> std::collections::HashSet<String> {
    records
        .iter()
        .filter(|record| record.kind == "holdout_open")
        .filter_map(|record| {
            record
                .payload
                .get("commitment")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn read_records(path: &Path) -> anyhow::Result<Vec<AuditRecord>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let mut previous_hash: Option<String> = None;
    for (index, line) in fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let record: AuditRecord = serde_json::from_str(line)
            .with_context(|| format!("decode audit record {}", index + 1))?;
        anyhow::ensure!(
            record.previous_hash.as_deref() == previous_hash.as_deref(),
            "audit hash chain is broken at record {}",
            index + 1
        );
        let expected = calculate_hash(
            &record.created_at,
            &record.kind,
            &record.mode,
            &record.symbol,
            record.snapshot_id.as_deref(),
            record.previous_hash.as_deref(),
            &record.payload,
        )?;
        anyhow::ensure!(
            record.record_hash == expected && record.id == expected[..20],
            "audit record integrity check failed at record {}",
            index + 1
        );
        previous_hash = Some(record.record_hash.clone());
        records.push(record);
    }
    Ok(records)
}

fn calculate_hash(
    created_at: &str,
    kind: &str,
    mode: &str,
    symbol: &str,
    snapshot_id: Option<&str>,
    previous_hash: Option<&str>,
    payload: &Value,
) -> anyhow::Result<String> {
    let material = serde_json::to_vec(&serde_json::json!({
        "created_at": created_at,
        "kind": kind,
        "mode": mode,
        "symbol": symbol,
        "snapshot_id": snapshot_id,
        "previous_hash": previous_hash,
        "payload": payload,
    }))?;
    Ok(hex::encode(Sha256::digest(material)))
}

fn validate_request(request: &AuditCaptureRequest) -> anyhow::Result<()> {
    anyhow::ensure!(
        !request.kind.trim().is_empty()
            && request
                .kind
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_-".contains(character)),
        "invalid audit event kind"
    );
    anyhow::ensure!(
        matches!(request.mode.as_str(), "live" | "replay" | "system"),
        "invalid audit mode"
    );
    anyhow::ensure!(
        !request.symbol.trim().is_empty() && request.symbol.len() <= 20,
        "invalid symbol"
    );
    anyhow::ensure!(
        !contains_secret(&request.payload),
        "credential-like fields are forbidden in audit payloads"
    );
    anyhow::ensure!(
        serde_json::to_vec(&request.payload)?.len() <= 6_000_000,
        "audit payload is too large"
    );
    Ok(())
}

fn contains_secret(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "app_key" | "app_secret" | "access_token" | "token" | "password"
            ) || contains_secret(value)
        }),
        Value::Array(values) => values.iter().any(contains_secret),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ledger_is_append_only_and_hash_chained() {
        let path = std::env::temp_dir().join(format!(
            "option-workstation-audit-{}.jsonl",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let store = AuditStore::new(path.clone());
        let first = store
            .append(AuditCaptureRequest {
                kind: "snapshot".into(),
                mode: "replay".into(),
                symbol: "SPY".into(),
                snapshot_id: Some("a".into()),
                payload: serde_json::json!({"spot": 100}),
            })
            .await
            .unwrap();
        let second = store
            .append(AuditCaptureRequest {
                kind: "snapshot".into(),
                mode: "live".into(),
                symbol: "QQQ".into(),
                snapshot_id: Some("b".into()),
                payload: serde_json::json!({"spot": 200}),
            })
            .await
            .unwrap();
        assert_eq!(
            second.previous_hash.as_deref(),
            Some(first.record_hash.as_str())
        );
        assert_eq!(store.list(10).await.unwrap().len(), 2);
        assert_eq!(store.get(&first.id).await.unwrap().symbol, "SPY");
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn tampered_ledger_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "option-workstation-audit-tamper-{}.jsonl",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let store = AuditStore::new(path.clone());
        store
            .append(AuditCaptureRequest {
                kind: "snapshot".into(),
                mode: "replay".into(),
                symbol: "SPY".into(),
                snapshot_id: Some("a".into()),
                payload: serde_json::json!({"spot": 100}),
            })
            .await
            .unwrap();
        let tampered = fs::read_to_string(&path)
            .unwrap()
            .replace("\"spot\":100", "\"spot\":101");
        fs::write(&path, tampered).unwrap();
        assert!(store.list(10).await.is_err());
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn symbol_holdout_lock_ignores_other_symbols() {
        let path = std::env::temp_dir().join(format!(
            "option-workstation-symbol-holdout-{}.jsonl",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let store = AuditStore::new(path.clone());
        store
            .append(AuditCaptureRequest {
                kind: "holdout_seal".into(),
                mode: "system".into(),
                symbol: "SPY".into(),
                snapshot_id: None,
                payload: serde_json::json!({
                    "commitment": "spy-commitment",
                    "strategy_id": "spy-strategy",
                    "holdout_start": "2026-08-01",
                    "holdout_end": "2026-08-31"
                }),
            })
            .await
            .unwrap();

        assert_eq!(
            store
                .active_holdout_seals_for_symbol("spy")
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .active_holdout_seals_for_symbol("QQQ")
                .await
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn holdout_open_requires_prior_seal() {
        let path = std::env::temp_dir().join(format!(
            "option-workstation-unsealed-open-{}.jsonl",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let store = AuditStore::new(path.clone());
        let result = store
            .append_holdout_open_once(
                AuditCaptureRequest {
                    kind: "holdout_open".into(),
                    mode: "system".into(),
                    symbol: "SPY".into(),
                    snapshot_id: None,
                    payload: serde_json::json!({
                        "commitment": "commit-never-sealed",
                        "strategy_id": "strategy-1"
                    }),
                },
                "commit-never-sealed",
            )
            .await;
        assert!(result.is_err());
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn holdout_seal_becomes_inactive_after_open() {
        let path = std::env::temp_dir().join(format!(
            "option-workstation-holdout-audit-{}.jsonl",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let store = AuditStore::new(path.clone());
        store
            .append(AuditCaptureRequest {
                kind: "holdout_seal".into(),
                mode: "system".into(),
                symbol: "SPY".into(),
                snapshot_id: None,
                payload: serde_json::json!({
                    "commitment": "commit-1",
                    "strategy_id": "strategy-1",
                    "holdout_start": "2026-08-01",
                    "holdout_end": "2026-08-31"
                }),
            })
            .await
            .unwrap();

        assert_eq!(
            store
                .active_holdout_seals_for_symbol("SPY")
                .await
                .unwrap()
                .len(),
            1
        );

        store
            .append(AuditCaptureRequest {
                kind: "holdout_open".into(),
                mode: "system".into(),
                symbol: "SPY".into(),
                snapshot_id: None,
                payload: serde_json::json!({
                    "commitment": "commit-1",
                    "strategy_id": "strategy-1"
                }),
            })
            .await
            .unwrap();

        let recent = store.recent_records(10).await.unwrap();
        assert!(recent.iter().any(|record| {
            record.kind == "holdout_open"
                && record.payload.get("commitment").and_then(Value::as_str) == Some("commit-1")
        }));
        assert!(
            store
                .active_holdout_seals_for_symbol("SPY")
                .await
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn credentials_are_rejected() {
        let request = AuditCaptureRequest {
            kind: "snapshot".into(),
            mode: "live".into(),
            symbol: "SPY".into(),
            snapshot_id: None,
            payload: serde_json::json!({"access_token": "secret"}),
        };
        assert!(validate_request(&request).is_err());
    }
}
