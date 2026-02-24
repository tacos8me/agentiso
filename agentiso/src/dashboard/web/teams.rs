use std::sync::Arc;

use axum::{
    extract::{Path, State},
    response::Response,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::team::RoleDef;
use crate::team::task_board::TaskPriority;

use super::{error_response, ok_response, DashboardState};

pub fn routes() -> Router<Arc<DashboardState>> {
    Router::new()
        .route("/teams", get(list_teams))
        .route("/teams", post(create_team))
        .route("/teams/{name}", get(get_team))
        .route("/teams/{name}", delete(destroy_team))
        .route("/teams/{name}/messages", get(get_messages))
        .route("/teams/{name}/messages", post(send_message))
        .route("/teams/{name}/tasks", get(list_tasks))
        .route("/teams/{name}/tasks", post(create_task))
        .route("/teams/{name}/tasks/{task_id}", get(get_task))
        .route("/teams/{name}/tasks/{task_id}/claim", post(claim_task))
        .route("/teams/{name}/tasks/{task_id}/start", post(start_task))
        .route("/teams/{name}/tasks/{task_id}/complete", post(complete_task))
        .route("/teams/{name}/tasks/{task_id}/fail", post(fail_task))
        .route("/teams/{name}/tasks/{task_id}/release", post(release_task))
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateTeamRequest {
    name: String,
    roles: Vec<RoleDefRequest>,
    golden_workspace: Option<String>,
    base_snapshot: Option<String>,
    max_vms: Option<u32>,
    parent_team: Option<String>,
}

#[derive(Deserialize)]
struct RoleDefRequest {
    name: String,
    role: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct SendMessageRequest {
    from: String,
    to: Option<String>,
    content: String,
    #[serde(default)]
    broadcast: bool,
}

#[derive(Deserialize)]
struct CreateTaskRequest {
    title: String,
    description: Option<String>,
    priority: Option<String>,
    depends_on: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ClaimTaskRequest {
    owner: String,
}

#[derive(Deserialize)]
struct CompleteTaskRequest {
    result: Option<String>,
}

#[derive(Deserialize)]
struct FailTaskRequest {
    reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Team response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct TeamInfo {
    name: String,
    state: String,
    member_count: usize,
    created_at: String,
    parent_team: Option<String>,
    max_vms: u32,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_teams(State(state): State<Arc<DashboardState>>) -> Response {
    let tm = match &state.team_manager {
        Some(tm) => tm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "TEAMS_DISABLED",
                "team management is not enabled",
            )
        }
    };

    match tm.list_teams().await {
        Ok(teams) => {
            let infos: Vec<TeamInfo> = teams
                .iter()
                .map(|t| TeamInfo {
                    name: t.name.clone(),
                    state: format!("{:?}", t.state),
                    member_count: t.member_workspace_ids.len(),
                    created_at: t.created_at.to_rfc3339(),
                    parent_team: t.parent_team.clone(),
                    max_vms: t.max_vms,
                })
                .collect();
            ok_response(infos)
        }
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "LIST_TEAMS_FAILED",
            e.to_string(),
        ),
    }
}

async fn create_team(
    State(state): State<Arc<DashboardState>>,
    Json(req): Json<CreateTeamRequest>,
) -> Response {
    let tm = match &state.team_manager {
        Some(tm) => tm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "TEAMS_DISABLED",
                "team management is not enabled",
            )
        }
    };

    let roles: Vec<RoleDef> = req
        .roles
        .into_iter()
        .map(|r| RoleDef {
            name: r.name,
            role: r.role,
            skills: r.skills,
            description: r.description,
        })
        .collect();

    // Resolve golden workspace ID if provided
    let golden_ws_id = if let Some(ref gw) = req.golden_workspace {
        match super::resolve_workspace_id(&state.workspace_manager, gw).await {
            Ok(id) => Some(id),
            Err((status, msg)) => return error_response(status, "NOT_FOUND", msg),
        }
    } else {
        None
    };

    let max_vms = req.max_vms.unwrap_or(20);

    match tm
        .create_team(
            &req.name,
            roles,
            golden_ws_id,
            req.base_snapshot.as_deref(),
            max_vms,
            req.parent_team.as_deref(),
        )
        .await
    {
        Ok(_team_state) => {
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
            axum::http::StatusCode::BAD_REQUEST,
            "CREATE_TEAM_FAILED",
            e.to_string(),
        ),
    }
}

async fn get_team(
    State(state): State<Arc<DashboardState>>,
    Path(name): Path<String>,
) -> Response {
    let tm = match &state.team_manager {
        Some(tm) => tm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "TEAMS_DISABLED",
                "team management is not enabled",
            )
        }
    };

    match tm.team_status(&name).await {
        Ok(report) => ok_response(report),
        Err(e) => error_response(
            axum::http::StatusCode::NOT_FOUND,
            "TEAM_NOT_FOUND",
            e.to_string(),
        ),
    }
}

