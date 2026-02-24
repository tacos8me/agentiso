use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use crate::workspace::{CreateParams, WorkspaceState};

use super::{error_response, ok_response, resolve_workspace_id, DashboardState};

pub fn routes() -> Router<Arc<DashboardState>> {
    Router::new()
        .route("/workspaces", get(list_workspaces))
        .route("/workspaces", post(create_workspace))
        .route("/workspaces/{id}", get(get_workspace))
        .route("/workspaces/{id}", delete(destroy_workspace))
        .route("/workspaces/{id}/start", post(start_workspace))
        .route("/workspaces/{id}/stop", post(stop_workspace))
        .route("/workspaces/{id}/suspend", post(suspend_workspace))
        .route("/workspaces/{id}/resume", post(resume_workspace))
        .route("/workspaces/{id}/exec", post(exec_command))
        .route("/workspaces/{id}/logs", get(get_logs))
        .route("/workspaces/{id}/snapshots", get(list_snapshots))
        .route("/workspaces/{id}/snapshots", post(create_snapshot))
        .route("/workspaces/{id}/fork", post(fork_workspace))
        .route("/workspaces/{id}/network-policy", post(update_network_policy))
        .route("/workspaces/{id}/port-forward", post(add_port_forward))
        .route("/workspaces/{id}/env", post(set_env))
        .route("/workspaces/{id}/adopt", post(adopt_workspace))
        .route("/workspaces/{id}/files", get(read_file))
        .route("/workspaces/{id}/files", axum::routing::put(write_file))
        .route("/workspaces/{id}/files/list", get(list_dir))
        .route("/workspaces/{id}/exec/stream", post(exec_stream))
        .route("/workspaces/{id}/exec/background", post(exec_background_start))
        .route(
            "/workspaces/{id}/exec/background/{job_id}",
            get(exec_background_poll),
        )
        .route(
            "/workspaces/{id}/exec/background/{job_id}",
            delete(exec_background_kill),
        )
        .route("/workspaces/{id}/git/clone", post(git_clone))
        .route("/workspaces/{id}/git/status", get(git_status))
        .route("/workspaces/{id}/git/commit", post(git_commit))
        .route("/workspaces/{id}/git/push", post(git_push))
        .route("/workspaces/{id}/git/diff", get(git_diff))
}

// ---------------------------------------------------------------------------
// Serializable workspace info for API responses
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct WorkspaceInfo {
    id: String,
    name: String,
    state: String,
    base_image: String,
    ip: String,
    vsock_cid: u32,
    vcpus: u32,
    memory_mb: u32,
    disk_gb: u32,
    allow_internet: bool,
    allow_inter_vm: bool,
    qemu_pid: Option<u32>,
    created_at: String,
    team_id: Option<String>,
    forked_from: Option<String>,
    snapshot_count: usize,
}

impl WorkspaceInfo {
    fn from_workspace(ws: &crate::workspace::Workspace) -> Self {
        Self {
            id: ws.id.to_string(),
            name: ws.name.clone(),
            state: ws.state.to_string(),
            base_image: ws.base_image.clone(),
            ip: ws.network.ip.to_string(),
            vsock_cid: ws.vsock_cid,
            vcpus: ws.resources.vcpus,
            memory_mb: ws.resources.memory_mb,
            disk_gb: ws.resources.disk_gb,
            allow_internet: ws.network.allow_internet,
            allow_inter_vm: ws.network.allow_inter_vm,
            qemu_pid: ws.qemu_pid,
            created_at: ws.created_at.to_rfc3339(),
            team_id: ws.team_id.clone(),
            forked_from: ws.forked_from.as_ref().map(|f| f.source_workspace_id.to_string()),
            snapshot_count: ws.snapshots.len(),
        }
    }
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    state_filter: Option<String>,
}

#[derive(Deserialize)]
struct CreateRequest {
    name: Option<String>,
    base_image: Option<String>,
    vcpus: Option<u32>,
    memory_mb: Option<u32>,
    disk_gb: Option<u32>,
    allow_internet: Option<bool>,
}

