//! HTTP server for AdminWeb — axum router and handlers.
//!
//! Routes:
//!   GET  /                    — dashboard SPA (inline HTML)
//!   GET  /api/status          — system status
//!   GET  /api/versions        — list all versions
//!   GET  /api/versions/{ver}  — version detail
//!   POST /api/rollback        — trigger rollback
//!   GET  /api/peers           — lease / peer info
//!   GET  /agent               — redirect to peripheral HTTP server

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use serde::Deserialize;

use reloopy_ipc::messages::{self, msg_types};

use crate::AppState;

const INDEX_HTML: &str = include_str!("static/index.html");

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/versions", get(api_versions))
        .route("/api/versions/{ver}", get(api_version_detail))
        .route("/api/rollback", post(api_rollback))
        .route("/api/peers", get(api_peers))
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
