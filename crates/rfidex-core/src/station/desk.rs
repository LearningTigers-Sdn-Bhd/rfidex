//! Registration desk flow (spec §3.1 Bind, §3.2 Write).

use std::sync::{Arc, Mutex};

use chrono::Utc;
use uuid::Uuid;

use crate::client::{ApiClient, ApiError};
use crate::codec::{self, PayloadError, PAYLOAD_LEN};
use crate::contract::*;
use crate::device::{DeviceError, TagMemory, TagReaderWriter};
use crate::store::{OutboxKind, Store, StoreError};
use crate::tag::{hex_upper, tag_key, TagRead, UidRule};

#[derive(Debug, Clone, PartialEq)]
pub enum Warning {
    StickerBoundElsewhere { holder: Option<TicketSummary> },
    TicketHasSticker,
    StickerHasOtherPayload { public_id: Uuid },
    StickerHasUnknownData,
}

#[derive(Debug, thiserror::Error)]
pub enum DeskError {
    #[error("ticket not found")]
    TicketNotFound,
    #[error("ticket is unpaid")]
    TicketUnpaid,
    #[error("ticket is cancelled")]
    TicketCancelled,
    #[error("ticket is not valid (offline check)")]
    TicketInvalid,
    #[error("no sticker on the reader")]
    NoTag,
    #[error("{0} stickers on the reader; keep only one")]
    MultipleTags(usize),
    #[error("sticker holds {capacity} bytes from the start block; payload needs {needed}")]
    PayloadTooLarge { capacity: usize, needed: usize },
    #[error("this reader cannot write stickers")]
    WriteUnsupported,
    #[error("write failed after retry; use another sticker")]
    WriteVerifyFailed,
    #[error("a reason is required to replace")]
    ReasonRequired,
    #[error("staff confirmation needed: {0:?}")]
    NeedsConfirm(Warning),
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scanned {
    pub ticket: TicketSummary,
    pub current_tag_key: Option<String>,
    pub offline: bool,
    /// The server's check-in outcome. `None` only for a scan this station
    /// queued itself: offline it cannot know whether the guest was already in.
    pub check_in: Option<CheckIn>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Linked {
    pub binding: Option<BindingInfo>,
    pub offline: bool,
}

pub struct DeskStation<R> {
    pub reader: R,
    store: Arc<Mutex<Store>>,
    client: ApiClient,
    mode: RfidMode,
    uid_rule: UidRule,
    write_start_block: u8,
}

impl<R: TagReaderWriter> DeskStation<R> {
    pub fn new(
        reader: R,
        store: Arc<Mutex<Store>>,
        client: ApiClient,
        mode: RfidMode,
        uid_rule: UidRule,
        write_start_block: u8,
    ) -> DeskStation<R> {
        DeskStation {
            reader,
            store,
            client,
            mode,
            uid_rule,
            write_start_block,
        }
    }

    /// Apply settings that arrived from a heartbeat or from Setup. The reader
    /// and store stay untouched, so a live station keeps working.
    pub fn configure(&mut self, mode: RfidMode, uid_rule: UidRule) {
        self.mode = mode;
        self.uid_rule = uid_rule;
    }

    pub fn mode(&self) -> RfidMode {
        self.mode
    }

    pub async fn scan_ticket(&mut self, code: &str) -> Result<Scanned, DeskError> {
        let public_id = Uuid::parse_str(code.trim()).map_err(|_| DeskError::TicketNotFound)?;
        let req = DeskScanReq {
            public_id,
            operation_id: Uuid::now_v7(),
            captured_at: Utc::now(),
        };
        match self.client.desk_scan(&req).await {
            Ok(resp) => {
                let s = self.store.lock().unwrap();
                s.upsert_ticket(&resp.ticket)?;
                // Cache the ticket's current sticker so `link` can warn before writing.
                if let Some(b) = &resp.binding {
                    s.upsert_binding(&b.tag_key, b.public_id)?;
                }
                drop(s);
                Ok(Scanned {
                    current_tag_key: resp.binding.map(|b| b.tag_key),
                    ticket: resp.ticket,
                    offline: false,
                    check_in: Some(resp.check_in),
                })
            }
            Err(ApiError::Rejected { body, .. }) => Err(match body.error {
                ErrorCode::TicketUnpaid => DeskError::TicketUnpaid,
                ErrorCode::TicketCancelled => DeskError::TicketCancelled,
                _ => DeskError::TicketNotFound,
            }),
            Err(ApiError::Retryable(_)) => self.scan_offline(req),
            Err(e) => Err(e.into()),
        }
    }

    fn scan_offline(&mut self, req: DeskScanReq) -> Result<Scanned, DeskError> {
        let s = self.store.lock().unwrap();
        let ticket = s.ticket(req.public_id)?.ok_or(DeskError::TicketNotFound)?;
        if !ticket.valid {
            return Err(DeskError::TicketInvalid);
        }
        let payload = serde_json::to_value(&req).expect("serializable");
        s.enqueue(
            OutboxKind::DeskScan,
            &req.operation_id.to_string(),
            &payload,
            req.captured_at,
        )?;
        let current_tag_key = s.binding_tag_for_ticket(req.public_id)?;
        Ok(Scanned {
            ticket,
            current_tag_key,
            offline: true,
            check_in: None,
        })
    }

    pub fn detect_tag(&mut self) -> Result<TagRead, DeskError> {
        let mut tags = self.reader.inventory()?;
        match tags.len() {
            0 => Err(DeskError::NoTag),
            1 => Ok(tags.remove(0)),
            n => Err(DeskError::MultipleTags(n)),
        }
    }

    pub async fn link(
        &mut self,
        ticket: &TicketSummary,
        tag: &TagRead,
        confirm: Option<Confirm>,
    ) -> Result<Linked, DeskError> {
        if confirm.as_ref().is_some_and(|c| c.reason.trim().is_empty()) {
            return Err(DeskError::ReasonRequired);
        }
        let key = tag_key(&tag.uid_raw, self.uid_rule);
        let uid_hex = hex_upper(&tag.uid_raw);

        let mut warning = self.precheck(ticket, &key, &uid_hex).await?;
        if self.mode == RfidMode::Write {
            if !self.reader.capabilities().write_supported {
                return Err(DeskError::WriteUnsupported);
            }
            let (mem, blocks) = self.capacity(&tag.uid_raw)?;
            if warning.is_none() {
                warning = self.payload_warning(ticket, tag, blocks)?;
            }
            if let (Some(w), None) = (&warning, &confirm) {
                return Err(DeskError::NeedsConfirm(w.clone()));
            }
            self.write_payload(ticket.public_id, tag, mem, blocks)?;
        } else if let (Some(w), None) = (&warning, &confirm) {
            return Err(DeskError::NeedsConfirm(w.clone()));
        }

        let mode = if self.mode == RfidMode::Write {
            BindMode::Written
        } else {
            BindMode::Bind
        };
        let req = BindingReq {
            public_id: ticket.public_id,
            protocol: tag.protocol,
            uid_raw_hex: uid_hex,
            mode,
            payload_version: (mode == BindMode::Written).then_some(codec::VERSION),
            operation_id: Uuid::now_v7(),
            captured_at: Utc::now(),
            replace: confirm.is_some(),
            reason: confirm.map(|c| c.reason),
        };
        match self.client.bind(&req).await {
            Ok(resp) => {
                let s = self.store.lock().unwrap();
                for old in &resp.revoked {
                    s.remove_binding(&old.tag_key)?;
                }
                s.upsert_binding(&resp.binding.tag_key, resp.binding.public_id)?;
                Ok(Linked {
                    binding: Some(resp.binding),
                    offline: false,
                })
            }
            Err(ApiError::Rejected { status, body }) => Err(match body.error {
                ErrorCode::UidBoundElsewhere => {
                    DeskError::NeedsConfirm(Warning::StickerBoundElsewhere {
                        holder: body.holder,
                    })
                }
                ErrorCode::TicketHasSticker => DeskError::NeedsConfirm(Warning::TicketHasSticker),
                ErrorCode::ReasonRequired => DeskError::ReasonRequired,
                _ => DeskError::Api(ApiError::Rejected { status, body }),
            }),
            Err(ApiError::Retryable(_)) => {
                let s = self.store.lock().unwrap();
                let payload = serde_json::to_value(&req).expect("serializable");
                s.enqueue(
                    OutboxKind::Binding,
                    &req.operation_id.to_string(),
                    &payload,
                    req.captured_at,
                )?;
                s.upsert_binding(&key, ticket.public_id)?;
                Ok(Linked {
                    binding: None,
                    offline: true,
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn precheck(
        &self,
        ticket: &TicketSummary,
        key: &str,
        uid_hex: &str,
    ) -> Result<Option<Warning>, DeskError> {
        let holder = match self.client.lookup(uid_hex).await {
            Ok(l) => l.binding.map(|b| (b.public_id, l.holder)),
            Err(ApiError::Retryable(_)) => {
                let s = self.store.lock().unwrap();
                match s.binding_holder(key)? {
                    Some(pid) => Some((pid, s.ticket(pid)?)),
                    None => None,
                }
            }
            Err(e) => return Err(e.into()),
        };
        if let Some((pid, holder)) = holder {
            return Ok(
                (pid != ticket.public_id).then_some(Warning::StickerBoundElsewhere { holder })
            );
        }
        let own = self
            .store
            .lock()
            .unwrap()
            .binding_tag_for_ticket(ticket.public_id)?;
        Ok(own.filter(|k| k != key).map(|_| Warning::TicketHasSticker))
    }

    fn capacity(&mut self, uid_raw: &[u8]) -> Result<(TagMemory, usize), DeskError> {
        let mem = self.reader.tag_memory(uid_raw)?;
        let blocks = codec::blocks_needed(mem.block_size);
        let available = mem
            .block_count
            .saturating_sub(self.write_start_block as usize);
        if blocks > available {
            return Err(DeskError::PayloadTooLarge {
                capacity: available * mem.block_size,
                needed: PAYLOAD_LEN,
            });
        }
        Ok((mem, blocks))
    }

    fn payload_warning(
        &mut self,
        ticket: &TicketSummary,
        tag: &TagRead,
        blocks: usize,
    ) -> Result<Option<Warning>, DeskError> {
        let existing =
            self.reader
                .read_blocks(&tag.uid_raw, self.write_start_block, blocks as u8)?;
        Ok(match codec::decode(&existing) {
            Ok(id) if id == ticket.public_id => None,
            Ok(id) => Some(Warning::StickerHasOtherPayload { public_id: id }),
            Err(PayloadError::Blank) => None,
            Err(_) => Some(Warning::StickerHasUnknownData),
        })
    }

    fn write_payload(
        &mut self,
        public_id: Uuid,
        tag: &TagRead,
        mem: TagMemory,
        blocks: usize,
    ) -> Result<(), DeskError> {
        let data = codec::padded(public_id, mem.block_size);
        for _attempt in 0..2 {
            if self
                .reader
                .write_blocks(&tag.uid_raw, self.write_start_block, &data)
                .is_err()
            {
                continue;
            }
            if self
                .reader
                .read_blocks(&tag.uid_raw, self.write_start_block, blocks as u8)?
                == data
            {
                return Ok(());
            }
        }
        Err(DeskError::WriteVerifyFailed)
    }
}