#[derive(Deserialize)]
struct ExecRequest {
    command: String,
    timeout_secs: Option<u64>,
    workdir: Option<String>,
    env: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct SnapshotCreateRequest {
    name: String,
}

#[derive(Deserialize)]
struct ForkRequest {
    name: Option<String>,
    snapshot: Option<String>,
    count: Option<u32>,
}

#[derive(Deserialize)]
struct NetworkPolicyRequest {
    allow_internet: Option<bool>,
    allow_inter_vm: Option<bool>,
}

#[derive(Deserialize)]
struct PortForwardRequest {
    host_port: u16,
    guest_port: u16,
}

#[derive(Deserialize)]
struct SetEnvRequest {
    env: HashMap<String, String>,
}

#[derive(Deserialize)]
struct AdoptRequest {
    force: Option<bool>,
}

#[derive(Deserialize)]
struct FileReadQuery {
    path: String,
    offset: Option<u64>,
    limit: Option<u64>,
}

#[derive(Deserialize)]
struct FileWriteRequest {
    path: String,
    content: String,
    mode: Option<String>,
}

#[derive(Deserialize)]
struct ListDirQuery {
    path: Option<String>,
}

#[derive(Deserialize)]
struct ExecBgStartRequest {
    command: String,
    workdir: Option<String>,
    env: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct GitCloneRequest {
    url: String,
    path: Option<String>,
    branch: Option<String>,
}

#[derive(Deserialize)]
struct GitCommitRequest {
    message: String,
    path: Option<String>,
}

#[derive(Deserialize)]
struct GitPushRequest {
    path: Option<String>,
    remote: Option<String>,
    branch: Option<String>,
}

#[derive(Deserialize)]
struct GitDiffQuery {
    path: Option<String>,
    cached: Option<bool>,
}

#[derive(Deserialize)]
struct GitStatusQuery {
    path: Option<String>,
}

#[derive(Deserialize)]
struct LogsQuery {
    lines: Option<usize>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_workspaces(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<ListQuery>,
) -> Response {
    let workspaces = match state.workspace_manager.list().await {
        Ok(ws) => ws,
        Err(e) => {
            return error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                e.to_string(),
            )
        }
    };

    let state_filter = query.state_filter.as_deref().and_then(|s| match s {
        "running" => Some(WorkspaceState::Running),
        "stopped" => Some(WorkspaceState::Stopped),
        "suspended" => Some(WorkspaceState::Suspended),
        _ => None,
    });

    let infos: Vec<WorkspaceInfo> = workspaces
        .iter()
        .filter(|ws| state_filter.is_none_or(|sf| ws.state == sf))
        .map(WorkspaceInfo::from_workspace)
        .collect();

    ok_response(infos)
}

async fn create_workspace(
    State(state): State<Arc<DashboardState>>,
    Json(req): Json<CreateRequest>,
) -> Response {
    let params = CreateParams {
        name: req.name,
        base_image: req.base_image,
        vcpus: req.vcpus,
        memory_mb: req.memory_mb,
        disk_gb: req.disk_gb,
        allow_internet: req.allow_internet,
    };

    // Register workspace quota under the dashboard session
    match state.workspace_manager.create(params).await {
        Ok(result) => {
            let info = WorkspaceInfo::from_workspace(&result.workspace);
            // Register with auth manager
            let _ = state.auth_manager.register_workspace(
                &state.session_id,
                result.workspace.id,
                result.workspace.resources.memory_mb as u64,
                result.workspace.resources.disk_gb as u64,
            ).await;
            use axum::response::IntoResponse;
            (
                axum::http::StatusCode::CREATED,
                axum::Json(super::ApiResponse::new(info)),
            )
                .into_response()
        }
        Err(e) => error_response(
            axum::http::StatusCode::BAD_REQUEST,
            "CREATE_FAILED",
            e.to_string(),
        ),
    }
}

async fn get_workspace(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.get(uuid).await {
        Ok(ws) => ok_response(WorkspaceInfo::from_workspace(&ws)),
        Err(e) => error_response(
            axum::http::StatusCode::NOT_FOUND,
            "NOT_FOUND",
            e.to_string(),
        ),
    }
}

async fn destroy_workspace(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.destroy(uuid).await {
        Ok(()) => {
            let _ = state.auth_manager.unregister_workspace(
                &state.session_id,
                uuid,
                0, 0, // We don't track exact resources here; quota is approximate
            ).await;
            ok_response(serde_json::json!({"destroyed": uuid.to_string()}))
        }
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "DESTROY_FAILED",
            e.to_string(),
        ),
    }
}

async fn start_workspace(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.start(uuid).await {
        Ok(()) => ok_response(serde_json::json!({"started": uuid.to_string()})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "START_FAILED",
            e.to_string(),
        ),
    }
}

async fn stop_workspace(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.stop(uuid).await {
        Ok(()) => ok_response(serde_json::json!({"stopped": uuid.to_string()})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "STOP_FAILED",
            e.to_string(),
        ),
    }
}

async fn suspend_workspace(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.suspend(uuid).await {
        Ok(()) => ok_response(serde_json::json!({"suspended": uuid.to_string()})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "SUSPEND_FAILED",
            e.to_string(),
        ),
    }
}

async fn resume_workspace(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.resume(uuid).await {
        Ok(()) => ok_response(serde_json::json!({"resumed": uuid.to_string()})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "RESUME_FAILED",
            e.to_string(),
        ),
    }
}

async fn exec_command(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state
        .workspace_manager
        .exec(
            uuid,
            &req.command,
            req.workdir.as_deref(),
            req.env.as_ref(),
            req.timeout_secs,
        )
        .await
    {
        Ok(result) => ok_response(serde_json::json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
        })),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "EXEC_FAILED",
            e.to_string(),
        ),
    }
}

