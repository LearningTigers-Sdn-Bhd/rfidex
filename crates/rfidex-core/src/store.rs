//! Local SQLite store: station config, offline ticket/binding cache, and the
//! durable outbox. Every write is committed (WAL, synchronous=FULL) before the
//! function returns, so a crash after return cannot lose the row.

use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use uuid::Uuid;

use crate::contract::{CacheResp, TicketSummary};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxKind {
    DeskScan,
    Binding,
    Observation,
}

impl OutboxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxKind::DeskScan => "desk_scan",
            OutboxKind::Binding => "binding",
            OutboxKind::Observation => "observation",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "desk_scan" => OutboxKind::DeskScan,
            "binding" => OutboxKind::Binding,
            _ => OutboxKind::Observation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxState {
    Pending,
    Sent,
    Conflict,
    Parked,
}

impl OutboxState {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxState::Pending => "pending",
            OutboxState::Sent => "sent",
            OutboxState::Conflict => "conflict",
            OutboxState::Parked => "parked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    New(i64),
    Existing(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutboxItem {
    pub id: i64,
    pub kind: OutboxKind,
    pub idem_key: String,
    pub payload: Value,
    pub captured_at: DateTime<Utc>,
    pub attempts: u32,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
CREATE TABLE IF NOT EXISTS config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tickets (
  public_id TEXT PRIMARY KEY, name TEXT NOT NULL, ticket_type TEXT NOT NULL,
  valid INTEGER NOT NULL, checked_in INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS bindings (tag_key TEXT PRIMARY KEY, public_id TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS bindings_ticket ON bindings(public_id);
CREATE TABLE IF NOT EXISTS outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  idem_key TEXT NOT NULL UNIQUE,
  payload TEXT NOT NULL,
  captured_at TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending',
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at TEXT NOT NULL,
  last_error TEXT,
  result TEXT
);
CREATE INDEX IF NOT EXISTS outbox_due ON outbox(state, kind, next_attempt_at, id);
";

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_default()
}

fn row_to_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoreResult<OutboxItem>> {
    let id: i64 = r.get(0)?;
    let kind: String = r.get(1)?;
    let idem_key: String = r.get(2)?;
    let payload: String = r.get(3)?;
    let captured_at: String = r.get(4)?;
    let attempts: i64 = r.get(5)?;
    Ok(serde_json::from_str(&payload)
        .map_err(StoreError::from)
        .map(|payload| OutboxItem {
            id,
            kind: OutboxKind::parse(&kind),
            idem_key,
            payload,
            captured_at: parse_ts(&captured_at),
            attempts: attempts as u32,
        }))
}

impl Store {
    pub fn open(path: &Path) -> StoreResult<Store> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn open_in_memory() -> StoreResult<Store> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn enqueue(
        &self,
        kind: OutboxKind,
        idem_key: &str,
        payload: &Value,
        captured_at: DateTime<Utc>,
    ) -> StoreResult<Enqueued> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO outbox (kind, idem_key, payload, captured_at, next_attempt_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![
                kind.as_str(),
                idem_key,
                serde_json::to_string(payload)?,
                ts(captured_at)
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM outbox WHERE idem_key = ?1",
            [idem_key],
            |r| r.get(0),
        )?;
        Ok(if inserted == 1 {
            Enqueued::New(id)
        } else {
            Enqueued::Existing(id)
        })
    }

    pub fn due(
        &self,
        kind: OutboxKind,
        limit: usize,
        now: DateTime<Utc>,
    ) -> StoreResult<Vec<OutboxItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, idem_key, payload, captured_at, attempts FROM outbox
             WHERE state = 'pending' AND kind = ?1 AND next_attempt_at <= ?2
             ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![kind.as_str(), ts(now), limit as i64], row_to_item)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    pub fn mark_sent(&self, id: i64, result: &Value) -> StoreResult<()> {
        self.set_state(id, OutboxState::Sent, Some(result), None)
    }

    pub fn mark_conflict(&self, id: i64, result: &Value) -> StoreResult<()> {
        self.set_state(id, OutboxState::Conflict, Some(result), None)
    }

    pub fn mark_parked(&self, id: i64, error: &str) -> StoreResult<()> {
        self.set_state(id, OutboxState::Parked, None, Some(error))
    }

    pub fn mark_retry(&self, id: i64, error: &str, next_at: DateTime<Utc>) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE outbox SET attempts = attempts + 1, last_error = ?2, next_attempt_at = ?3 WHERE id = ?1",
            params![id, error, ts(next_at)],
        )?;
        Ok(())
    }

