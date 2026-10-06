//! Operator-facing wording and the errors built from it, kept in one place so
//! the status bar, the alarm line and the station list cannot drift apart.

use rfidex_core::client::ApiError;

use crate::RuntimeError;

pub(crate) fn store_failure() -> RuntimeError {
    RuntimeError::new(
        "save_failed",
        "Could not save this action on this computer. Stop and ask for help.",
    )
}

/// Wording the operator sees, kept in one place so the status bar, the alarm
/// line and the station list cannot drift apart.
pub(super) const UNAUTHORIZED: &str =
    "The server did not accept the API key. Open Setup to check it.";
pub(super) const DIFFERENT_EVENT: &str =
    "This API key belongs to a different event. Open Setup and enter the key for this event.";
pub(super) const MANY_WAITING: &str = "Many actions are waiting to send.";
pub(super) const LOW_DISK: &str = "Disk space is low. Free some space before continuing.";
pub(super) const LINKS_NEED_ATTENTION: &str = "Some sticker links need attention.";
pub(super) const COULD_NOT_BE_SENT: &str = "Some saved actions could not be sent.";
pub(super) const COULD_NOT_CHECK_DISK: &str = "Could not check free disk space.";

pub(super) fn network_message(e: &ApiError) -> String {
    match e {
        ApiError::Unauthorized => UNAUTHORIZED.to_string(),
        ApiError::Retryable(why) => format!(
            "Cannot reach the server: {why}. Saved scans are safe and send by themselves when it returns."
        ),
        ApiError::BadResponse(_) => {
            "The server sent a reply this app cannot read. Ask for help.".to_string()
        }
        ApiError::Rejected { .. } => {
            "The server could not accept this action. Check the ticket and try again.".to_string()
        }
    }
}

pub(super) fn wrong_station(action: &str) -> RuntimeError {
    RuntimeError::new("wrong_station", &format!("This station cannot {action}."))
}

pub(super) fn reader_failed() -> RuntimeError {
    RuntimeError::new(
        "reader_config",
        "This station's reader settings cannot be used. Open Setup and check the reader.",
    )
}
