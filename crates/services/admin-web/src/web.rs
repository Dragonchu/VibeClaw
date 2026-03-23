//! HTTP server for AdminWeb — axum router and handlers.
//!
//! Routes:
//!   GET  /                    — dashboard SPA (inline HTML)
//!   GET  /api/status          — system status
//!   GET  /api/versions        — list all versions
//!   GET  /api/versions/{ver}  — version detail
//!   POST /api/rollback        — trigger rollback
//!   GET  /api/peers           — lease / peer info
//!   GET  /api/agent-url       — peripheral HTTP URL for iframe embedding

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
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
        .route("/api/agent-url", get(api_agent_url))
        .route("/api/quick-evolve", post(api_quick_evolve))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn api_agent_url(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Ask Boot for the peripheral's current HTTP port.
    let payload = match serialize_payload(&messages::AdminStatusRequest {}) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut ipc = state.ipc.lock().await;
    match ipc.request(msg_types::ADMIN_STATUS_REQUEST, payload).await {
        Ok(resp) => {
            let port = resp.payload.get("peripheral_http_port")
                .and_then(|v| v.as_u64())
                .map(|p| p as u16);
            match port {
                Some(p) => axum::Json(serde_json::json!({
                    "url": format!("http://localhost:{}", p)
                })).into_response(),
                None => axum::Json(serde_json::json!({
                    "url": null,
                    "error": "Peripheral not connected"
                })).into_response(),
            }
        }
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
    }
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

// ---------------------------------------------------------------------------
// Quick-evolve: one-click dark-theme patch for V0 onboarding
// ---------------------------------------------------------------------------

/// Light-to-dark theme patch applied to the peripheral's index.html.
const PATCH_OLD: &str = "background: #ffffff;\n        color: #0d0d0d;";
const PATCH_NEW: &str = "background: #0d1117;\n        color: #c9d1d9;";

async fn api_quick_evolve(State(state): State<Arc<AppState>>) -> axum::response::Response {
    // 1. Check we're on V0 — only allowed as first-run experience.
    let status_payload = match serialize_payload(&messages::AdminStatusRequest {}) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let version = {
        let mut ipc = state.ipc.lock().await;
        match ipc.request(msg_types::ADMIN_STATUS_REQUEST, status_payload).await {
            Ok(resp) => resp.payload.get("current_version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, format!("IPC error: {}", e)).into_response(),
        }
    };

    if !version.is_empty() && version != "V0" {
        return (
            StatusCode::CONFLICT,
            axum::Json(serde_json::json!({
                "success": false,
                "error": "Quick evolution is only available on V0 or fresh install"
            })),
        ).into_response();
    }

    // 2. Read the peripheral's index.html from workspace.
    let index_path = state.workspace_root
        .join("crates")
        .join("peripheral")
        .join("src")
        .join("static")
        .join("index.html");

    let content = match std::fs::read_to_string(&index_path) {
        Ok(c) => c,
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({
                "success": false,
                "error": format!("Failed to read {}: {}", index_path.display(), e)
            })),
        ).into_response(),
    };

    // 3. Apply the patch.
    if !content.contains(PATCH_OLD) {
        return (
            StatusCode::CONFLICT,
            axum::Json(serde_json::json!({
                "success": false,
                "error": "Patch target not found — file may have already been modified"
            })),
        ).into_response();
    }

    let patched = content.replacen(PATCH_OLD, PATCH_NEW, 1);

    if let Err(e) = std::fs::write(&index_path, &patched) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({
                "success": false,
                "error": format!("Failed to write {}: {}", index_path.display(), e)
            })),
        ).into_response();
    }

    tracing::info!("Quick-evolve patch applied to {}", index_path.display());

    // 4. Submit the update to Boot.
    let submit = messages::SubmitUpdate {
        source_path: state.workspace_root.to_string_lossy().to_string(),
    };
    let payload = match serde_json::to_value(&submit) {
        Ok(p) => p,
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Serialization error: {}", e),
        ).into_response(),
    };

    {
        let mut ipc = state.ipc.lock().await;
        if let Err(e) = ipc.send_fire_and_forget(msg_types::SUBMIT_UPDATE, payload).await {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "success": false,
                    "error": format!("Failed to submit update: {}", e)
                })),
            ).into_response();
        }
    }

    axum::Json(serde_json::json!({
        "success": true,
        "message": "Dark theme patch applied and submitted for compilation"
    })).into_response()
}
