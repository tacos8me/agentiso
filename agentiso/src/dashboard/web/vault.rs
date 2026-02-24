use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    response::Response,
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use super::{error_response, ok_response, DashboardState};

pub fn routes() -> Router<Arc<DashboardState>> {
    Router::new()
        .route("/vault/notes", get(list_notes))
        .route("/vault/notes/{*path}", get(read_note).put(write_note).delete(delete_note))
        .route("/vault/search", get(search_notes))
        .route("/vault/frontmatter/{*path}", get(get_frontmatter).put(set_frontmatter))
        .route("/vault/tags/{*path}", get(get_tags).post(add_tag))
        .route("/vault/stats", get(vault_stats))
        .route("/vault/graph", get(vault_graph))
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListNotesQuery {
    folder: Option<String>,
    recursive: Option<bool>,
}

#[derive(Deserialize)]
struct SearchQuery {
    query: String,
    folder: Option<String>,
    max_results: Option<usize>,
}

#[derive(Deserialize)]
struct WriteNoteRequest {
    content: String,
}

#[derive(Deserialize)]
struct SetFrontmatterRequest {
    key: String,
    value: serde_json::Value,
}

#[derive(Deserialize)]
struct AddTagRequest {
    tag: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[allow(clippy::result_large_err)] // Response is axum's own type; boxing would add complexity without benefit
fn get_vault(state: &DashboardState) -> Result<&Arc<crate::mcp::vault::VaultManager>, Response> {
    state.vault_manager.as_ref().ok_or_else(|| {
        error_response(
            axum::http::StatusCode::NOT_FOUND,
            "VAULT_DISABLED",
            "vault is not enabled in config",
        )
    })
}

async fn list_notes(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<ListNotesQuery>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    let folder = query.folder.as_deref();
    let recursive = query.recursive.unwrap_or(true);

    match vm.list_notes(folder, recursive).await {
        Ok(notes) => ok_response(notes),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "LIST_NOTES_FAILED",
            e.to_string(),
        ),
    }
}

async fn read_note(
    State(state): State<Arc<DashboardState>>,
    Path(path): Path<String>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    match vm.read_note(&path).await {
        Ok(note) => ok_response(note),
        Err(e) => error_response(
            axum::http::StatusCode::NOT_FOUND,
            "NOTE_NOT_FOUND",
            e.to_string(),
        ),
    }
}

async fn write_note(
    State(state): State<Arc<DashboardState>>,
    Path(path): Path<String>,
    Json(req): Json<WriteNoteRequest>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    let mode = crate::mcp::vault::WriteMode::Overwrite;

    match vm.write_note(&path, &req.content, mode).await {
        Ok(()) => ok_response(serde_json::json!({"written": path})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "WRITE_NOTE_FAILED",
            e.to_string(),
        ),
    }
}

async fn delete_note(
    State(state): State<Arc<DashboardState>>,
    Path(path): Path<String>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    match vm.delete_note(&path).await {
        Ok(()) => ok_response(serde_json::json!({"deleted": path})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "DELETE_NOTE_FAILED",
            e.to_string(),
        ),
    }
}

async fn search_notes(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<SearchQuery>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    let max_results = query.max_results.unwrap_or(20);

    match vm
        .search(&query.query, false, query.folder.as_deref(), None, max_results)
        .await
    {
        Ok(results) => ok_response(results),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "SEARCH_FAILED",
            e.to_string(),
        ),
    }
}

async fn get_frontmatter(
    State(state): State<Arc<DashboardState>>,
    Path(path): Path<String>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    match vm.get_frontmatter(&path).await {
        Ok(Some(fm)) => ok_response(fm),
        Ok(None) => ok_response(serde_json::json!(null)),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "FRONTMATTER_FAILED",
            e.to_string(),
        ),
    }
}

async fn set_frontmatter(
    State(state): State<Arc<DashboardState>>,
    Path(path): Path<String>,
    Json(req): Json<SetFrontmatterRequest>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    // Convert serde_json::Value to serde_yaml::Value
    let yaml_value: serde_yaml::Value =
        serde_json::from_value(serde_json::to_value(&req.value).unwrap_or_default())
            .unwrap_or(serde_yaml::Value::Null);

    match vm.set_frontmatter(&path, &req.key, yaml_value).await {
        Ok(()) => ok_response(serde_json::json!({"set": req.key})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "SET_FRONTMATTER_FAILED",
            e.to_string(),
        ),
    }
}

async fn get_tags(
    State(state): State<Arc<DashboardState>>,
    Path(path): Path<String>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    match vm.get_tags(&path).await {
        Ok(tags) => ok_response(tags),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "GET_TAGS_FAILED",
            e.to_string(),
        ),
    }
}

async fn add_tag(
    State(state): State<Arc<DashboardState>>,
    Path(path): Path<String>,
    Json(req): Json<AddTagRequest>,
) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    match vm.add_tag(&path, &req.tag).await {
        Ok(()) => ok_response(serde_json::json!({"added": req.tag})),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "ADD_TAG_FAILED",
            e.to_string(),
        ),
    }
}

async fn vault_stats(State(state): State<Arc<DashboardState>>) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    match vm.stats(10).await {
        Ok(stats) => ok_response(stats),
        Err(e) => error_response(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "STATS_FAILED",
            e.to_string(),
        ),
    }
}

async fn vault_graph(State(state): State<Arc<DashboardState>>) -> Response {
    let vm = match get_vault(&state) {
        Ok(vm) => vm,
        Err(resp) => return resp,
    };

    // Build graph data: list all notes and find [[wikilink]] references
    let entries = match vm.list_notes(None, true).await {
        Ok(n) => n,
        Err(e) => {
            return error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "GRAPH_FAILED",
                e.to_string(),
            )
        }
    };

    let wikilink_re = regex::Regex::new(r"\[\[([^\]]+)\]\]").unwrap();

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for entry in &entries {
        if entry.is_dir {
            continue;
        }
        let path_str = &entry.path;
        nodes.push(serde_json::json!({"id": path_str, "label": path_str}));

        if let Ok(note) = vm.read_note(path_str).await {
            for cap in wikilink_re.captures_iter(&note.content) {
                let target = cap[1].to_string();
                edges.push(serde_json::json!({
                    "source": path_str,
                    "target": target,
                }));
            }
        }
    }

    ok_response(serde_json::json!({
        "nodes": nodes,
        "edges": edges,
    }))
}
