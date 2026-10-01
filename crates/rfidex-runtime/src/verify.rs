//! Sticker verification: hold a sticker near the desk reader and see whose it
//! is. A question, never an action: no check-in, no print, no binding, no queue
//! row. The answer needs the server, because the sticker holds an ID and the
//! name lives on the ticket.
//!
//! Every sentence is written here so the screen only renders it.

use rfidex_core::client::ApiError;
use rfidex_core::contract::{LookupResp, TicketSummary};
use rfidex_core::device::DeviceError;
use rfidex_core::station::desk::DeskError;

pub const WAITING_MESSAGE: &str = "Hold your tag near the reader.";
const VERIFIED_MESSAGE: &str = "Your tag is good to go. Enjoy the event!";
const INVALID_MESSAGE: &str = "Your tag needs a little attention. Please ask our team for help.";
const UNKNOWN_MESSAGE: &str =
    "Your tag isn't linked to a participant yet. Our team can help you get set up.";
const LOST_TICKET_MESSAGE: &str =
    "We couldn't find the participant linked to your tag. Please ask our team for help.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyState {
    /// No sticker near the reader.
    Waiting,
    /// A linked sticker on a valid ticket.
    Verified,
    /// A linked sticker whose ticket is unpaid or cancelled.
    Invalid,
    /// A sticker nobody has linked.
    Unknown,
    /// Something stopped the check: the reader, the server, or several stickers.
    Problem,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VerifyView {
    pub state: VerifyState,
    pub code: Option<String>,
    pub message: String,
    pub holder: Option<TicketSummary>,
    /// The sticker's own ID, so the screen can tell one tap from the next.
    pub sticker: Option<String>,
}

impl VerifyView {
    pub fn waiting() -> VerifyView {
        VerifyView {
            state: VerifyState::Waiting,
            code: None,
            message: WAITING_MESSAGE.to_string(),
            holder: None,
            sticker: None,
        }
    }

    fn problem(code: &str, message: &str) -> VerifyView {
        VerifyView {
            state: VerifyState::Problem,
            code: Some(code.to_string()),
            message: message.to_string(),
            holder: None,
            sticker: None,
        }
    }

    /// True for an answer worth remembering while the same sticker stays put.
    /// A problem is asked again on the next poll.
    pub fn is_final(&self) -> bool {
        self.state != VerifyState::Problem && self.state != VerifyState::Waiting
    }
}

/// The server's answer for one sticker.
pub fn answer(sticker: &str, reply: LookupResp) -> VerifyView {
    let (state, message, holder) = match (reply.binding, reply.holder) {
        (None, _) => (VerifyState::Unknown, UNKNOWN_MESSAGE, None),
        (Some(_), None) => {
            return VerifyView {
                sticker: Some(sticker.to_string()),
                ..VerifyView::problem("ticket_missing", LOST_TICKET_MESSAGE)
            }
        }
        (Some(_), Some(holder)) if holder.valid => {
            (VerifyState::Verified, VERIFIED_MESSAGE, Some(holder))
        }
        (Some(_), Some(holder)) => (VerifyState::Invalid, INVALID_MESSAGE, Some(holder)),
    };
    VerifyView {
        state,
        code: None,
        message: message.to_string(),
        holder,
        sticker: Some(sticker.to_string()),
    }
}

/// Whether the server's answer is a firm "no" for a gate: nobody holds the
/// sticker, or its ticket is invalid. The same facts `answer` turns into
/// Unknown and Invalid, so a guest who verifies as a pass never alarms. A
/// sticker bound to a ticket the server cannot find is a data problem, not a
/// reason to sound the alarm on a guest.
pub fn gate_declines(reply: &LookupResp) -> bool {
    match (&reply.binding, &reply.holder) {
        (None, _) => true,
        (Some(_), Some(holder)) => !holder.valid,
        (Some(_), None) => false,
    }
}