async fn get_logs(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let max_lines = query.lines.unwrap_or(200);

    match state.workspace_manager.workspace_logs(uuid, max_lines).await {
        Ok((console, stderr)) => ok_response(serde_json::json!({
            "console": console,
            "stderr": stderr,
        })),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "LOGS_FAILED",
            e.to_string(),
        ),
    }
}

async fn list_snapshots(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.get(uuid).await {
        Ok(ws) => {
            let snaps: Vec<serde_json::Value> = ws
                .snapshots
                .list()
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "created_at": s.created_at.to_rfc3339(),
                        "parent": s.parent,
                    })
                })
                .collect();
            ok_response(snaps)
        }
        Err(e) => error_response(
            axum::http::StatusCode::NOT_FOUND,
            "NOT_FOUND",
            e.to_string(),
        ),
    }
}

async fn create_snapshot(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<SnapshotCreateRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.snapshot_create(uuid, &req.name, false).await {
        Ok(_snap) => {
            use axum::response::IntoResponse;
            (
                axum::http::StatusCode::CREATED,
                axum::Json(super::ApiResponse::new(
                    serde_json::json!({"created": req.name}),
                )),
            )
                .into_response()
        }
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "SNAPSHOT_FAILED",
            e.to_string(),
        ),
    }
}

async fn fork_workspace(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<ForkRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let snapshot_name = req.snapshot.as_deref().unwrap_or("latest");
    let count = req.count.unwrap_or(1).min(20);

    if count == 1 {
        match state
            .workspace_manager
            .fork(uuid, snapshot_name, req.name.clone())
            .await
        {
            Ok(forked) => {
                let info = WorkspaceInfo::from_workspace(&forked);
                use axum::response::IntoResponse;
                (
                    axum::http::StatusCode::CREATED,
                    axum::Json(super::ApiResponse::new(info)),
                )
                    .into_response()
            }
            Err(e) => error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "FORK_FAILED",
                e.to_string(),
            ),
        }
    } else {
        // Batch fork
        let mut results = Vec::new();
        for i in 0..count {
            let name = req
                .name
                .as_ref()
                .map(|n| format!("{}-{}", n, i + 1));
            match state
                .workspace_manager
                .fork(uuid, snapshot_name, name)
                .await
            {
                Ok(forked) => results.push(serde_json::json!({
                    "id": forked.id.to_string(),
                    "name": forked.name,
                    "state": forked.state.to_string(),
                })),
                Err(e) => results.push(serde_json::json!({
                    "error": e.to_string(),
                })),
            }
        }
        use axum::response::IntoResponse;
        (
            axum::http::StatusCode::CREATED,
            axum::Json(super::ApiResponse::new(results)),
        )
            .into_response()
    }
}

async fn update_network_policy(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<NetworkPolicyRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state
        .workspace_manager
        .update_network_policy(uuid, req.allow_internet, req.allow_inter_vm, None)
        .await
    {
        Ok(()) => ok_response(serde_json::json!({"updated": true})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "NETWORK_POLICY_FAILED",
            e.to_string(),
        ),
    }
}

async fn add_port_forward(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<PortForwardRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state
        .workspace_manager
        .port_forward_add(uuid, req.guest_port, Some(req.host_port))
        .await
    {
        Ok(actual_host_port) => ok_response(serde_json::json!({
            "host_port": actual_host_port,
            "guest_port": req.guest_port,
        })),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "PORT_FORWARD_FAILED",
            e.to_string(),
        ),
    }
}

async fn set_env(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<SetEnvRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state.workspace_manager.set_env(uuid, req.env).await {
        Ok(count) => ok_response(serde_json::json!({"set": true, "count": count})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "SET_ENV_FAILED",
            e.to_string(),
        ),
    }
}

