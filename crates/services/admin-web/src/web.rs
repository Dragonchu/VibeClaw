//! HTTP server for AdminWeb — axum router and handlers.
//!
//! Routes:
//!   GET  /                    — dashboard SPA (inline HTML)
//!   GET  /api/status          — system status
//!   GET  /api/versions        — list all versions
//!   GET  /api/versions/{ver}  — version detail
//!   POST /api/rollback        — trigger rollback
//!   GET  /api/peers           — lease / peer info
//!   GET  /api/audit           — audit log query
//!   POST /api/runlevel        — change runlevel
//!   POST /api/shutdown        — shutdown the system
//!   GET  /events              — SSE stream of real-time boot events
//!   GET  /agent               — redirect to peripheral HTTP server

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use reloopy_ipc::messages::{self, msg_types};

use crate::AppState;
use crate::ipc::AdminWebIpc;

const INDEX_HTML: &str = include_str!("static/index.html");

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/versions", get(api_versions))
        .route("/api/versions/{ver}", get(api_version_detail))
        .route("/api/rollback", post(api_rollback))
        .route("/api/peers", get(api_peers))
        .route("/api/audit", get(api_audit))
        .route("/api/runlevel", post(api_runlevel))
        .route("/api/shutdown", post(api_shutdown))
        .route("/events", get(events_sse))
        .route("/agent", get(agent_redirect))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn agent_redirect(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Redirect::temporary(&state.peripheral_url)
}

// ---------------------------------------------------------------------------
// Helper — serialize a request payload or return HTTP 500
// ---------------------------------------------------------------------------

fn serialize_payload<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, axum::response::Response> {
    serde_json::to_value(value).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to serialize request: {}", e),
        )
            .into_response()
    })
}

// ---------------------------------------------------------------------------
// REST API handlers
// ---------------------------------------------------------------------------

async fn api_status(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let payload = match serialize_payload(&messages::AdminStatusRequest {}) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::ADMIN_STATUS_REQUEST, payload).await {
        Ok(resp) => axum::Json(resp.payload).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
}

async fn api_versions(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let payload = match serialize_payload(&messages::AdminListVersionsRequest {}) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::ADMIN_LIST_VERSIONS_REQUEST, payload).await {
        Ok(resp) => axum::Json(resp.payload).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
}

async fn api_version_detail(
    State(state): State<Arc<AppState>>,
    Path(ver): Path<String>,
) -> axum::response::Response {
    let payload = match serialize_payload(&messages::AdminVersionDetailRequest { version: ver }) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::ADMIN_VERSION_DETAIL_REQUEST, payload).await {
        Ok(resp) => axum::Json(resp.payload).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
}

#[derive(Deserialize)]
struct RollbackParams {
    to: Option<String>,
    reason: Option<String>,
}

async fn api_rollback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RollbackParams>,
) -> axum::response::Response {
    let payload = match serialize_payload(&messages::AdminForceRollbackRequest {
        reason: params.reason.unwrap_or_else(|| "AdminWeb-initiated rollback".to_string()),
        to_version: params.to,
    }) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::ADMIN_FORCE_ROLLBACK_REQUEST, payload).await {
        Ok(resp) => axum::Json(resp.payload).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
}

async fn api_peers(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let payload = match serialize_payload(&messages::AdminLeaseStatusRequest {}) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::ADMIN_LEASE_STATUS_REQUEST, payload).await {
        Ok(resp) => axum::Json(resp.payload).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
}

#[derive(Deserialize)]
struct AuditParams {
    event: Option<String>,
    limit: Option<usize>,
}

async fn api_audit(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuditParams>,
) -> axum::response::Response {
    let payload = match serialize_payload(&messages::AdminAuditQueryRequest {
        event_filter: params.event,
        limit: params.limit,
    }) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::ADMIN_AUDIT_QUERY_REQUEST, payload).await {
        Ok(resp) => axum::Json(resp.payload).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
}

#[derive(Deserialize)]
struct RunlevelParams {
    level: u8,
    reason: Option<String>,
}

async fn api_runlevel(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RunlevelParams>,
) -> axum::response::Response {
    let payload = match serialize_payload(&messages::RunlevelRequest {
        to: params.level,
        reason: params.reason.unwrap_or_else(|| "AdminWeb runlevel change".to_string()),
    }) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::RUNLEVEL_REQUEST, payload).await {
        Ok(resp) => axum::Json(resp.payload).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
}

#[derive(Deserialize)]
struct ShutdownParams {
    reason: Option<String>,
}

async fn api_shutdown(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ShutdownParams>,
) -> axum::response::Response {
    let payload = match serialize_payload(&messages::AdminShutdownRequest {
        reason: params.reason.unwrap_or_else(|| "AdminWeb-initiated shutdown".to_string()),
    }) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::ADMIN_SHUTDOWN_REQUEST, payload).await {
        Ok(resp) => axum::Json(resp.payload).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
}

// ---------------------------------------------------------------------------
// SSE event stream
// ---------------------------------------------------------------------------

async fn events_sse(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Open a dedicated IPC connection for streaming so that point-in-time
    // REST requests on the shared Mutex<AdminWebIpc> remain unblocked.
    let sock_path = state.sock_path.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(256);

    tokio::spawn(async move {
        let stream_ipc = match AdminWebIpc::connect(&sock_path).await {
            Ok(c) => c,
            Err(e) => {
                let msg = serde_json::json!({"error": e.to_string()}).to_string();
                let _ = tx.send(Ok(Event::default().event("error").data(msg))).await;
                return;
            }
        };

        let mut event_rx = match stream_ipc.subscribe_events(vec![]).await {
            Ok(r) => r,
            Err(e) => {
                let msg = serde_json::json!({"error": e.to_string()}).to_string();
                let _ = tx.send(Ok(Event::default().event("error").data(msg))).await;
                return;
            }
        };

        while let Some(envelope) = event_rx.recv().await {
            let event_type = envelope.msg_type.clone();
            let data = envelope.payload.to_string();
            let sse_event = Event::default().event(event_type).data(data);
            if tx.send(Ok(sse_event)).await.is_err() {
                break;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}
