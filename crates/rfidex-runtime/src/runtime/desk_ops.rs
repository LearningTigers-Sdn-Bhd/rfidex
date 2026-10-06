//! What a desk does: scan, link, search, verify and print.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use rfidex_core::contract::{SearchBy, StationKind};
use uuid::Uuid;

use super::device::StationDevice;
use super::messages::{wrong_station, DIFFERENT_EVENT};
use super::station::StationRuntime;
use super::Runtime;
use crate::desk::{DeskSession, DeskView};
use crate::RuntimeError;

impl Runtime {
    /// Read a scanned ticket code at a desk. Business failures come back as an
    /// error step the desk screen can show; only a wrong station is rejected.
    pub async fn desk_scan(&self, station: Uuid, code: &str) -> Result<DeskView, RuntimeError> {
        let (runtime, mut session) = self.desk_session(station).await?;
        if runtime.settings().is_none() {
            return Ok(session.connect_first());
        }
        Ok(crate::desk::scan(&mut session, &runtime.store, code).await)
    }

    /// Link the sticker the operator just tapped.
    ///
    /// The reader half of this runs synchronously and can block for as long as
    /// the reader's own deadline, so it is moved off the async workers entirely:
    /// the existing linking workflow runs on a blocking worker with the Tokio
    /// handle, which keeps its confirmation, payload and print semantics
    /// unchanged while a stalled reader stays unable to occupy a worker thread.
    pub async fn desk_link(
        &self,
        station: Uuid,
        reason: Option<String>,
    ) -> Result<DeskView, RuntimeError> {
        let runtime = self.station(station)?.clone();
        let refresh = runtime.clone();
        let handle = tokio::runtime::Handle::current();
        let view = self
            .hardware_jobs
            .run(move || {
                handle.block_on(async move {
                    let StationDevice::Desk(device) = &runtime.device else {
                        return Err(wrong_station("link stickers"));
                    };
                    let mut session = device.lock().await;
                    if runtime.settings().is_none() {
                        return Ok(session.connect_first());
                    }
                    Ok(crate::desk::link(&mut session, &runtime.store, reason).await)
                })
            })
            .await;
        refresh.refresh_desk_connection().await;
        view?
    }

    pub async fn desk_reset(&self, station: Uuid) -> Result<DeskView, RuntimeError> {
        let (_, mut session) = self.desk_session(station).await?;
        Ok(crate::desk::reset(&mut session))
    }

    /// Find a guest by name, email or phone. A question, never an action: no
    /// check-in, no print, no queue row, and no lock held over the request.
    pub async fn desk_search(
        &self,
        station: Uuid,
        by: SearchBy,
        query: &str,
    ) -> Result<crate::search::SearchView, RuntimeError> {
        let runtime = self.station(station)?;
        if runtime.kind() != StationKind::Desk {
            return Err(wrong_station("search for a ticket"));
        }
        // Rows belong to another event: searching would show this desk the
        // wrong guests, so the cache is not used either.
        if runtime.event_mismatch() {
            return Err(RuntimeError::new("different_event", DIFFERENT_EVENT));
        }
        // Only a heartbeat that failed on the network skips the server; a
        // rejected key or a station not heard from yet still asks it.
        let known_offline = runtime.lock().network_error.is_some();
        crate::search::desk_search(&runtime.client, &runtime.store, !known_offline, by, query).await
    }

    /// One look at the desk reader for the Verify screen: whose sticker is
    /// this? Asked again and again while the screen is open, so it never holds
    /// a lock over the server and asks the server once per tap.
    pub async fn desk_verify(
        &self,
        station: Uuid,
    ) -> Result<crate::verify::VerifyView, RuntimeError> {
        use crate::verify::{answer, reader_problem, server_problem};
        let runtime = self.station(station)?.clone();
        let refresh = runtime.clone();
        let handle = tokio::runtime::Handle::current();
        let read = self
            .hardware_jobs
            .run(move || {
                handle.block_on(async move {
                    let StationDevice::Desk(device) = &runtime.device else {
                        return Err(wrong_station("verify stickers"));
                    };
                    let mut session = device.lock().await;
                    let read = session.station.detect_tag();
                    // A sticker that is gone forgets its answer, so the next
                    // tap of the same sticker asks the server afresh.
                    if read.is_err() {
                        session.verified = None;
                    }
                    let cached = match &read {
                        Ok(tag) => session.verified.clone().filter(|v| {
                            v.sticker.as_deref() == Some(&rfidex_core::tag::hex_upper(&tag.uid_raw))
                        }),
                        Err(_) => None,
                    };
                    Ok((read, cached))
                })
            })
            .await;
        refresh.refresh_desk_connection().await;
        let (read, cached) = read??;
        let tag = match read {
            Ok(tag) => tag,
            Err(e) => return Ok(reader_problem(&e)),
        };
        if let Some(cached) = cached {
            return Ok(cached);
        }
        let sticker = rfidex_core::tag::hex_upper(&tag.uid_raw);
        let view = match refresh.client.lookup(&sticker).await {
            Ok(reply) => answer(&sticker, reply),
            Err(e) => server_problem(&e),
        };
        if view.is_final() {
            if let StationDevice::Desk(device) = &refresh.device {
                device.lock().await.verified = Some(view.clone());
            }
        }
        Ok(view)
    }

