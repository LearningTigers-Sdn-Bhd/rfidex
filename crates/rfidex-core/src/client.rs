//! HTTP client for the device API, with retry classification.

use std::time::Duration;

use reqwest::header::AUTHORIZATION;
use reqwest::{RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;

use crate::contract::*;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The text is a plain sentence naming what went wrong (no URL, no key), so
    /// the operator screen and the saved outbox row can both show it as is.
    #[error("temporary failure: {0}")]
    Retryable(String),
    #[error("api key rejected")]
    Unauthorized,
    /// HTTP 2xx whose body does not match the contract (server/app version drift).
    #[error("unreadable server reply: {0}")]
    BadResponse(String),
    /// `body` is boxed: `ErrorBody` is ~184 bytes and would otherwise make
    /// every `Result<_, ApiError>` (and the errors wrapping it) oversized.
    #[error("rejected with {status}: {}", body.message)]
    Rejected { status: u16, body: Box<ErrorBody> },
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a request never got an answer, in words an operator can act on. Walks
/// the error's causes because reqwest's own text is only "error sending
/// request". The URL is never included.
fn explain_transport(e: &reqwest::Error) -> String {
    let mut chain = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        chain.push(' ');
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    let chain = chain.to_lowercase();
    let reason = if e.is_timeout() || chain.contains("timed out") {
        "the server did not answer in time"
    } else if chain.contains("dns")
        || chain.contains("lookup address")
        || chain.contains("name or service")
        || chain.contains("no such host")
        || chain.contains("nodename")
    {
        "the server address was not found (check the address and the internet connection)"
    } else if chain.contains("refused") {
        "the server refused the connection (it may be down or restarting)"
    } else if chain.contains("certificate") || chain.contains("tls") || chain.contains("ssl") {
        "the secure connection failed (check this computer's date and time)"
    } else if chain.contains("unreachable") || chain.contains("network is down") {
        "there is no network route to the server (check the cable or Wi-Fi)"
    } else if e.is_connect() {
        "could not connect to the server (check the internet connection)"
    } else {
        "the connection to the server failed"
    };
    reason.to_string()
}

fn explain_status(status: StatusCode) -> String {
    match status.as_u16() {
        429 => "the server is limiting requests from this network (HTTP 429)".to_string(),
        502..=504 => format!(
            "the server is restarting or busy (HTTP {})",
            status.as_u16()
        ),
        code => format!("the server had an internal error (HTTP {code})"),
    }
}

#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
    station: String,
}

impl ApiClient {
    pub fn new(base: &str, api_key: &str, station: &str, timeout: Duration) -> ApiClient {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            // A server that cannot be reached at all fails fast instead of
            // holding a desk scan for the whole request timeout.
            .connect_timeout(CONNECT_TIMEOUT.min(timeout))
            .build()
            .expect("reqwest client");
        ApiClient {
            http,
            base: base.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            station: station.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub async fn heartbeat(&self, req: &HeartbeatReq) -> Result<HeartbeatResp, ApiError> {
        self.send(self.http.post(self.url(paths::HEARTBEAT)).json(req))
            .await
    }

    pub async fn cache(&self) -> Result<CacheResp, ApiError> {
        self.send(self.http.get(self.url(paths::CACHE))).await
    }

    pub async fn desk_scan(&self, req: &DeskScanReq) -> Result<DeskScanResp, ApiError> {
        self.send(self.http.post(self.url(paths::DESK_SCANS)).json(req))
            .await
    }

    pub async fn bind(&self, req: &BindingReq) -> Result<BindingResp, ApiError> {
        self.send(self.http.post(self.url(paths::BINDINGS)).json(req))
            .await
    }

    /// Search is a query string, never a built URL: the caller's text is
    /// percent-encoded by reqwest, so it cannot change the request.
    pub async fn search_tickets(
        &self,
        by: SearchBy,
        query: &str,
    ) -> Result<TicketSearchResp, ApiError> {
        self.send(
            self.http
                .get(self.url(paths::TICKET_SEARCH))
                .query(&[("by", by.as_str()), ("q", query)]),
        )
        .await
    }

    pub async fn lookup(&self, uid_raw_hex: &str) -> Result<LookupResp, ApiError> {
        self.send(
            self.http
                .get(self.url(paths::LOOKUP))
                .query(&[("uid_raw_hex", uid_raw_hex)]),
        )
        .await
    }

    pub async fn observations(
        &self,
        items: &[ObservationItem],
    ) -> Result<ObservationsResp, ApiError> {
        let body = ObservationsReq {
            observations: items.to_vec(),
        };
        self.send(self.http.post(self.url(paths::OBSERVATIONS)).json(&body))
            .await
    }

    async fn send<T: DeserializeOwned>(&self, rb: RequestBuilder) -> Result<T, ApiError> {
        let resp = rb
            .header(AUTHORIZATION, &self.api_key)
            .header(HEADER_STATION, &self.station)
            .send()
            .await
            .map_err(|e| ApiError::Retryable(explain_transport(&e)))?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<T>()
                .await
                .map_err(|e| ApiError::BadResponse(e.to_string()));
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(ApiError::Retryable(explain_status(status)));
        }
        let body = resp
            .json::<ErrorBody>()
            .await
            .unwrap_or_else(|_| ErrorBody {
                error: ErrorCode::Malformed,
                message: format!("http {status}"),
                holder: None,
                binding: None,
            });
        Err(ApiError::Rejected {
            status: status.as_u16(),
            body: Box::new(body),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreachable_server_is_named_without_its_address() {
        let client = ApiClient::new("http://127.0.0.1:1", "key", "st", Duration::from_secs(3));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        match rt.block_on(client.cache()) {
            Err(ApiError::Retryable(text)) => {
                // Windows takes over 2 seconds to refuse a closed port, so the
                // connect timeout can answer first; both are honest.
                assert!(
                    text.contains("refused") || text.contains("did not answer"),
                    "{text}"
                );
                assert!(
                    !text.contains("127.0.0.1"),
                    "no address in the text: {text}"
                );
            }
            other => panic!("expected a retryable failure, got {other:?}"),
        }
    }

    #[test]
    fn server_statuses_are_named() {
        assert!(explain_status(StatusCode::SERVICE_UNAVAILABLE).contains("restarting"));
        assert!(explain_status(StatusCode::BAD_GATEWAY).contains("502"));
        assert!(explain_status(StatusCode::TOO_MANY_REQUESTS).contains("limiting"));
        assert!(explain_status(StatusCode::INTERNAL_SERVER_ERROR).contains("500"));
    }
}
