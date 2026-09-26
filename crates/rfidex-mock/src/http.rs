use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rfidex_core::contract::*;
use serde::{Deserialize, Serialize};

use crate::state::{ApiFailure, MockState};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Faults {
    pub down: bool,
    pub delay_ms: u64,
    pub fail_5xx: u32,
    pub hang_after_commit: u32,
    /// The next N requests get HTTP 200 with a body that does not match the contract.
    pub bad_body: u32,
}

pub struct AppState {
    pub mock: Mutex<MockState>,
    pub faults: Mutex<Faults>,
}

impl AppState {
    pub fn new(mock: MockState) -> AppState {
        AppState {
            mock: Mutex::new(mock),
            faults: Mutex::new(Faults::default()),
        }
    }
}

fn error(status: u16, code: ErrorCode, message: &str) -> Response {
    failure((
        status,
        Box::new(ErrorBody {
            error: code,
            message: message.into(),
            holder: None,
            binding: None,
        }),
    ))
}

fn failure((status, body): ApiFailure) -> Response {
    (
        StatusCode::from_u16(status).expect("valid status"),
        Json(body),
    )
        .into_response()
}

/// The rejection is boxed: axum's `Response` is over clippy's large-error limit.
async fn pre(s: &AppState, headers: &HeaderMap) -> Result<String, Box<Response>> {
    let f = s.faults.lock().unwrap().clone();
    if f.down {
        return Err(Box::new(error(503, ErrorCode::Malformed, "mock is down")));
    }
    if f.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(f.delay_ms)).await;
    }
    {
        let mut f = s.faults.lock().unwrap();
        if f.fail_5xx > 0 {
            f.fail_5xx -= 1;
            return Err(Box::new(error(
                500,
                ErrorCode::Malformed,
                "injected server error",
            )));
        }
        if f.bad_body > 0 {
            f.bad_body -= 1;
            return Err(Box::new(
                Json(serde_json::json!({ "unexpected": true })).into_response(),
            ));
        }
    }
    let key = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if key != s.mock.lock().unwrap().api_key {
        return Err(Box::new(error(
            401,
            ErrorCode::Unauthorized,
            "invalid api key",
        )));
    }
    headers
        .get(HEADER_STATION)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            Box::new(error(
                400,
                ErrorCode::Malformed,
                "missing X-RfiDex-Station header",
            ))
        })
}

async fn post_commit(s: &AppState) {
    let hang = {
        let mut f = s.faults.lock().unwrap();
        let hang = f.hang_after_commit > 0;
        if hang {
            f.hang_after_commit -= 1;
        }
        hang
    };
    if hang {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn heartbeat(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<HeartbeatReq>,
) -> Response {
    let station = match pre(&s, &headers).await {
        Ok(st) => st,
        Err(r) => return *r,
    };
    let resp = s.mock.lock().unwrap().heartbeat(&station, req);
    post_commit(&s).await;
    Json(resp).into_response()
}

async fn cache(State(s): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = pre(&s, &headers).await {
        return *r;
    }
    let resp = s.mock.lock().unwrap().cache();
    Json(resp).into_response()
}

async fn desk_scans(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<DeskScanReq>,
) -> Response {
    if let Err(r) = pre(&s, &headers).await {
        return *r;
    }
    let result = s.mock.lock().unwrap().desk_scan(req);
    post_commit(&s).await;
    match result {
        Ok(resp) => Json(resp).into_response(),
        Err(f) => failure(f),
    }
}

async fn bindings(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<BindingReq>,
) -> Response {
    if let Err(r) = pre(&s, &headers).await {
        return *r;
    }
    let result = s.mock.lock().unwrap().bind(req);
    post_commit(&s).await;
    match result {
        Ok((status, resp)) => (
            StatusCode::from_u16(status).expect("valid status"),
            Json(resp),
        )
            .into_response(),
        Err(f) => failure(f),
    }
}

#[derive(Deserialize)]
struct LookupQuery {
    uid_raw_hex: String,
}

/// Every field is optional so a malformed query is a typed error from this
/// handler instead of axum's own text.
#[derive(Default, Deserialize)]
struct SearchQuery {
    by: Option<String>,
    q: Option<String>,
}

async fn ticket_search(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Response {
    if let Err(r) = pre(&s, &headers).await {
        return *r;
    }
    let Some(by) = query.by.as_deref().and_then(SearchBy::parse) else {
        return error(400, ErrorCode::Malformed, "by must be name, email or phone");
    };
    let Some(text) = query.q else {
        return error(400, ErrorCode::Malformed, "q is required");
    };
    let resp = s.mock.lock().unwrap().search_tickets(by, &text);
    Json(resp).into_response()
}

async fn lookup(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<LookupQuery>,
) -> Response {
    if let Err(r) = pre(&s, &headers).await {
        return *r;
    }
    let resp = s.mock.lock().unwrap().lookup(&q.uid_raw_hex);
    Json(resp).into_response()
}

async fn observations(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ObservationsReq>,
) -> Response {
    let station = match pre(&s, &headers).await {
        Ok(st) => st,
        Err(r) => return *r,
    };
    if req.observations.len() > MAX_OBSERVATION_BATCH {
        return error(
            422,
            ErrorCode::BatchTooLarge,
            "at most 50 observations per batch",
        );
    }
    let results = {
        let mut m = s.mock.lock().unwrap();
        req.observations
            .into_iter()
            .map(|item| m.observe(&station, item))
            .collect()
    };
    post_commit(&s).await;
    Json(ObservationsResp { results }).into_response()
}

async fn set_faults(State(s): State<Arc<AppState>>, Json(f): Json<Faults>) -> StatusCode {
    *s.faults.lock().unwrap() = f;
    StatusCode::NO_CONTENT
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(paths::HEARTBEAT, post(heartbeat))
        .route(paths::CACHE, get(cache))
        .route(paths::DESK_SCANS, post(desk_scans))
        .route(paths::TICKET_SEARCH, get(ticket_search))
        .route(paths::BINDINGS, post(bindings))
        .route(paths::LOOKUP, get(lookup))
        .route(paths::OBSERVATIONS, post(observations))
        .route("/__mock/faults", post(set_faults))
        .with_state(state)
}

pub async fn serve(
    state: Arc<AppState>,
    addr: SocketAddr,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let app = router(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("mock server crashed");
    });
    Ok((local, handle))
}