    /// Print the badge for the guest on screen. The same call serves the first
    /// print and every Reprint: Rust never prints on its own.
    ///
    /// The desk lock is held only long enough to check the session and copy
    /// what is needed; the HTTP call happens with no lock held, so a print can
    /// never delay the sticker.
    pub async fn desk_print(
        &self,
        station: Uuid,
        session_id: Uuid,
    ) -> Result<crate::BadgeView, RuntimeError> {
        let runtime = self.station(station)?;
        let StationDevice::Desk(device) = &runtime.device else {
            return Err(wrong_station("print a badge"));
        };
        let printer_url = runtime.config.printer_url.clone();
        let selection = self.badge.selection();

        let prepared = {
            let mut session = device.lock().await;
            if session.session_id != session_id {
                return Err(RuntimeError::new(
                    "stale_session",
                    "That badge belongs to a guest who is no longer on screen. Scan the ticket again.",
                ));
            }
            let Some(ticket) = session.ticket.clone() else {
                return Err(RuntimeError::new(
                    "scan_first",
                    crate::desk::SCAN_FIRST_MESSAGE,
                ));
            };
            if !runtime.online() {
                // There is no printer when this PC cannot reach the event
                // server: event-printing looks the ticket up there.
                session.set_badge(crate::BadgeView {
                    print_now: false,
                    can_reprint: true,
                    hold: true,
                    message: Some(crate::desk::OFFLINE_PRINT_MESSAGE.to_string()),
                });
                return Ok(session.badge.clone());
            }
            if session.printing.swap(true, Ordering::SeqCst) {
                // One print in flight per desk: this press joins it.
                return Ok(session.badge.clone());
            }
            session.set_badge(crate::BadgeView {
                print_now: false,
                can_reprint: false,
                hold: true,
                message: Some(crate::desk::PRINTING_MESSAGE.to_string()),
            });
            (ticket.public_id, session.printing.clone())
        };

        let outcome = if let Some(selection) = selection {
            let native = async {
                let event_id = runtime
                    .settings()
                    .map(|s| s.event.event_id)
                    .ok_or_else(|| {
                        RuntimeError::new("print_failed", crate::desk::OFFLINE_PRINT_MESSAGE)
                    })?;
                let data = runtime
                    .client
                    .badge_ticket(event_id, prepared.0)
                    .await
                    .map_err(|_| {
                        RuntimeError::new("print_failed", crate::desk::PRINT_FAILED_MESSAGE)
                    })?;
                {
                    let session = device.lock().await;
                    if session.session_id != session_id {
                        return Err(RuntimeError::new("stale_session","That badge belongs to a guest who is no longer on screen. Scan the ticket again."));
                    }
                }
                let ticket =
                    rfidex_badge::backend::ticket_from_backend(&data, &selection.settings.layout)
                        .ok_or_else(|| {
                        RuntimeError::new("print_failed", crate::desk::PRINT_FAILED_MESSAGE)
                    })?;
                let badge = self.badge.clone();
                tokio::task::spawn_blocking(move || {
                    badge.print_selected(&selection, &ticket, &prepared.0.to_string())
                })
                .await
                .map_err(|_| RuntimeError::new("print_failed", crate::desk::PRINT_FAILED_MESSAGE))?
            };
            tokio::time::timeout(Duration::from_secs(15), native)
                .await
                .unwrap_or_else(|_| {
                    Err(RuntimeError::new(
                        "print_failed",
                        crate::desk::PRINT_FAILED_MESSAGE,
                    ))
                })
        } else {
            match crate::PrinterClient::new(&printer_url) {
                Ok(client) => client.reprint(prepared.0).await,
                Err(e) => Err(e),
            }
        };

        let mut session = device.lock().await;
        prepared.1.store(false, Ordering::SeqCst);
        if session.session_id != session_id {
            // A newer scan owns the screen: the job went out for the guest who
            // was on it, and its result must not land on anyone else.
            return Ok(session.badge.clone());
        }
        session.set_badge(match outcome {
            // "Accepted" is not "paper came out": keep the guest and Reprint
            // on screen until the next scan replaces them.
            Ok(()) => crate::BadgeView {
                print_now: false,
                can_reprint: true,
                hold: true,
                message: Some(crate::desk::PRINTED_MESSAGE.to_string()),
            },
            Err(error) => crate::BadgeView {
                print_now: false,
                can_reprint: true,
                hold: true,
                message: Some(error.message),
            },
        });
        Ok(session.badge.clone())
    }

    async fn desk_session(
        &self,
        id: Uuid,
    ) -> Result<
        (
            &Arc<StationRuntime>,
            tokio::sync::MutexGuard<'_, DeskSession>,
        ),
        RuntimeError,
    > {
        let runtime = self.station(id)?;
        let StationDevice::Desk(device) = &runtime.device else {
            return Err(wrong_station("use this at a desk"));
        };
        Ok((runtime, device.lock().await))
    }
}
