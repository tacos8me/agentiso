pub mod auth;
pub mod batch;
pub mod embedded;
pub mod system;
pub mod teams;
pub mod vault;
pub mod workspaces;
pub mod ws;

use std::sync::Arc;

use axum::Router;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::config::Config;
use crate::mcp::auth::AuthManager;
use crate::mcp::metrics::MetricsRegistry;
use crate::mcp::vault::VaultManager;
use crate::team::{MessageRelay, TeamManager};
use crate::workspace::WorkspaceManager;

use self::ws::BroadcastHub;

/// Shared state for all dashboard handlers, passed via axum `State`.
#[derive(Clone)]
pub struct DashboardState {
    pub workspace_manager: Arc<WorkspaceManager>,
    pub auth_manager: AuthManager,
    pub team_manager: Option<Arc<TeamManager>>,
    pub vault_manager: Option<Arc<VaultManager>>,
    pub message_relay: Arc<MessageRelay>,
    #[allow(dead_code)] // Reserved for future metrics integration
    pub metrics: Option<MetricsRegistry>,
    pub config: Arc<Config>,
    pub ws_hub: Arc<BroadcastHub>,
    /// Dedicated session ID for dashboard API operations.
    pub session_id: String,
}

/// Response envelope for successful API responses.
#[derive(serde::Serialize)]
pub struct ApiResponse<T: serde::Serialize> {
    pub data: T,
    pub meta: ApiMeta,
}

/// Response envelope for error API responses.
#[derive(serde::Serialize)]
pub struct ApiError {
    pub error: ApiErrorDetail,
    pub meta: ApiMeta,
}

#[derive(serde::Serialize)]
pub struct ApiErrorDetail {
    pub code: String,
    pub message: String,
}

#[derive(serde::Serialize)]
pub struct ApiMeta {
    pub request_id: String,
}

impl ApiMeta {
    pub fn new() -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl<T: serde::Serialize> ApiResponse<T> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            meta: ApiMeta::new(),
        }
    }
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorDetail {
                code: code.into(),
                message: message.into(),
            },
            meta: ApiMeta::new(),
        }
    }
}

/// Helper to convert an ApiError into an axum JSON response with appropriate status code.
pub fn error_response(
    status: axum::http::StatusCode,
    code: &str,
    message: impl Into<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let body = ApiError::new(code, message);
    (status, axum::Json(body)).into_response()
}

/// Helper to convert a successful payload into an ApiResponse JSON.
pub fn ok_response<T: serde::Serialize>(data: T) -> axum::response::Response {
    use axum::response::IntoResponse;
    let body = ApiResponse::new(data);
    (axum::http::StatusCode::OK, axum::Json(body)).into_response()
}

/// Resolve a workspace ID from a name-or-UUID string, matching the MCP pattern.
pub async fn resolve_workspace_id(
    wm: &WorkspaceManager,
    id_or_name: &str,
) -> Result<uuid::Uuid, (axum::http::StatusCode, String)> {
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        return Ok(uuid);
    }
    match wm.find_by_name(id_or_name).await {
        Some(uuid) => Ok(uuid),
        None => Err((
            axum::http::StatusCode::NOT_FOUND,
            format!(
                "workspace '{}' not found: not a valid UUID and no workspace with that name exists",
                id_or_name
            ),
        )),
    }
}

/// Build the full axum Router for the dashboard.
pub fn build_router(state: Arc<DashboardState>) -> Router {
    let api = Router::new()
        // Workspace routes
        .merge(workspaces::routes())
        // Team routes
        .merge(teams::routes())
        // Vault routes
        .merge(vault::routes())
        // System routes
        .merge(system::routes())
        // Batch routes
        .merge(batch::routes())
        // WebSocket
        .merge(ws::routes());

    let api = api
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .with_state(state.clone());

    let app = Router::new()
        .nest("/api", api)
        .layer(CorsLayer::permissive());

    // Static file serving: use static_dir if configured & exists, else embedded assets
    let static_dir = state.config.dashboard.static_dir.as_deref().unwrap_or("");
    if !static_dir.is_empty() && std::path::Path::new(static_dir).is_dir() {
        let serve_dir = tower_http::services::ServeDir::new(static_dir)
            .fallback(tower_http::services::ServeFile::new(
                format!("{}/index.html", static_dir),
            ));
        app.fallback_service(serve_dir)
    } else {
        // Serve from embedded assets (compiled into the binary)
        app.fallback(embedded::serve_embedded)
    }
}

/// Start the dashboard HTTP server as a background task.
pub async fn start_dashboard(state: Arc<DashboardState>) -> anyhow::Result<()> {
    let addr = format!(
        "{}:{}",
        state.config.dashboard.bind_addr, state.config.dashboard.port
    );

    // Wire broadcast hub into workspace manager for event-driven updates
    state.workspace_manager.set_broadcast_hub(state.ws_hub.clone()).await;

    // Start WebSocket polling task (fallback reconciliation at 10s interval)
    ws::start_polling_task(state.clone());

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "dashboard server listening");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "dashboard server error");
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_response_serialization() {
        let resp = ApiResponse::new(serde_json::json!({"count": 5}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"data\""));
        assert!(json.contains("\"meta\""));
        assert!(json.contains("\"request_id\""));
    }

    #[test]
    fn api_error_serialization() {
        let err = ApiError::new("NOT_FOUND", "workspace not found");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"error\""));
        assert!(json.contains("NOT_FOUND"));
        assert!(json.contains("workspace not found"));
    }
}