async fn destroy_team(
    State(state): State<Arc<DashboardState>>,
    Path(name): Path<String>,
) -> Response {
    let tm = match &state.team_manager {
        Some(tm) => tm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "TEAMS_DISABLED",
                "team management is not enabled",
            )
        }
    };

    match tm.destroy_team(&name).await {
        Ok(()) => ok_response(serde_json::json!({"destroyed": name})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "DESTROY_TEAM_FAILED",
            e.to_string(),
        ),
    }
}

async fn get_messages(
    State(state): State<Arc<DashboardState>>,
    Path(name): Path<String>,
) -> Response {
    // List all agents in this team and pull their messages
    let agents = state.message_relay.team_agents(&name).await;
    if agents.is_empty() {
        // Team might exist but no agents registered in relay
        return ok_response(serde_json::json!([]));
    }

    let mut all_messages = Vec::new();
    for agent_name in &agents {
        if let Ok(messages) = state.message_relay.receive(&name, agent_name, 50).await {
            all_messages.extend(messages);
        }
    }

    ok_response(all_messages)
}

async fn send_message(
    State(state): State<Arc<DashboardState>>,
    Path(name): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Response {
    let to = if req.broadcast { "*" } else {
        match &req.to {
            Some(t) => t.as_str(),
            None => {
                return error_response(
                    axum::http::StatusCode::BAD_REQUEST,
                    "MISSING_RECIPIENT",
                    "set 'to' for direct message or 'broadcast: true' for team broadcast",
                );
            }
        }
    };

    match state
        .message_relay
        .send(&name, &req.from, to, &req.content, "text")
        .await
    {
        Ok(msg_id) => ok_response(serde_json::json!({"sent": true, "message_id": msg_id})),
        Err(e) => error_response(
            axum::http::StatusCode::BAD_REQUEST,
            "SEND_FAILED",
            e,
        ),
    }
}

// ---------------------------------------------------------------------------
// Task board handlers
// ---------------------------------------------------------------------------

fn parse_priority(s: &str) -> TaskPriority {
    match s {
        "low" => TaskPriority::Low,
        "high" => TaskPriority::High,
        "critical" => TaskPriority::Critical,
        _ => TaskPriority::Medium,
    }
}

async fn list_tasks(
    State(state): State<Arc<DashboardState>>,
    Path(name): Path<String>,
) -> Response {
    let vm = match &state.vault_manager {
        Some(vm) => vm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "VAULT_DISABLED",
                "vault is required for task board",
            )
        }
    };

    let board = crate::team::task_board::TaskBoard::new(vm.clone(), name);
    match board.list_tasks(None).await {
        Ok(tasks) => ok_response(tasks),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "LIST_TASKS_FAILED",
            e.to_string(),
        ),
    }
}

async fn create_task(
    State(state): State<Arc<DashboardState>>,
    Path(name): Path<String>,
    Json(req): Json<CreateTaskRequest>,
) -> Response {
    let vm = match &state.vault_manager {
        Some(vm) => vm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "VAULT_DISABLED",
                "vault is required for task board",
            )
        }
    };

    let priority = parse_priority(req.priority.as_deref().unwrap_or("medium"));
    let depends_on = req.depends_on.unwrap_or_default();

    let board = crate::team::task_board::TaskBoard::new(vm.clone(), name);
    match board
        .create_task(
            &req.title,
            req.description.as_deref().unwrap_or(""),
            priority,
            depends_on,
        )
        .await
    {
        Ok(task) => {
            use axum::response::IntoResponse;
            (
                axum::http::StatusCode::CREATED,
                axum::Json(super::ApiResponse::new(task)),
            )
                .into_response()
        }
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "CREATE_TASK_FAILED",
            e.to_string(),
        ),
    }
}

