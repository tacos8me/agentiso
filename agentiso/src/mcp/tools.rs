use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{error, info};
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
    /// Maximum bytes of stdout/stderr to return. Defaults to 262144 (256 KiB).
    /// Output exceeding this limit is truncated with a '[TRUNCATED]' suffix.
    max_output_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileWriteParams {
    /// ID of the workspace
    workspace_id: String,
    /// Absolute path inside the VM where the file will be written
    path: String,
    /// File content (text)
    content: String,
    /// File permissions in octal notation (e.g. "0644" for rw-r--r--). Defaults to 0644.
    mode: Option<String>,
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
struct FileListParams {
    /// ID of the workspace
    workspace_id: String,
    /// Directory path inside the workspace (e.g. "/home/user" or "/app")
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileEditParams {
    /// ID of the workspace
    workspace_id: String,
    /// Absolute path to the file inside the workspace
    path: String,
    /// The exact string to find (must appear in the file; first occurrence is replaced)
    old_string: String,
    /// The replacement string
    new_string: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecBackgroundParams {
    /// ID of the workspace
    workspace_id: String,
    /// Shell command to run in the background
    command: String,
    /// Working directory (default: /root)
    workdir: Option<String>,
    /// Environment variables
    env: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecPollParams {
    /// ID of the workspace
    workspace_id: String,
    /// Job ID returned by exec_background
    job_id: u32,
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
        info!(
            tool = "workspace_create",
            name = ?params.name,
            base_image = ?params.base_image,
            vcpus = ?params.vcpus,
            memory_mb = ?params.memory_mb,
            disk_gb = ?params.disk_gb,
            "tool call"
        );

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
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "workspace_destroy", "tool call");
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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

        destroy_result.map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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
        info!(tool = "workspace_list", state_filter = ?params.state_filter, "tool call");

        let owned_ids = self
            .auth
            .list_workspaces(&self.session_id)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        let all = self
            .workspace_manager
            .list()
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "workspace_info", "tool call");
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "workspace_stop", "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .stop(ws_id)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "workspace_start", "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .start(ws_id)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "exec", command = %params.command, "tool call");
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
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

        let limit = params.max_output_bytes.unwrap_or(262144);
        let stdout = truncate_output(result.stdout, limit);
        let stderr = truncate_output(result.stderr, limit);

        let output = serde_json::json!({
            "exit_code": result.exit_code,
            "stdout": stdout,
            "stderr": stderr,
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
        info!(workspace_id = %ws_id, tool = "file_write", path = %params.path, "tool call");
        self.check_ownership(ws_id).await?;

        let mode = match &params.mode {
            Some(s) => Some(parse_octal_mode(s)?),
            None => None,
        };

        self.workspace_manager
            .file_write(ws_id, &params.path, params.content.as_bytes(), mode)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "file_read", path = %params.path, "tool call");
        self.check_ownership(ws_id).await?;

        let data = self
            .workspace_manager
            .file_read(ws_id, &params.path, params.offset, params.limit)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "file_upload", host_path = %params.host_path, guest_path = %params.guest_path, "tool call");
        self.check_ownership(ws_id).await?;

        // Validate host path is within allowed transfer directory.
        let safe_path = self.validate_host_path(&params.host_path, true)?;

        let host_data = tokio::fs::read(&safe_path).await.map_err(|e| {
            McpError::invalid_request(format!("failed to read host file: {}", e), None)
        })?;

        self.workspace_manager
            .file_write(ws_id, &params.guest_path, &host_data, None)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "file_download", host_path = %params.host_path, guest_path = %params.guest_path, "tool call");
        self.check_ownership(ws_id).await?;

        // Validate host path is within allowed transfer directory.
        let safe_path = self.validate_host_path(&params.host_path, false)?;

        let data = self
            .workspace_manager
            .file_read(ws_id, &params.guest_path, None, None)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "snapshot_create", name = %params.name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.name)?;

        let snapshot = self
            .workspace_manager
            .snapshot_create(ws_id, &params.name, params.include_memory.unwrap_or(false))
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "snapshot_restore", snapshot_name = %params.snapshot_name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.snapshot_name)?;

        self.workspace_manager
            .snapshot_restore(ws_id, &params.snapshot_name)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "snapshot_list", "tool call");
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "snapshot_delete", snapshot_name = %params.snapshot_name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.snapshot_name)?;

        self.workspace_manager
            .snapshot_delete(ws_id, &params.snapshot_name)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "workspace_fork", snapshot_name = %params.snapshot_name, new_name = ?params.new_name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.snapshot_name)?;

        // Check quota for the new workspace (use same resources as source).
        let source_ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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
            return Err(McpError::internal_error(format!("{:#}", e), None));
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
        info!(workspace_id = %ws_id, tool = "port_forward", guest_port = params.guest_port, host_port = ?params.host_port, "tool call");
        self.check_ownership(ws_id).await?;

        // Reject privileged ports to prevent binding to host services like SSH, HTTP, HTTPS.
        if let Some(hp) = params.host_port {
            if hp < 1024 {
                return Err(McpError::invalid_params(
                    format!(
                        "host_port {} is a privileged port (< 1024). Privileged ports are reserved for host services like SSH (22), HTTP (80), and HTTPS (443). Choose a host_port >= 1024.",
                        hp
                    ),
                    None,
                ));
            }
        }

        let assigned_host_port = self
            .workspace_manager
            .port_forward_add(ws_id, params.guest_port, params.host_port)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "port_forward_remove", guest_port = params.guest_port, "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .port_forward_remove(ws_id, params.guest_port)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "workspace_ip", "tool call");
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

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
        info!(workspace_id = %ws_id, tool = "network_policy", allow_internet = ?params.allow_internet, allow_inter_vm = ?params.allow_inter_vm, "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .update_network_policy(
                ws_id,
                params.allow_internet,
                params.allow_inter_vm,
                params.allowed_ports,
            )
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

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

    // -----------------------------------------------------------------------
    // New Tools: file_list, file_edit, exec_background, exec_poll
    // -----------------------------------------------------------------------

    /// List files and directories at a given path inside a running workspace VM.
    #[tool]
    async fn file_list(
        &self,
        Parameters(params): Parameters<FileListParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        info!(workspace_id = %ws_id, tool = "file_list", path = %params.path, "tool call");
        self.check_ownership(ws_id).await?;

        let entries = self
            .workspace_manager
            .list_dir(ws_id, &params.path)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

        let output: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "kind": e.kind,
                    "size": e.size,
                    "permissions": e.permissions,
                    "modified": e.modified,
                })
            })
            .collect();

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap(),
        )]))
    }

    /// Edit a file inside a running workspace VM by replacing an exact string match.
    /// The old_string must appear in the file; the first occurrence is replaced with new_string.
    #[tool]
    async fn file_edit(
        &self,
        Parameters(params): Parameters<FileEditParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        info!(workspace_id = %ws_id, tool = "file_edit", path = %params.path, "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .edit_file(ws_id, &params.path, &params.old_string, &params.new_string)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "File edited: {}",
            params.path
        ))]))
    }

    /// Start a shell command in the background inside a running workspace VM.
    /// Returns a job_id that can be polled with exec_poll to check status and retrieve output.
    #[tool]
    async fn exec_background(
        &self,
        Parameters(params): Parameters<ExecBackgroundParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        info!(workspace_id = %ws_id, tool = "exec_background", command = %params.command, "tool call");
        self.check_ownership(ws_id).await?;

        let job_id = self
            .workspace_manager
            .exec_background(
                ws_id,
                &params.command,
                params.workdir.as_deref(),
                params.env.as_ref(),
            )
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

        let output = serde_json::json!({
            "job_id": job_id,
            "status": "started",
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap(),
        )]))
    }

    /// Poll a background job started with exec_background. Returns whether the job is still running,
    /// its exit code (if finished), and any stdout/stderr output.
    #[tool]
    async fn exec_poll(
        &self,
        Parameters(params): Parameters<ExecPollParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = parse_uuid(&params.workspace_id)?;
        info!(workspace_id = %ws_id, tool = "exec_poll", job_id = params.job_id, "tool call");
        self.check_ownership(ws_id).await?;

        let status = self
            .workspace_manager
            .exec_poll(ws_id, params.job_id)
            .await
            .map_err(|e| McpError::invalid_params(format!("{:#}", e), None))?;

        let output = serde_json::json!({
            "job_id": params.job_id,
            "running": status.running,
            "exit_code": status.exit_code,
            "stdout": status.stdout,
            "stderr": status.stderr,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap(),
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

/// Validate that a snapshot name contains only safe characters.
fn validate_snapshot_name(name: &str) -> Result<(), McpError> {
    if name.is_empty() {
        return Err(McpError::invalid_params(
            "snapshot name must not be empty".to_string(),
            None,
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(McpError::invalid_params(
            format!(
                "snapshot name '{}' contains invalid characters. Use only letters, digits, underscores, hyphens, and dots.",
                name
            ),
            None,
        ));
    }
    Ok(())
}

/// Parse an octal mode string (e.g. "0644", "755") into a u32.
fn parse_octal_mode(s: &str) -> Result<u32, McpError> {
    let stripped = s
        .strip_prefix("0o")
        .or_else(|| s.strip_prefix('0'))
        .unwrap_or(s);
    if stripped.is_empty() {
        return Err(McpError::invalid_params(
            format!(
                "invalid octal mode '{}'. Use octal notation like \"0644\".",
                s
            ),
            None,
        ));
    }
    u32::from_str_radix(stripped, 8).map_err(|_| {
        McpError::invalid_params(
            format!(
                "invalid octal mode '{}'. Use octal notation like \"0644\".",
                s
            ),
            None,
        )
    })
}

/// Truncate a string to a byte limit, appending a notice if truncated.
fn truncate_output(s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let total = s.len();
    format!(
        "{}\n[TRUNCATED: {} bytes total, showing first {} bytes]",
        &s[..limit],
        total,
        limit
    )
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
        assert!(params.max_output_bytes.is_none());
    }

    #[test]
    fn test_file_write_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/tmp/test.txt",
            "content": "hello world",
            "mode": "0644"
        });
        let params: FileWriteParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "/tmp/test.txt");
        assert_eq!(params.content, "hello world");
        assert_eq!(params.mode, Some("0644".to_string()));
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

    // --- Snapshot name validation tests ---

    #[test]
    fn test_validate_snapshot_name_valid() {
        assert!(validate_snapshot_name("before-experiment").is_ok());
        assert!(validate_snapshot_name("snap_v1.0").is_ok());
        assert!(validate_snapshot_name("ABC123").is_ok());
        assert!(validate_snapshot_name("a").is_ok());
    }

    #[test]
    fn test_validate_snapshot_name_empty() {
        assert!(validate_snapshot_name("").is_err());
    }

    #[test]
    fn test_validate_snapshot_name_invalid_chars() {
        assert!(validate_snapshot_name("snap shot").is_err());
        assert!(validate_snapshot_name("snap/shot").is_err());
        assert!(validate_snapshot_name("snap@shot").is_err());
        assert!(validate_snapshot_name("snap;shot").is_err());
    }

    // --- Octal mode parsing tests ---

    #[test]
    fn test_parse_octal_mode_standard() {
        assert_eq!(parse_octal_mode("0644").unwrap(), 0o644);
        assert_eq!(parse_octal_mode("0755").unwrap(), 0o755);
        assert_eq!(parse_octal_mode("644").unwrap(), 0o644);
        assert_eq!(parse_octal_mode("755").unwrap(), 0o755);
    }

    #[test]
    fn test_parse_octal_mode_with_0o_prefix() {
        assert_eq!(parse_octal_mode("0o644").unwrap(), 0o644);
        assert_eq!(parse_octal_mode("0o755").unwrap(), 0o755);
    }

    #[test]
    fn test_parse_octal_mode_invalid() {
        assert!(parse_octal_mode("999").is_err());
        assert!(parse_octal_mode("abc").is_err());
    }

    // --- Output truncation tests ---

    #[test]
    fn test_truncate_output_no_truncation() {
        let s = "hello world".to_string();
        assert_eq!(truncate_output(s.clone(), 100), s);
    }

    #[test]
    fn test_truncate_output_truncates() {
        let s = "hello world".to_string();
        let result = truncate_output(s, 5);
        assert!(result.starts_with("hello"));
        assert!(result.contains("[TRUNCATED: 11 bytes total, showing first 5 bytes]"));
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

    // --- Port forward privileged port tests ---

    #[test]
    fn test_port_forward_params_privileged_port() {
        // Privileged ports (< 1024) can be deserialized but should be rejected at handler level.
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_port": 8080,
            "host_port": 80
        });
        let params: PortForwardParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.host_port, Some(80));
        // The handler rejects host_port < 1024 at runtime.
    }

    #[test]
    fn test_port_forward_params_boundary_port() {
        // Port 1024 is the first non-privileged port and should be accepted.
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_port": 8080,
            "host_port": 1024
        });
        let params: PortForwardParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.host_port, Some(1024));
    }

    // --- New tool param tests ---

    #[test]
    fn test_file_list_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/home/user"
        });
        let params: FileListParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.path, "/home/user");
    }

    #[test]
    fn test_file_edit_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/app/main.py",
            "old_string": "print('hello')",
            "new_string": "print('world')"
        });
        let params: FileEditParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "/app/main.py");
        assert_eq!(params.old_string, "print('hello')");
        assert_eq!(params.new_string, "print('world')");
    }

    #[test]
    fn test_exec_background_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "command": "sleep 60 && echo done",
            "workdir": "/app",
            "env": {"NODE_ENV": "production"}
        });
        let params: ExecBackgroundParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.command, "sleep 60 && echo done");
        assert_eq!(params.workdir.as_deref(), Some("/app"));
        assert_eq!(
            params.env.as_ref().unwrap().get("NODE_ENV").unwrap(),
            "production"
        );
    }

    #[test]
    fn test_exec_background_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "command": "make build"
        });
        let params: ExecBackgroundParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.command, "make build");
        assert!(params.workdir.is_none());
        assert!(params.env.is_none());
    }

    #[test]
    fn test_exec_poll_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "job_id": 42
        });
        let params: ExecPollParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.job_id, 42);
    }
}