    fn set_state(
        &self,
        id: i64,
        state: OutboxState,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> StoreResult<()> {
        let result = result.map(serde_json::to_string).transpose()?;
        self.conn.execute(
            "UPDATE outbox SET state = ?2, result = COALESCE(?3, result), last_error = COALESCE(?4, last_error)
             WHERE id = ?1",
            params![id, state.as_str(), result, error],
        )?;
        Ok(())
    }

    pub fn count(&self, state: OutboxState) -> StoreResult<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM outbox WHERE state = ?1",
            [state.as_str()],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    pub fn items(
        &self,
        state: OutboxState,
        limit: usize,
    ) -> StoreResult<Vec<(OutboxItem, Option<Value>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, idem_key, payload, captured_at, attempts, result FROM outbox
             WHERE state = ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![state.as_str(), limit as i64], |r| {
            let item = row_to_item(r)?;
            let result: Option<String> = r.get(6)?;
            Ok((item, result))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (item, result) = row?;
            let result = result.map(|s| serde_json::from_str(&s)).transpose()?;
            out.push((item?, result));
        }
        Ok(out)
    }

    pub fn get_config(&self, key: &str) -> StoreResult<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM config WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_config(&self, key: &str, value: &str) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO config (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn upsert_ticket(&self, t: &TicketSummary) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO tickets (public_id, name, ticket_type, valid, checked_in) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(public_id) DO UPDATE SET name = excluded.name, ticket_type = excluded.ticket_type,
               valid = excluded.valid, checked_in = excluded.checked_in",
            params![t.public_id.to_string(), t.name, t.ticket_type, t.valid, t.checked_in],
        )?;
        Ok(())
    }

