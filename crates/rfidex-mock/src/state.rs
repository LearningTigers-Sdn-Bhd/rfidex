use std::collections::{HashMap, HashSet};

use chrono::Utc;
use rfidex_core::codec;
use rfidex_core::contract::*;
use rfidex_core::tag::{hex_upper, parse_hex, tag_key, UidRule};
use serde::Deserialize;
use uuid::Uuid;

/// Boxed body keeps `Result<_, ApiFailure>` small; `ErrorBody` is ~184 bytes.
pub type ApiFailure = (u16, Box<ErrorBody>);

fn fail(status: u16, error: ErrorCode, message: &str) -> ApiFailure {
    (
        status,
        Box::new(ErrorBody {
            error,
            message: message.into(),
            holder: None,
            binding: None,
        }),
    )
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedTicket {
    pub public_id: Uuid,
    pub name: String,
    pub ticket_type: String,
    #[serde(default = "default_true")]
    pub paid: bool,
    #[serde(default)]
    pub cancelled: bool,
    /// Contacts are for search only: they never reach a DTO, the cache, or a
    /// station. Raw values stay private to this server.
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    /// Explicit creation time. Without one, the seed order decides: later rows
    /// are newer.
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<Utc>>,
    /// A guest who was already checked in before this run.
    #[serde(default)]
    pub checked_in_at: Option<chrono::DateTime<Utc>>,
}

/// The creation time given to a seed that carries none, before its position in
/// the seed list is added.
fn seed_epoch() -> chrono::DateTime<Utc> {
    "2026-01-01T00:00:00Z".parse().expect("a fixed epoch")
}

/// At most two visible characters of the local part, and never the whole local
/// part: a one-character local part shows none.
fn mask_email(value: &str) -> String {
    let (local, domain) = value.split_once('@').unwrap_or((value, ""));
    let visible = local.chars().count().saturating_sub(1).min(2);
    let shown: String = local.chars().take(visible).collect();
    format!("{shown}***@{domain}")
}

/// At most four trailing digits, and never the whole number.
fn mask_phone(value: &str) -> String {
    let digits: Vec<char> = value.chars().filter(char::is_ascii_digit).collect();
    let shown = digits.len().saturating_sub(1).min(4);
    let suffix: String = digits[digits.len() - shown..].iter().collect();
    format!("•••• {suffix}")
}

struct MockTicket {
    summary: TicketSummary,
    paid: bool,
    cancelled: bool,
    /// The first check-in this ticket ever had. Private: it never leaves the
    /// server except as the check-in metadata on a desk scan.
    checked_in_at: Option<chrono::DateTime<Utc>>,
    /// Search keys, and the masked hints an answer may carry.
    name_norm: String,
    email_norm: Option<String>,
    phone_norm: Option<String>,
    email_hint: Option<String>,
    phone_hint: Option<String>,
    /// When this row ranks in a newest-first search: its explicit creation
    /// time, or the fixed epoch plus its seed position.
    created_at: chrono::DateTime<Utc>,
}

struct MockBinding {
    info: BindingInfo,
    active: bool,
}

pub struct MockState {
    pub api_key: String,
    pub event: EventSettings,
    pub scan_log_count: usize,
    pub received_order: Vec<Uuid>,
    pub stations: HashMap<String, HeartbeatReq>,
    tickets: HashMap<Uuid, MockTicket>,
    bindings: Vec<MockBinding>,
    desk_ops: HashMap<Uuid, DeskScanResp>,
    bind_ops: HashMap<Uuid, BindingResp>,
    observations: HashMap<(String, Uuid), ObservationResult>,
    seen_seqs: HashSet<(String, u64)>,
    next_binding_id: u64,
}

fn key_of(uid_raw_hex: &str) -> Option<String> {
    parse_hex(uid_raw_hex)
        .ok()
        .filter(|r| !r.is_empty())
        .map(|r| tag_key(&r, UidRule::AsIs))
}

impl MockState {
    pub fn new(api_key: String, event: EventSettings, seeds: Vec<SeedTicket>) -> MockState {
        let tickets = seeds
            .into_iter()
            .enumerate()
            .map(|(order, s)| {
                let summary = TicketSummary {
                    public_id: s.public_id,
                    name: s.name.clone(),
                    ticket_type: s.ticket_type,
                    valid: s.paid && !s.cancelled,
                    checked_in: s.checked_in_at.is_some(),
                };
                // A seed without a creation time is ordered by its position in
                // the list, so the order is fixed at construction and can never
                // come from hash iteration.
                let created_at = s
                    .created_at
                    .unwrap_or_else(|| seed_epoch() + chrono::Duration::seconds(order as i64));
                let name_norm = rfidex_core::search::normalize_name(&s.name);
                let email_norm = s.email.as_deref().map(rfidex_core::search::normalize_email);
                let phone_norm = s.phone.as_deref().map(rfidex_core::search::normalize_phone);
                (
                    s.public_id,
                    MockTicket {
                        summary,
                        paid: s.paid,
                        cancelled: s.cancelled,
                        checked_in_at: s.checked_in_at,
                        name_norm,
                        // The hints reveal the stored, normalised contact, so a
                        // hint can never show a different value than the one a
                        // search matches.
                        email_hint: email_norm.as_deref().map(mask_email),
                        phone_hint: phone_norm.as_deref().map(mask_phone),
                        email_norm,
                        phone_norm,
                        created_at,
                    },
                )
            })
            .collect();
        MockState {
            api_key,
            event,
            scan_log_count: 0,
            received_order: Vec::new(),
            stations: HashMap::new(),
            tickets,
            bindings: Vec::new(),
            desk_ops: HashMap::new(),
            bind_ops: HashMap::new(),
            observations: HashMap::new(),
            seen_seqs: HashSet::new(),
            next_binding_id: 1,
        }
    }

    pub fn ticket(&self, id: Uuid) -> Option<TicketSummary> {
        self.tickets.get(&id).map(|t| t.summary.clone())
    }

    pub fn active_bindings(&self) -> Vec<BindingInfo> {
        self.bindings
            .iter()
            .filter(|b| b.active)
            .map(|b| b.info.clone())
            .collect()
    }

    /// Every binding row ever created, revoked ones included. Distinct from
    /// [`Self::binding_operation_count`]: one replayed operation creates one row.
    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }

    /// Binding operations the server has accepted or replayed, as opposed to
    /// binding rows. A replay adds to neither; a repeat of an already-bound
    /// ticket/sticker pair through a **new** operation adds only to this.
    pub fn binding_operation_count(&self) -> usize {
        self.bind_ops.len()
    }

    /// Desk scan operations, the same way: a replayed `operation_id` is one
    /// operation however many times it arrives.
    pub fn scan_operation_count(&self) -> usize {
        self.desk_ops.len()
    }

    /// Every stored observation result once, as `(station UUID, result)`, sorted
    /// by station then delivery UUID. Read-only: nothing is evaluated here, so
    /// inspecting can never change what a later request would be told.
    pub fn observation_results(&self) -> Vec<(String, ObservationResult)> {
        let mut out: Vec<(String, ObservationResult)> = self
            .observations
            .iter()
            .map(|((station, _), result)| (station.clone(), result.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.delivery_id.cmp(&b.1.delivery_id)));
        out
    }

    pub fn observation_count(&self) -> usize {
        self.observations.len()
    }

    fn check_ticket(&self, id: Uuid) -> Result<(), ApiFailure> {
        let t = self
            .tickets
            .get(&id)
            .ok_or_else(|| fail(404, ErrorCode::TicketNotFound, "ticket not found"))?;
        if t.cancelled {
            return Err(fail(422, ErrorCode::TicketCancelled, "ticket cancelled"));
        }
        if !t.paid {
            return Err(fail(422, ErrorCode::TicketUnpaid, "ticket unpaid"));
        }
        Ok(())
    }

    fn active_for_tag(&self, key: &str) -> Option<&BindingInfo> {
        self.bindings
            .iter()
            .find(|b| b.active && b.info.tag_key == key)
            .map(|b| &b.info)
    }

    fn active_for_ticket(&self, id: Uuid) -> Option<&BindingInfo> {
        self.bindings
            .iter()
            .find(|b| b.active && b.info.public_id == id)
            .map(|b| &b.info)
    }

    /// Paid tickets only, newest first, at most ten — the same rule the
    /// EventzFlow check-in page uses. Matching is literal in memory, so `%` and
    /// `_` are ordinary characters and cannot expand to every row.
    pub fn search_tickets(&self, by: SearchBy, query: &str) -> TicketSearchResp {
        let Some(q) = rfidex_core::search::normalized_query(by, query) else {
            return TicketSearchResp { tickets: vec![] };
        };
        let mut rows: Vec<&MockTicket> = self
            .tickets
            .values()
            .filter(|t| t.paid)
            .filter(|t| match by {
                SearchBy::Name => t.name_norm.contains(&q),
                SearchBy::Email => t.email_norm.as_deref() == Some(q.as_str()),
                SearchBy::Phone => t
                    .phone_norm
                    .as_deref()
                    .is_some_and(|stored| stored.contains(&q)),
            })
            .collect();
        rows.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(a.summary.public_id.cmp(&b.summary.public_id))
        });
        rows.truncate(10);
        TicketSearchResp {
            tickets: rows
                .into_iter()
                .map(|t| TicketSearchItem {
                    public_id: t.summary.public_id,
                    name: t.summary.name.clone(),
                    ticket_type: t.summary.ticket_type.clone(),
                    valid: t.summary.valid,
                    checked_in: t.summary.checked_in,
                    checked_in_at: t.checked_in_at,
                    email_hint: t.email_hint.clone(),
                    phone_hint: t.phone_hint.clone(),
                })
                .collect(),
        }
    }

    pub fn heartbeat(&mut self, station: &str, req: HeartbeatReq) -> HeartbeatResp {
        self.stations.insert(station.to_string(), req);
        HeartbeatResp {
            event: self.event.clone(),
            uid_rule: UidRule::AsIs,
            server_time: Utc::now(),
        }
    }

    pub fn cache(&self) -> CacheResp {
        let mut tickets: Vec<TicketSummary> =
            self.tickets.values().map(|t| t.summary.clone()).collect();
        tickets.sort_by_key(|t| t.public_id);
        let mut revoked: Vec<String> = self
            .bindings
            .iter()
            .filter(|b| !b.active && self.active_for_tag(&b.info.tag_key).is_none())
            .map(|b| b.info.tag_key.clone())
            .collect();
        revoked.sort();
        revoked.dedup();
        CacheResp {
            tickets,
            bindings: self.active_bindings(),
            revoked_tag_keys: revoked,
            server_time: Utc::now(),
        }
    }

    pub fn desk_scan(&mut self, req: DeskScanReq) -> Result<DeskScanResp, ApiFailure> {
        // A repeated operation id returns the answer it already gave, so a
        // queued first check-in still reads `checked_in` after later rescans.
        if let Some(r) = self.desk_ops.get(&req.operation_id) {
            return Ok(r.clone());
        }
        self.check_ticket(req.public_id)?;
        let (ticket, check_in) = {
            let t = self.tickets.get_mut(&req.public_id).expect("checked above");
            let first = t.checked_in_at.is_none();
            // Only the first valid scan sets the time, from the capture the
            // station recorded, so neither a rescan nor an offline drain moves it.
            let checked_in_at = *t.checked_in_at.get_or_insert(req.captured_at);
            t.summary.checked_in = true;
            (
                t.summary.clone(),
                CheckIn {
                    result: if first {
                        CheckInResult::CheckedIn
                    } else {
                        CheckInResult::AlreadyCheckedIn
                    },
                    checked_in_at,
                },
            )
        };
        self.scan_log_count += 1;
        let resp = DeskScanResp {
            ticket,
            binding: self.active_for_ticket(req.public_id).cloned(),
            check_in,
        };
        self.desk_ops.insert(req.operation_id, resp.clone());
        Ok(resp)
    }

    pub fn bind(&mut self, req: BindingReq) -> Result<(u16, BindingResp), ApiFailure> {
        if let Some(r) = self.bind_ops.get(&req.operation_id) {
            return Ok((200, r.clone()));
        }
        self.check_ticket(req.public_id)?;
        let key = key_of(&req.uid_raw_hex)
            .ok_or_else(|| fail(422, ErrorCode::Malformed, "uid_raw_hex is not valid hex"))?;
        if let Some(b) = self.active_for_tag(&key) {
            if b.public_id == req.public_id {
                let resp = BindingResp {
                    binding: b.clone(),
                    revoked: vec![],
                };
                self.bind_ops.insert(req.operation_id, resp.clone());
                return Ok((200, resp));
            }
        }
        let elsewhere = self.active_for_tag(&key).cloned();
        let own = self.active_for_ticket(req.public_id).cloned();
        if !req.replace {
            if let Some(b) = elsewhere {
                let mut e = fail(
                    409,
                    ErrorCode::UidBoundElsewhere,
                    "sticker is linked to another ticket",
                );
                e.1.holder = self.ticket(b.public_id);
                e.1.binding = Some(b);
                return Err(e);
            }
            if let Some(b) = own {
                let mut e = fail(
                    409,
                    ErrorCode::TicketHasSticker,
                    "ticket already has a sticker",
                );
                e.1.binding = Some(b);
                return Err(e);
            }
        } else if req
            .reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            return Err(fail(
                422,
                ErrorCode::ReasonRequired,
                "a reason is required to replace",
            ));
        }
        let mut revoked = Vec::new();
        for b in self
            .bindings
            .iter_mut()
            .filter(|b| b.active && (b.info.tag_key == key || b.info.public_id == req.public_id))
        {
            b.active = false;
            revoked.push(b.info.clone());
        }
        let raw = parse_hex(&req.uid_raw_hex).expect("validated by key_of");
        let info = BindingInfo {
            id: self.next_binding_id,
            public_id: req.public_id,
            protocol: req.protocol,
            uid_raw_hex: hex_upper(&raw),
            tag_key: key,
            mode: req.mode,
        };
        self.next_binding_id += 1;
        self.bindings.push(MockBinding {
            info: info.clone(),
            active: true,
        });
        let resp = BindingResp {
            binding: info,
            revoked,
        };
        self.bind_ops.insert(req.operation_id, resp.clone());
        Ok((201, resp))
    }

    pub fn lookup(&self, uid_raw_hex: &str) -> LookupResp {
        let binding = key_of(uid_raw_hex).and_then(|k| self.active_for_tag(&k).cloned());
        let holder = binding.as_ref().and_then(|b| self.ticket(b.public_id));
        LookupResp { binding, holder }
    }

    pub fn observe(&mut self, station: &str, item: ObservationItem) -> ObservationResult {
        let replay_key = (station.to_string(), item.delivery_id);
        if let Some(r) = self.observations.get(&replay_key) {
            return r.clone();
        }
        self.received_order.push(item.delivery_id);
        let result = self.evaluate(station, &item);
        self.observations.insert(replay_key, result.clone());
        result
    }

    fn evaluate(&mut self, station: &str, item: &ObservationItem) -> ObservationResult {
        let mut anomalies = Vec::new();
        let done = |outcome, anomalies, display| ObservationResult {
            delivery_id: item.delivery_id,
            outcome,
            anomalies,
            display,
        };
        if let Some(seq) = item.device_record_seq {
            if !self.seen_seqs.insert((station.to_string(), seq)) {
                return done(Outcome::PossibleDuplicate, anomalies, Display::default());
            }
        }
        if let Some(d) = item.device_direction_raw {
            // Vendor meeting-gate demo shows 0x00 as IN; unverified on real hardware, flag only.
            let hw = if d == 0 { Role::Entry } else { Role::Exit };
            if hw != item.role {
                anomalies.push("role_mismatch".to_string());
            }
        }
        let key = key_of(&item.uid_raw_hex).unwrap_or_default();
        let Some(binding) = self.active_for_tag(&key) else {
            let revoked = self.bindings.iter().any(|b| b.info.tag_key == key);
            let (outcome, reason) = if revoked {
                (Outcome::RevokedTag, "sticker was replaced")
            } else {
                (Outcome::UnknownTag, "sticker not linked to a ticket")
            };
            return done(
                outcome,
                anomalies,
                Display {
                    reason: Some(reason.into()),
                    ..Display::default()
                },
            );
        };
        let public_id = binding.public_id;
        let t = &self.tickets[&public_id].summary;
        let display = Display {
            name: Some(t.name.clone()),
            ticket_type: Some(t.ticket_type.clone()),
            reason: None,
        };
        let payload_id = item
            .payload_hex
            .as_deref()
            .and_then(|p| parse_hex(p).ok())
            .and_then(|bytes| codec::decode(&bytes).ok());
        if payload_id.is_some_and(|pid| pid != public_id) {
            anomalies.push("payload_binding_mismatch".to_string());
            return done(Outcome::TicketInvalid, anomalies, display);
        }
        if !t.valid {
            return done(Outcome::TicketInvalid, anomalies, display);
        }
        if item.role == Role::Entry && !t.checked_in {
            anomalies.push("entered_without_check_in".to_string());
        }
        done(Outcome::Accepted, anomalies, display)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rfidex_core::tag::Protocol;

    const TAG_A: &str = "3412CDAB500104E0";
    const TAG_B: &str = "5678CDAB500104E0";

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn state() -> MockState {
        let seed = |n, name: &str, paid, cancelled| SeedTicket {
            public_id: id(n),
            name: name.into(),
            ticket_type: "VIP".into(),
            paid,
            cancelled,
            email: None,
            phone: None,
            created_at: None,
            checked_in_at: None,
        };
        MockState::new(
            "k".repeat(32),
            EventSettings {
                event_id: 1,
                name: "Test Expo".into(),
                rfid_mode: RfidMode::Bind,
                require_check_in: false,
            },
            vec![
                seed(1, "Aina", true, false),
                seed(2, "Ben", true, false),
                seed(3, "Chong", false, false),
                seed(4, "Devi", true, true),
            ],
        )
    }

    fn scan(n: u128, op: u128) -> DeskScanReq {
        DeskScanReq {
            public_id: id(n),
            operation_id: id(1000 + op),
            captured_at: Utc::now(),
        }
    }

    fn bind_req(ticket: u128, uid: &str, op: u128) -> BindingReq {
        BindingReq {
            public_id: id(ticket),
            protocol: Protocol::Iso15693,
            uid_raw_hex: uid.into(),
            mode: BindMode::Bind,
            payload_version: None,
            operation_id: id(2000 + op),
            captured_at: Utc::now(),
            replace: false,
            reason: None,
        }
    }

    fn replace(mut r: BindingReq, reason: &str) -> BindingReq {
        r.replace = true;
        r.reason = Some(reason.into());
        r
    }

    fn obs(n: u128, uid: &str, role: Role) -> ObservationItem {
        ObservationItem {
            delivery_id: id(3000 + n),
            role,
            protocol: Protocol::Iso15693,
            uid_raw_hex: uid.into(),
            payload_hex: None,
            device_direction_raw: None,
            device_time_raw_hex: None,
            device_record_seq: None,
            flags_raw: serde_json::Value::Null,
            captured_at: Utc::now(),
        }
    }

    #[test]
    fn desk_scan_checks_in_once_per_operation() {
        let mut s = state();
        let r1 = s.desk_scan(scan(1, 1)).unwrap();
        let r2 = s.desk_scan(scan(1, 1)).unwrap();
        assert!(r1.ticket.checked_in);
        assert_eq!(r1, r2);
        assert_eq!(s.scan_log_count, 1);
        s.desk_scan(scan(1, 2)).unwrap();
        assert_eq!(s.scan_log_count, 2);
    }

    #[test]
    fn first_scan_checks_in_and_every_later_scan_reports_that_time() {
        let mut s = state();
        let at: chrono::DateTime<Utc> = "2026-09-26T09:14:03Z".parse().unwrap();
        let request = DeskScanReq {
            public_id: id(1),
            operation_id: id(900),
            captured_at: at,
        };
        let first = s.desk_scan(request.clone()).unwrap();
        assert_eq!(first.check_in.result, CheckInResult::CheckedIn);
        assert_eq!(first.check_in.checked_in_at, at);

        let next = s
            .desk_scan(DeskScanReq {
                operation_id: id(901),
                captured_at: at + chrono::Duration::minutes(5),
                ..request.clone()
            })
            .unwrap();
        assert_eq!(next.check_in.result, CheckInResult::AlreadyCheckedIn);
        assert_eq!(next.check_in.checked_in_at, first.check_in.checked_in_at);

        assert_eq!(
            s.desk_scan(request).unwrap(),
            first,
            "replaying the first operation returns its original answer"
        );
        assert_eq!(s.scan_operation_count(), 2);
    }

    #[test]
    fn desk_scan_rejects_bad_tickets() {
        let mut s = state();
        assert_eq!(s.desk_scan(scan(99, 1)).unwrap_err().0, 404);
        assert_eq!(
            s.desk_scan(scan(3, 2)).unwrap_err().1.error,
            ErrorCode::TicketUnpaid
        );
        assert_eq!(
            s.desk_scan(scan(4, 3)).unwrap_err().1.error,
            ErrorCode::TicketCancelled
        );
    }

    #[test]
    fn bind_creates_then_replays() {
        let mut s = state();
        let (st, r) = s.bind(bind_req(1, TAG_A, 1)).unwrap();
        assert_eq!(st, 201);
        assert_eq!(r.binding.tag_key, TAG_A);
        let (st2, r2) = s.bind(bind_req(1, TAG_A, 1)).unwrap();
        assert_eq!((st2, r2.binding.id), (200, r.binding.id));
        assert_eq!(s.active_bindings().len(), 1);
    }

    #[test]
    fn bind_conflicts_need_replace_with_reason() {
        let mut s = state();
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        let e = s.bind(bind_req(2, TAG_A, 2)).unwrap_err();
        assert_eq!((e.0, e.1.error), (409, ErrorCode::UidBoundElsewhere));
        assert_eq!(e.1.holder.unwrap().name, "Aina");
        let e = s.bind(bind_req(1, TAG_B, 3)).unwrap_err();
        assert_eq!((e.0, e.1.error), (409, ErrorCode::TicketHasSticker));
        assert_eq!(
            s.bind(replace(bind_req(2, TAG_A, 4), " "))
                .unwrap_err()
                .1
                .error,
            ErrorCode::ReasonRequired
        );
        let (st, r) = s
            .bind(replace(bind_req(2, TAG_A, 5), "badge swapped"))
            .unwrap();
        assert_eq!(st, 201);
        assert_eq!(r.revoked.len(), 1);
        assert_eq!(s.active_bindings().len(), 1);
        assert_eq!(s.active_bindings()[0].public_id, id(2));
    }

    #[test]
    fn revoked_sticker_can_be_reused() {
        let mut s = state();
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        s.bind(replace(bind_req(1, TAG_B, 2), "lost")).unwrap();
        let (st, _) = s.bind(bind_req(2, TAG_A, 3)).unwrap();
        assert_eq!(st, 201);
    }

    #[test]
    fn bind_rejects_bad_ticket_and_bad_hex() {
        let mut s = state();
        assert_eq!(
            s.bind(bind_req(3, TAG_A, 1)).unwrap_err().1.error,
            ErrorCode::TicketUnpaid
        );
        assert_eq!(
            s.bind(bind_req(1, "XYZ", 2)).unwrap_err().1.error,
            ErrorCode::Malformed
        );
    }

    #[test]
    fn lookup_reports_holder() {
        let mut s = state();
        assert_eq!(s.lookup(TAG_A).binding, None);
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        assert_eq!(s.lookup(TAG_A).holder.unwrap().name, "Aina");
    }

    #[test]
    fn observation_outcomes() {
        let mut s = state();
        assert_eq!(
            s.observe("gate-in", obs(1, TAG_A, Role::Entry)).outcome,
            Outcome::UnknownTag
        );
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        let r = s.observe("gate-in", obs(2, TAG_A, Role::Entry));
        assert_eq!(r.outcome, Outcome::Accepted);
        assert_eq!(r.display.name.as_deref(), Some("Aina"));
        assert_eq!(r.anomalies, vec!["entered_without_check_in".to_string()]);
        s.bind(replace(bind_req(1, TAG_B, 2), "lost")).unwrap();
        assert_eq!(
            s.observe("gate-in", obs(3, TAG_A, Role::Entry)).outcome,
            Outcome::RevokedTag
        );
    }

    #[test]
    fn observation_replay_and_sequence_duplicates() {
        let mut s = state();
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        let first = s.observe("gate-in", obs(1, TAG_A, Role::Exit));
        let again = s.observe("gate-in", obs(1, TAG_A, Role::Exit));
        assert_eq!(first, again);
        assert_eq!(s.received_order.len(), 1);
        let mut a = obs(2, TAG_A, Role::Exit);
        a.device_record_seq = Some(77);
        let mut b = obs(3, TAG_A, Role::Exit);
        b.device_record_seq = Some(77);
        assert_eq!(s.observe("gate-out", a).outcome, Outcome::Accepted);
        assert_eq!(s.observe("gate-out", b).outcome, Outcome::PossibleDuplicate);
    }

    #[test]
    fn observation_anomalies() {
        let mut s = state();
        s.desk_scan(scan(1, 1)).unwrap();
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        let mut dir = obs(1, TAG_A, Role::Exit);
        dir.device_direction_raw = Some(0);
        let r = s.observe("gate-out", dir);
        assert_eq!(r.outcome, Outcome::Accepted);
        assert_eq!(r.anomalies, vec!["role_mismatch".to_string()]);
        let mut wrong_payload = obs(2, TAG_A, Role::Entry);
        wrong_payload.payload_hex = Some(hex_upper(&codec::encode(id(2))));
        let r = s.observe("gate-in", wrong_payload);
        assert_eq!(r.outcome, Outcome::TicketInvalid);
        assert!(r
            .anomalies
            .contains(&"payload_binding_mismatch".to_string()));
    }

    #[test]
    fn replaying_a_scan_operation_commits_one_scan_log() {
        let mut s = state();
        s.desk_scan(scan(1, 1)).unwrap();
        s.desk_scan(scan(1, 1)).unwrap();
        assert_eq!(s.scan_log_count, 1);
        assert_eq!(s.scan_operation_count(), 1);
        // The same ticket under a new operation is a second logical operation.
        s.desk_scan(scan(1, 2)).unwrap();
        assert_eq!((s.scan_log_count, s.scan_operation_count()), (2, 2));
    }

    #[test]
    fn replaying_a_binding_operation_commits_one_binding_row() {
        let mut s = state();
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        assert_eq!(s.binding_count(), 1);
        assert_eq!(s.binding_operation_count(), 1);
        // A new operation for the same ticket and sticker: the row count cannot
        // tell this apart from a replayed operation, the operation count can.
        s.bind(bind_req(1, TAG_A, 2)).unwrap();
        assert_eq!(s.binding_count(), 1);
        assert_eq!(s.binding_operation_count(), 2);
        // A replacement keeps the revoked row, so rows and operations both grow.
        s.bind(replace(bind_req(1, TAG_B, 3), "lost")).unwrap();
        assert_eq!((s.binding_count(), s.active_bindings().len()), (2, 1));
        assert_eq!(s.binding_operation_count(), 3);
    }

    #[test]
    fn replaying_a_delivery_id_keeps_one_result_per_station() {
        let mut s = state();
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        let first = s.observe("gate-in", obs(1, TAG_A, Role::Entry));
        assert_eq!(first.outcome, Outcome::Accepted);
        assert_eq!(s.observe("gate-in", obs(1, TAG_A, Role::Entry)), first);
        let results = s.observation_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "gate-in");
        assert_eq!(results[0].1, first);

        // The same delivery ID at another station is a distinct mock key, and
        // it is replayed there too rather than re-evaluated.
        let exit = s.observe("gate-out", obs(1, TAG_A, Role::Exit));
        assert_eq!(exit.delivery_id, first.delivery_id);
        assert_eq!(s.observe("gate-out", obs(1, TAG_A, Role::Entry)), exit);
        let results = s.observation_results();
        assert_eq!(
            results
                .iter()
                .map(|(station, r)| (station.as_str(), r.delivery_id))
                .collect::<Vec<_>>(),
            vec![
                ("gate-in", first.delivery_id),
                ("gate-out", exit.delivery_id)
            ],
            "sorted by station, one result per station and delivery"
        );
        assert_eq!(s.observation_count(), 2);
        assert_eq!(s.received_order.len(), 2);
    }

    #[test]
    fn cache_lists_tickets_bindings_and_revocations() {
        let mut s = state();
        s.bind(bind_req(1, TAG_A, 1)).unwrap();
        s.bind(replace(bind_req(1, TAG_B, 2), "lost")).unwrap();
        let c = s.cache();
        assert_eq!(c.tickets.len(), 4);
        assert_eq!(c.bindings.len(), 1);
        assert_eq!(c.revoked_tag_keys, vec![TAG_A.to_string()]);
    }
}