async fn adopt_workspace(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<AdoptRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let force = req.force.unwrap_or(false);

    // Get workspace resources for quota tracking
    let ws = match state.workspace_manager.get(uuid).await {
        Ok(ws) => ws,
        Err(e) => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "NOT_FOUND",
                e.to_string(),
            )
        }
    };

    match state
        .auth_manager
        .adopt_workspace(
            &state.session_id,
            &uuid,
            ws.resources.memory_mb as u64,
            ws.resources.disk_gb as u64,
            force,
        )
        .await
    {
        Ok(()) => ok_response(serde_json::json!({"adopted": uuid.to_string()})),
        Err(e) => error_response(
            axum::http::StatusCode::CONFLICT,
            "ADOPT_FAILED",
            e.to_string(),
        ),
    }
}

async fn read_file(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Query(query): Query<FileReadQuery>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state
        .workspace_manager
        .file_read(uuid, &query.path, query.offset, query.limit)
        .await
    {
        Ok(bytes) => {
            let content = String::from_utf8_lossy(&bytes).to_string();
            ok_response(serde_json::json!({
                "path": query.path,
                "content": content,
            }))
        }
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "FILE_READ_FAILED",
            e.to_string(),
        ),
    }
}

async fn write_file(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<FileWriteRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let mode = req.mode.as_deref().and_then(|m| u32::from_str_radix(m, 8).ok());
    match state
        .workspace_manager
        .file_write(uuid, &req.path, req.content.as_bytes(), mode)
        .await
    {
        Ok(()) => ok_response(serde_json::json!({"written": req.path})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "FILE_WRITE_FAILED",
            e.to_string(),
        ),
    }
}

async fn list_dir(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Query(query): Query<ListDirQuery>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let path = query.path.as_deref().unwrap_or("/");

    match state.workspace_manager.list_dir(uuid, path).await {
        Ok(entries) => ok_response(entries),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "LIST_DIR_FAILED",
            e.to_string(),
        ),
    }
}

async fn exec_background_start(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<ExecBgStartRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    match state
        .workspace_manager
        .exec_background(uuid, &req.command, req.workdir.as_deref(), req.env.as_ref())
        .await
    {
        Ok(job_id) => {
            use axum::response::IntoResponse;
            (
                axum::http::StatusCode::CREATED,
                axum::Json(super::ApiResponse::new(
                    serde_json::json!({"job_id": job_id}),
                )),
            )
                .into_response()
        }
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "EXEC_BG_FAILED",
            e.to_string(),
        ),
    }
}

async fn exec_background_poll(
    State(state): State<Arc<DashboardState>>,
    Path((id, job_id_str)): Path<(String, String)>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let job_id: u32 = match job_id_str.parse() {
        Ok(j) => j,
        Err(_) => {
            return error_response(
                axum::http::StatusCode::BAD_REQUEST,
                "INVALID_JOB_ID",
                "job_id must be a number",
            )
        }
    };

    match state.workspace_manager.exec_poll(uuid, job_id).await {
        Ok(result) => ok_response(result),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "POLL_FAILED",
            e.to_string(),
        ),
    }
}

async fn exec_background_kill(
    State(state): State<Arc<DashboardState>>,
    Path((id, job_id_str)): Path<(String, String)>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let job_id: u32 = match job_id_str.parse() {
        Ok(j) => j,
        Err(_) => {
            return error_response(
                axum::http::StatusCode::BAD_REQUEST,
                "INVALID_JOB_ID",
                "job_id must be a number",
            )
        }
    };

    match state.workspace_manager.exec_kill(uuid, job_id, None).await {
        Ok(()) => ok_response(serde_json::json!({"killed": job_id})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "KILL_FAILED",
            e.to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Git endpoints
// ---------------------------------------------------------------------------

async fn git_clone(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<GitCloneRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let path = req.path.as_deref().unwrap_or("/workspace");
    let cmd = if let Some(branch) = &req.branch {
        format!("git clone --branch {} -- {} {}", shell_escape(branch), shell_escape(&req.url), shell_escape(path))
    } else {
        format!("git clone -- {} {}", shell_escape(&req.url), shell_escape(path))
    };

    match state
        .workspace_manager
        .exec(
            uuid,
            &cmd,
            None,
            None,
            Some(300),
        )
        .await
    {
        Ok(result) => ok_response(serde_json::json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
        })),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "GIT_CLONE_FAILED",
            e.to_string(),
        ),
    }
}

async fn git_status(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Query(query): Query<GitStatusQuery>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let path = query.path.as_deref().unwrap_or("/workspace");
    let cmd = format!("cd {} && git status --porcelain", shell_escape(path));

    match state
        .workspace_manager
        .exec(
            uuid,
            &cmd,
            None,
            None,
            Some(30),
        )
        .await
    {
        Ok(result) => ok_response(serde_json::json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
        })),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "GIT_STATUS_FAILED",
            e.to_string(),
        ),
    }
}

