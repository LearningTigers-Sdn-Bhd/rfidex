//! Device API contract between RfiDex stations and EventzFlow (spec §4).
//! The mock server and, later, the real backend implement exactly these shapes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::tag::{Protocol, UidRule};

pub const HEADER_STATION: &str = "X-RfiDex-Station";
pub const MAX_OBSERVATION_BATCH: usize = 50;

pub mod paths {
    pub const HEARTBEAT: &str = "/v1/rfid/stations/heartbeat";
    pub const CACHE: &str = "/v1/rfid/cache";
    pub const DESK_SCANS: &str = "/v1/rfid/desk_scans";
    pub const TICKET_SEARCH: &str = "/v1/rfid/tickets/search";
    pub const BINDINGS: &str = "/v1/rfid/bindings";
    pub const LOOKUP: &str = "/v1/rfid/bindings/lookup";
    pub const OBSERVATIONS: &str = "/v1/rfid/observations";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StationKind {
    Desk,
    Gate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Entry,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RfidMode {
    Bind,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindMode {
    Bind,
    Written,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Accepted,
    UnknownTag,
    RevokedTag,
    WrongEvent,
    TicketInvalid,
    NotCheckedIn,
    PossibleDuplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    TicketNotFound,
    TicketUnpaid,
    TicketCancelled,
    UidBoundElsewhere,
    TicketHasSticker,
    ReasonRequired,
    BatchTooLarge,
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeartbeatReq {
    pub name: String,
    pub kind: StationKind,
    pub role: Option<Role>,
    pub hw_model: Option<String>,
    pub firmware: Option<String>,
    pub app_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventSettings {
    pub event_id: i64,
    pub name: String,
    pub rfid_mode: RfidMode,
    pub require_check_in: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeartbeatResp {
    pub event: EventSettings,
    pub uid_rule: UidRule,
    pub server_time: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketSummary {
    pub public_id: Uuid,
    pub name: String,
    pub ticket_type: String,
    /// Paid and not cancelled/deleted.
    pub valid: bool,
    pub checked_in: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingInfo {
    pub id: u64,
    pub public_id: Uuid,
    pub protocol: Protocol,
    pub uid_raw_hex: String,
    pub tag_key: String,
    pub mode: BindMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheResp {
    pub tickets: Vec<TicketSummary>,
    pub bindings: Vec<BindingInfo>,
    pub revoked_tag_keys: Vec<String>,
    pub server_time: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckInResult {
    /// This call made the first check-in.
    CheckedIn,
    /// The guest was already checked in, by this desk or anywhere else.
    AlreadyCheckedIn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckIn {
    pub result: CheckInResult,
    /// The first check-in time, whichever call made it.
    pub checked_in_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchBy {
    Name,
    Email,
    Phone,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketSearchItem {
    pub public_id: Uuid,
    pub name: String,
    pub ticket_type: String,
    /// Paid and not cancelled, the same rule the check-in page uses.
    pub valid: bool,
    pub checked_in: bool,
    pub checked_in_at: Option<DateTime<Utc>>,
    /// Masked by the server; the full address never leaves it.
    pub email_hint: Option<String>,
    pub phone_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketSearchResp {
    pub tickets: Vec<TicketSearchItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeskScanReq {
    pub public_id: Uuid,
    pub operation_id: Uuid,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeskScanResp {
    pub ticket: TicketSummary,
    pub binding: Option<BindingInfo>,
    pub check_in: CheckIn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingReq {
    pub public_id: Uuid,
    pub protocol: Protocol,
    pub uid_raw_hex: String,
    pub mode: BindMode,
    pub payload_version: Option<u8>,
    pub operation_id: Uuid,
    pub captured_at: DateTime<Utc>,
    #[serde(default)]
    pub replace: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingResp {
    pub binding: BindingInfo,
    pub revoked: Vec<BindingInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LookupResp {
    pub binding: Option<BindingInfo>,
    pub holder: Option<TicketSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationItem {
    pub delivery_id: Uuid,
    pub role: Role,
    pub protocol: Protocol,
    pub uid_raw_hex: String,
    pub payload_hex: Option<String>,
    pub device_direction_raw: Option<u8>,
    pub device_time_raw_hex: Option<String>,
    pub device_record_seq: Option<u64>,
    #[serde(default)]
    pub flags_raw: serde_json::Value,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationsReq {
    pub observations: Vec<ObservationItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub name: Option<String>,
    pub ticket_type: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationResult {
    pub delivery_id: Uuid,
    pub outcome: Outcome,
    #[serde(default)]
    pub anomalies: Vec<String>,
    pub display: Display,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationsResp {
    pub results: Vec<ObservationResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorCode,
    pub message: String,
    pub holder: Option<TicketSummary>,
    pub binding: Option<BindingInfo>,
}