async fn get_task(
    State(state): State<Arc<DashboardState>>,
    Path((name, task_id)): Path<(String, String)>,
) -> Response {
    let vm = match &state.vault_manager {
        Some(vm) => vm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "VAULT_DISABLED",
                "vault is required for task board",
            )
        }
    };

    let board = crate::team::task_board::TaskBoard::new(vm.clone(), name);
    match board.get_task(&task_id).await {
        Ok(task) => ok_response(task),
        Err(e) => error_response(
            axum::http::StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            e.to_string(),
        ),
    }
}

async fn claim_task(
    State(state): State<Arc<DashboardState>>,
    Path((name, task_id)): Path<(String, String)>,
    Json(req): Json<ClaimTaskRequest>,
) -> Response {
    let vm = match &state.vault_manager {
        Some(vm) => vm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "VAULT_DISABLED",
                "vault is required for task board",
            )
        }
    };

    let board = crate::team::task_board::TaskBoard::new(vm.clone(), name);
    match board.claim_task(&task_id, &req.owner).await {
        Ok(true) => ok_response(serde_json::json!({"claimed": task_id, "owner": req.owner})),
        Ok(false) => error_response(
            axum::http::StatusCode::CONFLICT,
            "ALREADY_CLAIMED",
            format!("task '{}' is already claimed", task_id),
        ),
        Err(e) => error_response(
            axum::http::StatusCode::CONFLICT,
            "CLAIM_FAILED",
            e.to_string(),
        ),
    }
}

async fn start_task(
    State(state): State<Arc<DashboardState>>,
    Path((name, task_id)): Path<(String, String)>,
) -> Response {
    let vm = match &state.vault_manager {
        Some(vm) => vm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "VAULT_DISABLED",
                "vault is required for task board",
            )
        }
    };

    let board = crate::team::task_board::TaskBoard::new(vm.clone(), name);
    match board.start_task(&task_id).await {
        Ok(()) => ok_response(serde_json::json!({"started": task_id})),
        Err(e) => error_response(
            axum::http::StatusCode::CONFLICT,
            "START_FAILED",
            e.to_string(),
        ),
    }
}

async fn complete_task(
    State(state): State<Arc<DashboardState>>,
    Path((name, task_id)): Path<(String, String)>,
    Json(req): Json<Option<CompleteTaskRequest>>,
) -> Response {
    let vm = match &state.vault_manager {
        Some(vm) => vm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "VAULT_DISABLED",
                "vault is required for task board",
            )
        }
    };

    let board = crate::team::task_board::TaskBoard::new(vm.clone(), name);
    let result_msg = req.and_then(|r| r.result);
    match board.complete_task(&task_id, result_msg.as_deref()).await {
        Ok(()) => ok_response(serde_json::json!({"completed": task_id})),
        Err(e) => error_response(
            axum::http::StatusCode::CONFLICT,
            "COMPLETE_FAILED",
            e.to_string(),
        ),
    }
}

async fn fail_task(
    State(state): State<Arc<DashboardState>>,
    Path((name, task_id)): Path<(String, String)>,
    Json(req): Json<Option<FailTaskRequest>>,
) -> Response {
    let vm = match &state.vault_manager {
        Some(vm) => vm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "VAULT_DISABLED",
                "vault is required for task board",
            )
        }
    };

    let board = crate::team::task_board::TaskBoard::new(vm.clone(), name);
    let reason = req.and_then(|r| r.reason).unwrap_or_default();
    match board.fail_task(&task_id, &reason).await {
        Ok(()) => ok_response(serde_json::json!({"failed": task_id})),
        Err(e) => error_response(
            axum::http::StatusCode::CONFLICT,
            "FAIL_FAILED",
            e.to_string(),
        ),
    }
}

async fn release_task(
    State(state): State<Arc<DashboardState>>,
    Path((name, task_id)): Path<(String, String)>,
) -> Response {
    let vm = match &state.vault_manager {
        Some(vm) => vm,
        None => {
            return error_response(
                axum::http::StatusCode::NOT_FOUND,
                "VAULT_DISABLED",
                "vault is required for task board",
            )
        }
    };

    let board = crate::team::task_board::TaskBoard::new(vm.clone(), name);
    match board.release_task(&task_id).await {
        Ok(()) => ok_response(serde_json::json!({"released": task_id})),
        Err(e) => error_response(
            axum::http::StatusCode::CONFLICT,
            "RELEASE_FAILED",
            e.to_string(),
        ),
    }
}