/// Why the server could not answer, in words for the operator.
pub fn server_problem(e: &ApiError) -> VerifyView {
    match e {
        ApiError::Unauthorized => VerifyView::problem(
            "unauthorized",
            "The server did not accept the API key. Open Setup to check it.",
        ),
        ApiError::Retryable(_) => VerifyView::problem(
            "offline",
            "Verifying needs a connection to the server. Reconnect and try again.",
        ),
        ApiError::BadResponse(_) => VerifyView::problem(
            "server_reply",
            "The server sent a reply this app cannot read. Ask for help.",
        ),
        ApiError::Rejected { .. } => VerifyView::problem(
            "server_rejected",
            "The server could not check this sticker. Try again.",
        ),
    }
}

/// What a failed reader read means. No sticker is not a failure.
pub fn reader_problem(e: &DeskError) -> VerifyView {
    match e {
        DeskError::NoTag | DeskError::Device(DeviceError::TagNotFound) => VerifyView::waiting(),
        DeskError::MultipleTags(_) => {
            VerifyView::problem("multiple_tags", "Hold one sticker at a time.")
        }
        DeskError::Device(DeviceError::Disconnected) => VerifyView::problem(
            "reader_disconnected",
            "Reader disconnected. Reconnect it and try again.",
        ),
        _ => VerifyView::problem(
            "reader_error",
            "The reader could not read the sticker. Try again.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rfidex_core::contract::{BindMode, BindingInfo};
    use rfidex_core::tag::Protocol;
    use uuid::Uuid;

    fn binding() -> BindingInfo {
        BindingInfo {
            id: 1,
            public_id: Uuid::nil(),
            protocol: Protocol::Iso15693,
            uid_raw_hex: "AA".into(),
            tag_key: "AA".into(),
            mode: BindMode::Bind,
        }
    }

    fn holder(valid: bool) -> TicketSummary {
        TicketSummary {
            public_id: Uuid::nil(),
            name: "Aina".into(),
            ticket_type: "VIP".into(),
            valid,
            checked_in: true,
        }
    }

    #[test]
    fn the_gate_declines_only_what_verify_would_not_pass() {
        let reply = |binding, holder| LookupResp { binding, holder };
        assert!(gate_declines(&reply(None, None)));
        assert!(gate_declines(&reply(Some(binding()), Some(holder(false)))));
        assert!(!gate_declines(&reply(Some(binding()), Some(holder(true)))));
        assert!(!gate_declines(&reply(Some(binding()), None)));
        // Whatever verify passes, the gate never declines.
        let passed = reply(Some(binding()), Some(holder(true)));
        assert_eq!(answer("AA", passed.clone()).state, VerifyState::Verified);
        assert!(!gate_declines(&passed));
    }

    #[test]
    fn an_unlinked_sticker_is_unknown() {
        let view = answer(
            "AA",
            LookupResp {
                binding: None,
                holder: None,
            },
        );
        assert_eq!(view.state, VerifyState::Unknown);
        assert!(view.holder.is_none());
        assert!(view.is_final());
    }

    #[test]
    fn a_linked_sticker_shows_its_holder() {
        let view = answer(
            "AA",
            LookupResp {
                binding: Some(binding()),
                holder: Some(holder(true)),
            },
        );
        assert_eq!(view.state, VerifyState::Verified);
        assert_eq!(view.holder.unwrap().name, "Aina");
    }

    #[test]
    fn a_linked_sticker_on_an_invalid_ticket_is_not_verified() {
        let view = answer(
            "AA",
            LookupResp {
                binding: Some(binding()),
                holder: Some(holder(false)),
            },
        );
        assert_eq!(view.state, VerifyState::Invalid);
        assert!(view.holder.is_some());
    }

    #[test]
    fn a_binding_without_a_ticket_is_a_problem_to_ask_again() {
        let view = answer(
            "AA",
            LookupResp {
                binding: Some(binding()),
                holder: None,
            },
        );
        assert_eq!(view.state, VerifyState::Problem);
        assert!(!view.is_final());
    }

    #[test]
    fn no_sticker_is_waiting_not_an_error() {
        assert_eq!(
            reader_problem(&DeskError::NoTag).state,
            VerifyState::Waiting
        );
        assert_eq!(
            reader_problem(&DeskError::MultipleTags(2)).code.as_deref(),
            Some("multiple_tags")
        );
    }
}
