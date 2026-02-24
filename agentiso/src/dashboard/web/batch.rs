use std::sync::Arc;

use axum::{
    extract::State,
    response::Response,
    routing::post,
    Json, Router,
};
use serde::Deserialize;

use super::{error_response, ok_response, DashboardState};

pub fn routes() -> Router<Arc<DashboardState>> {
    Router::new()
        .route("/batch/exec-parallel", post(exec_parallel))
        .route("/batch/workspace-prepare", post(workspace_prepare))
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ExecParallelRequest {
    workspace_ids: Vec<String>,
    command: String,
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
struct WorkspacePrepareRequest {
    name: String,
    base_image: Option<String>,
    repo_url: Option<String>,
    setup_commands: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn exec_parallel(
    State(state): State<Arc<DashboardState>>,
    Json(req): Json<ExecParallelRequest>,
) -> Response {
    let timeout_secs = req.timeout_secs;

    // Resolve all workspace IDs
    let mut uuids = Vec::new();
    for id_str in &req.workspace_ids {
        match super::resolve_workspace_id(&state.workspace_manager, id_str).await {
            Ok(uuid) => uuids.push(uuid),
            Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
        }
    }

    // Execute in parallel
    let mut handles = Vec::new();
    for uuid in uuids {
        let wm = state.workspace_manager.clone();
        let cmd = req.command.clone();
        handles.push(tokio::spawn(async move {
            let result = wm.exec(uuid, &cmd, None, None, timeout_secs).await;
            (uuid, result)
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok((uuid, Ok(result))) => {
                results.push(serde_json::json!({
                    "workspace_id": uuid.to_string(),
                    "exit_code": result.exit_code,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                }));
            }
            Ok((uuid, Err(e))) => {
                results.push(serde_json::json!({
                    "workspace_id": uuid.to_string(),
                    "error": e.to_string(),
                }));
            }
            Err(e) => {
                results.push(serde_json::json!({
                    "error": format!("task join error: {}", e),
                }));
            }
        }
    }

    ok_response(results)
}

async fn workspace_prepare(
    State(state): State<Arc<DashboardState>>,
    Json(req): Json<WorkspacePrepareRequest>,
) -> Response {
    use crate::workspace::CreateParams;

    // Create workspace
    let params = CreateParams {
        name: Some(req.name.clone()),
        base_image: req.base_image,
        vcpus: None,
        memory_mb: None,
        disk_gb: None,
        allow_internet: Some(true), // Need internet for git clone
    };

    let ws = match state.workspace_manager.create(params).await {
        Ok(result) => result.workspace,
        Err(e) => {
            return error_response(
                axum::http::StatusCode::BAD_REQUEST,
                "CREATE_FAILED",
                e.to_string(),
            )
        }
    };

    let ws_id = ws.id;

    // Clone repo if specified
    if let Some(repo_url) = &req.repo_url {
        let cmd = format!("git clone -- '{}' /workspace", repo_url.replace('\'', "'\\''"));
        if let Err(e) = state
            .workspace_manager
            .exec(
                ws_id,
                &cmd,
                None,
                None,
                Some(300),
            )
            .await
        {
            // Clean up on failure
            let _ = state.workspace_manager.destroy(ws_id).await;
            return error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "GIT_CLONE_FAILED",
                e.to_string(),
            );
        }
    }

    // Run setup commands
    if let Some(commands) = &req.setup_commands {
        for cmd in commands {
            if let Err(e) = state
                .workspace_manager
                .exec(
                    ws_id,
                    cmd,
                    Some("/workspace"),
                    None,
                    Some(300),
                )
                .await
            {
                let _ = state.workspace_manager.destroy(ws_id).await;
                return error_response(
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "SETUP_FAILED",
                    format!("command '{}' failed: {}", cmd, e),
                );
            }
        }
    }

    // Create golden snapshot
    if let Err(e) = state
        .workspace_manager
        .snapshot_create(ws_id, "golden", false)
        .await
    {
        let _ = state.workspace_manager.destroy(ws_id).await;
        return error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "SNAPSHOT_FAILED",
            e.to_string(),
        );
    }

    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::CREATED,
        axum::Json(super::ApiResponse::new(serde_json::json!({
            "workspace_id": ws_id.to_string(),
            "name": req.name,
            "snapshot": "golden",
        }))),
    )
        .into_response()
}
