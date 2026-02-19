use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::workspace::WorkspaceManager;

use super::auth::AuthManager;
use super::metrics::MetricsRegistry;

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
    /// Allow outbound internet access (default: from server config)
    allow_internet: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceIdParams {
    /// UUID or name of the workspace
    workspace_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceListParams {
    /// Filter by state: running, stopped, or suspended
    state_filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Shell command to execute (passed to /bin/sh -c)
    command: String,
    /// Timeout in seconds (default: 120). Most commands complete well within this limit.
    /// For commands expected to run longer, use exec_background + exec_poll instead.
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
    /// UUID or name of the workspace
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
    /// UUID or name of the workspace
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
    /// UUID or name of the workspace
    workspace_id: String,
    /// Path on the host filesystem
    host_path: String,
    /// Path inside the guest VM
    guest_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotCreateParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Name for the snapshot
    name: String,
    /// Include VM memory state for live snapshot (default: false)
    include_memory: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotNameParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Name of the snapshot
    snapshot_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceForkParams {
    /// UUID or name of the source workspace
    workspace_id: String,
    /// Name of the snapshot to fork from
    snapshot_name: String,
    /// Name for the new forked workspace
    new_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PortForwardParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Port inside the guest VM to forward to
    guest_port: u16,
    /// Host port to listen on (auto-assigned if omitted)
    host_port: Option<u16>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PortForwardRemoveParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Guest port whose forwarding rule should be removed
    guest_port: u16,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileListParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Directory path inside the workspace (e.g. "/home/user" or "/app")
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileEditParams {
    /// UUID or name of the workspace
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
    /// UUID or name of the workspace
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
    /// UUID or name of the workspace
    workspace_id: String,
    /// Job ID returned by exec_background
    job_id: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NetworkPolicyParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Allow outbound internet access
    allow_internet: Option<bool>,
    /// Allow communication with other workspace VMs
    allow_inter_vm: Option<bool>,
    /// List of TCP ports allowed for inbound connections
    allowed_ports: Option<Vec<u16>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecKillParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Job ID returned by exec_background
    job_id: u32,
    /// Signal to send (default: 9 = SIGKILL). Common values: 2=SIGINT, 9=SIGKILL, 15=SIGTERM.
    signal: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetEnvParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Environment variables to set as key-value pairs. These persist across
    /// subsequent exec and exec_background calls until the VM is destroyed.
    vars: HashMap<String, String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceLogsParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Which log to retrieve: "console", "stderr", or "all" (default: "all")
    log_type: Option<String>,
    /// Maximum number of lines to return (default: 100)
    max_lines: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceAdoptParams {
    /// UUID or name of the workspace to adopt
    workspace_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceAdoptAllParams {}

#[derive(Debug, Deserialize, JsonSchema)]
struct GitCloneParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Git repository URL (https:// or git://)
    url: String,
    /// Destination path inside the VM (default: /workspace)
    path: Option<String>,
    /// Branch to checkout after cloning
    branch: Option<String>,
    /// Shallow clone depth (e.g. 1 for latest commit only)
    depth: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspacePrepareParams {
    /// Human-readable name for the golden workspace
    name: String,
    /// Base image to use (default: alpine-opencode)
    base_image: Option<String>,
    /// Git repository URL to clone into /workspace
    git_url: Option<String>,
    /// Shell commands to run in sequence after optional git clone (e.g. install deps)
    setup_commands: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceBatchForkParams {
    /// UUID or name of the source workspace to fork from
    workspace_id: String,
    /// Name of the snapshot to fork from (default: "golden")
    snapshot_name: Option<String>,
    /// Number of worker VMs to fork (1-20)
    count: u32,
    /// Prefix for worker names (default: "worker"). Workers are named "{prefix}-1", "{prefix}-2", etc.
    name_prefix: Option<String>,
}

// ---------------------------------------------------------------------------
// Vault parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultReadParams {
    /// Path to the note, relative to vault root (e.g. "projects/design.md")
    path: String,
    /// Output format: "markdown" (default) returns raw content, "json" returns structured data
    format: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultSearchParams {
    /// Search query string or regex pattern
    query: String,
    /// Treat query as a regular expression (default: false)
    regex: Option<bool>,
    /// Only search within this subdirectory (e.g. "projects/")
    path_prefix: Option<String>,
    /// Only return results from notes tagged with this tag
    tag: Option<String>,
    /// Maximum number of results to return (default: 20)
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultListParams {
    /// Directory path relative to vault root (default: vault root)
    path: Option<String>,
    /// List recursively into subdirectories (default: false)
    recursive: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultWriteParams {
    /// Path to the note, relative to vault root
    path: String,
    /// Content to write
    content: String,
    /// Write mode: "overwrite" (default), "append", or "prepend"
    mode: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultFrontmatterParams {
    /// Path to the note, relative to vault root
    path: String,
    /// Action to perform: "get", "set", or "delete"
    action: String,
    /// Frontmatter key (required for "set" and "delete" actions)
    key: Option<String>,
    /// Value to set (required for "set" action, as JSON)
    value: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultTagsParams {
    /// Path to the note, relative to vault root
    path: String,
    /// Action to perform: "list", "add", or "remove"
    action: String,
    /// Tag name (required for "add" and "remove" actions)
    tag: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultReplaceParams {
    /// Path to the note, relative to vault root
    path: String,
    /// String or regex pattern to search for
    search: String,
    /// Replacement string (supports $1, $2 backreferences when regex is true)
    replace: String,
    /// Treat search as a regular expression (default: false)
    regex: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultDeleteParams {
    /// Path to the note, relative to vault root
    path: String,
    /// Must be true to confirm deletion
    confirm: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultMoveParams {
    /// Current path to the note, relative to vault root
    path: String,
    /// New path for the note, relative to vault root
    new_path: String,
    /// Overwrite destination if it already exists (default: false)
    overwrite: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultBatchReadParams {
    /// Paths to read, relative to vault root (max 10)
    paths: Vec<String>,
    /// Include file content in results (default: true)
    include_content: Option<bool>,
    /// Include parsed frontmatter in results (default: true)
    include_frontmatter: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultStatsParams {
    /// Number of recently modified files to include (default: 10)
    recent_count: Option<usize>,
}

// ---------------------------------------------------------------------------
// New tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceGitStatusParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Path to the git repository inside the VM (default: /workspace)
    path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotDiffParams {
    /// UUID or name of the workspace
    workspace_id: String,
    /// Name of the snapshot to compare against current state
    snapshot_name: String,
}

/// Structured git status output parsed from `git status --porcelain=v2 --branch`.
#[derive(Debug, Serialize)]
struct GitStatusInfo {
    branch: String,
    ahead: i64,
    behind: i64,
    staged: Vec<String>,
    modified: Vec<String>,
    untracked: Vec<String>,
    conflicted: Vec<String>,
    dirty: bool,
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
    metrics: Option<MetricsRegistry>,
    /// Vault manager for Obsidian-style markdown knowledge base tools.
    /// None when vault is disabled in config.
    vault_manager: Option<Arc<super::vault::VaultManager>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AgentisoServer {
    pub fn with_metrics(
        workspace_manager: Arc<WorkspaceManager>,
        auth: AuthManager,
        session_id: String,
        transfer_dir: PathBuf,
        metrics: Option<MetricsRegistry>,
        vault_manager: Option<Arc<super::vault::VaultManager>>,
    ) -> Self {
        Self {
            workspace_manager,
            auth,
            session_id,
            transfer_dir,
            metrics,
            vault_manager,
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

        if let Some(ref base_image) = params.base_image {
            validate_base_image(base_image)?;
        }

        let create_params = crate::workspace::CreateParams {
            name: params.name,
            base_image: params.base_image,
            vcpus: params.vcpus,
            memory_mb: Some(mem),
            disk_gb: Some(disk),
            allow_internet: params.allow_internet,
        };

        let create_start = std::time::Instant::now();

        let workspace = self
            .workspace_manager
            .create(create_params)
            .await
            .map_err(|e| {
                if let Some(ref m) = self.metrics {
                    m.record_error("workspace_create");
                }
                McpError::internal_error(
                    format!(
                        "Failed to create workspace: {:#}. Check that the agentiso service has \
                         sufficient resources (ZFS pool space, available IPs). Use workspace_list \
                         to see existing workspaces.",
                        e
                    ),
                    None,
                )
            })?;

        let boot_time_ms = create_start.elapsed().as_millis() as u64;

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

        // TODO: Detect whether the VM came from the warm pool (from_pool).
        // This requires workspace_manager.create() to return a CreateResult
        // with a `from_pool` field, being added by another agent.
        let from_pool = false;

        let info = serde_json::json!({
            "workspace_id": workspace.id.to_string(),
            "name": workspace.name,
            "state": workspace.state,
            "ip": workspace.network.ip.to_string(),
            "boot_time_ms": boot_time_ms,
            "from_pool": from_pool,
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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "workspace_destroy", "tool call");
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Workspace '{}' not found: {:#}. Use workspace_list to see available workspaces.",
                        params.workspace_id, e
                    ),
                    None,
                )
            })?;

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

        destroy_result.map_err(|e| {
            if let Some(ref m) = self.metrics {
                m.record_error("workspace_destroy");
            }
            McpError::internal_error(
                format!(
                    "Failed to destroy workspace '{}': {:#}. The workspace may be in an \
                     inconsistent state. Try workspace_stop first, then workspace_destroy again.",
                    params.workspace_id, e
                ),
                None,
            )
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Workspace {} destroyed.",
            params.workspace_id
        ))]))
    }

    /// List all workspaces visible to this session, with optional state filter.
    /// Workspaces owned by this session have `"owned": true`. Workspaces that exist
    /// but are not owned by this session have `"owned": false` — use `workspace_adopt`
    /// or `workspace_adopt_all` to claim them.
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
            .filter(|ws| {
                state_filter
                    .map(|f| ws.state.to_string() == f)
                    .unwrap_or(true)
            })
            .map(|ws| {
                let owned = owned_ids.contains(&ws.id);
                serde_json::json!({
                    "workspace_id": ws.id.to_string(),
                    "name": ws.name,
                    "state": ws.state,
                    "ip": ws.network.ip.to_string(),
                    "owned": owned,
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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "workspace_info", "tool call");
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Workspace '{}' not found: {:#}. Use workspace_list to see available workspaces, \
                         or workspace_create to make a new one.",
                        params.workspace_id, e
                    ),
                    None,
                )
            })?;

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

        let mut info = serde_json::json!({
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

        // Attempt to fetch live disk usage from ZFS. If it fails (e.g. VM is
        // stopped and the dataset is not mounted), log a warning and omit the
        // fields rather than failing the whole request.
        match self.workspace_manager.workspace_zvol_info(ws_id).await {
            Ok(zvol_info) => {
                info["disk_used_bytes"] = serde_json::json!(zvol_info.used);
                info["disk_volsize_bytes"] = serde_json::json!(zvol_info.volsize);
            }
            Err(e) => {
                warn!(
                    workspace_id = %ws_id,
                    error = %e,
                    "failed to fetch zvol info for workspace_info, omitting disk fields"
                );
            }
        }

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "workspace_stop", "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .stop(ws_id)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to stop workspace '{}': {:#}. The workspace may already be stopped \
                         (use workspace_info to check state) or may not respond to graceful shutdown.",
                        params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "workspace_start", "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .start(ws_id)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to start workspace '{}': {:#}. The workspace may already be running \
                         (use workspace_info to check state). If the VM failed to boot, use \
                         workspace_logs to see console output.",
                        params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "exec", command = %params.command, "tool call");
        self.check_ownership(ws_id).await?;

        let exec_start = std::time::Instant::now();
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
            .map_err(|e| {
                if let Some(ref m) = self.metrics {
                    m.record_error("exec");
                }
                let msg = format!("{:#}", e);
                if msg.contains("timed out") || msg.contains("Timeout") {
                    let timeout = params.timeout_secs.unwrap_or(120);
                    McpError::invalid_request(
                        format!(
                            "exec timed out after {}s. For long-running commands, use exec_background \
                             to start the command, then exec_poll to check progress. \
                             You can also increase timeout_secs (current: {}s).",
                            timeout, timeout
                        ),
                        None,
                    )
                } else {
                    McpError::invalid_request(
                        format!(
                            "Command failed in workspace '{}': {}. Ensure the workspace is running \
                             (use workspace_start if stopped). The guest OS is Alpine Linux; install \
                             missing packages with: exec(command='apk add <package>').",
                            params.workspace_id, msg
                        ),
                        None,
                    )
                }
            })?;

        if let Some(ref m) = self.metrics {
            m.record_exec(exec_start.elapsed(), result.exit_code);
        }

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "file_write", path = %params.path, "tool call");
        self.check_ownership(ws_id).await?;

        let mode = match &params.mode {
            Some(s) => Some(parse_octal_mode(s)?),
            None => None,
        };

        self.workspace_manager
            .file_write(ws_id, &params.path, params.content.as_bytes(), mode)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to write file '{}' in workspace '{}': {:#}. Ensure the path is \
                         absolute (e.g. /workspace/file.txt), the workspace is running, and the \
                         parent directory exists.",
                        params.path, params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "file_read", path = %params.path, "tool call");
        self.check_ownership(ws_id).await?;

        let data = self
            .workspace_manager
            .file_read(ws_id, &params.path, params.offset, params.limit)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to read file '{}' in workspace '{}': {:#}. Ensure the path is \
                         absolute (e.g. /workspace/file.txt) and the file exists. Use file_list \
                         to browse directory contents.",
                        params.path, params.workspace_id, e
                    ),
                    None,
                )
            })?;

        let text = match String::from_utf8(data) {
            Ok(s) => s,
            Err(_) => {
                return Err(McpError::invalid_request(
                    "File contains binary data. Use file_download for binary files, or use offset/limit to read a text portion.".to_string(),
                    None,
                ));
            }
        };

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Upload a file from the host filesystem into a running workspace VM.
    /// The host_path must be within the configured transfer directory.
    #[tool]
    async fn file_upload(
        &self,
        Parameters(params): Parameters<FileTransferParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "file_upload", host_path = %params.host_path, guest_path = %params.guest_path, "tool call");
        self.check_ownership(ws_id).await?;

        // Validate host path is within allowed transfer directory.
        let safe_path = self.validate_host_path(&params.host_path, true)?;

        let host_data = tokio::fs::read(&safe_path).await.map_err(|e| {
            McpError::invalid_request(
                format!(
                    "Failed to read host file '{}': {}. Ensure the file exists and is \
                     readable within the configured transfer directory.",
                    params.host_path, e
                ),
                None,
            )
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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
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
            McpError::internal_error(
                format!(
                    "Failed to write host file '{}': {}. Ensure the transfer directory is \
                     writable and has sufficient disk space.",
                    params.host_path, e
                ),
                None,
            )
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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "snapshot_create", name = %params.name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.name)?;

        let snapshot = self
            .workspace_manager
            .snapshot_create(ws_id, &params.name, params.include_memory.unwrap_or(false))
            .await
            .map_err(|e| {
                McpError::internal_error(
                    format!(
                        "Failed to create snapshot '{}' on workspace '{}': {:#}. This may indicate \
                         the storage pool is full or the workspace is in a bad state. Use \
                         workspace_info to check workspace status and disk usage.",
                        params.name, params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "snapshot_restore", snapshot_name = %params.snapshot_name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.snapshot_name)?;

        // Count snapshots before restore so we can report how many were removed.
        let snap_count_before = self
            .workspace_manager
            .get(ws_id)
            .await
            .map(|ws| ws.snapshots.list().len())
            .unwrap_or(0);

        self.workspace_manager
            .snapshot_restore(ws_id, &params.snapshot_name)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to restore snapshot '{}' on workspace '{}': {:#}. Use \
                         snapshot_list to verify the snapshot exists. Note: snapshot_restore \
                         is destructive and removes all snapshots newer than the target.",
                        params.snapshot_name, params.workspace_id, e
                    ),
                    None,
                )
            })?;

        // Count snapshots after restore. The difference (minus 1 for the
        // restored snapshot itself remaining) is the number removed.
        let snap_count_after = self
            .workspace_manager
            .get(ws_id)
            .await
            .map(|ws| ws.snapshots.list().len())
            .unwrap_or(0);

        let removed = snap_count_before.saturating_sub(snap_count_after);

        let message = if removed > 0 {
            format!(
                "Workspace {} restored to snapshot '{}'. {} newer snapshot(s) were removed.",
                params.workspace_id, params.snapshot_name, removed
            )
        } else {
            format!(
                "Workspace {} restored to snapshot '{}'.",
                params.workspace_id, params.snapshot_name
            )
        };

        Ok(CallToolResult::success(vec![Content::text(message)]))
    }

    /// List all snapshots for a workspace, showing the snapshot tree with size information.
    #[tool]
    async fn snapshot_list(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "snapshot_list", "tool call");
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

        let mut snapshots: Vec<serde_json::Value> = Vec::new();
        for s in ws.snapshots.list().iter() {
            let snap_json = serde_json::json!({
                "id": s.id.to_string(),
                "name": s.name,
                "has_memory": s.qemu_state.is_some(),
                "parent": s.parent.map(|p| p.to_string()),
                "created_at": s.created_at.to_rfc3339(),
                // TODO: Add used_bytes and referenced_bytes fields here.
                // Call workspace_manager.snapshot_size(ws_id, &s.name) to get
                // (used_bytes, referenced_bytes) for each snapshot. The method is being
                // added by another agent to expose storage::Zfs::snapshot_size() through
                // WorkspaceManager. Once available, populate these fields.
            });

            snapshots.push(snap_json);
        }

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "snapshot_delete", snapshot_name = %params.snapshot_name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.snapshot_name)?;

        self.workspace_manager
            .snapshot_delete(ws_id, &params.snapshot_name)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to delete snapshot '{}' from workspace '{}': {:#}. Use \
                         snapshot_list to verify the snapshot exists.",
                        params.snapshot_name, params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "workspace_fork", snapshot_name = %params.snapshot_name, new_name = ?params.new_name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.snapshot_name)?;

        // Check quota for the new workspace (use same resources as source).
        let source_ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Source workspace '{}' not found: {:#}. Use workspace_list to see \
                         available workspaces.",
                        params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
            .map_err(|e| {
                McpError::internal_error(
                    format!(
                        "Failed to fork workspace '{}' from snapshot '{}': {:#}. Use \
                         snapshot_list to verify the snapshot exists and check that the \
                         storage pool has sufficient space.",
                        params.workspace_id, params.snapshot_name, e
                    ),
                    None,
                )
            })?;

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
                "source_workspace_id": source_ws.id.to_string(),
                "source_workspace_name": source_ws.name,
                "snapshot_name": params.snapshot_name,
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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
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
            .map_err(|e| {
                McpError::internal_error(
                    format!(
                        "Failed to forward port {} in workspace '{}': {:#}. The host port \
                         may already be in use. Omit host_port to auto-assign, or choose a \
                         different port >= 1024.",
                        params.guest_port, params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "port_forward_remove", guest_port = params.guest_port, "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .port_forward_remove(ws_id, params.guest_port)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to remove port forward for guest port {} in workspace '{}': {:#}. \
                         Use workspace_info to see active port forwarding rules.",
                        params.guest_port, params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "workspace_ip", "tool call");
        self.check_ownership(ws_id).await?;

        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "network_policy", allow_internet = ?params.allow_internet, allow_inter_vm = ?params.allow_inter_vm, "tool call");
        self.check_ownership(ws_id).await?;

        let requested_desc = format!(
            "Requested changes: allow_internet={:?}, allow_inter_vm={:?}, allowed_ports={:?}",
            params.allow_internet, params.allow_inter_vm, params.allowed_ports
        );

        self.workspace_manager
            .update_network_policy(
                ws_id,
                params.allow_internet,
                params.allow_inter_vm,
                params.allowed_ports,
            )
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!("{:#}. {}", e, requested_desc),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "file_list", path = %params.path, "tool call");
        self.check_ownership(ws_id).await?;

        let entries = self
            .workspace_manager
            .list_dir(ws_id, &params.path)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to list directory '{}' in workspace '{}': {:#}. Ensure the path \
                         is absolute (e.g. /workspace) and the directory exists.",
                        params.path, params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "file_edit", path = %params.path, "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .edit_file(ws_id, &params.path, &params.old_string, &params.new_string)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to edit file '{}' in workspace '{}': {:#}. Ensure the file \
                         exists and that old_string appears in it exactly. Use file_read to \
                         verify file contents first.",
                        params.path, params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
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
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to start background command in workspace '{}': {:#}. Ensure the \
                         workspace is running (use workspace_start if stopped).",
                        params.workspace_id, e
                    ),
                    None,
                )
            })?;

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
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "exec_poll", job_id = params.job_id, "tool call");
        self.check_ownership(ws_id).await?;

        let status = self
            .workspace_manager
            .exec_poll(ws_id, params.job_id)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to poll job {} in workspace '{}': {:#}. The job_id may be \
                         invalid or the workspace may have been restarted since the job was started.",
                        params.job_id, params.workspace_id, e
                    ),
                    None,
                )
            })?;

        let max_output_bytes = 262144; // 256 KiB, same as exec
        let stdout = truncate_output(status.stdout, max_output_bytes);
        let stderr = truncate_output(status.stderr, max_output_bytes);

        let output = serde_json::json!({
            "job_id": params.job_id,
            "running": status.running,
            "exit_code": status.exit_code,
            "stdout": stdout,
            "stderr": stderr,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap(),
        )]))
    }

    /// Kill a background job running in a workspace VM by sending it a signal.
    /// Use this to terminate jobs started with exec_background.
    #[tool]
    async fn exec_kill(
        &self,
        Parameters(params): Parameters<ExecKillParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "exec_kill", job_id = params.job_id, signal = ?params.signal, "tool call");
        self.check_ownership(ws_id).await?;

        self.workspace_manager
            .exec_kill(ws_id, params.job_id, params.signal)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to kill job {} in workspace '{}': {:#}. The job may have \
                         already exited. Use exec_poll to check job status.",
                        params.job_id, params.workspace_id, e
                    ),
                    None,
                )
            })?;

        let sig = params.signal.unwrap_or(9);
        let output = serde_json::json!({
            "job_id": params.job_id,
            "signal": sig,
            "status": "killed",
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap(),
        )]))
    }

    /// Set persistent environment variables inside a running workspace VM.
    /// These variables are automatically applied to all subsequent exec and exec_background calls.
    /// Use this to inject API keys, configuration, or credentials into the VM without writing files.
    /// Per-command env vars (in exec/exec_background) override these stored values.
    #[tool]
    async fn set_env(
        &self,
        Parameters(params): Parameters<SetEnvParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "set_env", var_count = params.vars.len(), "tool call");
        self.check_ownership(ws_id).await?;

        let count = self
            .workspace_manager
            .set_env(ws_id, params.vars)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        let output = serde_json::json!({
            "count": count,
            "status": "set",
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Debugging / Diagnostics Tools
    // -----------------------------------------------------------------------

    /// Retrieve QEMU console output and/or stderr logs for debugging workspace boot or runtime issues.
    /// Returns the last N lines (default: 100) of the requested log(s).
    #[tool]
    async fn workspace_logs(
        &self,
        Parameters(params): Parameters<WorkspaceLogsParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "workspace_logs", log_type = ?params.log_type, max_lines = ?params.max_lines, "tool call");
        self.check_ownership(ws_id).await?;

        let max_lines = params.max_lines.unwrap_or(100);
        let log_type = params.log_type.as_deref().unwrap_or("all");

        let (console, stderr) = self
            .workspace_manager
            .workspace_logs(ws_id, max_lines)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Failed to retrieve logs for workspace '{}': {:#}. The workspace may \
                         not have been started yet, or log files may not be available.",
                        params.workspace_id, e
                    ),
                    None,
                )
            })?;

        let output = match log_type {
            "console" => serde_json::json!({
                "console": console,
            }),
            "stderr" => serde_json::json!({
                "stderr": stderr,
            }),
            "all" | _ => serde_json::json!({
                "console": console,
                "stderr": stderr,
            }),
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Adoption Tools
    // -----------------------------------------------------------------------

    /// Adopt an orphaned workspace into the current session. Use this after a server restart
    /// to reclaim ownership of workspaces that exist in state but are not owned by any session.
    #[tool]
    async fn workspace_adopt(
        &self,
        Parameters(params): Parameters<WorkspaceAdoptParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "workspace_adopt", "tool call");

        // Verify workspace exists.
        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Workspace '{}' not found: {:#}. Use workspace_list to see all \
                         workspaces, including orphaned ones.",
                        params.workspace_id, e
                    ),
                    None,
                )
            })?;

        // Adopt into current session (checks orphan status and quota internally).
        self.auth
            .adopt_workspace(
                &self.session_id,
                &ws_id,
                ws.resources.memory_mb as u64,
                ws.resources.disk_gb as u64,
            )
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let info = serde_json::json!({
            "workspace_id": ws.id.to_string(),
            "name": ws.name,
            "state": ws.state,
            "ip": ws.network.ip.to_string(),
            "adopted": true,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Adopt all orphaned workspaces into the current session. Orphaned workspaces are those
    /// that exist in state but are not owned by any active session (common after a server restart).
    #[tool]
    async fn workspace_adopt_all(
        &self,
        #[allow(unused_variables)]
        Parameters(params): Parameters<WorkspaceAdoptAllParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "workspace_adopt_all", "tool call");

        // Purge ghost sessions from before the restart so their workspaces
        // become orphaned and adoptable by the current session.
        let purged = self.auth.purge_stale_sessions(&self.session_id).await;
        if purged > 0 {
            info!(purged, "purged stale sessions before adopt_all");
        }

        let all = self
            .workspace_manager
            .list()
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        let all_ids: Vec<Uuid> = all.iter().map(|ws| ws.id).collect();
        let orphaned_ids = self.auth.list_orphaned_workspaces(&all_ids).await;

        let mut adopted = Vec::new();
        let mut errors = Vec::new();

        for ws in &all {
            if !orphaned_ids.contains(&ws.id) {
                continue;
            }

            match self
                .auth
                .adopt_workspace(
                    &self.session_id,
                    &ws.id,
                    ws.resources.memory_mb as u64,
                    ws.resources.disk_gb as u64,
                )
                .await
            {
                Ok(()) => {
                    adopted.push(serde_json::json!({
                        "workspace_id": ws.id.to_string(),
                        "name": ws.name,
                        "state": ws.state,
                    }));
                }
                Err(e) => {
                    errors.push(serde_json::json!({
                        "workspace_id": ws.id.to_string(),
                        "error": e.to_string(),
                    }));
                }
            }
        }

        let info = serde_json::json!({
            "adopted_count": adopted.len(),
            "adopted": adopted,
            "error_count": errors.len(),
            "errors": errors,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Helper Tools
    // -----------------------------------------------------------------------

    /// Clone a git repository into a running workspace VM. Returns the clone path and HEAD commit SHA.
    /// This is a convenience wrapper around exec that handles git clone with proper error reporting.
    #[tool]
    async fn git_clone(
        &self,
        Parameters(params): Parameters<GitCloneParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "git_clone", url = %params.url, "tool call");
        self.check_ownership(ws_id).await?;

        validate_git_url(&params.url)?;

        let dest = params.path.as_deref().unwrap_or("/workspace");

        // Build the git clone command
        let mut cmd = format!("git clone");
        if let Some(depth) = params.depth {
            cmd.push_str(&format!(" --depth {}", depth));
        }
        if let Some(ref branch) = params.branch {
            cmd.push_str(&format!(" --branch {}", shell_escape(branch)));
        }
        cmd.push_str(&format!(" {} {}", shell_escape(&params.url), shell_escape(dest)));

        // Execute git clone with a generous timeout (clones can be slow).
        // Note: env vars set via set_env are automatically applied by the guest
        // agent for all exec calls (including this one), so we don't need to
        // pass them explicitly here. The None for env means no per-call overrides.
        let result = self
            .workspace_manager
            .exec(ws_id, &cmd, None, None, Some(300))
            .await
            .map_err(|e| {
                let msg = format!("{:#}", e);
                if msg.contains("timed out") || msg.contains("Timeout") {
                    McpError::invalid_request(
                        "git clone timed out after 300s. The repository may be very large; \
                         try using depth=1 for a shallow clone, or use exec_background for manual control."
                            .to_string(),
                        None,
                    )
                } else {
                    McpError::invalid_request(msg, None)
                }
            })?;

        if result.exit_code != 0 {
            let error_detail = if !result.stderr.is_empty() {
                &result.stderr
            } else {
                &result.stdout
            };
            return Err(McpError::invalid_request(
                format!(
                    "git clone failed (exit code {}): {}",
                    result.exit_code,
                    error_detail.trim()
                ),
                None,
            ));
        }

        // Get the HEAD commit SHA
        let sha_result = self
            .workspace_manager
            .exec(
                ws_id,
                &format!("git -C {} rev-parse HEAD", shell_escape(dest)),
                None,
                None,
                Some(10),
            )
            .await
            .ok();

        let commit_sha = sha_result
            .as_ref()
            .filter(|r| r.exit_code == 0)
            .map(|r| r.stdout.trim().to_string())
            .unwrap_or_default();

        let info = serde_json::json!({
            "success": true,
            "path": dest,
            "commit_sha": commit_sha,
            "url": params.url,
            "branch": params.branch,
            "depth": params.depth,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Orchestration Tools
    // -----------------------------------------------------------------------

    /// Create a "golden" workspace ready for mass forking. Creates a workspace, optionally
    /// clones a git repo and runs setup commands, then creates a snapshot named "golden".
    #[tool]
    async fn workspace_prepare(
        &self,
        Parameters(params): Parameters<WorkspacePrepareParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(
            tool = "workspace_prepare",
            name = %params.name,
            base_image = ?params.base_image,
            git_url = ?params.git_url,
            setup_commands_count = params.setup_commands.as_ref().map(|c| c.len()).unwrap_or(0),
            "tool call"
        );

        let base_image = params.base_image.unwrap_or_else(|| "alpine-opencode".to_string());

        if let Some(ref url) = params.git_url {
            validate_git_url(url)?;
        }

        validate_base_image(&base_image)?;

        // Step 1: Create workspace
        let mem = 512u32;
        let disk = 10u32;

        self.auth
            .check_quota(&self.session_id, mem as u64, disk as u64)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let create_params = crate::workspace::CreateParams {
            name: Some(params.name.clone()),
            base_image: Some(base_image),
            vcpus: None,
            memory_mb: Some(mem),
            disk_gb: Some(disk),
            allow_internet: Some(true), // Need internet for git clone
        };

        let workspace = self
            .workspace_manager
            .create(create_params)
            .await
            .map_err(|e| McpError::internal_error(format!("create failed: {:#}", e), None))?;

        let ws_id = workspace.id;

        // Register ownership
        if let Err(e) = self
            .auth
            .register_workspace(
                &self.session_id,
                ws_id,
                workspace.resources.memory_mb as u64,
                workspace.resources.disk_gb as u64,
            )
            .await
        {
            error!(workspace_id = %ws_id, error = %e, "quota check failed, rolling back");
            self.workspace_manager.destroy(ws_id).await.ok();
            return Err(McpError::invalid_request(e.to_string(), None));
        }

        // Step 2: Git clone if requested
        if let Some(ref git_url) = params.git_url {
            let cmd = format!("git clone {} /workspace", shell_escape(git_url));
            let result = self
                .workspace_manager
                .exec(ws_id, &cmd, None, None, Some(300))
                .await;
            match result {
                Ok(r) if r.exit_code != 0 => {
                    let err_msg = format!(
                        "git clone failed (exit {}): {}",
                        r.exit_code,
                        if !r.stderr.is_empty() { &r.stderr } else { &r.stdout }
                    );
                    self.workspace_manager.destroy(ws_id).await.ok();
                    self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                    return Err(McpError::internal_error(err_msg, None));
                }
                Err(e) => {
                    self.workspace_manager.destroy(ws_id).await.ok();
                    self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                    return Err(McpError::internal_error(format!("git clone failed: {:#}", e), None));
                }
                _ => {}
            }
        }

        // Step 3: Run setup commands in sequence
        if let Some(ref commands) = params.setup_commands {
            for (i, cmd) in commands.iter().enumerate() {
                let result = self
                    .workspace_manager
                    .exec(ws_id, cmd, None, None, Some(300))
                    .await;
                match result {
                    Ok(r) if r.exit_code != 0 => {
                        let err_msg = format!(
                            "setup command {} failed (exit {}): {}",
                            i + 1,
                            r.exit_code,
                            if !r.stderr.is_empty() { &r.stderr } else { &r.stdout }
                        );
                        self.workspace_manager.destroy(ws_id).await.ok();
                        self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                        return Err(McpError::internal_error(err_msg, None));
                    }
                    Err(e) => {
                        self.workspace_manager.destroy(ws_id).await.ok();
                        self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                        return Err(McpError::internal_error(
                            format!("setup command {} failed: {:#}", i + 1, e),
                            None,
                        ));
                    }
                    _ => {}
                }
            }
        }

        // Step 4: Flush guest filesystem before snapshotting
        if let Err(e) = self
            .workspace_manager
            .exec(ws_id, "sync", None, None, Some(30))
            .await
        {
            tracing::warn!(workspace_id = %ws_id, error = %e, "sync before snapshot failed");
        }

        // Step 5: Create golden snapshot
        let snapshot = self
            .workspace_manager
            .snapshot_create(ws_id, "golden", false)
            .await
            .map_err(|e| {
                // Best-effort cleanup on snapshot failure
                let mgr = self.workspace_manager.clone();
                let sid = self.session_id.clone();
                let auth = self.auth.clone();
                tokio::spawn(async move {
                    mgr.destroy(ws_id).await.ok();
                    auth.unregister_workspace(&sid, ws_id, mem as u64, disk as u64).await.ok();
                });
                McpError::internal_error(format!("snapshot creation failed: {:#}", e), None)
            })?;

        let info = serde_json::json!({
            "workspace_id": ws_id.to_string(),
            "name": params.name,
            "snapshot_name": "golden",
            "snapshot_id": snapshot.id.to_string(),
            "ip": workspace.network.ip.to_string(),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Fork N worker VMs from a workspace snapshot in parallel. Returns an array of worker details.
    /// Best-effort: if some forks fail, successful workers are still returned alongside errors.
    #[tool]
    async fn workspace_batch_fork(
        &self,
        Parameters(params): Parameters<WorkspaceBatchForkParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        let snapshot_name = params.snapshot_name.as_deref().unwrap_or("golden");
        let prefix = params.name_prefix.as_deref().unwrap_or("worker");
        let count = params.count;

        info!(
            workspace_id = %ws_id,
            tool = "workspace_batch_fork",
            snapshot_name = %snapshot_name,
            count,
            prefix = %prefix,
            "tool call"
        );

        self.check_ownership(ws_id).await?;
        validate_snapshot_name(snapshot_name)?;

        // Validate count range
        if count == 0 || count > 20 {
            return Err(McpError::invalid_params(
                format!("count must be between 1 and 20 (got {})", count),
                None,
            ));
        }

        // Validate prefix
        if prefix.is_empty()
            || !prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Err(McpError::invalid_params(
                format!(
                    "name_prefix '{}' contains invalid characters (allowed: alphanumeric, -, _, .)",
                    prefix
                ),
                None,
            ));
        }

        // Pre-check: verify source workspace exists and has the snapshot
        let source_ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

        source_ws.snapshots.get_by_name(snapshot_name).ok_or_else(|| {
            McpError::invalid_request(
                format!(
                    "snapshot '{}' not found on workspace '{}'. Use snapshot_list to see available snapshots.",
                    snapshot_name, params.workspace_id
                ),
                None,
            )
        })?;

        // Pre-check quota for all forks
        let mem = source_ws.resources.memory_mb as u64;
        let disk = source_ws.resources.disk_gb as u64;
        self.auth
            .check_quota(&self.session_id, mem * count as u64, disk * count as u64)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        // Fork N workspaces in parallel using JoinSet
        let mut join_set = tokio::task::JoinSet::new();
        for i in 1..=count {
            let name = format!("{}-{}", prefix, i);
            let mgr = self.workspace_manager.clone();
            let snap = snapshot_name.to_string();
            join_set.spawn(async move {
                let result = mgr.fork(ws_id, &snap, Some(name.clone())).await;
                (i, name, result)
            });
        }

        let mut successes = Vec::new();
        let mut failures = Vec::new();

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok((idx, name, Ok(new_ws))) => {
                    // Register ownership
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
                        warn!(workspace_id = %new_ws.id, error = %e, "quota registration failed for forked worker, destroying");
                        self.workspace_manager.destroy(new_ws.id).await.ok();
                        failures.push(serde_json::json!({
                            "index": idx,
                            "name": name,
                            "error": format!("quota registration failed: {}", e),
                        }));
                        continue;
                    }
                    successes.push(serde_json::json!({
                        "workspace_id": new_ws.id.to_string(),
                        "name": new_ws.name,
                        "ip": new_ws.network.ip.to_string(),
                    }));
                }
                Ok((idx, name, Err(e))) => {
                    failures.push(serde_json::json!({
                        "index": idx,
                        "name": name,
                        "error": format!("{:#}", e),
                    }));
                }
                Err(e) => {
                    failures.push(serde_json::json!({
                        "error": format!("task join error: {}", e),
                    }));
                }
            }
        }

        let info = serde_json::json!({
            "source_workspace_id": params.workspace_id,
            "snapshot_name": snapshot_name,
            "requested_count": count,
            "success_count": successes.len(),
            "failure_count": failures.len(),
            "workers": successes,
            "failures": failures,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Git & Diff Tools
    // -----------------------------------------------------------------------

    /// Get structured git status for a repository inside a running workspace VM.
    /// Returns branch info, ahead/behind counts, and categorized file changes.
    #[tool]
    async fn workspace_git_status(
        &self,
        Parameters(params): Parameters<WorkspaceGitStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        let path = params.path.as_deref().unwrap_or("/workspace");
        info!(workspace_id = %ws_id, tool = "workspace_git_status", path = %path, "tool call");
        self.check_ownership(ws_id).await?;

        let cmd = format!(
            "git -C {} status --porcelain=v2 --branch 2>&1",
            shell_escape(path)
        );

        let result = self
            .workspace_manager
            .exec(ws_id, &cmd, None, None, Some(30))
            .await
            .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

        if result.exit_code != 0 {
            // Check if git is not initialized
            let output = if !result.stderr.is_empty() {
                &result.stderr
            } else {
                &result.stdout
            };
            if output.contains("not a git repository") {
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "error": "not_a_git_repository",
                        "message": format!("'{}' is not a git repository. Run 'git init' or 'git clone' first.", path),
                    }))
                    .unwrap(),
                )]));
            }
            return Err(McpError::invalid_request(
                format!("git status failed (exit code {}): {}", result.exit_code, output.trim()),
                None,
            ));
        }

        let status = parse_git_porcelain_v2(&result.stdout);

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&status).unwrap(),
        )]))
    }

    /// Show what changed between a snapshot and the current workspace state.
    /// Returns snapshot metadata (creation time, size). Full file-level diff
    /// is not supported for ZFS zvol datasets.
    #[tool]
    async fn snapshot_diff(
        &self,
        Parameters(params): Parameters<SnapshotDiffParams>,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "snapshot_diff", snapshot_name = %params.snapshot_name, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.snapshot_name)?;

        // Verify the snapshot exists
        let ws = self
            .workspace_manager
            .get(ws_id)
            .await
            .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

        let snapshot = ws.snapshots.get_by_name(&params.snapshot_name).ok_or_else(|| {
            McpError::invalid_request(
                format!(
                    "snapshot '{}' not found on workspace '{}'. Use snapshot_list to see available snapshots.",
                    params.snapshot_name, params.workspace_id
                ),
                None,
            )
        })?;

        // TODO: Full file-level diff between a snapshot and current state requires
        // filesystem datasets (not zvols). ZFS zvols are block devices, and `zfs diff`
        // only works on mounted filesystem datasets. A future enhancement could mount
        // both the snapshot zvol and current zvol temporarily, or run diff commands
        // inside the VM against a snapshot-restored copy.
        //
        // For now, return snapshot metadata and a note about the limitation.

        // TODO: Call workspace_manager.snapshot_size(ws_id, &params.snapshot_name)
        // to get (used_bytes, referenced_bytes). The method is being added by another
        // agent to expose storage::Zfs::snapshot_size() through WorkspaceManager.

        let info = serde_json::json!({
            "snapshot_name": snapshot.name,
            "snapshot_id": snapshot.id.to_string(),
            "created_at": snapshot.created_at.to_rfc3339(),
            "has_memory": snapshot.qemu_state.is_some(),
            "parent": snapshot.parent.map(|p| p.to_string()),
            "limitation": "Full file-level diff is not supported for ZFS zvol datasets. \
                          Use 'exec' to run diff commands inside the VM if you need to compare files. \
                          Snapshot size info will be available once storage layer integration is complete.",
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Vault Tools
    // -----------------------------------------------------------------------

    /// Read a note from the vault by path. Returns content and parsed YAML frontmatter.
    #[tool]
    async fn vault_read(
        &self,
        Parameters(params): Parameters<VaultReadParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_read", path = %params.path, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        let note = vm.read_note(&params.path).await.map_err(|e| {
            McpError::internal_error(
                format!(
                    "Failed to read vault note '{}': {:#}. Use vault_list to browse \
                     available notes, or vault_search to find notes by content.",
                    params.path, e
                ),
                None,
            )
        })?;

        let format = params.format.as_deref().unwrap_or("markdown");
        let output = match format {
            "json" => {
                let json = serde_json::json!({
                    "path": note.path,
                    "content": note.content,
                    "frontmatter": note.frontmatter,
                });
                serde_json::to_string_pretty(&json).unwrap()
            }
            _ => note.content,
        };

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Search vault notes for a query string or regex pattern. Returns matching lines with context.
    #[tool]
    async fn vault_search(
        &self,
        Parameters(params): Parameters<VaultSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_search", query = %params.query, regex = ?params.regex, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        let max = params.max_results.unwrap_or(20);
        let is_regex = params.regex.unwrap_or(false);

        let results = vm
            .search(
                &params.query,
                is_regex,
                params.path_prefix.as_deref(),
                params.tag.as_deref(),
                max,
            )
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        let info = serde_json::json!({
            "query": params.query,
            "result_count": results.len(),
            "results": results,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// List notes and directories in the vault.
    #[tool]
    async fn vault_list(
        &self,
        Parameters(params): Parameters<VaultListParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_list", path = ?params.path, recursive = ?params.recursive, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        let recursive = params.recursive.unwrap_or(false);
        let entries = vm
            .list_notes(params.path.as_deref(), recursive)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        let info = serde_json::json!({
            "path": params.path.as_deref().unwrap_or("/"),
            "recursive": recursive,
            "count": entries.len(),
            "entries": entries,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Create or update a note in the vault. Supports overwrite, append, and prepend modes.
    #[tool]
    async fn vault_write(
        &self,
        Parameters(params): Parameters<VaultWriteParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_write", path = %params.path, mode = ?params.mode, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        let mode = match params.mode.as_deref().unwrap_or("overwrite") {
            "overwrite" => super::vault::WriteMode::Overwrite,
            "append" => super::vault::WriteMode::Append,
            "prepend" => super::vault::WriteMode::Prepend,
            other => {
                return Err(McpError::invalid_params(
                    format!("invalid write mode '{}': expected 'overwrite', 'append', or 'prepend'", other),
                    None,
                ));
            }
        };

        vm.write_note(&params.path, &params.content, mode)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Note '{}' written ({}).",
            params.path,
            params.mode.as_deref().unwrap_or("overwrite")
        ))]))
    }

    /// Get, set, or delete YAML frontmatter keys on a vault note.
    #[tool]
    async fn vault_frontmatter(
        &self,
        Parameters(params): Parameters<VaultFrontmatterParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_frontmatter", path = %params.path, action = %params.action, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        match params.action.as_str() {
            "get" => {
                let fm = vm.get_frontmatter(&params.path).await.map_err(|e| {
                    McpError::internal_error(format!("{:#}", e), None)
                })?;

                let info = serde_json::json!({
                    "path": params.path,
                    "frontmatter": fm,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&info).unwrap(),
                )]))
            }
            "set" => {
                let key = params.key.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'key' is required for action 'set'".to_string(), None)
                })?;
                let json_value = params.value.ok_or_else(|| {
                    McpError::invalid_params("'value' is required for action 'set'".to_string(), None)
                })?;

                let yaml_value: serde_yaml::Value =
                    serde_json::from_value(serde_json::to_value(&json_value).unwrap())
                        .map_err(|e| McpError::invalid_params(format!("invalid value: {}", e), None))?;

                vm.set_frontmatter(&params.path, key, yaml_value)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Frontmatter key '{}' set on '{}'.",
                    key, params.path
                ))]))
            }
            "delete" => {
                let key = params.key.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'key' is required for action 'delete'".to_string(), None)
                })?;

                vm.delete_frontmatter(&params.path, key)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Frontmatter key '{}' deleted from '{}'.",
                    key, params.path
                ))]))
            }
            other => Err(McpError::invalid_params(
                format!("invalid action '{}': expected 'get', 'set', or 'delete'", other),
                None,
            )),
        }
    }

    /// List, add, or remove tags on a vault note.
    #[tool]
    async fn vault_tags(
        &self,
        Parameters(params): Parameters<VaultTagsParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_tags", path = %params.path, action = %params.action, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        match params.action.as_str() {
            "list" => {
                let tags = vm.get_tags(&params.path).await.map_err(|e| {
                    McpError::internal_error(format!("{:#}", e), None)
                })?;

                let info = serde_json::json!({
                    "path": params.path,
                    "tags": tags,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&info).unwrap(),
                )]))
            }
            "add" => {
                let tag = params.tag.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'tag' is required for action 'add'".to_string(), None)
                })?;

                vm.add_tag(&params.path, tag)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Tag '{}' added to '{}'.",
                    tag, params.path
                ))]))
            }
            "remove" => {
                let tag = params.tag.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'tag' is required for action 'remove'".to_string(), None)
                })?;

                vm.remove_tag(&params.path, tag)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Tag '{}' removed from '{}'.",
                    tag, params.path
                ))]))
            }
            other => Err(McpError::invalid_params(
                format!("invalid action '{}': expected 'list', 'add', or 'remove'", other),
                None,
            )),
        }
    }

    /// Search and replace text within a vault note. Returns the number of replacements made.
    #[tool]
    async fn vault_replace(
        &self,
        Parameters(params): Parameters<VaultReplaceParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_replace", path = %params.path, regex = ?params.regex, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        let is_regex = params.regex.unwrap_or(false);
        let count = vm
            .search_replace(&params.path, &params.search, &params.replace, is_regex)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        let info = serde_json::json!({
            "path": params.path,
            "replacements": count,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Delete a note from the vault. Requires confirm=true to prevent accidental deletion.
    #[tool]
    async fn vault_delete(
        &self,
        Parameters(params): Parameters<VaultDeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_delete", path = %params.path, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        if !params.confirm {
            return Err(McpError::invalid_params(
                "confirm must be true to delete a note".to_string(),
                None,
            ));
        }

        vm.delete_note(&params.path)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Note '{}' deleted.",
            params.path
        ))]))
    }

    /// Move (rename) a note within the vault. Creates parent directories at the destination if needed.
    #[tool]
    async fn vault_move(
        &self,
        Parameters(params): Parameters<VaultMoveParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_move", path = %params.path, new_path = %params.new_path, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        let overwrite = params.overwrite.unwrap_or(false);
        let (from, to) = vm
            .move_note(&params.path, &params.new_path, overwrite)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        let info = serde_json::json!({
            "moved": true,
            "from": from,
            "to": to,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    /// Read multiple vault notes in a single call. Partial failures are returned per-path.
    #[tool]
    async fn vault_batch_read(
        &self,
        Parameters(params): Parameters<VaultBatchReadParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_batch_read", count = params.paths.len(), "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        let include_content = params.include_content.unwrap_or(true);
        let include_frontmatter = params.include_frontmatter.unwrap_or(true);

        let results = vm
            .batch_read(&params.paths, include_content, include_frontmatter)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&results).unwrap(),
        )]))
    }

    /// Get aggregate statistics about the vault: total notes, folders, size, and recently modified files.
    #[tool]
    async fn vault_stats(
        &self,
        Parameters(params): Parameters<VaultStatsParams>,
    ) -> Result<CallToolResult, McpError> {
        info!(tool = "vault_stats", recent_count = ?params.recent_count, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        let recent_count = params.recent_count.unwrap_or(10);
        let stats = vm
            .stats(recent_count)
            .await
            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&stats).unwrap(),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for AgentisoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "agentiso: QEMU microvm workspace manager for AI agents. Each workspace is a fully \
                 isolated Linux VM with its own filesystem, network stack, and process space. You can \
                 create, snapshot, fork, and destroy workspaces on demand.\n\
                 \n\
                 == TOOL GROUPS ==\n\
                 \n\
                 WORKSPACE LIFECYCLE: workspace_create, workspace_destroy, workspace_start, workspace_stop, \
                 workspace_list, workspace_info, workspace_ip, workspace_logs\n\
                 \n\
                 EXECUTION & FILES: exec, exec_background, exec_poll, exec_kill, file_read, file_write, \
                 file_edit, file_list, file_upload, file_download, set_env\n\
                 \n\
                 SNAPSHOTS & FORKS: snapshot_create, snapshot_restore, snapshot_list, snapshot_delete, \
                 workspace_fork\n\
                 \n\
                 NETWORKING: port_forward, port_forward_remove, network_policy, workspace_ip\n\
                 \n\
                 SESSION: workspace_adopt, workspace_adopt_all\n\
                 \n\
                 ORCHESTRATION: git_clone, workspace_prepare, workspace_batch_fork\n\
                 \n\
                 VAULT (shared knowledge base): vault_read, vault_write, vault_search, vault_list, \
                 vault_delete, vault_frontmatter, vault_tags, vault_replace, vault_move, vault_batch_read, \
                 vault_stats\n\
                 \n\
                 == QUICK START ==\n\
                 \n\
                 1. workspace_create(name=\"my-project\") -- get a workspace_id and a running VM\n\
                 2. git_clone(workspace_id, url=\"https://...\") -- clone a repo into /workspace\n\
                 3. exec(workspace_id, command=\"cd /workspace && npm install\") -- run setup\n\
                 4. snapshot_create(workspace_id, name=\"baseline\") -- checkpoint your progress\n\
                 5. ... work, experiment, snapshot, restore, fork as needed ...\n\
                 6. workspace_destroy(workspace_id) -- clean up when done\n\
                 \n\
                 == WORKFLOW TIPS ==\n\
                 \n\
                 - Start with workspace_create to get an isolated Linux VM. Each workspace has its own \
                 filesystem, network, and process space. Workspaces are identified by UUID or by the \
                 human-readable name you give them.\n\
                 \n\
                 - Use snapshot_create before risky operations (rm -rf, database migrations, config changes). \
                 Restore with snapshot_restore if something goes wrong. Snapshots are cheap (ZFS \
                 copy-on-write).\n\
                 \n\
                 - For parallel work, use workspace_fork to create independent copies from a snapshot. \
                 Each fork gets its own VM with a copy-on-write clone of the disk. For bulk parallelism, \
                 use workspace_prepare to build a golden image, then workspace_batch_fork to spin up N \
                 workers at once.\n\
                 \n\
                 - exec runs commands synchronously with a default timeout of 120 seconds. For long-running \
                 processes (servers, builds, test suites), use exec_background to start the command, \
                 exec_poll to check on it, and exec_kill to stop it. Output from exec and exec_poll is \
                 capped at 256KB; longer output is truncated.\n\
                 \n\
                 - Use set_env to inject persistent environment variables (API keys, tokens, config) into \
                 a workspace. These apply to all subsequent exec and exec_background calls until the VM \
                 is destroyed. Per-command env vars override stored values.\n\
                 \n\
                 - Internet access is on by default (allow_internet=true). If network requests fail, check \
                 the workspace's network_policy. Set allow_internet=false on workspace_create for fully \
                 isolated environments.\n\
                 \n\
                 - The vault is a shared markdown knowledge base on the host, accessible from any session \
                 without needing a workspace. Use vault_write to save findings, vault_search to find them \
                 later. Vault tools require [vault] to be enabled in the server's config.toml.\n\
                 \n\
                 - file_upload and file_download transfer files between the host and guest VM. Both require \
                 a configured transfer directory on the host; paths outside that directory are rejected for \
                 security.\n\
                 \n\
                 - Workspaces persist across reconnects. After a server restart, use workspace_adopt_all to \
                 reclaim ownership of existing workspaces before interacting with them. Use workspace_list \
                 to see all workspaces and their ownership status.\n\
                 \n\
                 == COMMON PITFALLS ==\n\
                 \n\
                 - snapshot_restore is DESTRUCTIVE: it removes all snapshots created after the restore \
                 target. If you need to preserve the full timeline, fork first with workspace_fork, then \
                 restore on the original.\n\
                 \n\
                 - file_read and file_write paths must be absolute paths inside the VM (e.g. \
                 /workspace/myfile.txt, /root/.config/settings.json). Do not use relative paths like \
                 ./myfile.txt.\n\
                 \n\
                 - exec has a default timeout of 120 seconds. For commands expected to run longer, either \
                 set timeout_secs explicitly (e.g. timeout_secs=600) or use exec_background + exec_poll \
                 instead.\n\
                 \n\
                 - The guest OS is Alpine Linux. Package installation uses 'apk add <package>', not apt \
                 or yum. Common dev tools are pre-installed in the alpine-dev image.\n\
                 \n\
                 - Use workspace_logs to debug boot failures or VM issues. It shows QEMU console output \
                 and stderr.\n\
                 \n\
                 - workspace_batch_fork accepts count 1-20. For larger parallelism, call it multiple times \
                 or fork sequentially with workspace_fork."
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
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Workspace {} is not owned by this session: {}. Use workspace_adopt \
                         to claim it, or workspace_adopt_all to claim all orphaned workspaces.",
                        workspace_id, e
                    ),
                    None,
                )
            })
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

    /// Resolve a workspace identifier that may be either a UUID or a workspace name.
    ///
    /// Tries to parse as UUID first. If that fails, looks up by name via the
    /// workspace manager. This lets agents use human-readable names like
    /// "my-project" instead of UUIDs.
    async fn resolve_workspace_id(&self, id_or_name: &str) -> Result<Uuid, McpError> {
        // Fast path: try UUID parse first
        if let Ok(uuid) = Uuid::parse_str(id_or_name) {
            return Ok(uuid);
        }

        // Slow path: look up by name
        match self.workspace_manager.find_by_name(id_or_name).await {
            Some(uuid) => Ok(uuid),
            None => Err(McpError::invalid_request(
                format!(
                    "workspace '{}' not found: not a valid UUID and no workspace with that name exists. \
                     Use workspace_list to see available workspaces and their IDs/names.",
                    id_or_name
                ),
                None,
            )),
        }
    }
}