async fn git_commit(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<GitCommitRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let path = req.path.as_deref().unwrap_or("/workspace");
    let cmd = format!(
        "cd {} && git add -A && git commit -m {}",
        shell_escape(path),
        shell_escape(&req.message)
    );

    match state
        .workspace_manager
        .exec(
            uuid,
            &cmd,
            None,
            None,
            Some(60),
        )
        .await
    {
        Ok(result) => ok_response(serde_json::json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
        })),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "GIT_COMMIT_FAILED",
            e.to_string(),
        ),
    }
}

async fn git_push(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<GitPushRequest>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let path = req.path.as_deref().unwrap_or("/workspace");
    let remote = req.remote.as_deref().unwrap_or("origin");
    let cmd = if let Some(branch) = &req.branch {
        format!(
            "cd {} && git push {} {}",
            shell_escape(path),
            shell_escape(remote),
            shell_escape(branch)
        )
    } else {
        format!("cd {} && git push {}", shell_escape(path), shell_escape(remote))
    };

    match state
        .workspace_manager
        .exec(
            uuid,
            &cmd,
            None,
            None,
            Some(120),
        )
        .await
    {
        Ok(result) => ok_response(serde_json::json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
        })),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "GIT_PUSH_FAILED",
            e.to_string(),
        ),
    }
}

async fn git_diff(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Query(query): Query<GitDiffQuery>,
) -> Response {
    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    let path = query.path.as_deref().unwrap_or("/workspace");
    let cached_flag = if query.cached.unwrap_or(false) {
        " --cached"
    } else {
        ""
    };
    let cmd = format!("cd {} && git diff{}", shell_escape(path), cached_flag);

    match state
        .workspace_manager
        .exec(
            uuid,
            &cmd,
            None,
            None,
            Some(30),
        )
        .await
    {
        Ok(result) => ok_response(serde_json::json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
        })),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "GIT_DIFF_FAILED",
            e.to_string(),
        ),
    }
}

/// SSE exec streaming — starts a background job and polls for output, streaming
/// chunks as SSE events to the client in real time.
async fn exec_stream(
    State(state): State<Arc<DashboardState>>,
    Path(id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Response {
    use axum::response::IntoResponse;

    let uuid = match resolve_workspace_id(&state.workspace_manager, &id).await {
        Ok(u) => u,
        Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
    };

    // Start a background job in the VM
    let job_id = match state
        .workspace_manager
        .exec_background(uuid, &req.command, req.workdir.as_deref(), req.env.as_ref())
        .await
    {
        Ok(j) => j,
        Err(e) => {
            return error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "EXEC_STREAM_FAILED",
                e.to_string(),
            )
        }
    };

    let wm = state.workspace_manager.clone();

    // Create an async stream that polls the background job and yields SSE events
    let stream = async_stream::stream! {
        let mut prev_stdout_len: usize = 0;
        let mut prev_stderr_len: usize = 0;

        // Emit the job ID so the client knows what we're tracking
        yield Ok::<_, Infallible>(Event::default()
            .event("started")
            .data(serde_json::json!({"job_id": job_id}).to_string()));

        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            let poll_result = wm.exec_poll(uuid, job_id).await;
            match poll_result {
                Ok(status) => {
                    // Stream new stdout chunks
                    if status.stdout.len() > prev_stdout_len {
                        let new_text = &status.stdout[prev_stdout_len..];
                        prev_stdout_len = status.stdout.len();
                        yield Ok(Event::default()
                            .event("stdout")
                            .data(serde_json::json!({"data": new_text}).to_string()));
                    }

                    // Stream new stderr chunks
                    if status.stderr.len() > prev_stderr_len {
                        let new_text = &status.stderr[prev_stderr_len..];
                        prev_stderr_len = status.stderr.len();
                        yield Ok(Event::default()
                            .event("stderr")
                            .data(serde_json::json!({"data": new_text}).to_string()));
                    }

                    // If the job has finished, emit exit event and close
                    if !status.running {
                        yield Ok(Event::default()
                            .event("exit")
                            .data(serde_json::json!({
                                "exit_code": status.exit_code.unwrap_or(-1),
                            }).to_string()));
                        break;
                    }
                }
                Err(e) => {
                    yield Ok(Event::default()
                        .event("error")
                        .data(serde_json::json!({"message": e.to_string()}).to_string()));
                    break;
                }
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Simple shell escaping (single-quote wrap with escape of internal quotes).
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
