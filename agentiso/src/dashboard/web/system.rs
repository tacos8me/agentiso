use std::sync::Arc;

use axum::{
    extract::State,
    response::Response,
    routing::get,
    Router,
};

use super::{ok_response, DashboardState};

pub fn routes() -> Router<Arc<DashboardState>> {
    Router::new()
        .route("/system/health", get(health))
        .route("/system/config", get(get_config))
        .route("/system/metrics/json", get(metrics_json))
}

async fn health(State(state): State<Arc<DashboardState>>) -> Response {
    let workspace_count = state
        .workspace_manager
        .list()
        .await
        .map(|ws| ws.len())
        .unwrap_or(0);

    let running_count = state
        .workspace_manager
        .list()
        .await
        .map(|ws| {
            ws.iter()
                .filter(|w| w.state == crate::workspace::WorkspaceState::Running)
                .count()
        })
        .unwrap_or(0);

    let team_count = state.workspace_manager.list_teams().await.len();

    ok_response(serde_json::json!({
        "status": "healthy",
        "workspaces": workspace_count,
        "running": running_count,
        "teams": team_count,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
}

async fn get_config(State(state): State<Arc<DashboardState>>) -> Response {
    // Return a redacted view of the config (no secrets)
    let config = &state.config;
    ok_response(serde_json::json!({
        "storage": {
            "zfs_pool": config.storage.zfs_pool,
            "dataset_prefix": config.storage.dataset_prefix,
            "base_image": config.storage.base_image,
            "base_snapshot": config.storage.base_snapshot,
        },
        "network": {
            "bridge_name": config.network.bridge_name,
            "gateway_ip": config.network.gateway_ip.to_string(),
            "subnet_prefix": config.network.subnet_prefix,
            "default_allow_internet": config.network.default_allow_internet,
            "default_allow_inter_vm": config.network.default_allow_inter_vm,
        },
        "resources": {
            "default_vcpus": config.resources.default_vcpus,
            "default_memory_mb": config.resources.default_memory_mb,
            "default_disk_gb": config.resources.default_disk_gb,
            "max_vcpus": config.resources.max_vcpus,
            "max_memory_mb": config.resources.max_memory_mb,
            "max_disk_gb": config.resources.max_disk_gb,
            "max_workspaces": config.resources.max_workspaces,
            "max_total_vms": config.resources.max_total_vms,
            "max_vms_per_team": config.resources.max_vms_per_team,
        },
        "pool": {
            "enabled": config.pool.enabled,
            "min_size": config.pool.min_size,
            "max_size": config.pool.max_size,
            "target_free": config.pool.target_free,
        },
        "vault": {
            "enabled": config.vault.enabled,
        },
        "dashboard": {
            "enabled": config.dashboard.enabled,
            "bind_addr": config.dashboard.bind_addr,
            "port": config.dashboard.port,
            // admin_token intentionally omitted
        },
    }))
}

async fn metrics_json(State(state): State<Arc<DashboardState>>) -> Response {
    // Gather workspace-level metrics
    let workspaces = state.workspace_manager.list().await.unwrap_or_default();
    let total = workspaces.len();
    let running = workspaces
        .iter()
        .filter(|w| w.state == crate::workspace::WorkspaceState::Running)
        .count();
    let stopped = workspaces
        .iter()
        .filter(|w| w.state == crate::workspace::WorkspaceState::Stopped)
        .count();
    let suspended = workspaces
        .iter()
        .filter(|w| w.state == crate::workspace::WorkspaceState::Suspended)
        .count();

    let total_memory_mb: u32 = workspaces
        .iter()
        .filter(|w| w.state == crate::workspace::WorkspaceState::Running)
        .map(|w| w.resources.memory_mb)
        .sum();

    let total_vcpus: u32 = workspaces
        .iter()
        .filter(|w| w.state == crate::workspace::WorkspaceState::Running)
        .map(|w| w.resources.vcpus)
        .sum();

    let teams = state.workspace_manager.list_teams().await;

    ok_response(serde_json::json!({
        "workspaces": {
            "total": total,
            "running": running,
            "stopped": stopped,
            "suspended": suspended,
        },
        "resources": {
            "allocated_memory_mb": total_memory_mb,
            "allocated_vcpus": total_vcpus,
            "max_memory_mb": state.config.resources.max_memory_mb,
            "max_vcpus": state.config.resources.max_vcpus,
            "max_workspaces": state.config.resources.max_workspaces,
        },
        "teams": {
            "total": teams.len(),
        },
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
}
