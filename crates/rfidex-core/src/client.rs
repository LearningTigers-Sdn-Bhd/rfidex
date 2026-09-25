//! HTTP client for the device API, with retry classification.

use std::time::Duration;

use reqwest::header::AUTHORIZATION;
use reqwest::{RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;

use crate::contract::*;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
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
            .map_err(|e| ApiError::Retryable(e.to_string()))?;
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
            return Err(ApiError::Retryable(format!("http {status}")));
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
