use serde::Serialize;

use crate::audit::AuditRecord;

#[derive(Debug, Clone, Serialize)]
pub struct JournalEvent {
    pub id: String,
    pub created_at: String,
    pub kind: String,
    pub mode: String,
    pub symbol: String,
    pub snapshot_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct JournalReplay {
    pub engine: &'static str,
    pub events: Vec<JournalEvent>,
    pub symbols: Vec<String>,
    pub event_count: usize,
    pub notes: Vec<String>,
}

pub fn build(records: Vec<AuditRecord>, symbol: Option<&str>) -> JournalReplay {
    let wanted = symbol.map(|value| value.trim().to_uppercase());
    let mut symbols = std::collections::BTreeSet::new();
    let events: Vec<_> = records
        .into_iter()
        .filter(|record| {
            wanted
                .as_ref()
                .is_none_or(|value| record.symbol.eq_ignore_ascii_case(value))
        })
        .map(|record| {
            symbols.insert(record.symbol.clone());
            JournalEvent {
                id: record.id,
                created_at: record.created_at,
                kind: record.kind,
                mode: record.mode,
                symbol: record.symbol,
                snapshot_id: record.snapshot_id,
                payload: record.payload,
            }
        })
        .collect();

    JournalReplay {
        engine: "audit_journal_replay_v1",
        event_count: events.len(),
        events,
        symbols: symbols.into_iter().collect(),
        notes: vec![
            "events are returned in verified ledger order".into(),
            "snapshot IDs preserve the link to the market state captured at decision time".into(),
            "journal replay reflects recorded events only and does not fabricate missing decisions"
                .into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_filters_symbol_without_reordering() {
        let record = |id: &str, symbol: &str| AuditRecord {
            id: id.into(),
            created_at: "2026-09-24T00:00:00Z".into(),
            kind: "snapshot".into(),
            mode: "replay".into(),
            symbol: symbol.into(),
            snapshot_id: None,
            previous_hash: None,
            record_hash: format!("hash-{id}"),
            payload: serde_json::json!({}),
        };
        let output = build(
            vec![record("a", "SPY"), record("b", "QQQ"), record("c", "SPY")],
            Some("spy"),
        );
        assert_eq!(output.event_count, 2);
        assert_eq!(output.events[0].id, "a");
        assert_eq!(output.events[1].id, "c");
    }
}
