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
}

struct MockTicket {
    summary: TicketSummary,
    paid: bool,
    cancelled: bool,
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
            .map(|s| {
                let summary = TicketSummary {
                    public_id: s.public_id,
                    name: s.name,
                    ticket_type: s.ticket_type,
                    valid: s.paid && !s.cancelled,
                    checked_in: false,
                };
                (
                    s.public_id,
                    MockTicket {
                        summary,
                        paid: s.paid,
                        cancelled: s.cancelled,
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
        if let Some(r) = self.desk_ops.get(&req.operation_id) {
            return Ok(r.clone());
        }
        self.check_ticket(req.public_id)?;
        let t = self.tickets.get_mut(&req.public_id).expect("checked above");
        t.summary.checked_in = true;
        let ticket = t.summary.clone();
        self.scan_log_count += 1;
        let resp = DeskScanResp {
            ticket,
            binding: self.active_for_ticket(req.public_id).cloned(),
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
