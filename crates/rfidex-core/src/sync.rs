//! Drains the outbox to the server and keeps the offline cache fresh.
//! Never holds the store lock across an await.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::client::{ApiClient, ApiError};
use crate::contract::*;
use crate::store::{OutboxItem, OutboxKind, Store, StoreError};

/// A 2xx reply the app cannot read is retried this many times, then parked,
/// so version drift cannot block the queue forever. Network errors retry forever.
pub const MAX_BAD_RESPONSE_ATTEMPTS: u32 = 5;
/// Sent rows are kept this long for support, then pruned.
pub const SENT_RETENTION_DAYS: i64 = 7;

pub fn backoff(attempts: u32, max: Duration, id: i64) -> Duration {
    let secs = (1u64 << attempts.min(6)).min(max.as_secs());
    Duration::from_secs(secs) + Duration::from_millis(id.unsigned_abs() % 250)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub sent: usize,
    pub conflicts: usize,
    pub parked: usize,
    pub retried: usize,
    pub unauthorized: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct SyncWorker {
    store: Arc<Mutex<Store>>,
    client: ApiClient,
    max_backoff: Duration,
}

fn after(now: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    now + chrono::Duration::from_std(d).expect("backoff fits chrono")
}

impl SyncWorker {
    pub fn new(store: Arc<Mutex<Store>>, client: ApiClient) -> SyncWorker {
        SyncWorker {
            store,
            client,
            max_backoff: Duration::from_secs(60),
        }
    }

    pub async fn run_once(&self, now: DateTime<Utc>) -> Result<SyncReport, StoreError> {
        let mut r = SyncReport::default();
        for kind in [OutboxKind::DeskScan, OutboxKind::Binding] {
            self.drain_single(kind, now, &mut r).await?;
            if r.unauthorized {
                return Ok(r);
            }
        }
        self.drain_observations(now, &mut r).await?;
        Ok(r)
    }

    /// Outer `Err` = the stored payload cannot be decoded (e.g. written by an
    /// older app version); the caller parks that row instead of aborting.
    async fn send_single(
        &self,
        item: &OutboxItem,
    ) -> Result<Result<Value, ApiError>, serde_json::Error> {
        Ok(match item.kind {
            OutboxKind::DeskScan => {
                let req: DeskScanReq = serde_json::from_value(item.payload.clone())?;
                self.client
                    .desk_scan(&req)
                    .await
                    .map(|v| serde_json::to_value(v).expect("serializable"))
            }
            OutboxKind::Binding => {
                let req: BindingReq = serde_json::from_value(item.payload.clone())?;
                self.client
                    .bind(&req)
                    .await
                    .map(|v| serde_json::to_value(v).expect("serializable"))
            }
            OutboxKind::Observation => unreachable!("observations are batched"),
        })
    }

    fn apply(s: &Store, kind: OutboxKind, v: &Value) -> Result<(), StoreError> {
        match kind {
            OutboxKind::DeskScan => {
                let resp: DeskScanResp = serde_json::from_value(v.clone())?;
                s.upsert_ticket(&resp.ticket)
            }
            OutboxKind::Binding => {
                let resp: BindingResp = serde_json::from_value(v.clone())?;
                for old in &resp.revoked {
                    s.remove_binding(&old.tag_key)?;
                }
                s.upsert_binding(&resp.binding.tag_key, resp.binding.public_id)
            }
            OutboxKind::Observation => Ok(()),
        }
    }

    async fn drain_single(
        &self,
        kind: OutboxKind,
        now: DateTime<Utc>,
        r: &mut SyncReport,
    ) -> Result<(), StoreError> {
        loop {
            let next = self
                .store
                .lock()
                .unwrap()
                .due(kind, 1, now)?
                .into_iter()
                .next();
            let Some(item) = next else { return Ok(()) };
            let result = match self.send_single(&item).await {
                Ok(result) => result,
                Err(e) => {
                    let msg = format!("undecodable payload: {e}");
                    self.store.lock().unwrap().mark_parked(item.id, &msg)?;
                    r.parked += 1;
                    continue;
                }
            };
            let s = self.store.lock().unwrap();
            match result {
                Ok(v) => {
                    Self::apply(&s, kind, &v)?;
                    s.mark_sent(item.id, &v)?;
                    r.sent += 1;
                }
                Err(ApiError::Rejected { status: 409, body }) => {
                    s.mark_conflict(item.id, &serde_json::to_value(&body)?)?;
                    r.conflicts += 1;
                }
                Err(ApiError::Rejected { body, .. }) => {
                    s.mark_parked(item.id, &body.message)?;
                    r.parked += 1;
                }
                Err(ApiError::Unauthorized) => {
                    s.mark_retry(item.id, "api key rejected", after(now, self.max_backoff))?;
                    r.unauthorized = true;
                    return Ok(());
                }
                Err(ApiError::BadResponse(e)) => {
                    if !self.retry_or_park(&s, &item, &e, now, r)? {
                        return Ok(());
                    }
                }
                Err(ApiError::Retryable(e)) => {
                    s.mark_retry(
                        item.id,
                        &e,
                        after(now, backoff(item.attempts, self.max_backoff, item.id)),
                    )?;
                    r.retried += 1;
                    return Ok(());
                }
            }
        }
    }

    /// Unreadable reply: retry with backoff until the attempt limit, then park.
    /// Returns true when the row was parked (the queue may move on).
    fn retry_or_park(
        &self,
        s: &Store,
        item: &OutboxItem,
        error: &str,
        now: DateTime<Utc>,
        r: &mut SyncReport,
    ) -> Result<bool, StoreError> {
        if item.attempts + 1 >= MAX_BAD_RESPONSE_ATTEMPTS {
            s.mark_parked(item.id, &format!("unreadable server reply: {error}"))?;
            r.parked += 1;
            Ok(true)
        } else {
            let next = after(now, backoff(item.attempts, self.max_backoff, item.id));
            s.mark_retry(item.id, error, next)?;
            r.retried += 1;
            Ok(false)
        }
    }

    async fn drain_observations(
        &self,
        now: DateTime<Utc>,
        r: &mut SyncReport,
    ) -> Result<(), StoreError> {
        loop {
            let items = self.store.lock().unwrap().due(
                OutboxKind::Observation,
                MAX_OBSERVATION_BATCH,
                now,
            )?;
            if items.is_empty() {
                return Ok(());
            }
            let mut batch = Vec::with_capacity(items.len());
            {
                let s = self.store.lock().unwrap();
                for item in items {
                    match serde_json::from_value::<ObservationItem>(item.payload.clone()) {
                        Ok(req) => batch.push((item, req)),
                        Err(e) => {
                            s.mark_parked(item.id, &format!("undecodable payload: {e}"))?;
                            r.parked += 1;
                        }
                    }
                }
            }
            if batch.is_empty() {
                continue;
            }
            let reqs: Vec<ObservationItem> = batch.iter().map(|(_, req)| req.clone()).collect();
            let result = self.client.observations(&reqs).await;
            let s = self.store.lock().unwrap();
            match result {
                Ok(resp) => {
                    let by_id: HashMap<Uuid, &ObservationResult> =
                        resp.results.iter().map(|x| (x.delivery_id, x)).collect();
                    for (item, req) in &batch {
                        match by_id.get(&req.delivery_id) {
                            Some(res) => {
                                s.mark_sent(item.id, &serde_json::to_value(res)?)?;
                                r.sent += 1;
                            }
                            None => {
                                s.mark_retry(
                                    item.id,
                                    "no result for delivery",
                                    after(now, backoff(item.attempts, self.max_backoff, item.id)),
                                )?;
                                r.retried += 1;
                            }
                        }
                    }
                }
                Err(ApiError::Rejected { body, .. }) => {
                    for (item, _) in &batch {
                        s.mark_parked(item.id, &body.message)?;
                    }
                    r.parked += batch.len();
                }
                Err(ApiError::BadResponse(e)) => {
                    let mut any_retried = false;
                    for (item, _) in &batch {
                        any_retried |= !self.retry_or_park(&s, item, &e, now, r)?;
                    }
                    if any_retried {
                        return Ok(());
                    }
                }
                Err(ApiError::Unauthorized) => {
                    for (item, _) in &batch {
                        s.mark_retry(item.id, "api key rejected", after(now, self.max_backoff))?;
                    }
                    r.unauthorized = true;
                    return Ok(());
                }
                Err(ApiError::Retryable(e)) => {
                    for (item, _) in &batch {
                        s.mark_retry(
                            item.id,
                            &e,
                            after(now, backoff(item.attempts, self.max_backoff, item.id)),
                        )?;
                    }
                    r.retried += batch.len();
                    return Ok(());
                }
            }
        }
    }

    pub async fn refresh_cache(&self) -> Result<(), SyncError> {
        let cache = self.client.cache().await?;
        self.store.lock().unwrap().replace_cache(&cache)?;
        Ok(())
    }

    pub async fn run_forever(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut last_cache: Option<Instant> = None;
        let mut last_prune: Option<Instant> = None;
        loop {
            if *stop.borrow() {
                return;
            }
            if let Err(e) = self.run_once(Utc::now()).await {
                eprintln!("rfidex sync: store error: {e}");
            }
            if last_cache.is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
                && self.refresh_cache().await.is_ok()
            {
                last_cache = Some(Instant::now());
            }
            if last_prune.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600)) {
                let cutoff = Utc::now() - chrono::Duration::days(SENT_RETENTION_DAYS);
                if let Err(e) = self.store.lock().unwrap().prune_sent(cutoff) {
                    eprintln!("rfidex sync: prune failed: {e}");
                }
                last_prune = Some(Instant::now());
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = stop.changed() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let max = Duration::from_secs(60);
        assert_eq!(backoff(0, max, 0), Duration::from_secs(1));
        assert_eq!(backoff(3, max, 0), Duration::from_secs(8));
        assert_eq!(backoff(20, max, 0), Duration::from_secs(60));
        assert_eq!(backoff(0, max, 251), Duration::from_millis(1001));
    }
}
