//! A fake badge printer: the real event-printing endpoint shapes over loopback
//! HTTP, with every failure mode a station has to survive. Development and
//! tests only — nothing here prints anything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{header::LOCATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::Notify;
use uuid::Uuid;

/// One request the fake printer received. Counted as a request, never as a
/// badge: nothing here knows whether paper moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Lower-cased header names, so an absent credential is easy to assert.
    pub headers: HashMap<String, String>,
    /// The ticket id taken from `/scan/{public_id}/reprint`, when there is one.
    pub public_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// The real answers.
    Success,
    /// HTTP 500 from both endpoints, the way printer trouble is reported.
    ServerError,
    /// HTTP 200 whose body is not the contract.
    BadJson,
    /// HTTP 200 that says `ok:false`.
    NotOk,
    /// A redirect the client must not follow.
    Redirect(String),
    /// Hold the answer until the test releases it.
    Held,
}

struct Inner {
    mode: Mutex<Mode>,
    requests: Mutex<Vec<Request>>,
    arrived: Notify,
    release: Notify,
}

pub struct Printer {
    base: String,
    inner: Arc<Inner>,
    handle: tokio::task::JoinHandle<()>,
}

impl Printer {
    pub async fn start() -> Printer {
        Printer::with_mode(Mode::Success).await
    }

    pub async fn with_mode(mode: Mode) -> Printer {
        let inner = Arc::new(Inner {
            mode: Mutex::new(mode),
            requests: Mutex::new(Vec::new()),
            arrived: Notify::new(),
            release: Notify::new(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("a bound address");
        let app = Router::new()
            .route("/health", get(health))
            .route("/scan/{public_id}/reprint", post(reprint))
            .with_state(inner.clone());
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("fake printer crashed");
        });
        Printer {
            base: format!("http://{addr}"),
            inner,
            handle,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn requests(&self) -> Vec<Request> {
        self.inner.requests.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.inner.requests.lock().unwrap().len()
    }

    pub fn set_mode(&self, mode: Mode) {
        *self.inner.mode.lock().unwrap() = mode;
    }

    /// Wait until a request has arrived. No sleep: a stored notification wakes
    /// this the moment the request is recorded, however the timing falls out.
    pub async fn wait_for_request(&self) -> usize {
        loop {
            let seen = self.count();
            if seen > 0 {
                return seen;
            }
            self.inner.arrived.notified().await;
        }
    }

    /// Let a held answer through.
    pub fn release(&self) {
        self.inner.release.notify_one();
    }
}

impl Drop for Printer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn record(headers: &HeaderMap, method: &str, path: &str) -> Request {
    let headers = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_lowercase(), value.to_string()))
        })
        .collect();
    Request {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        public_id: path
            .strip_prefix("/scan/")
            .and_then(|rest| rest.strip_suffix("/reprint"))
            .and_then(|id| Uuid::parse_str(id).ok()),
    }
}

fn health_body() -> serde_json::Value {
    json!({
        "ok": true,
        "printer": "Fake Printer 2000",
        "output_dir": "/tmp/fake",
        "version": "9.9",
    })
}

fn reprint_body(public_id: &str) -> serde_json::Value {
    json!({
        "ok": true,
        "reprinted": true,
        "ticket": { "public_id": public_id, "name": "Aina" },
        "pdf": "/tmp/fake/badge.pdf",
        "print_job": "job-1",
    })
}

async fn hold(inner: &Inner) {
    inner.arrived.notify_one();
    inner.release.notified().await;
}

async fn health(State(inner): State<Arc<Inner>>, headers: HeaderMap) -> Response {
    let mode = inner.mode.lock().unwrap().clone();
    inner
        .requests
        .lock()
        .unwrap()
        .push(record(&headers, "GET", "/health"));
    match mode {
        Mode::Success => Json(health_body()).into_response(),
        Mode::ServerError => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Mode::BadJson => Json(json!({ "unexpected": true })).into_response(),
        Mode::NotOk => Json(json!({ "ok": false })).into_response(),
        Mode::Redirect(target) => redirect(&target),
        Mode::Held => {
            hold(&inner).await;
            Json(health_body()).into_response()
        }
    }
}

async fn reprint(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    Path(public_id): Path<String>,
) -> Response {
    let mode = inner.mode.lock().unwrap().clone();
    let path = format!("/scan/{public_id}/reprint");
    inner
        .requests
        .lock()
        .unwrap()
        .push(record(&headers, "POST", &path));
    match mode {
        Mode::Success => Json(reprint_body(&public_id)).into_response(),
        Mode::ServerError => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Mode::BadJson => Json(json!({ "unexpected": true })).into_response(),
        Mode::NotOk => Json(json!({ "ok": true, "reprinted": false })).into_response(),
        Mode::Redirect(target) => redirect(&target),
        Mode::Held => {
            hold(&inner).await;
            Json(reprint_body(&public_id)).into_response()
        }
    }
}

fn redirect(target: &str) -> Response {
    (StatusCode::TEMPORARY_REDIRECT, [(LOCATION, target)]).into_response()
}
