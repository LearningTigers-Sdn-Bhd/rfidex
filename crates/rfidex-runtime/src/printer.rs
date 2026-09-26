//! The badge printer app on this PC: a separate, credential-free HTTP client.
//!
//! event-printing is asked to print one ticket id and applies its own badge
//! layout, so nothing here builds a badge, maps fields, keeps a job or retries.
//! A timeout may already have printed, so only staff can decide to try again —
//! which is why every failure is a sentence that ends in "press Reprint".

use std::time::Duration;

use uuid::Uuid;

use crate::config::validate_printer_url;
use crate::diagnostics::ConnectionView;
use crate::RuntimeError;

/// The whole request must finish inside this. Independent of the five-second
/// EventzFlow API timeout: a badge job is allowed to take longer.
pub const PRINT_TIMEOUT: Duration = Duration::from_secs(10);

const NOT_RUNNING: &str = "Printer app not running on this PC";
const NOT_PRINTED: &str = "Badge not printed — press Reprint";

/// Only `ok` and the printer's name are read. `output_dir` and `version` are
/// ignored: the desk has no use for them and does not show them.
#[derive(serde::Deserialize)]
struct HealthBody {
    ok: bool,
    printer: String,
}

/// Only the two booleans are read. The returned `ticket`, `pdf` and
/// `print_job` are deliberately ignored: the desk must not save or show them.
#[derive(serde::Deserialize)]
struct ReprintBody {
    ok: bool,
    reprinted: bool,
}

#[derive(Clone)]
pub struct PrinterClient {
    base: reqwest::Url,
    http: reqwest::Client,
}

impl PrinterClient {
    /// The production client. Setup and every desk call use this one.
    pub fn new(url: &str) -> Result<Self, RuntimeError> {
        Self::with_timeout(url, PRINT_TIMEOUT)
    }

    /// A shorter deadline is for this module's own tests only: the ten-second
    /// production value is a constant and is never passed in from outside.
    fn with_timeout(url: &str, timeout: Duration) -> Result<Self, RuntimeError> {
        let base = validate_printer_url(url).map_err(|e| match e {
            crate::ConfigError::Invalid(message) => RuntimeError::new("invalid_setup", &message),
            other => RuntimeError::new("invalid_setup", &other.to_string()),
        })?;
        let http = reqwest::Client::builder()
            .timeout(timeout)
            // A printer on this PC is reached directly: an environment proxy
            // must not be asked, and a redirect must never send this request
            // to an address the operator did not choose.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| not_running())?;
        Ok(PrinterClient { base, http })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base.as_str().trim_end_matches('/'))
    }

    /// The printer app's name, when it answers that it is working.
    pub async fn health(&self) -> Result<String, RuntimeError> {
        let resp = self
            .http
            .get(self.url("/health"))
            .send()
            .await
            .map_err(|_| not_running())?;
        if !resp.status().is_success() {
            return Err(not_running());
        }
        let body: HealthBody = resp.json().await.map_err(|_| not_running())?;
        if !body.ok {
            return Err(not_running());
        }
        Ok(body.printer)
    }

    /// Ask the printer app to print this ticket's badge. `Ok` means the app
    /// accepted the job, never that paper came out.
    pub async fn reprint(&self, public_id: Uuid) -> Result<(), RuntimeError> {
        let resp = self
            .http
            .post(self.url(&format!("/scan/{public_id}/reprint")))
            .send()
            .await
            .map_err(|_| not_printed())?;
        if !resp.status().is_success() {
            return Err(not_printed());
        }
        let body: ReprintBody = resp.json().await.map_err(|_| not_printed())?;
        if !body.ok || !body.reprinted {
            return Err(not_printed());
        }
        Ok(())
    }
}

fn not_running() -> RuntimeError {
    RuntimeError::new("printer_offline", NOT_RUNNING)
}

fn not_printed() -> RuntimeError {
    RuntimeError::new("print_failed", NOT_PRINTED)
}

/// Setup's "Test printer": validate an address that has not been saved, then
/// read the printer's own name. Needs no API key and changes nothing.
pub async fn test_printer(url: &str) -> ConnectionView {
    let client = match PrinterClient::new(url) {
        Ok(client) => client,
        Err(e) => return view(false, "invalid_setup", &e.message),
    };
    match client.health().await {
        Ok(printer) => view(true, "printer_ready", &format!("Printer ready: {printer}")),
        Err(_) => view(false, "printer_offline", NOT_RUNNING),
    }
}

fn view(ok: bool, code: &str, message: &str) -> ConnectionView {
    ConnectionView {
        ok,
        code: code.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rfidex_mock::printer::{Mode, Printer};

    /// A held printer plus a short deadline: the timeout path, proven over
    /// real HTTP without ever waiting ten seconds.
    #[tokio::test]
    async fn a_held_printer_ends_in_a_plain_language_timeout() {
        let printer = Printer::with_mode(Mode::Held).await;
        let client =
            PrinterClient::with_timeout(printer.base(), Duration::from_millis(150)).unwrap();
        let error = client.reprint(Uuid::from_u128(3)).await.unwrap_err();
        assert_eq!(error.message, NOT_PRINTED);
        assert_eq!(
            printer.count(),
            1,
            "the request went out once, and was not retried"
        );
        assert_eq!(
            PrinterClient::with_timeout(printer.base(), Duration::from_millis(150))
                .unwrap()
                .health()
                .await
                .unwrap_err()
                .message,
            NOT_RUNNING
        );
        printer.release();
    }

    #[tokio::test]
    async fn the_production_timeout_is_ten_seconds() {
        assert_eq!(PRINT_TIMEOUT, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn an_invalid_address_is_refused_without_a_request() {
        for url in ["http://192.168.1.50:8000", "", "not a url"] {
            let error = match PrinterClient::with_timeout(url, Duration::from_millis(50)) {
                Ok(_) => panic!("{url} must be refused"),
                Err(error) => error,
            };
            assert_eq!(error.code, "invalid_setup", "{url}");
        }
    }

    #[tokio::test]
    async fn test_printer_reports_the_printer_name() {
        let printer = Printer::start().await;
        let ok = test_printer(printer.base()).await;
        assert!(ok.ok);
        assert!(ok.message.contains("Fake Printer 2000"), "{}", ok.message);

        printer.set_mode(Mode::ServerError);
        let broken = test_printer(printer.base()).await;
        assert!(!broken.ok);
        assert_eq!(broken.message, NOT_RUNNING);

        let bad = test_printer("http://192.168.1.50:8000").await;
        assert!(!bad.ok);
        assert_eq!(bad.code, "invalid_setup");
    }
}