    pub fn ticket(&self, public_id: Uuid) -> StoreResult<Option<TicketSummary>> {
        Ok(self
            .conn
            .query_row(
                "SELECT name, ticket_type, valid, checked_in FROM tickets WHERE public_id = ?1",
                [public_id.to_string()],
                |r| {
                    Ok(TicketSummary {
                        public_id,
                        name: r.get(0)?,
                        ticket_type: r.get(1)?,
                        valid: r.get(2)?,
                        checked_in: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn upsert_binding(&self, tag_key: &str, public_id: Uuid) -> StoreResult<()> {
        let id = public_id.to_string();
        self.conn.execute(
            "DELETE FROM bindings WHERE public_id = ?1 AND tag_key <> ?2",
            params![id, tag_key],
        )?;
        self.conn.execute(
            "INSERT INTO bindings (tag_key, public_id) VALUES (?1, ?2)
             ON CONFLICT(tag_key) DO UPDATE SET public_id = excluded.public_id",
            params![tag_key, id],
        )?;
        Ok(())
    }

    pub fn remove_binding(&self, tag_key: &str) -> StoreResult<()> {
        self.conn
            .execute("DELETE FROM bindings WHERE tag_key = ?1", [tag_key])?;
        Ok(())
    }

    pub fn binding_holder(&self, tag_key: &str) -> StoreResult<Option<Uuid>> {
        let s: Option<String> = self
            .conn
            .query_row(
                "SELECT public_id FROM bindings WHERE tag_key = ?1",
                [tag_key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(s.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    pub fn binding_tag_for_ticket(&self, public_id: Uuid) -> StoreResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT tag_key FROM bindings WHERE public_id = ?1",
                [public_id.to_string()],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn replace_cache(&self, cache: &CacheResp) -> StoreResult<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            for t in &cache.tickets {
                self.upsert_ticket(t)?;
            }
            for key in &cache.revoked_tag_keys {
                self.remove_binding(key)?;
            }
            for b in &cache.bindings {
                self.upsert_binding(&b.tag_key, b.public_id)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(_) => self.conn.execute_batch("ROLLBACK")?,
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-25T06:30:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ticket(n: u128, name: &str) -> TicketSummary {
        TicketSummary {
            public_id: Uuid::from_u128(n),
            name: name.into(),
            ticket_type: "VIP".into(),
            valid: true,
            checked_in: false,
        }
    }

    #[test]
    fn enqueue_is_idempotent_by_key() {
        let s = Store::open_in_memory().unwrap();
        let a = s
            .enqueue(OutboxKind::Observation, "d1", &json!({"n": 1}), t0())
            .unwrap();
        let b = s
            .enqueue(OutboxKind::Observation, "d1", &json!({"n": 2}), t0())
            .unwrap();
        let Enqueued::New(id) = a else {
            panic!("first insert must be New")
        };
        assert_eq!(b, Enqueued::Existing(id));
        assert_eq!(s.count(OutboxState::Pending).unwrap(), 1);
        assert_eq!(
            s.due(OutboxKind::Observation, 10, t0()).unwrap()[0].payload,
            json!({"n": 1})
        );
    }

    #[test]
    fn due_is_fifo_per_kind_and_respects_backoff() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..3 {
            s.enqueue(OutboxKind::Observation, &format!("d{i}"), &json!(i), t0())
                .unwrap();
        }
        s.enqueue(OutboxKind::Binding, "b0", &json!("b"), t0())
            .unwrap();
        let due = s.due(OutboxKind::Observation, 10, t0()).unwrap();
        assert_eq!(
            due.iter().map(|i| i.idem_key.as_str()).collect::<Vec<_>>(),
            ["d0", "d1", "d2"]
        );
        s.mark_retry(due[0].id, "down", t0() + Duration::seconds(5))
            .unwrap();
        assert_eq!(s.due(OutboxKind::Observation, 10, t0()).unwrap().len(), 2);
        let later = s
            .due(OutboxKind::Observation, 10, t0() + Duration::seconds(5))
            .unwrap();
        assert_eq!(later[0].idem_key, "d0");
        assert_eq!(later[0].attempts, 1);
    }

    #[test]
    fn states_move_rows_out_of_pending() {
        let s = Store::open_in_memory().unwrap();
        let ids: Vec<i64> = (0..3)
            .map(|i| {
                match s
                    .enqueue(OutboxKind::Binding, &format!("o{i}"), &json!(i), t0())
                    .unwrap()
                {
                    Enqueued::New(id) => id,
                    Enqueued::Existing(_) => unreachable!(),
                }
            })
            .collect();
        s.mark_sent(ids[0], &json!({"ok": true})).unwrap();
        s.mark_conflict(ids[1], &json!({"error": "uid_bound_elsewhere"}))
            .unwrap();
        s.mark_parked(ids[2], "ticket not found").unwrap();
        assert_eq!(s.count(OutboxState::Pending).unwrap(), 0);
        assert_eq!(s.count(OutboxState::Sent).unwrap(), 1);
        let conflicts = s.items(OutboxState::Conflict, 10).unwrap();
        assert_eq!(
            conflicts[0].1,
            Some(json!({"error": "uid_bound_elsewhere"}))
        );
        assert_eq!(s.items(OutboxState::Parked, 10).unwrap().len(), 1);
    }

    #[test]
    fn rows_survive_reopen_without_clean_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rfidex.db");
        {
            let s = Store::open(&path).unwrap();
            s.enqueue(
                OutboxKind::Observation,
                "crash-1",
                &json!({"uid": "E0"}),
                t0(),
            )
            .unwrap();
            std::mem::forget(s); // simulate a crash: no Drop, no close
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.count(OutboxState::Pending).unwrap(), 1);
    }

    #[test]
    fn config_round_trip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.get_config("api_key").unwrap(), None);
        s.set_config("api_key", "abc").unwrap();
        s.set_config("api_key", "def").unwrap();
        assert_eq!(s.get_config("api_key").unwrap().as_deref(), Some("def"));
    }

    #[test]
    fn ticket_and_binding_cache() {
        let s = Store::open_in_memory().unwrap();
        let a = ticket(1, "Aina");
        s.upsert_ticket(&a).unwrap();
        assert_eq!(s.ticket(a.public_id).unwrap(), Some(a.clone()));
        s.upsert_binding("TAG1", a.public_id).unwrap();
        s.upsert_binding("TAG2", a.public_id).unwrap(); // replaces TAG1 for this ticket
        assert_eq!(s.binding_holder("TAG1").unwrap(), None);
        assert_eq!(s.binding_holder("TAG2").unwrap(), Some(a.public_id));
        assert_eq!(
            s.binding_tag_for_ticket(a.public_id).unwrap().as_deref(),
            Some("TAG2")
        );
        s.remove_binding("TAG2").unwrap();
        assert_eq!(s.binding_tag_for_ticket(a.public_id).unwrap(), None);
    }

    #[test]
    fn replace_cache_applies_tickets_bindings_and_revocations() {
        use crate::contract::{BindMode, BindingInfo};
        use crate::tag::Protocol;
        let s = Store::open_in_memory().unwrap();
        s.upsert_binding("OLD", Uuid::from_u128(9)).unwrap();
        let cache = CacheResp {
            tickets: vec![ticket(1, "Aina")],
            bindings: vec![BindingInfo {
                id: 1,
                public_id: Uuid::from_u128(1),
                protocol: Protocol::Iso15693,
                uid_raw_hex: "AA".into(),
                tag_key: "AA".into(),
                mode: BindMode::Bind,
            }],
            revoked_tag_keys: vec!["OLD".into()],
            server_time: t0(),
        };
        s.replace_cache(&cache).unwrap();
        assert!(s.ticket(Uuid::from_u128(1)).unwrap().is_some());
        assert_eq!(s.binding_holder("AA").unwrap(), Some(Uuid::from_u128(1)));
        assert_eq!(s.binding_holder("OLD").unwrap(), None);
    }
}