/// Validate that a base image name contains only safe characters and no path traversal.
fn validate_base_image(name: &str) -> Result<(), McpError> {
    if name.is_empty() {
        return Err(McpError::invalid_params(
            "base_image must not be empty".to_string(),
            None,
        ));
    }
    if name.len() > 128 {
        return Err(McpError::invalid_params(
            format!(
                "base_image name too long: {} chars (max 128)",
                name.len()
            ),
            None,
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(McpError::invalid_params(
            format!(
                "base_image '{}' contains invalid characters (allowed: alphanumeric, -, _, .)",
                name
            ),
            None,
        ));
    }
    if name.contains("..") {
        return Err(McpError::invalid_params(
            format!("base_image '{}' contains path traversal", name),
            None,
        ));
    }
    Ok(())
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
        let invalid: Vec<char> = name
            .chars()
            .filter(|c| !c.is_ascii_alphanumeric() && *c != '_' && *c != '-' && *c != '.')
            .collect();
        return Err(McpError::invalid_params(
            format!(
                "snapshot name '{}' contains invalid character(s): {:?}. \
                 Allowed: letters (a-z, A-Z), digits (0-9), underscores (_), hyphens (-), and dots (.).",
                name, invalid
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
/// Uses `is_char_boundary` to avoid panicking on multi-byte UTF-8 characters.
fn truncate_output(s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    let total = s.len();
    // Find the largest valid char boundary <= limit
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[TRUNCATED: {} bytes total, showing first {} bytes]",
        &s[..end],
        total,
        end
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

/// Validate a git URL. Allows https://, http://, git://, and ssh:// URLs,
/// as well as SCP-style git@host:path URLs.
fn validate_git_url(url: &str) -> Result<(), McpError> {
    let valid = url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("git://")
        || url.starts_with("ssh://")
        || (url.contains('@') && url.contains(':'));

    if !valid {
        return Err(McpError::invalid_params(
            format!(
                "invalid git URL '{}'. Expected https://, http://, git://, ssh://, \
                 or SCP-style (git@host:path) URL.",
                url
            ),
            None,
        ));
    }

    // Reject URLs with shell metacharacters to prevent command injection
    if url.chars().any(|c| matches!(c, ';' | '|' | '&' | '$' | '`' | '\'' | '"' | '\\' | '\n' | '\r')) {
        return Err(McpError::invalid_params(
            "git URL contains invalid characters".to_string(),
            None,
        ));
    }

    Ok(())
}

/// Escape a string for safe use in a shell command.
/// Wraps in single quotes and escapes any internal single quotes.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Parse `git status --porcelain=v2 --branch` output into structured data.
///
/// Porcelain v2 format:
/// - `# branch.head <name>` -- current branch
/// - `# branch.ab +<ahead> -<behind>` -- ahead/behind counts
/// - `1 <XY> ...` -- tracked file, ordinary change
/// - `2 <XY> ...` -- tracked file, rename/copy
/// - `u <XY> ...` -- unmerged (conflicted) file
/// - `? <path>` -- untracked file
fn parse_git_porcelain_v2(output: &str) -> GitStatusInfo {
    let mut branch = String::from("(unknown)");
    let mut ahead: i64 = 0;
    let mut behind: i64 = 0;
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicted = Vec::new();

    for line in output.lines() {
        if line.starts_with("# branch.head ") {
            branch = line.trim_start_matches("# branch.head ").to_string();
        } else if line.starts_with("# branch.ab ") {
            // Format: "# branch.ab +N -M"
            let ab = line.trim_start_matches("# branch.ab ");
            for token in ab.split_whitespace() {
                if let Some(val) = token.strip_prefix('+') {
                    ahead = val.parse().unwrap_or(0);
                } else if let Some(val) = token.strip_prefix('-') {
                    behind = val.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") {
            // Ordinary change.
            // Format: "1 XY <sub> <mH> <mI> <mW> <hH> <hI> <path>"
            let parts: Vec<&str> = line.splitn(9, ' ').collect();
            if parts.len() >= 9 {
                let xy = parts[1];
                let xy_bytes = xy.as_bytes();
                let file_path = parts[8];

                // X (index/staged status): not '.' means staged
                if !xy_bytes.is_empty() && xy_bytes[0] != b'.' {
                    staged.push(file_path.to_string());
                }
                // Y (worktree status): not '.' means modified in worktree
                if xy_bytes.len() >= 2 && xy_bytes[1] != b'.' {
                    modified.push(file_path.to_string());
                }
            }
        } else if line.starts_with("2 ") {
            // Rename/copy change.
            // Format: "2 XY <sub> <mH> <mI> <mW> <hH> <hI> <X_score> <path>\t<origPath>"
            let parts: Vec<&str> = line.splitn(10, ' ').collect();
            if parts.len() >= 10 {
                let xy = parts[1];
                let xy_bytes = xy.as_bytes();
                // The 10th field is "path\torigPath"
                let file_path = parts[9].split('\t').next().unwrap_or(parts[9]);

                // X (index/staged status): not '.' means staged
                if !xy_bytes.is_empty() && xy_bytes[0] != b'.' {
                    staged.push(file_path.to_string());
                }
                // Y (worktree status): not '.' means modified in worktree
                if xy_bytes.len() >= 2 && xy_bytes[1] != b'.' {
                    modified.push(file_path.to_string());
                }
            }
        } else if line.starts_with("u ") {
            // Unmerged (conflicted) file.
            // Format: "u XY <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>"
            let parts: Vec<&str> = line.splitn(11, ' ').collect();
            if let Some(path) = parts.last() {
                conflicted.push(path.to_string());
            }
        } else if line.starts_with("? ") {
            // Untracked file
            let path = line.trim_start_matches("? ");
            untracked.push(path.to_string());
        }
    }

    let dirty = !staged.is_empty() || !modified.is_empty() || !untracked.is_empty() || !conflicted.is_empty();

    GitStatusInfo {
        branch,
        ahead,
        behind,
        staged,
        modified,
        untracked,
        conflicted,
        dirty,
    }
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
            "disk_gb": 20,
            "allow_internet": true
        });
        let params: WorkspaceCreateParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.name.as_deref(), Some("my-workspace"));
        assert_eq!(params.base_image.as_deref(), Some("alpine-dev"));
        assert_eq!(params.vcpus, Some(4));
        assert_eq!(params.memory_mb, Some(1024));
        assert_eq!(params.disk_gb, Some(20));
        assert_eq!(params.allow_internet, Some(true));
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
    fn test_validate_snapshot_name_invalid_chars_detail() {
        let err = validate_snapshot_name("snap shot/v2").unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("Allowed:"), "error should list allowed chars");
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

    #[test]
    fn test_truncate_output_multibyte_utf8() {
        // "hello 世界" — '世' is 3 bytes starting at index 6
        let s = "hello 世界".to_string();
        // Truncate at byte 7, which is in the middle of '世'
        let result = truncate_output(s, 7);
        // Should back up to byte 6 (just after the space)
        assert!(result.starts_with("hello "));
        assert!(result.contains("TRUNCATED"));
    }

    // --- Base image validation tests ---

    #[test]
    fn test_validate_base_image_valid() {
        assert!(validate_base_image("alpine-dev").is_ok());
        assert!(validate_base_image("ubuntu_22.04").is_ok());
        assert!(validate_base_image("my-image.v2").is_ok());
        assert!(validate_base_image("ABC123").is_ok());
    }

    #[test]
    fn test_validate_base_image_empty() {
        assert!(validate_base_image("").is_err());
    }

    #[test]
    fn test_validate_base_image_too_long() {
        let long_name = "a".repeat(129);
        assert!(validate_base_image(&long_name).is_err());
        // Exactly 128 should be ok
        let ok_name = "a".repeat(128);
        assert!(validate_base_image(&ok_name).is_ok());
    }

    #[test]
    fn test_validate_base_image_invalid_chars() {
        assert!(validate_base_image("../etc/passwd").is_err());
        assert!(validate_base_image("image name").is_err());
        assert!(validate_base_image("image/name").is_err());
        assert!(validate_base_image("image;rm -rf").is_err());
    }

    #[test]
    fn test_validate_base_image_path_traversal() {
        assert!(validate_base_image("foo..bar").is_err());
        assert!(validate_base_image("..").is_err());
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

    // --- Adoption param tests ---

    #[test]
    fn test_workspace_adopt_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: WorkspaceAdoptParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn test_workspace_adopt_params_missing_required() {
        let json = serde_json::json!({});
        let result = serde_json::from_value::<WorkspaceAdoptParams>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_workspace_adopt_all_params() {
        let json = serde_json::json!({});
        let _params: WorkspaceAdoptAllParams = serde_json::from_value(json).unwrap();
    }

    // --- exec_kill param tests ---

    #[test]
    fn test_exec_kill_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "job_id": 42,
            "signal": 15
        });
        let params: ExecKillParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.job_id, 42);
        assert_eq!(params.signal, Some(15));
    }

    #[test]
    fn test_exec_kill_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "job_id": 7
        });
        let params: ExecKillParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.job_id, 7);
        assert!(params.signal.is_none());
    }

    #[test]
    fn test_exec_kill_params_missing_required() {
        // Missing job_id
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<ExecKillParams>(json).is_err());

        // Missing workspace_id
        let json = serde_json::json!({
            "job_id": 1
        });
        assert!(serde_json::from_value::<ExecKillParams>(json).is_err());
    }

    // --- workspace_logs param tests ---

    #[test]
    fn test_workspace_logs_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "log_type": "console",
            "max_lines": 50
        });
        let params: WorkspaceLogsParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.log_type.as_deref(), Some("console"));
        assert_eq!(params.max_lines, Some(50));
    }

    #[test]
    fn test_workspace_logs_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: WorkspaceLogsParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert!(params.log_type.is_none());
        assert!(params.max_lines.is_none());
    }

    #[test]
    fn test_workspace_logs_params_stderr() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "log_type": "stderr"
        });
        let params: WorkspaceLogsParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.log_type.as_deref(), Some("stderr"));
    }

    #[test]
    fn test_workspace_logs_params_all() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "log_type": "all"
        });
        let params: WorkspaceLogsParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.log_type.as_deref(), Some("all"));
    }

    #[test]
    fn test_workspace_logs_params_missing_required() {
        let json = serde_json::json!({});
        assert!(serde_json::from_value::<WorkspaceLogsParams>(json).is_err());
    }

    // --- Git URL validation tests ---

    #[test]
    fn test_validate_git_url_https() {
        assert!(validate_git_url("https://github.com/user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_http() {
        assert!(validate_git_url("http://github.com/user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_ssh() {
        assert!(validate_git_url("ssh://git@github.com/user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_scp_style() {
        assert!(validate_git_url("git@github.com:user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_git_protocol() {
        assert!(validate_git_url("git://github.com/user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_invalid() {
        assert!(validate_git_url("not-a-url").is_err());
        assert!(validate_git_url("/local/path").is_err());
        assert!(validate_git_url("ftp://host/repo").is_err());
    }

    #[test]
    fn test_validate_git_url_rejects_injection() {
        assert!(validate_git_url("https://github.com/repo; rm -rf /").is_err());
        assert!(validate_git_url("https://github.com/repo|cat /etc/passwd").is_err());
        assert!(validate_git_url("https://github.com/repo$(whoami)").is_err());
    }

    // --- Shell escape tests ---

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn test_shell_escape_with_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn test_shell_escape_with_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    // --- Git clone params tests ---

    #[test]
    fn test_git_clone_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "url": "https://github.com/user/repo.git",
            "path": "/home/user/project",
            "branch": "main",
            "depth": 1
        });
        let params: GitCloneParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.url, "https://github.com/user/repo.git");
        assert_eq!(params.path.as_deref(), Some("/home/user/project"));
        assert_eq!(params.branch.as_deref(), Some("main"));
        assert_eq!(params.depth, Some(1));
    }

    #[test]
    fn test_git_clone_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "url": "https://github.com/user/repo.git"
        });
        let params: GitCloneParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.url, "https://github.com/user/repo.git");
        assert!(params.path.is_none());
        assert!(params.branch.is_none());
        assert!(params.depth.is_none());
    }

    #[test]
    fn test_git_clone_params_missing_required() {
        // Missing url
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<GitCloneParams>(json).is_err());

        // Missing workspace_id
        let json = serde_json::json!({
            "url": "https://github.com/user/repo.git"
        });
        assert!(serde_json::from_value::<GitCloneParams>(json).is_err());
    }

    // --- workspace_prepare param tests ---

    #[test]
    fn test_workspace_prepare_params_full() {
        let json = serde_json::json!({
            "name": "golden-project",
            "base_image": "alpine-opencode",
            "git_url": "https://github.com/user/repo.git",
            "setup_commands": ["apk add nodejs npm", "cd /workspace && npm install"]
        });
        let params: WorkspacePrepareParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.name, "golden-project");
        assert_eq!(params.base_image.as_deref(), Some("alpine-opencode"));
        assert_eq!(params.git_url.as_deref(), Some("https://github.com/user/repo.git"));
        assert_eq!(params.setup_commands.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_workspace_prepare_params_minimal() {
        let json = serde_json::json!({
            "name": "my-golden"
        });
        let params: WorkspacePrepareParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.name, "my-golden");
        assert!(params.base_image.is_none());
        assert!(params.git_url.is_none());
        assert!(params.setup_commands.is_none());
    }

    #[test]
    fn test_workspace_prepare_params_missing_name() {
        let json = serde_json::json!({
            "base_image": "alpine-opencode"
        });
        assert!(serde_json::from_value::<WorkspacePrepareParams>(json).is_err());
    }

    // --- workspace_batch_fork param tests ---

    #[test]
    fn test_workspace_batch_fork_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "snapshot_name": "golden",
            "count": 5,
            "name_prefix": "task"
        });
        let params: WorkspaceBatchForkParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.snapshot_name.as_deref(), Some("golden"));
        assert_eq!(params.count, 5);
        assert_eq!(params.name_prefix.as_deref(), Some("task"));
    }

    #[test]
    fn test_workspace_batch_fork_params_defaults() {
        let json = serde_json::json!({
            "workspace_id": "my-golden-workspace",
            "count": 3
        });
        let params: WorkspaceBatchForkParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "my-golden-workspace");
        assert!(params.snapshot_name.is_none());
        assert_eq!(params.count, 3);
        assert!(params.name_prefix.is_none());
    }

    #[test]
    fn test_workspace_batch_fork_params_missing_count() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<WorkspaceBatchForkParams>(json).is_err());
    }

    #[test]
    fn test_workspace_batch_fork_params_missing_workspace_id() {
        let json = serde_json::json!({
            "count": 5
        });
        assert!(serde_json::from_value::<WorkspaceBatchForkParams>(json).is_err());
    }

    #[test]
    fn test_workspace_batch_fork_name_generation() {
        // Verify the naming pattern used in the handler
        let prefix = "worker";
        let count = 3u32;
        let names: Vec<String> = (1..=count).map(|i| format!("{}-{}", prefix, i)).collect();
        assert_eq!(names, vec!["worker-1", "worker-2", "worker-3"]);
    }

    #[test]
    fn test_workspace_batch_fork_custom_prefix_name_generation() {
        let prefix = "task";
        let count = 5u32;
        let names: Vec<String> = (1..=count).map(|i| format!("{}-{}", prefix, i)).collect();
        assert_eq!(
            names,
            vec!["task-1", "task-2", "task-3", "task-4", "task-5"]
        );
    }

    // --- set_env param tests ---

    #[test]
    fn test_set_env_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "vars": {
                "ANTHROPIC_API_KEY": "sk-ant-test123",
                "MY_VAR": "hello"
            }
        });
        let params: SetEnvParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.vars.len(), 2);
        assert_eq!(params.vars.get("ANTHROPIC_API_KEY").unwrap(), "sk-ant-test123");
        assert_eq!(params.vars.get("MY_VAR").unwrap(), "hello");
    }

    #[test]
    fn test_set_env_params_empty_vars() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "vars": {}
        });
        let params: SetEnvParams = serde_json::from_value(json).unwrap();
        assert!(params.vars.is_empty());
    }

    #[test]
    fn test_set_env_params_missing_vars() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<SetEnvParams>(json).is_err());
    }

    #[test]
    fn test_set_env_params_missing_workspace_id() {
        let json = serde_json::json!({
            "vars": {"KEY": "val"}
        });
        assert!(serde_json::from_value::<SetEnvParams>(json).is_err());
    }

    // --- Vault tool param tests ---

    #[test]
    fn test_vault_read_params_full() {
        let json = serde_json::json!({
            "path": "projects/design.md",
            "format": "json"
        });
        let params: VaultReadParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "projects/design.md");
        assert_eq!(params.format.as_deref(), Some("json"));
    }

    #[test]
    fn test_vault_read_params_minimal() {
        let json = serde_json::json!({
            "path": "notes/hello.md"
        });
        let params: VaultReadParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "notes/hello.md");
        assert!(params.format.is_none());
    }

    #[test]
    fn test_vault_read_params_missing_required() {
        let json = serde_json::json!({});
        assert!(serde_json::from_value::<VaultReadParams>(json).is_err());
    }

    #[test]
    fn test_vault_search_params_full() {
        let json = serde_json::json!({
            "query": "auth.*pattern",
            "regex": true,
            "path_prefix": "projects/",
            "tag": "architecture",
            "max_results": 50
        });
        let params: VaultSearchParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.query, "auth.*pattern");
        assert_eq!(params.regex, Some(true));
        assert_eq!(params.path_prefix.as_deref(), Some("projects/"));
        assert_eq!(params.tag.as_deref(), Some("architecture"));
        assert_eq!(params.max_results, Some(50));
    }

    #[test]
    fn test_vault_search_params_minimal() {
        let json = serde_json::json!({
            "query": "hello"
        });
        let params: VaultSearchParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.query, "hello");
        assert!(params.regex.is_none());
        assert!(params.path_prefix.is_none());
        assert!(params.tag.is_none());
        assert!(params.max_results.is_none());
    }

    #[test]
    fn test_vault_list_params_full() {
        let json = serde_json::json!({
            "path": "projects",
            "recursive": true
        });
        let params: VaultListParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path.as_deref(), Some("projects"));
        assert_eq!(params.recursive, Some(true));
    }

    #[test]
    fn test_vault_list_params_empty() {
        let json = serde_json::json!({});
        let params: VaultListParams = serde_json::from_value(json).unwrap();
        assert!(params.path.is_none());
        assert!(params.recursive.is_none());
    }

    #[test]
    fn test_vault_write_params_full() {
        let json = serde_json::json!({
            "path": "notes/new.md",
            "content": "# New Note\n\nHello world.",
            "mode": "append"
        });
        let params: VaultWriteParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "notes/new.md");
        assert_eq!(params.content, "# New Note\n\nHello world.");
        assert_eq!(params.mode.as_deref(), Some("append"));
    }

    #[test]
    fn test_vault_write_params_minimal() {
        let json = serde_json::json!({
            "path": "notes/new.md",
            "content": "Hello"
        });
        let params: VaultWriteParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "notes/new.md");
        assert!(params.mode.is_none());
    }

    #[test]
    fn test_vault_frontmatter_params_get() {
        let json = serde_json::json!({
            "path": "design.md",
            "action": "get"
        });
        let params: VaultFrontmatterParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "get");
        assert!(params.key.is_none());
        assert!(params.value.is_none());
    }

    #[test]
    fn test_vault_frontmatter_params_set() {
        let json = serde_json::json!({
            "path": "design.md",
            "action": "set",
            "key": "status",
            "value": "draft"
        });
        let params: VaultFrontmatterParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "set");
        assert_eq!(params.key.as_deref(), Some("status"));
        assert_eq!(params.value, Some(serde_json::json!("draft")));
    }

    #[test]
    fn test_vault_frontmatter_params_delete() {
        let json = serde_json::json!({
            "path": "design.md",
            "action": "delete",
            "key": "deprecated"
        });
        let params: VaultFrontmatterParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "delete");
        assert_eq!(params.key.as_deref(), Some("deprecated"));
    }

    #[test]
    fn test_vault_tags_params_list() {
        let json = serde_json::json!({
            "path": "notes/hello.md",
            "action": "list"
        });
        let params: VaultTagsParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "list");
        assert!(params.tag.is_none());
    }

    #[test]
    fn test_vault_tags_params_add() {
        let json = serde_json::json!({
            "path": "notes/hello.md",
            "action": "add",
            "tag": "important"
        });
        let params: VaultTagsParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "add");
        assert_eq!(params.tag.as_deref(), Some("important"));
    }

    #[test]
    fn test_vault_replace_params_full() {
        let json = serde_json::json!({
            "path": "notes/hello.md",
            "search": "old_text",
            "replace": "new_text",
            "regex": false
        });
        let params: VaultReplaceParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "notes/hello.md");
        assert_eq!(params.search, "old_text");
        assert_eq!(params.replace, "new_text");
        assert_eq!(params.regex, Some(false));
    }

    #[test]
    fn test_vault_replace_params_minimal() {
        let json = serde_json::json!({
            "path": "notes/hello.md",
            "search": "old",
            "replace": "new"
        });
        let params: VaultReplaceParams = serde_json::from_value(json).unwrap();
        assert!(params.regex.is_none());
    }

    #[test]
    fn test_vault_delete_params() {
        let json = serde_json::json!({
            "path": "notes/old.md",
            "confirm": true
        });
        let params: VaultDeleteParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "notes/old.md");
        assert!(params.confirm);
    }

    #[test]
    fn test_vault_delete_params_confirm_false() {
        let json = serde_json::json!({
            "path": "notes/old.md",
            "confirm": false
        });
        let params: VaultDeleteParams = serde_json::from_value(json).unwrap();
        assert!(!params.confirm);
    }

    #[test]
    fn test_vault_delete_params_missing_confirm() {
        let json = serde_json::json!({
            "path": "notes/old.md"
        });
        assert!(serde_json::from_value::<VaultDeleteParams>(json).is_err());
    }

    // --- vault_move param tests ---

    #[test]
    fn test_vault_move_params_full() {
        let json = serde_json::json!({
            "path": "notes/old.md",
            "new_path": "archive/old.md",
            "overwrite": true
        });
        let params: VaultMoveParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "notes/old.md");
        assert_eq!(params.new_path, "archive/old.md");
        assert_eq!(params.overwrite, Some(true));
    }

    #[test]
    fn test_vault_move_params_minimal() {
        let json = serde_json::json!({
            "path": "notes/old.md",
            "new_path": "notes/new.md"
        });
        let params: VaultMoveParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path, "notes/old.md");
        assert_eq!(params.new_path, "notes/new.md");
        assert!(params.overwrite.is_none());
    }

    #[test]
    fn test_vault_move_params_missing_new_path() {
        let json = serde_json::json!({
            "path": "notes/old.md"
        });
        assert!(serde_json::from_value::<VaultMoveParams>(json).is_err());
    }

    // --- vault_batch_read param tests ---

    #[test]
    fn test_vault_batch_read_params_full() {
        let json = serde_json::json!({
            "paths": ["notes/a.md", "notes/b.md"],
            "include_content": false,
            "include_frontmatter": true
        });
        let params: VaultBatchReadParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.paths.len(), 2);
        assert_eq!(params.include_content, Some(false));
        assert_eq!(params.include_frontmatter, Some(true));
    }

    #[test]
    fn test_vault_batch_read_params_minimal() {
        let json = serde_json::json!({
            "paths": ["notes/a.md"]
        });
        let params: VaultBatchReadParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.paths.len(), 1);
        assert!(params.include_content.is_none());
        assert!(params.include_frontmatter.is_none());
    }

    #[test]
    fn test_vault_batch_read_params_missing_paths() {
        let json = serde_json::json!({});
        assert!(serde_json::from_value::<VaultBatchReadParams>(json).is_err());
    }

    // --- vault_stats param tests ---

    #[test]
    fn test_vault_stats_params_full() {
        let json = serde_json::json!({
            "recent_count": 5
        });
        let params: VaultStatsParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.recent_count, Some(5));
    }

    #[test]
    fn test_vault_stats_params_empty() {
        let json = serde_json::json!({});
        let params: VaultStatsParams = serde_json::from_value(json).unwrap();
        assert!(params.recent_count.is_none());
    }

    // --- Tool registration verification ---

    #[test]
    fn test_tool_router_has_exactly_45_tools() {
        let router = AgentisoServer::tool_router();
        assert_eq!(
            router.map.len(),
            45,
            "expected exactly 45 tools registered, got {}. Tool list: {:?}",
            router.map.len(),
            router.map.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_tool_router_contains_all_core_tools() {
        let router = AgentisoServer::tool_router();
        let expected_tools = [
            "workspace_create",
            "workspace_destroy",
            "workspace_list",
            "workspace_info",
            "workspace_stop",
            "workspace_start",
            "exec",
            "file_write",
            "file_read",
            "file_upload",
            "file_download",
            "snapshot_create",
            "snapshot_restore",
            "snapshot_list",
            "snapshot_delete",
            "workspace_fork",
            "port_forward",
            "port_forward_remove",
            "workspace_ip",
            "network_policy",
            "file_list",
            "file_edit",
            "exec_background",
            "exec_poll",
            "exec_kill",
            "set_env",
            "workspace_logs",
            "workspace_adopt",
            "workspace_adopt_all",
            "git_clone",
            "workspace_prepare",
            "workspace_batch_fork",
            "workspace_git_status",
            "snapshot_diff",
            "vault_read",
            "vault_search",
            "vault_list",
            "vault_write",
            "vault_frontmatter",
            "vault_tags",
            "vault_replace",
            "vault_delete",
            "vault_move",
            "vault_batch_read",
            "vault_stats",
        ];
        for tool_name in &expected_tools {
            assert!(
                router.has_route(tool_name),
                "tool '{}' missing from router",
                tool_name
            );
        }
    }

    // --- Tool metadata verification ---

    #[test]
    fn test_all_tools_have_descriptions() {
        let router = AgentisoServer::tool_router();
        let tools = router.list_all();
        for tool in &tools {
            assert!(
                tool.description.as_ref().map_or(false, |d| !d.is_empty()),
                "tool '{}' is missing a description",
                tool.name
            );
        }
    }

    #[test]
    fn test_all_tools_have_input_schemas() {
        let router = AgentisoServer::tool_router();
        let tools = router.list_all();
        for tool in &tools {
            // input_schema should be an object with "type": "object" at minimum
            let schema = &tool.input_schema;
            assert!(
                !schema.is_empty(),
                "tool '{}' has an empty input schema",
                tool.name
            );
        }
    }

    // --- Param struct completeness: untested structs ---

    #[test]
    fn test_file_transfer_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "host_path": "/tmp/transfer/data.tar.gz",
            "guest_path": "/home/user/data.tar.gz"
        });
        let params: FileTransferParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.host_path, "/tmp/transfer/data.tar.gz");
        assert_eq!(params.guest_path, "/home/user/data.tar.gz");
    }

    #[test]
    fn test_file_transfer_params_missing_required() {
        // Missing guest_path
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "host_path": "/tmp/file.txt"
        });
        assert!(serde_json::from_value::<FileTransferParams>(json).is_err());

        // Missing host_path
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_path": "/home/user/file.txt"
        });
        assert!(serde_json::from_value::<FileTransferParams>(json).is_err());
    }

    #[test]
    fn test_snapshot_name_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "snapshot_name": "before-deploy"
        });
        let params: SnapshotNameParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.snapshot_name, "before-deploy");
    }

    #[test]
    fn test_snapshot_name_params_missing_required() {
        // Missing snapshot_name
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<SnapshotNameParams>(json).is_err());

        // Missing workspace_id
        let json = serde_json::json!({
            "snapshot_name": "snap1"
        });
        assert!(serde_json::from_value::<SnapshotNameParams>(json).is_err());
    }

    #[test]
    fn test_workspace_fork_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "snapshot_name": "golden",
            "new_name": "experiment-branch"
        });
        let params: WorkspaceForkParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.snapshot_name, "golden");
        assert_eq!(params.new_name.as_deref(), Some("experiment-branch"));
    }

    #[test]
    fn test_workspace_fork_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "snapshot_name": "snap1"
        });
        let params: WorkspaceForkParams = serde_json::from_value(json).unwrap();
        assert!(params.new_name.is_none());
    }

    #[test]
    fn test_workspace_fork_params_missing_required() {
        // Missing snapshot_name
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<WorkspaceForkParams>(json).is_err());
    }

    #[test]
    fn test_workspace_list_params_with_filter() {
        let json = serde_json::json!({
            "state_filter": "running"
        });
        let params: WorkspaceListParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.state_filter.as_deref(), Some("running"));
    }

    #[test]
    fn test_workspace_list_params_empty() {
        let json = serde_json::json!({});
        let params: WorkspaceListParams = serde_json::from_value(json).unwrap();
        assert!(params.state_filter.is_none());
    }

    #[test]
    fn test_port_forward_remove_params() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_port": 8080
        });
        let params: PortForwardRemoveParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.guest_port, 8080);
    }

    #[test]
    fn test_port_forward_remove_params_missing_required() {
        // Missing guest_port
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<PortForwardRemoveParams>(json).is_err());
    }

    // --- workspace_git_status param tests ---

    #[test]
    fn test_workspace_git_status_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/home/user/project"
        });
        let params: WorkspaceGitStatusParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.path.as_deref(), Some("/home/user/project"));
    }

    #[test]
    fn test_workspace_git_status_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: WorkspaceGitStatusParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert!(params.path.is_none());
    }

    #[test]
    fn test_workspace_git_status_params_missing_required() {
        let json = serde_json::json!({});
        assert!(serde_json::from_value::<WorkspaceGitStatusParams>(json).is_err());
    }

    // --- snapshot_diff param tests ---

    #[test]
    fn test_snapshot_diff_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "snapshot_name": "checkpoint-1"
        });
        let params: SnapshotDiffParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.snapshot_name, "checkpoint-1");
    }

    #[test]
    fn test_snapshot_diff_params_missing_required() {
        // Missing snapshot_name
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<SnapshotDiffParams>(json).is_err());

        // Missing workspace_id
        let json = serde_json::json!({
            "snapshot_name": "snap1"
        });
        assert!(serde_json::from_value::<SnapshotDiffParams>(json).is_err());
    }

    // --- Git porcelain v2 parser tests ---

    #[test]
    fn test_parse_git_porcelain_v2_clean() {
        let output = "# branch.oid abc123def456\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.branch, "main");
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
        assert!(status.staged.is_empty());
        assert!(status.modified.is_empty());
        assert!(status.untracked.is_empty());
        assert!(status.conflicted.is_empty());
        assert!(!status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_with_changes() {
        let output = "\
# branch.oid abc123def456
# branch.head feature-branch
# branch.upstream origin/feature-branch
# branch.ab +3 -1
1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 abc123def456abc123def456abc123def456abc12345 README.md
1 .M N... 100644 100644 100644 abc123def456abc123def456abc123def456abc12345 abc123def456abc123def456abc123def456abc12345 src/main.rs
? test.log
";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.branch, "feature-branch");
        assert_eq!(status.ahead, 3);
        assert_eq!(status.behind, 1);
        assert_eq!(status.staged, vec!["README.md"]);
        assert_eq!(status.modified, vec!["src/main.rs"]);
        assert_eq!(status.untracked, vec!["test.log"]);
        assert!(status.conflicted.is_empty());
        assert!(status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_untracked_only() {
        let output = "# branch.head main\n? newfile.txt\n? another.txt\n";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.branch, "main");
        assert_eq!(status.untracked, vec!["newfile.txt", "another.txt"]);
        assert!(status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_conflicted() {
        let output = "\
# branch.head main
u UU N... 100644 100644 100644 100644 abc123 def456 789abc conflicted-file.rs
";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.conflicted, vec!["conflicted-file.rs"]);
        assert!(status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_rename() {
        let output = "\
# branch.head main
2 R. N... 100644 100644 100644 abc123 def456 R100 new-name.rs\told-name.rs
";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.staged, vec!["new-name.rs"]);
        assert!(status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_detached_head() {
        let output = "# branch.oid abc123\n# branch.head (detached)\n";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.branch, "(detached)");
        assert!(!status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_empty_output() {
        let status = parse_git_porcelain_v2("");
        assert_eq!(status.branch, "(unknown)");
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
        assert!(!status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_staged_and_modified() {
        // A file that is both staged and has worktree modifications
        let output = "\
# branch.head main
1 MM N... 100644 100644 100644 abc123 def456 both-changed.rs
";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.staged, vec!["both-changed.rs"]);
        assert_eq!(status.modified, vec!["both-changed.rs"]);
        assert!(status.dirty);
    }
}
