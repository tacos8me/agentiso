use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::error;
use uuid::Uuid;

use crate::workspace::WorkspaceManager;

use super::auth::AuthManager;

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceCreateParams {
    /// Human-readable name for the workspace
    name: Option<String>,
    /// Base image to use (default: alpine-dev)
    base_image: Option<String>,
    /// Number of virtual CPUs (default: 2)
    vcpus: Option<u32>,
    /// Memory in megabytes (default: 512)
    memory_mb: Option<u32>,
    /// Disk size in gigabytes (default: 10)
    disk_gb: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceIdParams {
    /// ID of the workspace
    workspace_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceListParams {
    /// Filter by state: running, stopped, or suspended
    state_filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecParams {
    /// ID of the workspace
    workspace_id: String,
    /// Shell command to execute (passed to /bin/sh -c)
    command: String,
    /// Timeout in seconds (default: 30)
    timeout_secs: Option<u64>,
    /// Working directory inside the VM
    workdir: Option<String>,
    /// Environment variables as key=value pairs
    env: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileWriteParams {
    /// ID of the workspace
    workspace_id: String,
    /// Absolute path inside the VM where the file will be written
    path: String,
    /// File content (text)
    content: String,
    /// File permission mode as octal integer (e.g. 644)
    mode: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileReadParams {
    /// ID of the workspace
    workspace_id: String,
    /// Absolute path of the file inside the VM
    path: String,
    /// Byte offset to start reading from
    offset: Option<u64>,
    /// Maximum number of bytes to read
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileTransferParams {
    /// ID of the workspace
    workspace_id: String,
    /// Path on the host filesystem
    host_path: String,
    /// Path inside the guest VM
    guest_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotCreateParams {
    /// ID of the workspace
    workspace_id: String,
    /// Name for the snapshot
    name: String,
    /// Include VM memory state for live snapshot (default: false)
    include_memory: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotNameParams {
    /// ID of the workspace
    workspace_id: String,
    /// Name of the snapshot
    snapshot_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceForkParams {
    /// ID of the source workspace
    workspace_id: String,
    /// Name of the snapshot to fork from
    snapshot_name: String,
    /// Name for the new forked workspace
    new_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PortForwardParams {
    /// ID of the workspace
    workspace_id: String,
    /// Port inside the guest VM to forward to
    guest_port: u16,
    /// Host port to listen on (auto-assigned if omitted)
    host_port: Option<u16>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PortForwardRemoveParams {
    /// ID of the workspace
    workspace_id: String,
    /// Guest port whose forwarding rule should be removed
    guest_port: u16,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NetworkPolicyParams {
    /// ID of the workspace
    workspace_id: String,
    /// Allow outbound internet access
    allow_internet: Option<bool>,
    /// Allow communication with other workspace VMs
    allow_inter_vm: Option<bool>,
    /// List of TCP ports allowed for inbound connections
    allowed_ports: Option<Vec<u16>>,
}

// ---------------------------------------------------------------------------
// MCP server struct
// ---------------------------------------------------------------------------

/// The agentiso MCP server. Holds shared state and routes tool calls.
#[derive(Clone)]
pub struct AgentisoServer {
    workspace_manager: Arc<WorkspaceManager>,
    auth: AuthManager,
    session_id: String,
    /// Allowed directory for host-side file transfers. All host_path values
    /// in file_upload/file_download must resolve within this directory.
    transfer_dir: PathBuf,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AgentisoServer {
    pub fn new(
        workspace_manager: Arc<WorkspaceManager>,
        auth: AuthManager,
        session_id: String,
        transfer_dir: PathBuf,
    ) -> Self {
        Self {
            workspace_manager,
            auth,
            session_id,
            transfer_dir,
            tool_router: Self::tool_router(),
        }
    }

    // -----------------------------------------------------------------------
    // Lifecycle Tools
    // -----------------------------------------------------------------------

    /// Create and start a new isolated workspace VM. Returns the workspace ID and connection details.
    #[tool]
    async fn workspace_create(
        &self,
        Parameters(params): Parameters<WorkspaceCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let mem = params.memory_mb.unwrap_or(512);
        let disk = params.disk_gb.unwrap_or(10);

        // Enforce session quota before creating.
        self.auth
            .check_quota(&self.session_id, mem as u64, disk as u64)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let create_params = crate::workspace::CreateParams {
            name: params.name,
            base_image: params.base_image,
            vcpus: params.vcpus,
            memory_mb: Some(mem),
            disk_gb: Some(disk),
        };

        let workspace = self
            .workspace_manager
            .create(create_params)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Register ownership. If this fails (e.g. quota exceeded), destroy
        // the workspace we just created to avoid orphaned resources.
        if let Err(e) = self
            .auth
            .register_workspace(
                &self.session_id,
                workspace.id,
                workspace.resources.memory_mb as u64,
                workspace.resources.disk_gb as u64,
            )
            .await
        {
            error!(
                workspace_id = %workspace.id,
                error = %e,
                "quota check failed after workspace creation, rolling back"
            );
            if let Err(destroy_err) = self.workspace_manager.destroy(workspace.id).await {
                error!(
                    workspace_id = %workspace.id,
                    error = %destroy_err,
                    "failed to destroy workspace during rollback"
                );
            }
            return Err(McpError::invalid_request(e.to_string(), None));
        }

        let info = serde_json::json!({
            "workspace_id": workspace.id.to_string(),
            "name": workspace.name,
            "state": workspace.state,
            "ip": workspace.network.ip.to_string(),
            "resources": {
                "vcpus": workspace.resources.vcpus,
                "memory_mb": workspace.resources.memory_mb,
                "disk_gb": workspace.resources.disk_gb,
            },
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Stop and permanently destroy a workspace VM and all its storage.
    #[tool]
    async fn workspace_destroy(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let destroy_result = self.workspace_manager.destroy(ws_id).await;

        // Always unregister from auth/quota tracking, even if destroy failed.
        // This prevents leaked quota when a workspace is partially destroyed.
        self.auth
            .unregister_workspace(
                &self.session_id,
                ws_id,
                ws.resources.memory_mb as u64,
                ws.resources.disk_gb as u64,
            )
            .await
            .ok();

        destroy_result.map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Workspace {} destroyed.",
            params.workspace_id
        ))]))
    }

    /// List all workspaces owned by this session, with optional state filter.
    #[tool]
    async fn workspace_list(
        &self,
        Parameters(params): Parameters<WorkspaceListParams>,
    ) -> Result<CallToolResult, McpError> {
        let owned_ids = self
            .auth
            .list_workspaces(&self.session_id)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let all = self
            .workspace_manager
            .list()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let state_filter = params.state_filter.as_deref();

        let workspaces: Vec<serde_json::Value> = all
            .into_iter()
            .filter(|ws| owned_ids.contains(&ws.id))
            .filter(|ws| {
                state_filter
                    .map(|f| ws.state.to_string() == f)
                    .unwrap_or(true)
            })
            .map(|ws| {
                serde_json::json!({
                    "workspace_id": ws.id.to_string(),
                    "name": ws.name,
                    "state": ws.state,
                    "ip": ws.network.ip.to_string(),
                    "resources": {
                        "vcpus": ws.resources.vcpus,
                        "memory_mb": ws.resources.memory_mb,
                        "disk_gb": ws.resources.disk_gb,
                    },
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&workspaces).unwrap(),
        )]))
    }

    /// Get detailed information about a workspace including snapshots and network config.
    #[tool]
    async fn workspace_info(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let snapshots: Vec<serde_json::Value> = ws
            .snapshots
            .list()
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id.to_string(),
                    "name": s.name,
                    "has_memory": s.qemu_state.is_some(),
                    "parent": s.parent.map(|p| p.to_string()),
                    "created_at": s.created_at.to_rfc3339(),
                })
            })
            .collect();

        let port_forwards: Vec<serde_json::Value> = ws
            .network
            .port_forwards
            .iter()
            .map(|pf| {
                serde_json::json!({
                    "host_port": pf.host_port,
                    "guest_port": pf.guest_port,
                })
            })
            .collect();

        let info = serde_json::json!({
            "workspace_id": ws.id.to_string(),
            "name": ws.name,
            "state": ws.state,
            "base_image": ws.base_image,
            "ip": ws.network.ip.to_string(),
            "resources": {
                "vcpus": ws.resources.vcpus,
                "memory_mb": ws.resources.memory_mb,
                "disk_gb": ws.resources.disk_gb,
            },
            "network": {
                "allow_internet": ws.network.allow_internet,
                "allow_inter_vm": ws.network.allow_inter_vm,
                "allowed_ports": ws.network.allowed_ports,
                "port_forwards": port_forwards,
            },
            "snapshots": snapshots,
            "created_at": ws.created_at.to_rfc3339(),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Gracefully stop a running workspace VM. The workspace can be started again later.
    #[tool]
    async fn workspace_stop(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .stop(ws_id)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Workspace {} stopped.",
            params.workspace_id
        ))]))
    }

    /// Start (boot) a stopped workspace VM.
    #[tool]
    async fn workspace_start(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .start(ws_id)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Workspace {} started.",
            params.workspace_id
        ))]))
    }

    // -----------------------------------------------------------------------
    // Execution Tools
    // -----------------------------------------------------------------------

    /// Execute a shell command inside a running workspace VM. Returns stdout, stderr, and exit code.
    #[tool]
    async fn exec(
        &self,
        Parameters(params): Parameters<ExecParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        let result = self
            .workspace_manager
            .exec(
                ws_id,
                &params.command,
                params.workdir.as_deref(),
                params.env.as_ref(),
                params.timeout_secs,
            )
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap(),
        )]))
    }

    /// Write a file inside a running workspace VM.
    #[tool]
    async fn file_write(
        &self,
        Parameters(params): Parameters<FileWriteParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .file_write(ws_id, &params.path, params.content.as_bytes(), params.mode)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "File written: {}",
            params.path
        ))]))
    }

    /// Read a file from inside a running workspace VM.
    #[tool]
    async fn file_read(
        &self,
        Parameters(params): Parameters<FileReadParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        let data = self
            .workspace_manager
            .file_read(ws_id, &params.path, params.offset, params.limit)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let text = String::from_utf8_lossy(&data);

        Ok(CallToolResult::success(vec![Content::text(
            text.into_owned(),
        )]))
    }

    /// Upload a file from the host filesystem into a running workspace VM.
    /// The host_path must be within the configured transfer directory.
    #[tool]
    async fn file_upload(
        &self,
        Parameters(params): Parameters<FileTransferParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        // Validate host path is within allowed transfer directory.
        let safe_path = self.validate_host_path(&params.host_path, true)?;

        let host_data = tokio::fs::read(&safe_path).await.map_err(|e| {
            McpError::invalid_request(format!("failed to read host file: {}", e), None)
        })?;

        self.workspace_manager
            .file_write(ws_id, &params.guest_path, &host_data, None)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Uploaded {} -> {} ({} bytes)",
            safe_path.display(),
            params.guest_path,
            host_data.len()
        ))]))
    }

    /// Download a file from a running workspace VM to the host filesystem.
    /// The host_path must be within the configured transfer directory.
    #[tool]
    async fn file_download(
        &self,
        Parameters(params): Parameters<FileTransferParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        // Validate host path is within allowed transfer directory.
        let safe_path = self.validate_host_path(&params.host_path, false)?;

        let data = self
            .workspace_manager
            .file_read(ws_id, &params.guest_path, None, None)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        tokio::fs::write(&safe_path, &data).await.map_err(|e| {
            McpError::internal_error(format!("failed to write host file: {}", e), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Downloaded {} -> {} ({} bytes)",
            params.guest_path,
            safe_path.display(),
            data.len()
        ))]))
    }

    // -----------------------------------------------------------------------
    // Snapshot Tools
    // -----------------------------------------------------------------------

    /// Create a named snapshot (checkpoint) of a workspace. Optionally includes VM memory state for live restore.
    #[tool]
    async fn snapshot_create(
        &self,
        Parameters(params): Parameters<SnapshotCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        let snapshot = self
            .workspace_manager
            .snapshot_create(ws_id, &params.name, params.include_memory.unwrap_or(false))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let info = serde_json::json!({
            "snapshot_id": snapshot.id.to_string(),
            "name": snapshot.name,
            "workspace_id": snapshot.workspace_id.to_string(),
            "has_memory": snapshot.qemu_state.is_some(),
            "created_at": snapshot.created_at.to_rfc3339(),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Restore a workspace to a previously created snapshot. The workspace will be stopped and restarted.
    #[tool]
    async fn snapshot_restore(
        &self,
        Parameters(params): Parameters<SnapshotNameParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .snapshot_restore(ws_id, &params.snapshot_name)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Workspace {} restored to snapshot '{}'.",
            params.workspace_id, params.snapshot_name
        ))]))
    }

    /// List all snapshots for a workspace, showing the snapshot tree.
    #[tool]
    async fn snapshot_list(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let snapshots: Vec<serde_json::Value> = ws
            .snapshots
            .list()
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id.to_string(),
                    "name": s.name,
                    "has_memory": s.qemu_state.is_some(),
                    "parent": s.parent.map(|p| p.to_string()),
                    "created_at": s.created_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&snapshots).unwrap(),
        )]))
    }

    /// Delete a snapshot from a workspace.
    #[tool]
    async fn snapshot_delete(
        &self,
        Parameters(params): Parameters<SnapshotNameParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .snapshot_delete(ws_id, &params.snapshot_name)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Snapshot '{}' deleted from workspace {}.",
            params.snapshot_name, params.workspace_id
        ))]))
    }

    /// Fork (clone) a new workspace from an existing workspace's snapshot. Creates an independent copy.
    #[tool]
    async fn workspace_fork(
        &self,
        Parameters(params): Parameters<WorkspaceForkParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        // Check quota for the new workspace (use same resources as source).
        let source_ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let mem = source_ws.resources.memory_mb as u64;
        let disk = source_ws.resources.disk_gb as u64;

        // Pre-check quota before forking.
        self.auth
            .check_quota(&self.session_id, mem, disk)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let new_ws = self
            .workspace_manager
            .fork(ws_id, &params.snapshot_name, params.new_name)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if let Err(e) = self
            .auth
            .register_workspace(
                &self.session_id,
                new_ws.id,
                new_ws.resources.memory_mb as u64,
                new_ws.resources.disk_gb as u64,
            )
            .await
        {
            error!(
                workspace_id = %new_ws.id,
                error = %e,
                "quota registration failed after fork, rolling back"
            );
            if let Err(destroy_err) = self.workspace_manager.destroy(new_ws.id).await {
                error!(
                    workspace_id = %new_ws.id,
                    error = %destroy_err,
                    "failed to destroy forked workspace during rollback"
                );
            }
            return Err(McpError::internal_error(e.to_string(), None));
        }

        let info = serde_json::json!({
            "workspace_id": new_ws.id.to_string(),
            "name": new_ws.name,
            "state": new_ws.state,
            "ip": new_ws.network.ip.to_string(),
            "forked_from": {
                "workspace_id": params.workspace_id,
                "snapshot": params.snapshot_name,
            },
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Network Tools
    // -----------------------------------------------------------------------

    /// Forward a host port to a guest port in a workspace VM. Returns the assigned host port.
    #[tool]
    async fn port_forward(
        &self,
        Parameters(params): Parameters<PortForwardParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        let assigned_host_port = self
            .workspace_manager
            .port_forward_add(ws_id, params.guest_port, params.host_port)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let info = serde_json::json!({
            "workspace_id": params.workspace_id,
            "guest_port": params.guest_port,
            "host_port": assigned_host_port,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Remove a port forwarding rule from a workspace.
    #[tool]
    async fn port_forward_remove(
        &self,
        Parameters(params): Parameters<PortForwardRemoveParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .port_forward_remove(ws_id, params.guest_port)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Port forward for guest port {} removed from workspace {}.",
            params.guest_port, params.workspace_id
        ))]))
    }

    /// Get the IP address of a workspace VM.
    #[tool]
    async fn workspace_ip(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let info = serde_json::json!({
            "workspace_id": params.workspace_id,
            "ip": ws.network.ip.to_string(),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Set the network isolation policy for a workspace. Controls internet access, inter-VM communication, and allowed ports.
    #[tool]
    async fn network_policy(
        &self,
        Parameters(params): Parameters<NetworkPolicyParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .update_network_policy(
                ws_id,
                params.allow_internet,
                params.allow_inter_vm,
                params.allowed_ports,
            )
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let info = serde_json::json!({
            "workspace_id": params.workspace_id,
            "network_policy": {
                "allow_internet": ws.network.allow_internet,
                "allow_inter_vm": ws.network.allow_inter_vm,
                "allowed_ports": ws.network.allowed_ports,
            },
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for AgentisoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "agentiso: QEMU microvm workspace manager. Create, manage, and execute commands in isolated VM workspaces."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl AgentisoServer {
    /// Verify the current session owns the given workspace.
    async fn check_ownership(&self, workspace_id: Uuid) -> Result<(), McpError> {
        self.auth
            .check_ownership(&self.session_id, workspace_id)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))
    }

    /// Validate that a host_path for file transfer is safe.
    ///
    /// The path must resolve (after canonicalizing `..` and symlinks) to a
    /// location within the configured `transfer_dir`. This prevents path
    /// traversal attacks where an MCP client could read/write arbitrary
    /// host files.
    ///
    /// For uploads (host -> guest), the file must already exist so we can
    /// canonicalize it. For downloads (guest -> host), the file may not
    /// exist yet, so we canonicalize the parent directory instead and
    /// verify it is within transfer_dir.
    fn validate_host_path(&self, host_path: &str, must_exist: bool) -> Result<PathBuf, McpError> {
        validate_host_path_in_dir(host_path, &self.transfer_dir, must_exist)
    }
}

/// Validate that `host_path` resolves to a location within `transfer_dir`.
///
/// If `must_exist` is true, the file itself is canonicalized and checked.
/// If `must_exist` is false, the parent directory is canonicalized and the
/// filename is appended, then checked.
fn validate_host_path_in_dir(
    host_path: &str,
    transfer_dir: &Path,
    must_exist: bool,
) -> Result<PathBuf, McpError> {
    let path = Path::new(host_path);

    // Canonicalize the transfer_dir (it must exist).
    let canonical_root = transfer_dir.canonicalize().map_err(|e| {
        McpError::internal_error(
            format!("transfer directory not accessible: {}", e),
            None,
        )
    })?;

    if must_exist {
        // File must exist -- canonicalize the full path.
        let canonical = path.canonicalize().map_err(|e| {
            McpError::invalid_request(format!("host path not accessible: {}", e), None)
        })?;
        if !canonical.starts_with(&canonical_root) {
            return Err(McpError::invalid_request(
                format!(
                    "host_path must be within transfer directory: {}",
                    transfer_dir.display()
                ),
                None,
            ));
        }
        Ok(canonical)
    } else {
        // File may not exist -- canonicalize the parent, then append filename.
        let parent = path.parent().ok_or_else(|| {
            McpError::invalid_request("host_path has no parent directory".to_string(), None)
        })?;
        let file_name = path.file_name().ok_or_else(|| {
            McpError::invalid_request("host_path has no file name".to_string(), None)
        })?;
        let canonical_parent = parent.canonicalize().map_err(|e| {
            McpError::invalid_request(
                format!("host path parent directory not accessible: {}", e),
                None,
            )
        })?;
        let canonical = canonical_parent.join(file_name);
        if !canonical.starts_with(&canonical_root) {
            return Err(McpError::invalid_request(
                format!(
                    "host_path must be within transfer directory: {}",
                    transfer_dir.display()
                ),
                None,
            ));
        }
        Ok(canonical)
    }
}

/// Parse a workspace ID string into a UUID.
fn parse_uuid(s: &str) -> Result<Uuid, McpError> {
    Uuid::parse_str(s)
        .map_err(|e| McpError::invalid_request(format!("invalid workspace_id: {}", e), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workspace_create_params_all_fields() {
        let json = serde_json::json!({
            "name": "my-workspace",
            "base_image": "alpine-dev",
            "vcpus": 4,
            "memory_mb": 1024,
            "disk_gb": 20
        });
        let params: WorkspaceCreateParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.name.as_deref(), Some("my-workspace"));
        assert_eq!(params.base_image.as_deref(), Some("alpine-dev"));
        assert_eq!(params.vcpus, Some(4));
        assert_eq!(params.memory_mb, Some(1024));
        assert_eq!(params.disk_gb, Some(20));
    }

    #[test]
    fn test_workspace_create_params_defaults() {
        // All fields are optional.
        let json = serde_json::json!({});
        let params: WorkspaceCreateParams = serde_json::from_value(json).unwrap();
        assert!(params.name.is_none());
        assert!(params.base_image.is_none());
        assert!(params.vcpus.is_none());
        assert!(params.memory_mb.is_none());
        assert!(params.disk_gb.is_none());
    }

    #[test]
    fn test_workspace_id_params_required() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: WorkspaceIdParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_workspace_id_params_missing_required() {
        let json = serde_json::json!({});
        let result = serde_json::from_value::<WorkspaceIdParams>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_exec_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "command": "ls -la /tmp",
            "timeout_secs": 60,
            "workdir": "/home/user",
            "env": {"PATH": "/usr/bin", "HOME": "/root"}
        });
        let params: ExecParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.command, "ls -la /tmp");
        assert_eq!(params.timeout_secs, Some(60));
        assert_eq!(params.workdir.as_deref(), Some("/home/user"));
        assert_eq!(params.env.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_exec_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "command": "echo hello"
        });
        let params: ExecParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.command, "echo hello");
        assert!(params.timeout_secs.is_none());
        assert!(params.workdir.is_none());
        assert!(params.env.is_none());
    }

    #[test]
    fn test_file_write_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/tmp/test.txt",
            "content": "hello world",
            "mode": 644
        });
        let params: FileWriteParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "/tmp/test.txt");
        assert_eq!(params.content, "hello world");
        assert_eq!(params.mode, Some(644));
    }

    #[test]
    fn test_file_read_params_with_offset() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/var/log/syslog",
            "offset": 1024,
            "limit": 4096
        });
        let params: FileReadParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.offset, Some(1024));
        assert_eq!(params.limit, Some(4096));
    }

    #[test]
    fn test_snapshot_create_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "before-experiment",
            "include_memory": true
        });
        let params: SnapshotCreateParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.name, "before-experiment");
        assert_eq!(params.include_memory, Some(true));
    }

    #[test]
    fn test_port_forward_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_port": 8080,
            "host_port": 9090
        });
        let params: PortForwardParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.guest_port, 8080);
        assert_eq!(params.host_port, Some(9090));
    }

    #[test]
    fn test_port_forward_params_auto_assign() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_port": 3000
        });
        let params: PortForwardParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.guest_port, 3000);
        assert!(params.host_port.is_none());
    }

    #[test]
    fn test_network_policy_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "allow_internet": false,
            "allow_inter_vm": true,
            "allowed_ports": [80, 443, 8080]
        });
        let params: NetworkPolicyParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.allow_internet, Some(false));
        assert_eq!(params.allow_inter_vm, Some(true));
        assert_eq!(params.allowed_ports.as_ref().unwrap(), &[80, 443, 8080]);
    }

    #[test]
    fn test_parse_uuid_valid() {
        let result = parse_uuid("550e8400-e29b-41d4-a716-446655440000");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_uuid_invalid() {
        let result = parse_uuid("not-a-uuid");
        assert!(result.is_err());
    }

    // --- Host path validation tests ---

    #[test]
    fn test_validate_host_path_within_transfer_dir() {
        let dir = std::env::temp_dir().join(format!("agentiso-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, "hello").unwrap();

        let result = validate_host_path_in_dir(file.to_str().unwrap(), &dir, true);
        assert!(result.is_ok());
        assert!(result.unwrap().starts_with(dir.canonicalize().unwrap()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_validate_host_path_rejects_traversal() {
        let dir = std::env::temp_dir().join(format!("agentiso-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Try to escape via ..
        let bad_path = format!("{}/../etc/passwd", dir.display());
        let result = validate_host_path_in_dir(&bad_path, &dir, true);
        // Should fail (either file doesn't exist or path is outside transfer_dir)
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_validate_host_path_rejects_absolute_outside() {
        let dir = std::env::temp_dir().join(format!("agentiso-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let result = validate_host_path_in_dir("/etc/passwd", &dir, true);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_validate_host_path_download_new_file() {
        let dir = std::env::temp_dir().join(format!("agentiso-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Download target file doesn't need to exist, but parent must be in transfer_dir
        let new_file = dir.join("new-download.txt");
        let result = validate_host_path_in_dir(new_file.to_str().unwrap(), &dir, false);
        assert!(result.is_ok());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_validate_host_path_download_rejects_traversal() {
        let dir = std::env::temp_dir().join(format!("agentiso-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let bad_path = format!("{}/../evil.txt", dir.display());
        let result = validate_host_path_in_dir(&bad_path, &dir, false);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
