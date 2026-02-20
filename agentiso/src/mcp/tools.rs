use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::team::TeamManager;
use crate::workspace::WorkspaceManager;

use crate::config::RateLimitConfig;
use super::auth::AuthManager;
use super::git_tools::{
    GitCloneParams, GitCommitParams, GitDiffParams, GitMergeParams, GitPushParams,
    GitStatusParams, validate_git_url,
};
use super::metrics::MetricsRegistry;
use super::rate_limit::RateLimiter;
use super::team_tools::TeamParams;

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
    /// For commands expected to run longer, use exec_background(action="start") then exec_background(action="poll").
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
    /// Direction of transfer: "upload" (host to guest) or "download" (guest to host)
    direction: String,
    /// UUID or name of the workspace
    workspace_id: String,
    /// Path on the host filesystem
    host_path: String,
    /// Path inside the guest VM
    guest_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotParams {
    /// Action to perform: "create", "restore", "list", or "delete"
    action: String,
    /// Workspace ID (required for all actions)
    workspace_id: String,
    /// Snapshot name (required for create, restore, delete)
    #[serde(default)]
    name: Option<String>,
    /// Include VM memory state in snapshot (optional, for create only)
    #[serde(default)]
    include_memory: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkspaceForkParams {
    /// UUID or name of the source workspace
    workspace_id: String,
    /// Name of the snapshot to fork from
    snapshot_name: String,
    /// Name for the new forked workspace (used when count is omitted or 1)
    new_name: Option<String>,
    /// Number of forks to create (default: 1). When > 1, creates N worker VMs in parallel (max 20).
    #[serde(default)]
    count: Option<u32>,
    /// Prefix for worker names when count > 1 (default: "worker"). Workers are named "{prefix}-1", "{prefix}-2", etc.
    #[serde(default)]
    name_prefix: Option<String>,
    /// Shell commands to run on each forked worker after boot (optional, for batch setup)
    #[serde(default)]
    #[allow(dead_code)]
    setup_commands: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PortForwardParams {
    /// Action: "add" to create a port forward, "remove" to delete one
    action: String,
    /// UUID or name of the workspace
    workspace_id: String,
    /// Port inside the guest VM to forward to
    guest_port: u16,
    /// Host port to listen on (auto-assigned if omitted). Required for "add", ignored for "remove".
    host_port: Option<u16>,
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
    /// Action to perform: "start", "poll", or "kill"
    action: String,
    /// UUID or name of the workspace
    workspace_id: String,
    /// Shell command to run (required for action="start")
    #[serde(default)]
    command: Option<String>,
    /// Working directory (default: /root, used with action="start")
    #[serde(default)]
    workdir: Option<String>,
    /// Environment variables (used with action="start")
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    /// Job ID (required for action="poll" and action="kill")
    #[serde(default)]
    job_id: Option<u32>,
    /// Signal to send (used with action="kill", default: 9 = SIGKILL). Common values: 2=SIGINT, 9=SIGKILL, 15=SIGTERM.
    #[serde(default)]
    signal: Option<i32>,
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
    /// UUID or name of the workspace to adopt. If omitted, adopts all orphaned workspaces.
    #[serde(default)]
    workspace_id: Option<String>,
    /// Force adoption even if another session owns the workspace. Use this when the
    /// previous session crashed or disconnected, leaving the workspace stuck.
    #[serde(default)]
    force: bool,
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

// ---------------------------------------------------------------------------
// Swarm tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
enum ExecParallelCommands {
    /// A single command string, broadcast to all workspaces
    Single(String),
    /// An array of commands, one per workspace (must match workspace_ids length)
    Multiple(Vec<String>),
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecParallelParams {
    /// Array of workspace UUIDs or names to execute on (max 20)
    workspace_ids: Vec<String>,
    /// Either a single command (broadcast to all) or an array of commands matching workspace_ids length
    commands: ExecParallelCommands,
    /// Per-command timeout in seconds (default: 300). Set higher for AI coding tasks.
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// Working directory inside each VM
    #[serde(default)]
    workdir: Option<String>,
    /// Environment variables applied to all commands
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    /// Maximum bytes of stdout/stderr per command (default: 262144 = 256 KiB)
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
struct SwarmVaultQuery {
    /// "search" (full-text search across vault notes) or "read" (read a specific note by path)
    #[serde(default = "default_swarm_vault_query_kind")]
    kind: String,
    /// Search query string (for kind="search") or note path (for kind="read")
    query: String,
}

fn default_swarm_vault_query_kind() -> String {
    "search".to_string()
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
struct SwarmTask {
    /// Unique name for this task (used in results and merge tracking)
    name: String,
    /// Shell command to execute in the forked workspace
    command: String,
    /// Working directory inside the VM (default: /root)
    #[serde(default)]
    workdir: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SwarmRunParams {
    /// Name or UUID of the golden workspace to fork from
    golden_workspace: String,
    /// Snapshot name to fork from (must exist on the golden workspace)
    snapshot_name: String,
    /// Array of task definitions (max 20). Each task runs in its own forked workspace.
    tasks: Vec<SwarmTask>,
    /// Environment variables to inject into all workers before execution (e.g. API keys)
    #[serde(default)]
    env_vars: Option<HashMap<String, String>>,
    /// Merge strategy: "sequential", "branch-per-source", or "cherry-pick". If omitted, no merge is performed.
    #[serde(default)]
    merge_strategy: Option<String>,
    /// Workspace to merge results into (required if merge_strategy is set)
    #[serde(default)]
    merge_target: Option<String>,
    /// Maximum concurrent workers during fork and execution phases (default: 4, max: 20)
    #[serde(default)]
    max_parallel: Option<u32>,
    /// Per-task execution timeout in seconds (default: 600)
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// Whether to destroy worker VMs after execution (default: true). Set to false to keep workers alive for debugging.
    #[serde(default)]
    cleanup: Option<bool>,
    /// Enable internet access on forked workers (default: inherits from golden workspace).
    /// OpenCode tasks require internet access to reach the Anthropic API.
    /// If the golden workspace was created with the server default (allow_internet=false),
    /// you MUST set this to true for OpenCode tasks to work.
    #[serde(default)]
    allow_internet: Option<bool>,
    /// Vault queries to resolve and inject into all workers as context before execution.
    /// Each entry has `kind` ("search" or "read") and `query` (search term or note path).
    /// Resolved context is written to /tmp/.vault-context in each worker VM and the
    /// VAULT_CONTEXT env var is set to point to it.
    #[serde(default)]
    vault_context: Vec<SwarmVaultQuery>,
    /// Shared context written to /tmp/.shared-context in every worker VM before execution.
    /// Use this for project docs, guidelines, or any text all workers need.
    /// Avoids repeating the same context in each task's command.
    #[serde(default)]
    shared_context: Option<String>,
}

// ---------------------------------------------------------------------------
// Vault parameter struct (consolidated)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct VaultParams {
    /// Action: "read", "write", "search", "list", "delete", "frontmatter", "tags", "replace", "move", "batch_read", "stats"
    action: String,
    /// Note path (for read, write, delete, frontmatter, tags, replace, move)
    #[serde(default)]
    path: Option<String>,
    /// Note content (for write)
    #[serde(default)]
    content: Option<String>,
    /// Write mode: "overwrite", "append", "prepend" (for write, default: overwrite)
    #[serde(default)]
    mode: Option<String>,
    /// Search query string (for search)
    #[serde(default)]
    query: Option<String>,
    /// Use regex for search/replace (for search, replace)
    #[serde(default)]
    regex: Option<bool>,
    /// Tag filter for search (for search)
    #[serde(default)]
    tag: Option<String>,
    /// Max results (for search)
    #[serde(default)]
    max_results: Option<usize>,
    /// Path prefix filter (for search, list)
    #[serde(default)]
    path_prefix: Option<String>,
    /// Recursive listing (for list)
    #[serde(default)]
    recursive: Option<bool>,
    /// Confirm deletion (for delete)
    #[serde(default)]
    confirm: Option<bool>,
    /// Frontmatter key (for frontmatter)
    #[serde(default)]
    key: Option<String>,
    /// Frontmatter value (for frontmatter set)
    #[serde(default)]
    value: Option<serde_json::Value>,
    /// Frontmatter sub-action: "get", "set", "delete" (for frontmatter)
    #[serde(default)]
    frontmatter_action: Option<String>,
    /// Tags sub-action: "list", "add", "remove" (for tags)
    #[serde(default)]
    tags_action: Option<String>,
    /// Old string for replace (for replace)
    #[serde(default)]
    old_string: Option<String>,
    /// New string for replace (for replace)
    #[serde(default)]
    new_string: Option<String>,
    /// New path for move (for move)
    #[serde(default)]
    new_path: Option<String>,
    /// Allow overwrite on move (for move)
    #[serde(default)]
    overwrite: Option<bool>,
    /// Paths for batch read (for batch_read)
    #[serde(default)]
    paths: Option<Vec<String>>,
    /// Include content in batch read (for batch_read)
    #[serde(default)]
    include_content: Option<bool>,
    /// Include frontmatter in batch read (for batch_read)
    #[serde(default)]
    include_frontmatter: Option<bool>,
    /// Recent count for stats (for stats)
    #[serde(default)]
    recent_count: Option<usize>,
    /// Response format: "markdown", "json" (for read)
    #[serde(default)]
    format: Option<String>,
}

// ---------------------------------------------------------------------------
// PrepareNameGuard — RAII guard for workspace_prepare name reservation
// ---------------------------------------------------------------------------

/// RAII guard that removes a name from the prepare_locks set on drop.
/// Used by `workspace_prepare` to prevent concurrent calls with the same name
/// from both creating a workspace.
struct PrepareNameGuard {
    name: String,
    locks: Arc<tokio::sync::Mutex<HashSet<String>>>,
}

impl Drop for PrepareNameGuard {
    fn drop(&mut self) {
        // Use try_lock to avoid blocking in the destructor.
        // If the lock is contended, spawn a task to clean up.
        if let Ok(mut set) = self.locks.try_lock() {
            set.remove(&self.name);
        } else {
            let name = self.name.clone();
            let locks = self.locks.clone();
            tokio::spawn(async move {
                locks.lock().await.remove(&name);
            });
        }
    }
}

// ---------------------------------------------------------------------------
// MCP server struct
// ---------------------------------------------------------------------------

/// The agentiso MCP server. Holds shared state and routes tool calls.
#[derive(Clone)]
pub struct AgentisoServer {
    pub(crate) workspace_manager: Arc<WorkspaceManager>,
    auth: AuthManager,
    session_id: String,
    /// Allowed directory for host-side file transfers. All host_path values
    /// in file_transfer must resolve within this directory.
    transfer_dir: PathBuf,
    metrics: Option<MetricsRegistry>,
    /// Vault manager for Obsidian-style markdown knowledge base tools.
    /// None when vault is disabled in config.
    vault_manager: Option<Arc<super::vault::VaultManager>>,
    /// Team manager for multi-agent team lifecycle operations.
    /// None when team support is not configured.
    pub(crate) team_manager: Option<Arc<TeamManager>>,
    /// Message relay for inter-agent team communication.
    pub(crate) message_relay: Arc<crate::team::MessageRelay>,
    /// Rate limiter for tool call categories (create, exec, default, team_message).
    pub(crate) rate_limiter: Arc<RateLimiter>,
    /// Names currently being reserved by workspace_prepare to prevent duplicate creation.
    prepare_locks: Arc<tokio::sync::Mutex<HashSet<String>>>,
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
        rate_limit_config: RateLimitConfig,
        team_manager: Option<Arc<TeamManager>>,
        message_relay: Arc<crate::team::MessageRelay>,
    ) -> Self {
        Self {
            workspace_manager,
            auth,
            session_id,
            transfer_dir,
            metrics,
            vault_manager,
            team_manager,
            message_relay,
            rate_limiter: Arc::new(RateLimiter::new(&rate_limit_config)),
            prepare_locks: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
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
        self.touch_activity().await;
        self.check_rate_limit(super::rate_limit::CATEGORY_CREATE)?;
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

        let result = self
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

        let workspace = result.workspace;
        let from_pool = result.from_pool;
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
            serde_json::to_string(&info).unwrap(),
        )]))
    }

    /// Stop and permanently destroy a workspace VM and all its storage.
    #[tool]
    async fn workspace_destroy(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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
    /// to claim them (omit workspace_id to adopt all).
    #[tool]
    async fn workspace_list(
        &self,
        Parameters(params): Parameters<WorkspaceListParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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
            serde_json::to_string(&workspaces).unwrap(),
        )]))
    }

    /// Get detailed information about a workspace including snapshots and network config.
    #[tool]
    async fn workspace_info(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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
            serde_json::to_string(&info).unwrap(),
        )]))
    }

    /// Gracefully stop a running workspace VM. The workspace can be started again later.
    #[tool]
    async fn workspace_stop(
        &self,
        Parameters(params): Parameters<WorkspaceIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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
        self.touch_activity().await;
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
        self.touch_activity().await;
        self.check_rate_limit(super::rate_limit::CATEGORY_EXEC)?;
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
                             to start the command, then exec_background(action=\"poll\") to check progress. \
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
            serde_json::to_string(&output).unwrap(),
        )]))
    }

    /// Write a file inside a running workspace VM.
    #[tool]
    async fn file_write(
        &self,
        Parameters(params): Parameters<FileWriteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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
        self.touch_activity().await;
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
                    "File contains binary data. Use file_transfer(direction=\"download\") for binary files, or use offset/limit to read a text portion.".to_string(),
                    None,
                ));
            }
        };

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Transfer a file between the host filesystem and a running workspace VM.
    /// Use direction="upload" to copy from host to guest, or direction="download" to copy from guest to host.
    /// The host_path must be within the configured transfer directory.
    #[tool]
    async fn file_transfer(
        &self,
        Parameters(params): Parameters<FileTransferParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "file_transfer", direction = %params.direction, host_path = %params.host_path, guest_path = %params.guest_path, "tool call");
        self.check_ownership(ws_id).await?;

        match params.direction.as_str() {
            "upload" => {
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
            "download" => {
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
            _ => Err(McpError::invalid_params(
                format!(
                    "Unknown file_transfer direction '{}'. Valid directions: upload, download.",
                    params.direction
                ),
                None,
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Snapshot Tool (consolidated)
    // -----------------------------------------------------------------------

    /// Manage workspace snapshots — create checkpoints, restore to previous state, list snapshots,
    /// or delete. Actions: create (save checkpoint), restore (rollback, DESTRUCTIVE: removes
    /// newer snapshots), list (show all snapshots with sizes), delete (remove snapshot).
    #[tool]
    async fn snapshot(
        &self,
        Parameters(params): Parameters<SnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        // Helper: extract and validate snapshot name for actions that require it.
        let require_name = |action: &str, name: &Option<String>| -> Result<String, McpError> {
            name.clone().ok_or_else(|| {
                McpError::invalid_params(
                    format!(
                        "The '{}' action requires a 'name' parameter with the snapshot name.",
                        action
                    ),
                    None,
                )
            })
        };

        match params.action.as_str() {
            "create" => {
                let name = require_name(&params.action, &params.name)?;
                let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
                info!(workspace_id = %ws_id, tool = "snapshot", action = "create", name = %name, "tool call");
                self.check_ownership(ws_id).await?;
                validate_snapshot_name(&name)?;

                let snapshot = self
                    .workspace_manager
                    .snapshot_create(ws_id, &name, params.include_memory.unwrap_or(false))
                    .await
                    .map_err(|e| {
                        McpError::internal_error(
                            format!(
                                "Failed to create snapshot '{}' on workspace '{}': {:#}. This may indicate \
                                 the storage pool is full or the workspace is in a bad state. Use \
                                 snapshot(action=\"list\") to check workspace status and disk usage.",
                                name, params.workspace_id, e
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
                    serde_json::to_string(&info).unwrap(),
                )]))
            }
            "restore" => {
                let name = require_name(&params.action, &params.name)?;
                let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
                info!(workspace_id = %ws_id, tool = "snapshot", action = "restore", name = %name, "tool call");
                self.check_ownership(ws_id).await?;
                validate_snapshot_name(&name)?;

                // Count snapshots before restore so we can report how many were removed.
                let snap_count_before = self
                    .workspace_manager
                    .get(ws_id)
                    .await
                    .map(|ws| ws.snapshots.list().len())
                    .unwrap_or(0);

                self.workspace_manager
                    .snapshot_restore(ws_id, &name)
                    .await
                    .map_err(|e| {
                        McpError::invalid_request(
                            format!(
                                "Failed to restore snapshot '{}' on workspace '{}': {:#}. Use \
                                 snapshot(action=\"list\") to verify the snapshot exists. Note: restore \
                                 is destructive and removes all snapshots newer than the target.",
                                name, params.workspace_id, e
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
                        params.workspace_id, name, removed
                    )
                } else {
                    format!(
                        "Workspace {} restored to snapshot '{}'.",
                        params.workspace_id, name
                    )
                };

                Ok(CallToolResult::success(vec![Content::text(message)]))
            }
            "list" => {
                let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
                info!(workspace_id = %ws_id, tool = "snapshot", action = "list", "tool call");
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
                    serde_json::to_string(&snapshots).unwrap(),
                )]))
            }
            "delete" => {
                let name = require_name(&params.action, &params.name)?;
                let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
                info!(workspace_id = %ws_id, tool = "snapshot", action = "delete", name = %name, "tool call");
                self.check_ownership(ws_id).await?;
                validate_snapshot_name(&name)?;

                self.workspace_manager
                    .snapshot_delete(ws_id, &name)
                    .await
                    .map_err(|e| {
                        McpError::invalid_request(
                            format!(
                                "Failed to delete snapshot '{}' from workspace '{}': {:#}. Use \
                                 snapshot(action=\"list\") to verify the snapshot exists.",
                                name, params.workspace_id, e
                            ),
                            None,
                        )
                    })?;

                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Snapshot '{}' deleted from workspace {}.",
                    name, params.workspace_id
                ))]))
            }
            _ => Err(McpError::invalid_params(
                format!(
                    "Unknown snapshot action '{}'. Valid actions: create, restore, list, delete.",
                    params.action
                ),
                None,
            )),
        }
    }

    /// Fork (clone) a new workspace from an existing workspace's snapshot. Creates an independent copy.
    /// When count > 1, creates N worker VMs in parallel from the same snapshot (batch fork).
    #[tool]
    async fn workspace_fork(
        &self,
        Parameters(params): Parameters<WorkspaceForkParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        self.check_rate_limit(super::rate_limit::CATEGORY_CREATE)?;
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        let count = params.count.unwrap_or(1);

        info!(workspace_id = %ws_id, tool = "workspace_fork", snapshot_name = %params.snapshot_name, new_name = ?params.new_name, count, "tool call");
        self.check_ownership(ws_id).await?;
        validate_snapshot_name(&params.snapshot_name)?;

        // Check quota for the new workspace(s) (use same resources as source).
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

        if count <= 1 {
            // Single fork path
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
                             snapshot(action=\"list\") to verify the snapshot exists and check that the \
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
                serde_json::to_string(&info).unwrap(),
            )]))
        } else {
            // Batch fork path (count > 1)
            let prefix = params.name_prefix.as_deref().unwrap_or("worker");
            let snapshot_name = &params.snapshot_name;

            // Validate count range
            if count > 20 {
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

            // Pre-check: verify source workspace has the snapshot
            source_ws.snapshots.get_by_name(snapshot_name).ok_or_else(|| {
                McpError::invalid_request(
                    format!(
                        "snapshot '{}' not found on workspace '{}'. Use snapshot(action=\"list\") to see available snapshots.",
                        snapshot_name, params.workspace_id
                    ),
                    None,
                )
            })?;

            // Pre-check quota for all forks
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
                serde_json::to_string(&info).unwrap(),
            )]))
        }
    }

    // -----------------------------------------------------------------------
    // Network Tools
    // -----------------------------------------------------------------------

    /// Manage port forwarding for a workspace VM. Use action="add" to forward a host port to a guest port
    /// (returns the assigned host port), or action="remove" to delete a forwarding rule.
    #[tool]
    async fn port_forward(
        &self,
        Parameters(params): Parameters<PortForwardParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "port_forward", action = %params.action, guest_port = params.guest_port, host_port = ?params.host_port, "tool call");
        self.check_ownership(ws_id).await?;

        match params.action.as_str() {
            "add" => {
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
                    serde_json::to_string(&info).unwrap(),
                )]))
            }
            "remove" => {
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
            _ => Err(McpError::invalid_params(
                format!(
                    "Unknown port_forward action '{}'. Valid actions: add, remove.",
                    params.action
                ),
                None,
            )),
        }
    }

    /// Set the network isolation policy for a workspace. Controls internet access, inter-VM communication, and allowed ports.
    #[tool]
    async fn network_policy(
        &self,
        Parameters(params): Parameters<NetworkPolicyParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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
            serde_json::to_string(&info).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // New Tools: file_list, file_edit, exec_background
    // -----------------------------------------------------------------------

    /// List files and directories at a given path inside a running workspace VM.
    #[tool]
    async fn file_list(
        &self,
        Parameters(params): Parameters<FileListParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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
            serde_json::to_string(&output).unwrap(),
        )]))
    }

    /// Edit a file inside a running workspace VM by replacing an exact string match.
    /// The old_string must appear in the file; the first occurrence is replaced with new_string.
    #[tool]
    async fn file_edit(
        &self,
        Parameters(params): Parameters<FileEditParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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

    /// Manage background jobs in a workspace VM. Actions: "start" (launch a command),
    /// "poll" (check status and get output), "kill" (send signal to terminate).
    /// Start returns a job_id. Use poll with that job_id to check completion and get output.
    #[tool]
    async fn exec_background(
        &self,
        Parameters(params): Parameters<ExecBackgroundParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        self.check_ownership(ws_id).await?;

        match params.action.as_str() {
            "start" => {
                self.check_rate_limit(super::rate_limit::CATEGORY_EXEC)?;
                let command = params.command.as_deref().ok_or_else(|| {
                    McpError::invalid_request(
                        "The 'start' action requires a 'command' parameter.".to_string(),
                        None,
                    )
                })?;
                info!(workspace_id = %ws_id, tool = "exec_background", action = "start", command = %command, "tool call");

                let job_id = self
                    .workspace_manager
                    .exec_background(
                        ws_id,
                        command,
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
                    serde_json::to_string(&output).unwrap(),
                )]))
            }
            "poll" => {
                let job_id = params.job_id.ok_or_else(|| {
                    McpError::invalid_request(
                        "The 'poll' action requires a 'job_id' parameter.".to_string(),
                        None,
                    )
                })?;
                info!(workspace_id = %ws_id, tool = "exec_background", action = "poll", job_id = job_id, "tool call");

                let status = self
                    .workspace_manager
                    .exec_poll(ws_id, job_id)
                    .await
                    .map_err(|e| {
                        McpError::invalid_request(
                            format!(
                                "Failed to poll job {} in workspace '{}': {:#}. The job_id may be \
                                 invalid or the workspace may have been restarted since the job was started.",
                                job_id, params.workspace_id, e
                            ),
                            None,
                        )
                    })?;

                let max_output_bytes = 262144;
                let stdout = truncate_output(status.stdout, max_output_bytes);
                let stderr = truncate_output(status.stderr, max_output_bytes);

                let output = serde_json::json!({
                    "job_id": job_id,
                    "running": status.running,
                    "exit_code": status.exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&output).unwrap(),
                )]))
            }
            "kill" => {
                let job_id = params.job_id.ok_or_else(|| {
                    McpError::invalid_request(
                        "The 'kill' action requires a 'job_id' parameter.".to_string(),
                        None,
                    )
                })?;
                info!(workspace_id = %ws_id, tool = "exec_background", action = "kill", job_id = job_id, signal = ?params.signal, "tool call");

                self.workspace_manager
                    .exec_kill(ws_id, job_id, params.signal)
                    .await
                    .map_err(|e| {
                        McpError::invalid_request(
                            format!(
                                "Failed to kill job {} in workspace '{}': {:#}. The job may have \
                                 already exited. Use exec_background(action=\"poll\") to check job status.",
                                job_id, params.workspace_id, e
                            ),
                            None,
                        )
                    })?;

                let sig = params.signal.unwrap_or(9);
                let output = serde_json::json!({
                    "job_id": job_id,
                    "signal": sig,
                    "status": "killed",
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&output).unwrap(),
                )]))
            }
            _ => Err(McpError::invalid_request(
                format!(
                    "Unknown exec_background action '{}'. Valid actions: \"start\", \"poll\", \"kill\".",
                    params.action
                ),
                None,
            )),
        }
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
        self.touch_activity().await;
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
            serde_json::to_string(&output).unwrap(),
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
        self.touch_activity().await;
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
            serde_json::to_string(&output).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Adoption Tools
    // -----------------------------------------------------------------------

    /// Adopt orphaned workspace(s) into the current session. Use this after a server restart
    /// to reclaim ownership of workspaces that exist in state but are not owned by any session.
    /// If workspace_id is provided, adopts that single workspace. If omitted, adopts all orphaned workspaces.
    #[tool]
    async fn workspace_adopt(
        &self,
        Parameters(params): Parameters<WorkspaceAdoptParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        if let Some(ref ws_id_str) = params.workspace_id {
            // Single workspace adoption
            let ws_id = self.resolve_workspace_id(ws_id_str).await?;
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
                            ws_id_str, e
                        ),
                        None,
                    )
                })?;

            // Adopt into current session (checks orphan status and quota internally).
            if params.force {
                info!(workspace_id = %ws_id, "force-adopting workspace (may transfer from another session)");
            }
            self.auth
                .adopt_workspace(
                    &self.session_id,
                    &ws_id,
                    ws.resources.memory_mb as u64,
                    ws.resources.disk_gb as u64,
                    params.force,
                )
                .await
                .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

            let info = serde_json::json!({
                "workspace_id": ws.id.to_string(),
                "name": ws.name,
                "state": ws.state,
                "ip": ws.network.ip.to_string(),
                "adopted": true,
                "forced": params.force,
            });

            Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string(&info).unwrap(),
            )]))
        } else {
            // Adopt all orphaned workspaces (or all workspaces if force=true)
            info!(tool = "workspace_adopt", mode = "all", force = params.force, "tool call");

            // Purge ghost sessions from before the restart so their workspaces
            // become orphaned and adoptable by the current session.
            let purged = self.auth.purge_stale_sessions(&self.session_id).await;
            if purged > 0 {
                info!(purged, "purged stale sessions before adopt all");
            }

            let all = self
                .workspace_manager
                .list()
                .await
                .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

            // When force=true, try to adopt ALL workspaces not owned by this session.
            // When force=false, only adopt orphaned workspaces (no owner).
            let candidates: Vec<Uuid> = if params.force {
                let my_workspaces = self.auth.list_workspaces(&self.session_id).await
                    .unwrap_or_default();
                all.iter()
                    .filter(|ws| !my_workspaces.contains(&ws.id))
                    .map(|ws| ws.id)
                    .collect()
            } else {
                let all_ids: Vec<Uuid> = all.iter().map(|ws| ws.id).collect();
                self.auth.list_orphaned_workspaces(&all_ids).await
            };

            let mut adopted = Vec::new();
            let mut errors = Vec::new();

            for ws in &all {
                if !candidates.contains(&ws.id) {
                    continue;
                }

                match self
                    .auth
                    .adopt_workspace(
                        &self.session_id,
                        &ws.id,
                        ws.resources.memory_mb as u64,
                        ws.resources.disk_gb as u64,
                        params.force,
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
                serde_json::to_string(&info).unwrap(),
            )]))
        }
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
        self.touch_activity().await;
        self.handle_git_clone(params).await
    }

    // -----------------------------------------------------------------------
    // Orchestration Tools
    // -----------------------------------------------------------------------

    /// Create a "golden" workspace ready for mass forking. Creates a workspace, optionally
    /// clones a git repo and runs setup commands, then creates a snapshot named "golden".
    ///
    /// **Idempotency**: If a workspace with the given name already exists:
    /// - If it already has a "golden" snapshot, returns success immediately (no-op).
    /// - If it exists without a golden snapshot, adopts and reuses it for
    ///   git clone + setup steps (avoids wasting VM boot time).
    /// - If it exists but cannot be adopted, automatically tries suffixed names
    ///   (`{name}-2`, `{name}-3`, etc.) up to 5 attempts.
    ///
    /// **Timeout limitation**: If the MCP framework times out this call mid-execution
    /// (e.g., during a slow git clone), the workspace may be left in a partial state.
    /// A subsequent call with the same name will detect and reuse it.
    #[tool]
    async fn workspace_prepare(
        &self,
        Parameters(params): Parameters<WorkspacePrepareParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
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

        let mem = 512u32;
        let disk = 10u32;

        // Reserve the name to prevent concurrent prepare calls from creating duplicates.
        {
            let mut locks = self.prepare_locks.lock().await;
            if !locks.insert(params.name.clone()) {
                return Err(McpError::invalid_request(
                    format!(
                        "workspace_prepare for '{}' is already in progress from another call",
                        params.name
                    ),
                    None,
                ));
            }
        }
        // RAII guard releases the name when this function returns (success or error).
        let _guard = PrepareNameGuard {
            name: params.name.clone(),
            locks: self.prepare_locks.clone(),
        };

        // Step 0: Acquire a workspace — reuse existing or create new.
        // Handles idempotent retries after timeouts/crashes and auto-suffix
        // on unrecoverable name collisions.
        let (ws_id, actual_name, reused) =
            self.prepare_acquire_workspace(&params.name, &base_image, mem, disk).await?;

        if reused {
            info!(workspace_id = %ws_id, name = %actual_name, "reusing existing workspace for prepare");

            // Check if this workspace already has a "golden" snapshot.
            // If so, return immediately — fully idempotent.
            if let Ok(ws) = self.workspace_manager.get(ws_id).await {
                if ws.snapshots.get_by_name("golden").is_some() {
                    let info = serde_json::json!({
                        "workspace_id": ws_id.to_string(),
                        "name": actual_name,
                        "snapshot_name": "golden",
                        "ip": ws.network.ip.to_string(),
                        "reused_existing": true,
                    });
                    return Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string(&info).unwrap(),
                    )]));
                }
            }
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
                    // If we reused a workspace, the clone dir may already exist from
                    // a previous partial run. Try removing and re-cloning.
                    if reused && (r.stderr.contains("already exists") || r.stdout.contains("already exists")) {
                        info!(workspace_id = %ws_id, "clone target exists from previous run, removing and retrying");
                        self.workspace_manager
                            .exec(ws_id, "rm -rf /workspace", None, None, Some(60))
                            .await
                            .ok();
                        let retry = self
                            .workspace_manager
                            .exec(ws_id, &cmd, None, None, Some(300))
                            .await;
                        match retry {
                            Ok(r2) if r2.exit_code != 0 => {
                                let err_msg = format!(
                                    "git clone failed on retry (exit {}): {}",
                                    r2.exit_code,
                                    if !r2.stderr.is_empty() { &r2.stderr } else { &r2.stdout }
                                );
                                if !reused {
                                    self.workspace_manager.destroy(ws_id).await.ok();
                                    self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                                }
                                return Err(McpError::internal_error(err_msg, None));
                            }
                            Err(e) => {
                                if !reused {
                                    self.workspace_manager.destroy(ws_id).await.ok();
                                    self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                                }
                                return Err(McpError::internal_error(format!("git clone retry failed: {:#}", e), None));
                            }
                            _ => {} // retry succeeded
                        }
                    } else {
                        let err_msg = format!(
                            "git clone failed (exit {}): {}",
                            r.exit_code,
                            if !r.stderr.is_empty() { &r.stderr } else { &r.stdout }
                        );
                        if !reused {
                            self.workspace_manager.destroy(ws_id).await.ok();
                            self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                        }
                        return Err(McpError::internal_error(err_msg, None));
                    }
                }
                Err(e) => {
                    if !reused {
                        self.workspace_manager.destroy(ws_id).await.ok();
                        self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                    }
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
                        if !reused {
                            self.workspace_manager.destroy(ws_id).await.ok();
                            self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                        }
                        return Err(McpError::internal_error(err_msg, None));
                    }
                    Err(e) => {
                        if !reused {
                            self.workspace_manager.destroy(ws_id).await.ok();
                            self.auth.unregister_workspace(&self.session_id, ws_id, mem as u64, disk as u64).await.ok();
                        }
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
                // Best-effort cleanup on snapshot failure (only for freshly created workspaces)
                if !reused {
                    let mgr = self.workspace_manager.clone();
                    let sid = self.session_id.clone();
                    let auth = self.auth.clone();
                    tokio::spawn(async move {
                        mgr.destroy(ws_id).await.ok();
                        auth.unregister_workspace(&sid, ws_id, mem as u64, disk as u64).await.ok();
                    });
                }
                McpError::internal_error(format!("snapshot creation failed: {:#}", e), None)
            })?;

        let ip = self
            .workspace_manager
            .get(ws_id)
            .await
            .map(|ws| ws.network.ip.to_string())
            .unwrap_or_default();

        let info = serde_json::json!({
            "workspace_id": ws_id.to_string(),
            "name": actual_name,
            "snapshot_name": "golden",
            "snapshot_id": snapshot.id.to_string(),
            "ip": ip,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&info).unwrap(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Git & Diff Tools (implementations in git_tools.rs)
    // -----------------------------------------------------------------------

    /// Get structured git status for a repository inside a running workspace VM.
    /// Returns branch info, ahead/behind counts, and categorized file changes.
    #[tool]
    async fn git_status(
        &self,
        Parameters(params): Parameters<GitStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        self.handle_git_status(params).await
    }

    /// Commit changes in a git repository inside a running workspace VM.
    /// Optionally stages all changes first (default: true). Returns the commit SHA and summary.
    #[tool]
    async fn git_commit(
        &self,
        Parameters(params): Parameters<GitCommitParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        self.handle_git_commit(params).await
    }

    /// Push commits to a remote git repository from a running workspace VM.
    /// Returns push result including remote, branch, and any output messages.
    #[tool]
    async fn git_push(
        &self,
        Parameters(params): Parameters<GitPushParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        self.handle_git_push(params).await
    }

    /// Show git diff for a repository inside a running workspace VM.
    /// Returns the diff output, optionally with stat summary and truncation control.
    #[tool]
    async fn git_diff(
        &self,
        Parameters(params): Parameters<GitDiffParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        self.handle_git_diff(params).await
    }

    /// Merge changes from one or more source workspaces into a target workspace via git.
    /// Strategies: "sequential" (format-patch/am in order), "branch-per-source" (one branch per source, then merge), "cherry-pick" (apply individual commits).
    /// Returns merged sources, conflict details, and commit counts.
    #[tool]
    async fn workspace_merge(
        &self,
        Parameters(params): Parameters<GitMergeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        self.handle_workspace_merge(params).await
    }

    // -----------------------------------------------------------------------
    // Vault Tool (consolidated)
    // -----------------------------------------------------------------------

    /// Markdown knowledge vault for persistent notes across workspaces. Actions: read (get note content), write (create/update note), search (find notes by query/regex/tag), list (browse notes/dirs), delete (remove note), frontmatter (get/set/delete YAML metadata), tags (list/add/remove tags), replace (search-replace in notes), move (rename/relocate note), batch_read (read multiple notes), stats (vault statistics).
    #[tool]
    async fn vault(
        &self,
        Parameters(params): Parameters<VaultParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        info!(tool = "vault", action = %params.action, "tool call");

        let vm = self.vault_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request("Vault not configured. Enable [vault] in config.toml.".to_string(), None)
        })?;

        match params.action.as_str() {
            "read" => {
                let path = params.path.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'path' is required for action 'read'".to_string(), None)
                })?;

                let note = vm.read_note(path).await.map_err(|e| {
                    McpError::internal_error(
                        format!(
                            "Failed to read vault note '{}': {:#}. Use vault(action=\"list\") to browse \
                             available notes, or vault(action=\"search\") to find notes by content.",
                            path, e
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
                        serde_json::to_string(&json).unwrap()
                    }
                    _ => note.content,
                };

                Ok(CallToolResult::success(vec![Content::text(output)]))
            }

            "write" => {
                let path = params.path.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'path' is required for action 'write'".to_string(), None)
                })?;
                let content = params.content.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'content' is required for action 'write'".to_string(), None)
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

                vm.write_note(path, content, mode)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Note '{}' written ({}).",
                    path,
                    params.mode.as_deref().unwrap_or("overwrite")
                ))]))
            }

            "search" => {
                let query = params.query.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'query' is required for action 'search'".to_string(), None)
                })?;

                let max = params.max_results.unwrap_or(20);
                let is_regex = params.regex.unwrap_or(false);

                let results = vm
                    .search(
                        query,
                        is_regex,
                        params.path_prefix.as_deref(),
                        params.tag.as_deref(),
                        max,
                    )
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                let info = serde_json::json!({
                    "query": query,
                    "result_count": results.len(),
                    "results": results,
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&info).unwrap(),
                )]))
            }

            "list" => {
                let list_path = params.path.as_deref().or(params.path_prefix.as_deref());
                let recursive = params.recursive.unwrap_or(false);
                let entries = vm
                    .list_notes(list_path, recursive)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                let info = serde_json::json!({
                    "path": list_path.unwrap_or("/"),
                    "recursive": recursive,
                    "count": entries.len(),
                    "entries": entries,
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&info).unwrap(),
                )]))
            }

            "delete" => {
                let path = params.path.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'path' is required for action 'delete'".to_string(), None)
                })?;

                if !params.confirm.unwrap_or(false) {
                    return Err(McpError::invalid_params(
                        "confirm must be true to delete a note".to_string(),
                        None,
                    ));
                }

                vm.delete_note(path)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Note '{}' deleted.",
                    path
                ))]))
            }

            "frontmatter" => {
                let path = params.path.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'path' is required for action 'frontmatter'".to_string(), None)
                })?;
                let fm_action = params.frontmatter_action.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'frontmatter_action' is required for action 'frontmatter' (one of: 'get', 'set', 'delete')".to_string(),
                        None,
                    )
                })?;

                match fm_action {
                    "get" => {
                        let fm = vm.get_frontmatter(path).await.map_err(|e| {
                            McpError::internal_error(format!("{:#}", e), None)
                        })?;

                        let info = serde_json::json!({
                            "path": path,
                            "frontmatter": fm,
                        });
                        Ok(CallToolResult::success(vec![Content::text(
                            serde_json::to_string(&info).unwrap(),
                        )]))
                    }
                    "set" => {
                        let key = params.key.as_deref().ok_or_else(|| {
                            McpError::invalid_params("'key' is required for frontmatter_action 'set'".to_string(), None)
                        })?;
                        let json_value = params.value.ok_or_else(|| {
                            McpError::invalid_params("'value' is required for frontmatter_action 'set'".to_string(), None)
                        })?;

                        let yaml_value: serde_yaml::Value =
                            serde_json::from_value(serde_json::to_value(&json_value).unwrap())
                                .map_err(|e| McpError::invalid_params(format!("invalid value: {}", e), None))?;

                        vm.set_frontmatter(path, key, yaml_value)
                            .await
                            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                        Ok(CallToolResult::success(vec![Content::text(format!(
                            "Frontmatter key '{}' set on '{}'.",
                            key, path
                        ))]))
                    }
                    "delete" => {
                        let key = params.key.as_deref().ok_or_else(|| {
                            McpError::invalid_params("'key' is required for frontmatter_action 'delete'".to_string(), None)
                        })?;

                        vm.delete_frontmatter(path, key)
                            .await
                            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                        Ok(CallToolResult::success(vec![Content::text(format!(
                            "Frontmatter key '{}' deleted from '{}'.",
                            key, path
                        ))]))
                    }
                    other => Err(McpError::invalid_params(
                        format!("invalid frontmatter_action '{}': expected 'get', 'set', or 'delete'", other),
                        None,
                    )),
                }
            }

            "tags" => {
                let path = params.path.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'path' is required for action 'tags'".to_string(), None)
                })?;
                let tags_action = params.tags_action.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'tags_action' is required for action 'tags' (one of: 'list', 'add', 'remove')".to_string(),
                        None,
                    )
                })?;

                match tags_action {
                    "list" => {
                        let tags = vm.get_tags(path).await.map_err(|e| {
                            McpError::internal_error(format!("{:#}", e), None)
                        })?;

                        let info = serde_json::json!({
                            "path": path,
                            "tags": tags,
                        });
                        Ok(CallToolResult::success(vec![Content::text(
                            serde_json::to_string(&info).unwrap(),
                        )]))
                    }
                    "add" => {
                        let tag = params.tag.as_deref().ok_or_else(|| {
                            McpError::invalid_params("'tag' is required for tags_action 'add'".to_string(), None)
                        })?;

                        vm.add_tag(path, tag)
                            .await
                            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                        Ok(CallToolResult::success(vec![Content::text(format!(
                            "Tag '{}' added to '{}'.",
                            tag, path
                        ))]))
                    }
                    "remove" => {
                        let tag = params.tag.as_deref().ok_or_else(|| {
                            McpError::invalid_params("'tag' is required for tags_action 'remove'".to_string(), None)
                        })?;

                        vm.remove_tag(path, tag)
                            .await
                            .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                        Ok(CallToolResult::success(vec![Content::text(format!(
                            "Tag '{}' removed from '{}'.",
                            tag, path
                        ))]))
                    }
                    other => Err(McpError::invalid_params(
                        format!("invalid tags_action '{}': expected 'list', 'add', or 'remove'", other),
                        None,
                    )),
                }
            }

            "replace" => {
                let path = params.path.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'path' is required for action 'replace'".to_string(), None)
                })?;
                let old_string = params.old_string.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'old_string' is required for action 'replace'".to_string(), None)
                })?;
                let new_string = params.new_string.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'new_string' is required for action 'replace'".to_string(), None)
                })?;

                let is_regex = params.regex.unwrap_or(false);
                let count = vm
                    .search_replace(path, old_string, new_string, is_regex)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                let info = serde_json::json!({
                    "path": path,
                    "replacements": count,
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&info).unwrap(),
                )]))
            }

            "move" => {
                let path = params.path.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'path' is required for action 'move'".to_string(), None)
                })?;
                let new_path = params.new_path.as_deref().ok_or_else(|| {
                    McpError::invalid_params("'new_path' is required for action 'move'".to_string(), None)
                })?;

                let overwrite = params.overwrite.unwrap_or(false);
                let (from, to) = vm
                    .move_note(path, new_path, overwrite)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                let info = serde_json::json!({
                    "moved": true,
                    "from": from,
                    "to": to,
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&info).unwrap(),
                )]))
            }

            "batch_read" => {
                let paths = params.paths.as_ref().ok_or_else(|| {
                    McpError::invalid_params("'paths' is required for action 'batch_read'".to_string(), None)
                })?;

                let include_content = params.include_content.unwrap_or(true);
                let include_frontmatter = params.include_frontmatter.unwrap_or(true);

                let results = vm
                    .batch_read(paths, include_content, include_frontmatter)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&results).unwrap(),
                )]))
            }

            "stats" => {
                let recent_count = params.recent_count.unwrap_or(10);
                let stats = vm
                    .stats(recent_count)
                    .await
                    .map_err(|e| McpError::internal_error(format!("{:#}", e), None))?;

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&stats).unwrap(),
                )]))
            }

            other => Err(McpError::invalid_params(
                format!(
                    "invalid vault action '{}': expected one of 'read', 'write', 'search', 'list', \
                     'delete', 'frontmatter', 'tags', 'replace', 'move', 'batch_read', 'stats'",
                    other
                ),
                None,
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Team Tool (implementation in team_tools.rs)
    // -----------------------------------------------------------------------

    /// Manage multi-agent teams of isolated workspace VMs. Actions: create (provision a team with named roles, each getting its own VM), destroy (tear down all team member VMs and clean up), status (get team state and member details), list (show all teams), message (send a message from one agent to another or broadcast with to="*"), receive (retrieve pending messages for an agent).
    #[tool]
    async fn team(
        &self,
        Parameters(params): Parameters<TeamParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        self.handle_team(params).await
    }

    // -----------------------------------------------------------------------
    // Swarm Tools
    // -----------------------------------------------------------------------

    /// Execute commands across multiple workspaces concurrently in a single call.
    /// Pass an array of workspace IDs and commands. All commands run in parallel; results are collected and returned together.
    /// This eliminates the need for exec_background + polling loops when running the same or different commands across multiple VMs.
    #[tool]
    async fn exec_parallel(
        &self,
        Parameters(params): Parameters<ExecParallelParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        let start = std::time::Instant::now();

        // Validate workspace count
        if params.workspace_ids.is_empty() {
            return Err(McpError::invalid_params("workspace_ids must not be empty", None));
        }
        if params.workspace_ids.len() > 20 {
            return Err(McpError::invalid_params("workspace_ids max length is 20", None));
        }

        // Resolve commands: single string broadcasts to all, array must match length
        let commands: Vec<String> = match &params.commands {
            ExecParallelCommands::Single(cmd) => {
                vec![cmd.clone(); params.workspace_ids.len()]
            }
            ExecParallelCommands::Multiple(cmds) => {
                if cmds.len() != params.workspace_ids.len() {
                    return Err(McpError::invalid_params(
                        format!(
                            "commands array length ({}) must match workspace_ids length ({})",
                            cmds.len(),
                            params.workspace_ids.len()
                        ),
                        None,
                    ));
                }
                cmds.clone()
            }
        };

        // Resolve all workspace IDs upfront (fail fast)
        let mut resolved = Vec::with_capacity(params.workspace_ids.len());
        for ws_ref in &params.workspace_ids {
            let ws_id = self.resolve_workspace_id(ws_ref).await?;
            self.check_ownership(ws_id).await?;
            // Get name for response
            let name = self.workspace_manager.get(ws_id).await
                .map(|ws| ws.name.clone())
                .unwrap_or_else(|_| ws_ref.clone());
            resolved.push((ws_id, name));
        }

        // Rate limit: charge once per workspace
        for _ in 0..resolved.len() {
            self.check_rate_limit(super::rate_limit::CATEGORY_EXEC)?;
        }

        let timeout = params.timeout_secs.unwrap_or(300);
        let workdir = params.workdir.clone();
        let env = params.env.clone();
        let max_output = params.max_output_bytes.unwrap_or(262144);

        // Spawn concurrent exec tasks
        let mut join_set = tokio::task::JoinSet::new();
        for (i, ((ws_id, ws_name), command)) in resolved.into_iter().zip(commands.into_iter()).enumerate() {
            let wm = self.workspace_manager.clone();
            let wd = workdir.clone();
            let ev = env.clone();
            join_set.spawn(async move {
                let result = wm.exec(
                    ws_id,
                    &command,
                    wd.as_deref(),
                    ev.as_ref(),
                    Some(timeout),
                ).await;
                (i, ws_id, ws_name, result)
            });
        }

        // Collect results in order
        let mut results: Vec<Option<serde_json::Value>> = vec![None; params.workspace_ids.len()];
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        let mut timed_out = 0u32;

        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok((i, ws_id, ws_name, Ok(exec_result))) => {
                    let is_timeout = false; // exec returns error on timeout
                    if exec_result.exit_code == 0 {
                        succeeded += 1;
                    } else {
                        failed += 1;
                    }
                    results[i] = Some(serde_json::json!({
                        "workspace_id": ws_id.to_string(),
                        "workspace_name": ws_name,
                        "exit_code": exec_result.exit_code,
                        "stdout": truncate_output(exec_result.stdout, max_output),
                        "stderr": truncate_output(exec_result.stderr, max_output),
                        "timed_out": is_timeout,
                    }));
                }
                Ok((i, ws_id, ws_name, Err(e))) => {
                    let msg = format!("{:#}", e);
                    let is_timeout = msg.contains("timed out") || msg.contains("Timeout");
                    if is_timeout {
                        timed_out += 1;
                    } else {
                        failed += 1;
                    }
                    results[i] = Some(serde_json::json!({
                        "workspace_id": ws_id.to_string(),
                        "workspace_name": ws_name,
                        "exit_code": -1,
                        "stdout": "",
                        "stderr": msg,
                        "timed_out": is_timeout,
                    }));
                }
                Err(e) => {
                    // JoinSet task panicked — shouldn't happen
                    failed += 1;
                    tracing::error!(error = %e, "exec_parallel task panicked");
                }
            }
        }

        let elapsed = start.elapsed().as_millis() as u64;
        let total = params.workspace_ids.len() as u32;
        let results_vec: Vec<serde_json::Value> = results.into_iter().flatten().collect();

        let output = serde_json::json!({
            "results": results_vec,
            "summary": {
                "total": total,
                "succeeded": succeeded,
                "failed": failed,
                "timed_out": timed_out,
                "elapsed_ms": elapsed,
            }
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap(),
        )]))
    }

    /// Run a parallel coding swarm: fork workers from a snapshot, inject environment variables, execute commands concurrently, optionally merge results via git, and clean up.
    /// This is the high-level orchestration tool that replaces manual workspace_fork + set_env + exec + workspace_merge + workspace_destroy sequences.
    #[tool]
    async fn swarm_run(
        &self,
        Parameters(params): Parameters<SwarmRunParams>,
    ) -> Result<CallToolResult, McpError> {
        self.touch_activity().await;
        // Rate-limit swarm_run as a single create operation (not per-fork).
        self.check_rate_limit(super::rate_limit::CATEGORY_CREATE)?;
        let start = std::time::Instant::now();
        let swarm_id = format!("swarm-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));

        // Validate
        if params.tasks.is_empty() {
            return Err(McpError::invalid_params("tasks must not be empty", None));
        }
        if params.tasks.len() > 20 {
            return Err(McpError::invalid_params("tasks max length is 20", None));
        }
        // Check unique task names
        let mut seen_names = std::collections::HashSet::new();
        for task in &params.tasks {
            if !seen_names.insert(&task.name) {
                return Err(McpError::invalid_params(
                    format!("duplicate task name: '{}'", task.name),
                    None,
                ));
            }
        }

        // Limit shared_context to 1 MiB (same limit as vault writes).
        if let Some(ref ctx) = params.shared_context {
            const MAX_SHARED_CONTEXT: usize = 1024 * 1024; // 1 MiB
            if ctx.len() > MAX_SHARED_CONTEXT {
                return Err(McpError::invalid_params(
                    format!(
                        "shared_context too large: {} bytes (max {} bytes)",
                        ctx.len(),
                        MAX_SHARED_CONTEXT
                    ),
                    None,
                ));
            }
        }

        // Note: swarm_run's internal fork operations bypass the MCP-level rate
        // limiter. The swarm_run tool call itself is already rate-limited at
        // the MCP dispatch level. Charging per-fork would double-count against
        // the create bucket and cause spurious "rate limit exceeded" failures
        // when the test (or user) has already consumed create tokens earlier.

        // Resolve golden workspace
        let golden_id = self.resolve_workspace_id(&params.golden_workspace).await?;
        self.check_ownership(golden_id).await?;

        info!(
            swarm_id = %swarm_id,
            golden = %params.golden_workspace,
            snapshot = %params.snapshot_name,
            task_count = params.tasks.len(),
            "swarm_run starting"
        );

        // Fork workers
        let max_parallel = params.max_parallel.unwrap_or(4).min(20) as usize;
        let fork_semaphore = Arc::new(tokio::sync::Semaphore::new(max_parallel));
        let mut fork_set = tokio::task::JoinSet::new();

        for (i, task) in params.tasks.iter().enumerate() {
            let wm = self.workspace_manager.clone();
            let golden = golden_id;
            let snap = params.snapshot_name.clone();
            let name = format!("{}-{}", swarm_id, task.name);
            let sem = fork_semaphore.clone();
            fork_set.spawn(async move {
                let _permit = sem.acquire().await;
                let result = wm.fork(golden, &snap, Some(name)).await;
                (i, result)
            });
        }

        // Collect forked workspace IDs (id, name, memory_mb, disk_gb)
        let mut workers: Vec<Option<(uuid::Uuid, String, u64, u64)>> = vec![None; params.tasks.len()];
        let mut fork_failures = Vec::new();
        while let Some(join_result) = fork_set.join_next().await {
            match join_result {
                Ok((i, Ok(workspace))) => {
                    workers[i] = Some((
                        workspace.id,
                        workspace.name.clone(),
                        workspace.resources.memory_mb as u64,
                        workspace.resources.disk_gb as u64,
                    ));
                }
                Ok((i, Err(e))) => {
                    fork_failures.push((i, format!("{:#}", e)));
                }
                Err(e) => {
                    tracing::error!(error = %e, "fork task panicked");
                }
            }
        }

        if !fork_failures.is_empty() && workers.iter().all(|w| w.is_none()) {
            return Err(McpError::internal_error(
                format!("all forks failed: {:?}", fork_failures),
                None,
            ));
        }

        // Register ownership for forked workspaces
        for w in workers.iter().flatten() {
            if let Err(e) = self.auth.register_workspace(&self.session_id, w.0, w.2, w.3).await {
                tracing::warn!(workspace_id = %w.0, error = %e, "quota registration failed for swarm worker");
            }
        }

        // Override network policy on forked workers if allow_internet is set
        if let Some(allow_internet) = params.allow_internet {
            for worker in workers.iter().flatten() {
                if let Err(e) = self.workspace_manager.update_network_policy(
                    worker.0, Some(allow_internet), None, None,
                ).await {
                    tracing::warn!(
                        workspace_id = %worker.0,
                        error = %e,
                        "failed to set network policy on swarm worker"
                    );
                }
            }
        }

        // Inject env vars, shared_context, and vault_context into all workers in parallel
        let mut env_failures = Vec::new();

        // Resolve vault context once (shared across all workers)
        let resolved_vault_ctx = if !params.vault_context.is_empty() {
            if let Some(ref vault) = self.vault_manager {
                let ctx = resolve_swarm_vault_context(vault, &params.vault_context).await;
                if ctx.is_empty() { None } else { Some(ctx) }
            } else {
                tracing::warn!(
                    "vault_context provided but vault is not enabled — context will be empty"
                );
                None
            }
        } else {
            None
        };

        let has_env = params.env_vars.is_some();
        let has_shared_ctx = params.shared_context.as_ref().map_or(false, |s| !s.is_empty());
        let has_vault_ctx = resolved_vault_ctx.is_some();
        if has_env || has_shared_ctx || has_vault_ctx {
            let mut inject_set = tokio::task::JoinSet::new();
            for worker in workers.iter().flatten() {
                let wm = self.workspace_manager.clone();
                let ws_id = worker.0;
                let env_vars = params.env_vars.clone();
                let shared_ctx = params.shared_context.clone();
                let vault_ctx = resolved_vault_ctx.clone();
                inject_set.spawn(async move {
                    // Inject env vars
                    if let Some(env) = env_vars {
                        wm.set_env(ws_id, env).await.map_err(|e| (ws_id, format!("env: {:#}", e)))?;
                    }
                    // Write shared context file + set env var pointing to it
                    if let Some(ref ctx) = shared_ctx {
                        if !ctx.is_empty() {
                            wm.file_write(ws_id, "/tmp/.shared-context", ctx.as_bytes(), None).await
                                .map_err(|e| (ws_id, format!("shared_context write: {:#}", e)))?;
                            let mut extra_env = HashMap::new();
                            extra_env.insert("SHARED_CONTEXT".to_string(), "/tmp/.shared-context".to_string());
                            wm.set_env(ws_id, extra_env).await
                                .map_err(|e| (ws_id, format!("shared_context env: {:#}", e)))?;
                        }
                    }
                    // Write vault context file + set env var pointing to it
                    if let Some(ref vctx) = vault_ctx {
                        wm.file_write(ws_id, "/tmp/.vault-context", vctx.as_bytes(), None).await
                            .map_err(|e| (ws_id, format!("vault_context write: {:#}", e)))?;
                        let mut vc_env = HashMap::new();
                        vc_env.insert("VAULT_CONTEXT".to_string(), "/tmp/.vault-context".to_string());
                        wm.set_env(ws_id, vc_env).await
                            .map_err(|e| (ws_id, format!("vault_context env: {:#}", e)))?;
                    }
                    Ok::<_, (uuid::Uuid, String)>(ws_id)
                });
            }
            while let Some(join_result) = inject_set.join_next().await {
                match join_result {
                    Ok(Err((ws_id, err_msg))) => {
                        tracing::error!(
                            workspace_id = %ws_id,
                            error = %err_msg,
                            "failed to inject env/context into worker — task will likely fail"
                        );
                        env_failures.push(ws_id);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "inject task panicked");
                    }
                    Ok(Ok(_)) => {}
                }
            }
        }

        // Execute commands in parallel
        let timeout = params.timeout_secs.unwrap_or(600);
        let exec_semaphore = Arc::new(tokio::sync::Semaphore::new(max_parallel));
        let mut exec_set = tokio::task::JoinSet::new();

        for (i, task) in params.tasks.iter().enumerate() {
            if let Some((ws_id, _, _, _)) = &workers[i] {
                // Skip workers that failed env injection
                if env_failures.contains(ws_id) {
                    continue;
                }
                let wm = self.workspace_manager.clone();
                let ws = *ws_id;
                let cmd = task.command.clone();
                let wd = task.workdir.clone();
                let sem = exec_semaphore.clone();
                let task_start = std::time::Instant::now();
                exec_set.spawn(async move {
                    let _permit = sem.acquire().await;
                    let result = wm.exec(ws, &cmd, wd.as_deref(), None, Some(timeout)).await;
                    (i, result, task_start.elapsed().as_millis() as u64)
                });
            }
        }

        // Collect exec results
        let mut task_results: Vec<serde_json::Value> = Vec::with_capacity(params.tasks.len());
        let mut exec_results: Vec<Option<(bool, i32)>> = vec![None; params.tasks.len()];
        // Pre-populate with fork failures and env injection failures
        for (i, task) in params.tasks.iter().enumerate() {
            let env_failed = workers[i].as_ref()
                .map(|w| env_failures.contains(&w.0))
                .unwrap_or(false);
            if env_failed {
                task_results.push(serde_json::json!({
                    "name": task.name,
                    "workspace_id": workers[i].as_ref().map(|w| w.0.to_string()),
                    "success": false,
                    "exit_code": -1,
                    "stdout": "",
                    "stderr": "env var injection failed — task skipped (API keys not available)",
                    "elapsed_ms": 0,
                }));
                exec_results[i] = Some((false, -1));
            } else if workers[i].is_none() {
                let err_msg = fork_failures.iter()
                    .find(|(idx, _)| *idx == i)
                    .map(|(_, msg)| msg.clone())
                    .unwrap_or_else(|| "fork failed".to_string());
                task_results.push(serde_json::json!({
                    "name": task.name,
                    "workspace_id": null,
                    "success": false,
                    "exit_code": -1,
                    "stdout": "",
                    "stderr": err_msg,
                    "elapsed_ms": 0,
                }));
                exec_results[i] = Some((false, -1));
            }
        }

        while let Some(join_result) = exec_set.join_next().await {
            match join_result {
                Ok((i, Ok(exec_result), elapsed)) => {
                    let success = exec_result.exit_code == 0;
                    exec_results[i] = Some((success, exec_result.exit_code));
                    task_results.push(serde_json::json!({
                        "name": params.tasks[i].name,
                        "workspace_id": workers[i].as_ref().map(|w| w.0.to_string()),
                        "success": success,
                        "exit_code": exec_result.exit_code,
                        "stdout": truncate_output(exec_result.stdout, 262144),
                        "stderr": truncate_output(exec_result.stderr, 262144),
                        "elapsed_ms": elapsed,
                    }));
                }
                Ok((i, Err(e), elapsed)) => {
                    exec_results[i] = Some((false, -1));
                    task_results.push(serde_json::json!({
                        "name": params.tasks[i].name,
                        "workspace_id": workers[i].as_ref().map(|w| w.0.to_string()),
                        "success": false,
                        "exit_code": -1,
                        "stdout": "",
                        "stderr": format!("{:#}", e),
                        "elapsed_ms": elapsed,
                    }));
                }
                Err(e) => {
                    tracing::error!(error = %e, "exec task panicked");
                }
            }
        }

        // Sort task_results by task name to ensure deterministic ordering
        task_results.sort_by(|a, b| {
            let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            a_name.cmp(b_name)
        });

        // Optional merge
        let merge_result = if let (Some(strategy), Some(target_ref)) =
            (&params.merge_strategy, &params.merge_target)
        {
            let target_id = self.resolve_workspace_id(target_ref).await?;
            // Collect successful worker IDs for merge
            let source_ids: Vec<String> = workers.iter().enumerate()
                .filter(|(i, w)| {
                    w.is_some() && exec_results.get(*i).and_then(|r| r.as_ref()).map(|(s, _)| *s).unwrap_or(false)
                })
                .filter_map(|(_, w)| w.as_ref().map(|(id, _, _, _)| id.to_string()))
                .collect();

            if !source_ids.is_empty() {
                let merge_params = super::git_tools::GitMergeParams {
                    source_workspaces: source_ids,
                    target_workspace: target_id.to_string(),
                    strategy: strategy.clone(),
                    path: Some("/workspace".to_string()),
                    commit_message: None,
                };
                match self.handle_workspace_merge(merge_params).await {
                    Ok(result) => {
                        // Extract text from result
                        let text = result.content.first()
                            .and_then(|c| match c.raw {
                                rmcp::model::RawContent::Text(ref t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        Some(serde_json::json!({
                            "strategy": strategy,
                            "target_workspace": target_ref,
                            "detail": text,
                        }))
                    }
                    Err(e) => Some(serde_json::json!({
                        "strategy": strategy,
                        "target_workspace": target_ref,
                        "error": format!("{}", e),
                    })),
                }
            } else {
                Some(serde_json::json!({
                    "strategy": strategy,
                    "target_workspace": target_ref,
                    "error": "no successful workers to merge",
                }))
            }
        } else {
            None
        };

        // Cleanup — destroy workers in parallel
        let cleanup = params.cleanup.unwrap_or(true);
        let mut destroyed = 0u32;
        if cleanup {
            let mut destroy_set = tokio::task::JoinSet::new();
            for worker in workers.iter().flatten() {
                let wm = self.workspace_manager.clone();
                let ws_id = worker.0;
                destroy_set.spawn(async move {
                    (ws_id, wm.destroy(ws_id).await)
                });
            }
            while let Some(join_result) = destroy_set.join_next().await {
                match join_result {
                    Ok((_, Ok(_))) => { destroyed += 1; }
                    Ok((ws_id, Err(e))) => {
                        tracing::warn!(workspace_id = %ws_id, error = %e, "failed to destroy swarm worker");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "destroy task panicked");
                    }
                }
            }
        }

        let total = params.tasks.len() as u32;
        let succeeded = exec_results.iter()
            .filter(|r| r.as_ref().map(|(s, _)| *s).unwrap_or(false))
            .count() as u32;
        let failed = total - succeeded;

        let mut output = serde_json::json!({
            "swarm_id": swarm_id,
            "tasks": task_results,
            "summary": {
                "total_tasks": total,
                "succeeded": succeeded,
                "failed": failed,
                "total_elapsed_ms": start.elapsed().as_millis() as u64,
                "workers_destroyed": destroyed,
            }
        });

        if let Some(merge) = merge_result {
            output.as_object_mut().unwrap().insert("merge".to_string(), merge);
        }

        info!(
            swarm_id = %swarm_id,
            succeeded = succeeded,
            failed = failed,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "swarm_run complete"
        );

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&output).unwrap(),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for AgentisoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "agentiso: QEMU microvm workspace manager for AI agents (31 tools). Each workspace is a fully \
                 isolated Linux VM with its own filesystem, network stack, and process space. You can \
                 create, snapshot, fork, and destroy workspaces on demand.\n\
                 \n\
                 == TOOL GROUPS ==\n\
                 \n\
                 WORKSPACE LIFECYCLE: workspace_create, workspace_destroy, workspace_start, workspace_stop, \
                 workspace_list, workspace_info, workspace_logs\n\
                 \n\
                 EXECUTION & FILES: exec, exec_background (start/poll/kill), file_read, file_write, \
                 file_edit, file_list, file_transfer (direction=\"upload\"/\"download\"), set_env\n\
                 \n\
                 SNAPSHOTS & FORKS: snapshot (action=\"create\"/\"restore\"/\"list\"/\"delete\"), \
                 workspace_fork (count=N for batch)\n\
                 \n\
                 NETWORKING: port_forward (action=\"add\"/\"remove\"), network_policy\n\
                 \n\
                 SESSION: workspace_adopt (workspace_id=... for one, omit for all, force=true to reclaim from dead sessions)\n\
                 \n\
                 GIT: git_clone, git_status, git_commit, git_push, git_diff, workspace_merge\n\
                 \n\
                 ORCHESTRATION: workspace_prepare, exec_parallel, swarm_run\n\
                 \n\
                 TEAMS: team (action=\"create\"/\"destroy\"/\"status\"/\"list\"/\"message\"/\"receive\")\n\
                 \n\
                 VAULT (shared knowledge base): vault (with action parameter: read, write, search, \
                 list, delete, frontmatter, tags, replace, move, batch_read, stats)\n\
                 \n\
                 == QUICK START ==\n\
                 \n\
                 1. workspace_create(name=\"my-project\") -- get a workspace_id and a running VM\n\
                 2. git_clone(workspace_id, url=\"https://...\") -- clone a repo into /workspace\n\
                 3. exec(workspace_id, command=\"cd /workspace && npm install\") -- run setup\n\
                 4. snapshot(workspace_id, action=\"create\", name=\"baseline\") -- checkpoint your progress\n\
                 5. ... work, experiment, snapshot, restore, fork as needed ...\n\
                 6. workspace_destroy(workspace_id) -- clean up when done\n\
                 \n\
                 == WORKFLOW TIPS ==\n\
                 \n\
                 - Start with workspace_create to get an isolated Linux VM. Each workspace has its own \
                 filesystem, network, and process space. Workspaces are identified by UUID or by the \
                 human-readable name you give them.\n\
                 \n\
                 - Use snapshot(action=\"create\") before risky operations (rm -rf, database migrations, config changes). \
                 Restore with snapshot(action=\"restore\") if something goes wrong. Snapshots are cheap (ZFS \
                 copy-on-write).\n\
                 \n\
                 - For parallel work, use workspace_fork to create independent copies from a snapshot. \
                 Each fork gets its own VM with a copy-on-write clone of the disk. For bulk parallelism, \
                 use workspace_prepare to build a golden image, then workspace_fork(count=N) to spin up N \
                 workers at once.\n\
                 \n\
                 - exec_parallel runs commands across multiple workspaces concurrently in a single call. \
                 Pass workspace_ids and commands (single string broadcasts to all, or array for per-workspace). \
                 This eliminates the need for exec_background + polling loops when running on multiple VMs.\n\
                 \n\
                 - swarm_run is the all-in-one orchestration tool: fork workers from a snapshot, inject env vars, \
                 exec commands in parallel, optionally merge results back via git, and clean up. It replaces \
                 the manual sequence of workspace_fork + set_env + exec + workspace_merge + workspace_destroy. \
                 A 5-worker swarm becomes 2 MCP calls (workspace_prepare + swarm_run) instead of ~120.\n\
                 \n\
                 - exec runs commands synchronously with a default timeout of 120 seconds. For long-running \
                 processes (servers, builds, test suites), use exec_background to start the command, \
                 exec_background(action=\"poll\") to check on it, and exec_background(action=\"kill\") to stop it. Output from exec and exec_background(action=\"poll\") is \
                 capped at 256KB; longer output is truncated.\n\
                 \n\
                 - Use set_env to inject persistent environment variables (API keys, tokens, config) into \
                 a workspace. These apply to all subsequent exec and exec_background calls until the VM \
                 is destroyed. Per-command env vars override stored values.\n\
                 \n\
                 - Internet access is OFF by default (allow_internet=false) for security. Set \
                 allow_internet=true on workspace_create if the VM needs to reach external APIs (e.g. \
                 Anthropic API for OpenCode). For swarm_run, set allow_internet=true to override on forks.\n\
                 \n\
                 - The vault is a shared markdown knowledge base on the host, accessible from any session \
                 without needing a workspace. Use vault(action=\"write\") to save findings, vault(action=\"search\") to find them \
                 later. Vault tools require [vault] to be enabled in the server's config.toml.\n\
                 \n\
                 - file_transfer transfers files between the host and guest VM. Use direction=\"upload\" or \
                 direction=\"download\". Requires a configured transfer directory on the host; paths outside \
                 that directory are rejected for security.\n\
                 \n\
                 - Workspaces persist across reconnects. After a server restart, use workspace_adopt(force=true) \
                 to reclaim ownership of all existing workspaces before interacting with them. If a workspace \
                 is stuck as \"owned by another session\", use workspace_adopt(workspace_id=..., force=true) to \
                 transfer ownership. Use workspace_list to see all workspaces and their ownership status.\n\
                 \n\
                 == COMMON PITFALLS ==\n\
                 \n\
                 - snapshot(action=\"restore\") is DESTRUCTIVE: it removes all snapshots created after the restore \
                 target. If you need to preserve the full timeline, fork first with workspace_fork, then \
                 restore on the original.\n\
                 \n\
                 - file_read and file_write paths must be absolute paths inside the VM (e.g. \
                 /workspace/myfile.txt, /root/.config/settings.json). Do not use relative paths like \
                 ./myfile.txt.\n\
                 \n\
                 - exec has a default timeout of 120 seconds. For commands expected to run longer, either \
                 set timeout_secs explicitly (e.g. timeout_secs=600) or use exec_background(action=\"start\") then exec_background(action=\"poll\") \
                 instead.\n\
                 \n\
                 - The guest OS is Alpine Linux. Package installation uses 'apk add <package>', not apt \
                 or yum. Common dev tools are pre-installed in the alpine-dev image.\n\
                 \n\
                 - Use workspace_logs to debug boot failures or VM issues. It shows QEMU console output \
                 and stderr.\n\
                 \n\
                 - workspace_fork(count=N) accepts count 1-20. For larger parallelism, call it multiple times."
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
    /// Check rate limit for a tool call category.
    ///
    /// Categories: "create" (workspace_create, workspace_fork),
    /// "exec" (exec, exec_background), "default" (everything else).
    /// Returns an MCP error if the rate limit is exceeded.
    fn check_rate_limit(&self, category: &str) -> Result<(), McpError> {
        self.rate_limiter.check(category).map_err(|msg| {
            warn!(category, "rate limit exceeded");
            McpError::invalid_request(msg, None)
        })
    }

    /// Update the session's last-activity timestamp.
    /// Called at the start of every tool handler so that `force-adopt` can
    /// detect dead (stale) sessions and only steal from them.
    async fn touch_activity(&self) {
        self.auth.touch_session(&self.session_id).await;
    }

    /// Verify the current session owns the given workspace.
    pub(crate) async fn check_ownership(&self, workspace_id: Uuid) -> Result<(), McpError> {
        self.auth
            .check_ownership(&self.session_id, workspace_id)
            .await
            .map_err(|e| {
                McpError::invalid_request(
                    format!(
                        "Workspace {} is not owned by this session: {}. \
                         Use workspace_adopt(workspace_id=\"{}\", force=true) to claim it, \
                         or workspace_adopt(force=true) to claim all workspaces.",
                        workspace_id, e, workspace_id
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
    pub(crate) async fn resolve_workspace_id(&self, id_or_name: &str) -> Result<Uuid, McpError> {
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

    /// Try to reclaim an existing workspace by name for `workspace_prepare`.
    ///
    /// Returns one of:
    /// - `NotFound` — no workspace with this name exists
    /// - `AlreadyGolden` — workspace has a "golden" snapshot, return idempotent success
    /// - `Reusable` — workspace adopted and Running, caller should proceed with setup
    /// - `CannotAdopt` — workspace exists but cannot be claimed by this session
    async fn try_reclaim_for_prepare(
        &self,
        name: &str,
    ) -> Result<PrepareReclaim, McpError> {
        let existing_id = match self.workspace_manager.find_by_name(name).await {
            Some(id) => id,
            None => return Ok(PrepareReclaim::NotFound),
        };

        let existing = self
            .workspace_manager
            .get(existing_id)
            .await
            .map_err(|e| McpError::internal_error(
                format!("workspace '{}' exists but could not be retrieved: {:#}", name, e),
                None,
            ))?;

        // Try to adopt (force=true since we know a previous session likely left it).
        if let Err(_adopt_err) = self
            .auth
            .adopt_workspace(
                &self.session_id,
                &existing_id,
                existing.resources.memory_mb as u64,
                existing.resources.disk_gb as u64,
                true,
            )
            .await
        {
            // adopt_workspace with force=true only fails if we can't find the session.
            // Also check if we already own it.
            if self.auth.check_ownership(&self.session_id, existing_id).await.is_err() {
                return Ok(PrepareReclaim::CannotAdopt);
            }
        }

        info!(
            workspace_id = %existing_id,
            name = %name,
            "workspace_prepare: reclaimed existing workspace"
        );

        // Check if it already has a "golden" snapshot — fully idempotent return.
        if existing.snapshots.get_by_name("golden").is_some() {
            return Ok(PrepareReclaim::AlreadyGolden(existing_id));
        }

        // No golden snapshot — workspace is from a partial previous run.
        // Ensure it's Running so we can reuse it for git clone + setup.
        if existing.state == crate::workspace::WorkspaceState::Stopped {
            self.workspace_manager.start(existing_id).await.map_err(|e| {
                McpError::internal_error(
                    format!("failed to start existing workspace '{}': {:#}", name, e),
                    None,
                )
            })?;
        }

        Ok(PrepareReclaim::Reusable(existing_id))
    }

    /// Acquire a workspace for `workspace_prepare`: reuse existing or create new.
    ///
    /// Returns `(workspace_id, actual_name, reused)`. If the workspace already
    /// has a "golden" snapshot, returns early from `workspace_prepare` via the
    /// `AlreadyGolden` variant handled in the caller.
    async fn prepare_acquire_workspace(
        &self,
        name: &str,
        base_image: &str,
        mem: u32,
        disk: u32,
    ) -> Result<(Uuid, String, bool), McpError> {
        // First, try the requested name directly.
        match self.try_reclaim_for_prepare(name).await? {
            PrepareReclaim::Reusable(id) => return Ok((id, name.to_string(), true)),
            PrepareReclaim::AlreadyGolden(id) => {
                // Return a sentinel that workspace_prepare will detect
                // and short-circuit into an idempotent success response.
                // We use reused=true and a special ID that signals "already done".
                // Actually, we need to handle this at the workspace_prepare level.
                // For now, return as reusable — workspace_prepare will check for golden
                // snapshot before creating one (snapshot_create would fail on duplicate anyway).
                return Ok((id, name.to_string(), true));
            }
            PrepareReclaim::NotFound => {
                return self.prepare_create_fresh(name, base_image, mem, disk).await;
            }
            PrepareReclaim::CannotAdopt => {} // Fall through to suffix logic
        }

        // Name is taken and can't be reclaimed. Try suffixed names.
        for suffix in 2..=6 {
            let candidate = format!("{}-{}", name, suffix);
            match self.try_reclaim_for_prepare(&candidate).await? {
                PrepareReclaim::Reusable(id) => return Ok((id, candidate, true)),
                PrepareReclaim::AlreadyGolden(id) => return Ok((id, candidate, true)),
                PrepareReclaim::NotFound => {
                    match self.prepare_create_fresh(&candidate, base_image, mem, disk).await {
                        Ok(r) => return Ok(r),
                        Err(_) => continue, // Race condition, try next suffix
                    }
                }
                PrepareReclaim::CannotAdopt => continue,
            }
        }

        Err(McpError::internal_error(
            format!(
                "workspace name '{}' and suffixes -2 through -6 are all in use. \
                 Destroy unused workspaces first.",
                name
            ),
            None,
        ))
    }

    /// Create a fresh workspace for `workspace_prepare` and register ownership.
    async fn prepare_create_fresh(
        &self,
        name: &str,
        base_image: &str,
        mem: u32,
        disk: u32,
    ) -> Result<(Uuid, String, bool), McpError> {
        self.auth
            .check_quota(&self.session_id, mem as u64, disk as u64)
            .await
            .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

        let create_params = crate::workspace::CreateParams {
            name: Some(name.to_string()),
            base_image: Some(base_image.to_string()),
            vcpus: None,
            memory_mb: Some(mem),
            disk_gb: Some(disk),
            allow_internet: Some(true), // Need internet for git clone
        };

        let create_result = self
            .workspace_manager
            .create(create_params)
            .await
            .map_err(|e| McpError::internal_error(format!("create failed: {:#}", e), None))?;

        let workspace = create_result.workspace;
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

        Ok((ws_id, name.to_string(), false))
    }
}

/// Result of trying to reclaim an existing workspace for `workspace_prepare`.
enum PrepareReclaim {
    /// No workspace with this name exists.
    NotFound,
    /// Workspace exists and already has a "golden" snapshot.
    AlreadyGolden(Uuid),
    /// Workspace exists, was adopted, and is Running (no golden snapshot).
    Reusable(Uuid),
    /// Workspace exists but cannot be adopted by this session.
    CannotAdopt,
}

/// Resolve vault queries into a formatted context string for swarm_run.
///
/// For "search" queries, runs a full-text search and formats the top results.
/// For "read" queries, reads the full note content.
/// Individual query failures are logged and skipped.
async fn resolve_swarm_vault_context(
    vault: &super::vault::VaultManager,
    queries: &[SwarmVaultQuery],
) -> String {
    let mut sections = Vec::new();

    for q in queries {
        match q.kind.as_str() {
            "search" => {
                match vault.search(&q.query, false, None, None, 5).await {
                    Ok(results) if !results.is_empty() => {
                        let mut section = format!("### Search: \"{}\"\n", q.query);
                        for hit in &results {
                            section.push_str(&format!(
                                "\n**{}** (line {}):\n```\n{}\n```\n",
                                hit.path, hit.line_number, hit.context
                            ));
                        }
                        sections.push(section);
                    }
                    Ok(_) => {
                        warn!(query = %q.query, "vault search returned no results");
                    }
                    Err(e) => {
                        warn!(query = %q.query, error = %e, "vault search failed, skipping");
                    }
                }
            }
            "read" => {
                match vault.read_note(&q.query).await {
                    Ok(note) => {
                        sections.push(format!(
                            "### {}\n\n{}\n",
                            note.path, note.content
                        ));
                    }
                    Err(e) => {
                        warn!(path = %q.query, error = %e, "vault read failed, skipping");
                    }
                }
            }
            other => {
                warn!(kind = %other, query = %q.query, "unknown vault_context kind, skipping");
            }
        }
    }

    if sections.is_empty() {
        return String::new();
    }

    format!("## Project Knowledge Base\n\n{}", sections.join("\n"))
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
/// Escape a string for safe use in a shell command.
/// Wraps in single quotes and escapes any internal single quotes.
pub(crate) fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    fn test_snapshot_params_create() {
        let json = serde_json::json!({
            "action": "create",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "before-experiment",
            "include_memory": true
        });
        let params: SnapshotParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "create");
        assert_eq!(params.name, Some("before-experiment".to_string()));
        assert_eq!(params.include_memory, Some(true));
    }

    #[test]
    fn test_snapshot_params_list() {
        let json = serde_json::json!({
            "action": "list",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: SnapshotParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "list");
        assert!(params.name.is_none());
        assert!(params.include_memory.is_none());
    }

    #[test]
    fn test_snapshot_params_missing_action() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "snap1"
        });
        assert!(serde_json::from_value::<SnapshotParams>(json).is_err());
    }

    #[test]
    fn test_snapshot_params_missing_workspace_id() {
        let json = serde_json::json!({
            "action": "create",
            "name": "snap1"
        });
        assert!(serde_json::from_value::<SnapshotParams>(json).is_err());
    }

    #[test]
    fn test_port_forward_params_add() {
        let json = serde_json::json!({
            "action": "add",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_port": 8080,
            "host_port": 9090
        });
        let params: PortForwardParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "add");
        assert_eq!(params.guest_port, 8080);
        assert_eq!(params.host_port, Some(9090));
    }

    #[test]
    fn test_port_forward_params_auto_assign() {
        let json = serde_json::json!({
            "action": "add",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_port": 3000
        });
        let params: PortForwardParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.guest_port, 3000);
        assert!(params.host_port.is_none());
    }

    #[test]
    fn test_port_forward_params_remove() {
        let json = serde_json::json!({
            "action": "remove",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_port": 8080
        });
        let params: PortForwardParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "remove");
        assert_eq!(params.guest_port, 8080);
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
            "action": "add",
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
            "action": "add",
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
    fn test_exec_background_params_start() {
        let json = serde_json::json!({
            "action": "start",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "command": "sleep 60 && echo done",
            "workdir": "/app",
            "env": {"NODE_ENV": "production"}
        });
        let params: ExecBackgroundParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "start");
        assert_eq!(params.command.as_deref(), Some("sleep 60 && echo done"));
        assert_eq!(params.workdir.as_deref(), Some("/app"));
    }

    #[test]
    fn test_exec_background_params_poll() {
        let json = serde_json::json!({
            "action": "poll",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "job_id": 42
        });
        let params: ExecBackgroundParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "poll");
        assert_eq!(params.job_id, Some(42));
    }

    #[test]
    fn test_exec_background_params_kill() {
        let json = serde_json::json!({
            "action": "kill",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "job_id": 7,
            "signal": 15
        });
        let params: ExecBackgroundParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "kill");
        assert_eq!(params.job_id, Some(7));
        assert_eq!(params.signal, Some(15));
    }

    // --- Adoption param tests ---

    #[test]
    fn test_workspace_adopt_params_single() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: WorkspaceAdoptParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!params.force);
    }

    #[test]
    fn test_workspace_adopt_params_all() {
        // When workspace_id is omitted, adopts all orphaned workspaces.
        let json = serde_json::json!({});
        let params: WorkspaceAdoptParams = serde_json::from_value(json).unwrap();
        assert!(params.workspace_id.is_none());
        assert!(!params.force);
    }

    #[test]
    fn test_workspace_adopt_params_force() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "force": true
        });
        let params: WorkspaceAdoptParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert!(params.force);
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

    // --- PrepareNameGuard tests ---

    #[tokio::test]
    async fn test_prepare_name_guard_prevents_double_insert() {
        let locks = Arc::new(tokio::sync::Mutex::new(HashSet::new()));

        // First insert succeeds
        {
            let mut set = locks.lock().await;
            assert!(set.insert("my-ws".to_string()));
        }

        // Second insert with same name fails (simulates concurrent call)
        {
            let mut set = locks.lock().await;
            assert!(!set.insert("my-ws".to_string()));
        }
    }

    #[tokio::test]
    async fn test_prepare_name_guard_releases_on_drop() {
        let locks = Arc::new(tokio::sync::Mutex::new(HashSet::new()));

        // Simulate reservation + guard in a scope
        {
            {
                let mut set = locks.lock().await;
                assert!(set.insert("test-ws".to_string()));
            }
            let _guard = PrepareNameGuard {
                name: "test-ws".to_string(),
                locks: locks.clone(),
            };
            // Guard is alive — name should be in the set
            assert!(locks.lock().await.contains("test-ws"));
        }
        // Guard dropped — name should be removed
        assert!(!locks.lock().await.contains("test-ws"));
    }

    #[tokio::test]
    async fn test_prepare_name_guard_different_names_independent() {
        let locks = Arc::new(tokio::sync::Mutex::new(HashSet::new()));

        {
            let mut set = locks.lock().await;
            assert!(set.insert("ws-a".to_string()));
        }
        // Different name should succeed
        {
            let mut set = locks.lock().await;
            assert!(set.insert("ws-b".to_string()));
        }

        // Both present
        let set = locks.lock().await;
        assert!(set.contains("ws-a"));
        assert!(set.contains("ws-b"));
    }

    // --- workspace_fork batch mode param tests ---

    #[test]
    fn test_workspace_fork_params_batch() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "snapshot_name": "golden",
            "count": 5,
            "name_prefix": "task"
        });
        let params: WorkspaceForkParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.snapshot_name, "golden");
        assert_eq!(params.count, Some(5));
        assert_eq!(params.name_prefix.as_deref(), Some("task"));
    }

    #[test]
    fn test_workspace_fork_params_batch_defaults() {
        // count defaults to None (single fork)
        let json = serde_json::json!({
            "workspace_id": "my-golden-workspace",
            "snapshot_name": "golden"
        });
        let params: WorkspaceForkParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "my-golden-workspace");
        assert!(params.count.is_none());
        assert!(params.name_prefix.is_none());
    }

    #[test]
    fn test_workspace_fork_batch_name_generation() {
        // Verify the naming pattern used in the handler
        let prefix = "worker";
        let count = 3u32;
        let names: Vec<String> = (1..=count).map(|i| format!("{}-{}", prefix, i)).collect();
        assert_eq!(names, vec!["worker-1", "worker-2", "worker-3"]);
    }

    #[test]
    fn test_workspace_fork_batch_custom_prefix_name_generation() {
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
    fn test_vault_params_read_full() {
        let json = serde_json::json!({
            "action": "read",
            "path": "projects/design.md",
            "format": "json"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "read");
        assert_eq!(params.path.as_deref(), Some("projects/design.md"));
        assert_eq!(params.format.as_deref(), Some("json"));
    }

    #[test]
    fn test_vault_params_read_minimal() {
        let json = serde_json::json!({
            "action": "read",
            "path": "notes/hello.md"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "read");
        assert_eq!(params.path.as_deref(), Some("notes/hello.md"));
        assert!(params.format.is_none());
    }

    #[test]
    fn test_vault_params_missing_action() {
        let json = serde_json::json!({});
        assert!(serde_json::from_value::<VaultParams>(json).is_err());
    }

    #[test]
    fn test_vault_params_search_full() {
        let json = serde_json::json!({
            "action": "search",
            "query": "auth.*pattern",
            "regex": true,
            "path_prefix": "projects/",
            "tag": "architecture",
            "max_results": 50
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "search");
        assert_eq!(params.query.as_deref(), Some("auth.*pattern"));
        assert_eq!(params.regex, Some(true));
        assert_eq!(params.path_prefix.as_deref(), Some("projects/"));
        assert_eq!(params.tag.as_deref(), Some("architecture"));
        assert_eq!(params.max_results, Some(50));
    }

    #[test]
    fn test_vault_params_search_minimal() {
        let json = serde_json::json!({
            "action": "search",
            "query": "hello"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.query.as_deref(), Some("hello"));
        assert!(params.regex.is_none());
        assert!(params.path_prefix.is_none());
        assert!(params.tag.is_none());
        assert!(params.max_results.is_none());
    }

    #[test]
    fn test_vault_params_list_full() {
        let json = serde_json::json!({
            "action": "list",
            "path": "projects",
            "recursive": true
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "list");
        assert_eq!(params.path.as_deref(), Some("projects"));
        assert_eq!(params.recursive, Some(true));
    }

    #[test]
    fn test_vault_params_list_empty() {
        let json = serde_json::json!({
            "action": "list"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "list");
        assert!(params.path.is_none());
        assert!(params.recursive.is_none());
    }

    #[test]
    fn test_vault_params_write_full() {
        let json = serde_json::json!({
            "action": "write",
            "path": "notes/new.md",
            "content": "# New Note\n\nHello world.",
            "mode": "append"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "write");
        assert_eq!(params.path.as_deref(), Some("notes/new.md"));
        assert_eq!(params.content.as_deref(), Some("# New Note\n\nHello world."));
        assert_eq!(params.mode.as_deref(), Some("append"));
    }

    #[test]
    fn test_vault_params_write_minimal() {
        let json = serde_json::json!({
            "action": "write",
            "path": "notes/new.md",
            "content": "Hello"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path.as_deref(), Some("notes/new.md"));
        assert!(params.mode.is_none());
    }

    #[test]
    fn test_vault_params_frontmatter_get() {
        let json = serde_json::json!({
            "action": "frontmatter",
            "path": "design.md",
            "frontmatter_action": "get"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "frontmatter");
        assert_eq!(params.frontmatter_action.as_deref(), Some("get"));
        assert!(params.key.is_none());
        assert!(params.value.is_none());
    }

    #[test]
    fn test_vault_params_frontmatter_set() {
        let json = serde_json::json!({
            "action": "frontmatter",
            "path": "design.md",
            "frontmatter_action": "set",
            "key": "status",
            "value": "draft"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "frontmatter");
        assert_eq!(params.frontmatter_action.as_deref(), Some("set"));
        assert_eq!(params.key.as_deref(), Some("status"));
        assert_eq!(params.value, Some(serde_json::json!("draft")));
    }

    #[test]
    fn test_vault_params_frontmatter_delete() {
        let json = serde_json::json!({
            "action": "frontmatter",
            "path": "design.md",
            "frontmatter_action": "delete",
            "key": "deprecated"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "frontmatter");
        assert_eq!(params.frontmatter_action.as_deref(), Some("delete"));
        assert_eq!(params.key.as_deref(), Some("deprecated"));
    }

    #[test]
    fn test_vault_params_tags_list() {
        let json = serde_json::json!({
            "action": "tags",
            "path": "notes/hello.md",
            "tags_action": "list"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "tags");
        assert_eq!(params.tags_action.as_deref(), Some("list"));
        assert!(params.tag.is_none());
    }

    #[test]
    fn test_vault_params_tags_add() {
        let json = serde_json::json!({
            "action": "tags",
            "path": "notes/hello.md",
            "tags_action": "add",
            "tag": "important"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "tags");
        assert_eq!(params.tags_action.as_deref(), Some("add"));
        assert_eq!(params.tag.as_deref(), Some("important"));
    }

    #[test]
    fn test_vault_params_replace_full() {
        let json = serde_json::json!({
            "action": "replace",
            "path": "notes/hello.md",
            "old_string": "old_text",
            "new_string": "new_text",
            "regex": false
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "replace");
        assert_eq!(params.path.as_deref(), Some("notes/hello.md"));
        assert_eq!(params.old_string.as_deref(), Some("old_text"));
        assert_eq!(params.new_string.as_deref(), Some("new_text"));
        assert_eq!(params.regex, Some(false));
    }

    #[test]
    fn test_vault_params_replace_minimal() {
        let json = serde_json::json!({
            "action": "replace",
            "path": "notes/hello.md",
            "old_string": "old",
            "new_string": "new"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert!(params.regex.is_none());
    }

    #[test]
    fn test_vault_params_delete() {
        let json = serde_json::json!({
            "action": "delete",
            "path": "notes/old.md",
            "confirm": true
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "delete");
        assert_eq!(params.path.as_deref(), Some("notes/old.md"));
        assert_eq!(params.confirm, Some(true));
    }

    #[test]
    fn test_vault_params_delete_confirm_false() {
        let json = serde_json::json!({
            "action": "delete",
            "path": "notes/old.md",
            "confirm": false
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.confirm, Some(false));
    }

    #[test]
    fn test_vault_params_delete_no_confirm() {
        // confirm defaults to None when omitted (handler treats as false)
        let json = serde_json::json!({
            "action": "delete",
            "path": "notes/old.md"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert!(params.confirm.is_none());
    }

    // --- vault move param tests ---

    #[test]
    fn test_vault_params_move_full() {
        let json = serde_json::json!({
            "action": "move",
            "path": "notes/old.md",
            "new_path": "archive/old.md",
            "overwrite": true
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "move");
        assert_eq!(params.path.as_deref(), Some("notes/old.md"));
        assert_eq!(params.new_path.as_deref(), Some("archive/old.md"));
        assert_eq!(params.overwrite, Some(true));
    }

    #[test]
    fn test_vault_params_move_minimal() {
        let json = serde_json::json!({
            "action": "move",
            "path": "notes/old.md",
            "new_path": "notes/new.md"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.path.as_deref(), Some("notes/old.md"));
        assert_eq!(params.new_path.as_deref(), Some("notes/new.md"));
        assert!(params.overwrite.is_none());
    }

    #[test]
    fn test_vault_params_move_no_new_path() {
        // new_path is optional in the struct (handler validates at runtime)
        let json = serde_json::json!({
            "action": "move",
            "path": "notes/old.md"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert!(params.new_path.is_none());
    }

    // --- vault batch_read param tests ---

    #[test]
    fn test_vault_params_batch_read_full() {
        let json = serde_json::json!({
            "action": "batch_read",
            "paths": ["notes/a.md", "notes/b.md"],
            "include_content": false,
            "include_frontmatter": true
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "batch_read");
        assert_eq!(params.paths.as_ref().unwrap().len(), 2);
        assert_eq!(params.include_content, Some(false));
        assert_eq!(params.include_frontmatter, Some(true));
    }

    #[test]
    fn test_vault_params_batch_read_minimal() {
        let json = serde_json::json!({
            "action": "batch_read",
            "paths": ["notes/a.md"]
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.paths.as_ref().unwrap().len(), 1);
        assert!(params.include_content.is_none());
        assert!(params.include_frontmatter.is_none());
    }

    #[test]
    fn test_vault_params_batch_read_no_paths() {
        // paths is optional in the struct (handler validates at runtime)
        let json = serde_json::json!({
            "action": "batch_read"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert!(params.paths.is_none());
    }

    // --- vault stats param tests ---

    #[test]
    fn test_vault_params_stats_full() {
        let json = serde_json::json!({
            "action": "stats",
            "recent_count": 5
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "stats");
        assert_eq!(params.recent_count, Some(5));
    }

    #[test]
    fn test_vault_params_stats_empty() {
        let json = serde_json::json!({
            "action": "stats"
        });
        let params: VaultParams = serde_json::from_value(json).unwrap();
        assert!(params.recent_count.is_none());
    }

    // --- Tool registration verification ---

    #[test]
    fn test_tool_router_has_exactly_31_tools() {
        let router = AgentisoServer::tool_router();
        assert_eq!(
            router.map.len(),
            31,
            "expected exactly 31 tools registered, got {}. Tool list: {:?}",
            router.map.len(),
            router.map.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_tool_router_contains_all_core_tools() {
        let router = AgentisoServer::tool_router();
        let expected_tools = [
            // Lifecycle
            "workspace_create",
            "workspace_destroy",
            "workspace_list",
            "workspace_info",
            "workspace_stop",
            "workspace_start",
            "workspace_logs",
            // Execution & files
            "exec",
            "exec_background",
            "file_write",
            "file_read",
            "file_transfer",
            "file_list",
            "file_edit",
            "set_env",
            // Snapshots & forks
            "snapshot",
            "workspace_fork",
            // Networking
            "port_forward",
            "network_policy",
            // Session
            "workspace_adopt",
            // Git
            "git_clone",
            "git_status",
            "git_commit",
            "git_push",
            "git_diff",
            "workspace_merge",
            // Orchestration
            "workspace_prepare",
            "exec_parallel",
            "swarm_run",
            // Teams
            "team",
            // Vault
            "vault",
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
    fn test_file_transfer_params_upload() {
        let json = serde_json::json!({
            "direction": "upload",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "host_path": "/tmp/transfer/data.tar.gz",
            "guest_path": "/home/user/data.tar.gz"
        });
        let params: FileTransferParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.direction, "upload");
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.host_path, "/tmp/transfer/data.tar.gz");
        assert_eq!(params.guest_path, "/home/user/data.tar.gz");
    }

    #[test]
    fn test_file_transfer_params_download() {
        let json = serde_json::json!({
            "direction": "download",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "host_path": "/tmp/transfer/out.txt",
            "guest_path": "/home/user/out.txt"
        });
        let params: FileTransferParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.direction, "download");
    }

    #[test]
    fn test_file_transfer_params_missing_required() {
        // Missing direction
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "host_path": "/tmp/file.txt",
            "guest_path": "/home/user/file.txt"
        });
        assert!(serde_json::from_value::<FileTransferParams>(json).is_err());

        // Missing guest_path
        let json = serde_json::json!({
            "direction": "upload",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "host_path": "/tmp/file.txt"
        });
        assert!(serde_json::from_value::<FileTransferParams>(json).is_err());

        // Missing host_path
        let json = serde_json::json!({
            "direction": "upload",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "guest_path": "/home/user/file.txt"
        });
        assert!(serde_json::from_value::<FileTransferParams>(json).is_err());
    }

    #[test]
    fn test_snapshot_params_restore() {
        let json = serde_json::json!({
            "action": "restore",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "before-deploy"
        });
        let params: SnapshotParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "restore");
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.name, Some("before-deploy".to_string()));
    }

    #[test]
    fn test_snapshot_params_delete() {
        let json = serde_json::json!({
            "action": "delete",
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "old-snap"
        });
        let params: SnapshotParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "delete");
        assert_eq!(params.name, Some("old-snap".to_string()));
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
    fn test_exec_parallel_params_single_command() {
        let json = serde_json::json!({
            "workspace_ids": ["a", "b"],
            "commands": "echo hello"
        });
        let params: ExecParallelParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_ids, vec!["a", "b"]);
        assert!(matches!(params.commands, ExecParallelCommands::Single(ref s) if s == "echo hello"));
        assert!(params.timeout_secs.is_none());
        assert!(params.workdir.is_none());
        assert!(params.env.is_none());
        assert!(params.max_output_bytes.is_none());
    }

    #[test]
    fn test_exec_parallel_params_multiple_commands() {
        let json = serde_json::json!({
            "workspace_ids": ["a", "b"],
            "commands": ["echo a", "echo b"]
        });
        let params: ExecParallelParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_ids, vec!["a", "b"]);
        assert!(matches!(params.commands, ExecParallelCommands::Multiple(ref v) if v == &["echo a", "echo b"]));
    }

    #[test]
    fn test_swarm_run_params_minimal() {
        let json = serde_json::json!({
            "golden_workspace": "proj",
            "snapshot_name": "base",
            "tasks": [{"name": "t1", "command": "echo hi"}]
        });
        let params: SwarmRunParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.golden_workspace, "proj");
        assert_eq!(params.snapshot_name, "base");
        assert_eq!(params.tasks.len(), 1);
        assert_eq!(params.tasks[0].name, "t1");
        assert_eq!(params.tasks[0].command, "echo hi");
        assert!(params.tasks[0].workdir.is_none());
        assert!(params.env_vars.is_none());
        assert!(params.merge_strategy.is_none());
        assert!(params.merge_target.is_none());
        assert!(params.max_parallel.is_none());
        assert!(params.timeout_secs.is_none());
        assert!(params.cleanup.is_none());
        assert!(params.vault_context.is_empty());
        assert!(params.shared_context.is_none());
    }

    #[test]
    fn test_swarm_run_params_full() {
        let json = serde_json::json!({
            "golden_workspace": "proj",
            "snapshot_name": "base",
            "tasks": [
                {"name": "t1", "command": "echo hi", "workdir": "/app"},
                {"name": "t2", "command": "make test"}
            ],
            "env_vars": {"API_KEY": "secret123"},
            "merge_strategy": "sequential",
            "merge_target": "proj",
            "max_parallel": 4,
            "timeout_secs": 300,
            "cleanup": false
        });
        let params: SwarmRunParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.golden_workspace, "proj");
        assert_eq!(params.snapshot_name, "base");
        assert_eq!(params.tasks.len(), 2);
        assert_eq!(params.tasks[0].name, "t1");
        assert_eq!(params.tasks[0].command, "echo hi");
        assert_eq!(params.tasks[0].workdir.as_deref(), Some("/app"));
        assert_eq!(params.tasks[1].name, "t2");
        assert_eq!(params.tasks[1].command, "make test");
        assert!(params.tasks[1].workdir.is_none());
        let env = params.env_vars.as_ref().unwrap();
        assert_eq!(env.get("API_KEY").unwrap(), "secret123");
        assert_eq!(params.merge_strategy.as_deref(), Some("sequential"));
        assert_eq!(params.merge_target.as_deref(), Some("proj"));
        assert_eq!(params.max_parallel, Some(4));
        assert_eq!(params.timeout_secs, Some(300));
        assert_eq!(params.cleanup, Some(false));
    }

    #[test]
    fn test_swarm_run_params_with_vault_context() {
        let json = serde_json::json!({
            "golden_workspace": "proj",
            "snapshot_name": "base",
            "tasks": [{"name": "t1", "command": "echo hi"}],
            "vault_context": [
                {"kind": "search", "query": "auth patterns"},
                {"kind": "read", "query": "conventions/rust.md"}
            ]
        });
        let params: SwarmRunParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.vault_context.len(), 2);
        assert_eq!(params.vault_context[0].kind, "search");
        assert_eq!(params.vault_context[0].query, "auth patterns");
        assert_eq!(params.vault_context[1].kind, "read");
        assert_eq!(params.vault_context[1].query, "conventions/rust.md");
    }

    #[test]
    fn test_swarm_run_params_with_shared_context() {
        let json = serde_json::json!({
            "golden_workspace": "proj",
            "snapshot_name": "base",
            "tasks": [{"name": "t1", "command": "cat /tmp/.shared-context"}],
            "shared_context": "# Project Guidelines\n\nUse Rust 2021 edition.\nAll errors use anyhow."
        });
        let params: SwarmRunParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.shared_context.as_deref(), Some("# Project Guidelines\n\nUse Rust 2021 edition.\nAll errors use anyhow."));
        assert!(params.env_vars.is_none());
        assert!(params.vault_context.is_empty());
    }

    #[test]
    fn test_swarm_run_params_shared_context_empty_string() {
        let json = serde_json::json!({
            "golden_workspace": "proj",
            "snapshot_name": "base",
            "tasks": [{"name": "t1", "command": "echo hi"}],
            "shared_context": ""
        });
        let params: SwarmRunParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.shared_context.as_deref(), Some(""));
    }

    #[test]
    fn test_swarm_run_params_shared_and_vault_context() {
        let json = serde_json::json!({
            "golden_workspace": "proj",
            "snapshot_name": "base",
            "tasks": [{"name": "t1", "command": "echo hi"}],
            "shared_context": "shared docs here",
            "vault_context": [{"kind": "read", "query": "notes/arch.md"}]
        });
        let params: SwarmRunParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.shared_context.as_deref(), Some("shared docs here"));
        assert_eq!(params.vault_context.len(), 1);
    }

    #[test]
    fn test_swarm_vault_query_default_kind() {
        let json = serde_json::json!({"query": "something"});
        let vq: SwarmVaultQuery = serde_json::from_value(json).unwrap();
        assert_eq!(vq.kind, "search");
        assert_eq!(vq.query, "something");
    }

    #[test]
    fn test_swarm_vault_query_explicit_read() {
        let json = serde_json::json!({"kind": "read", "query": "notes/design.md"});
        let vq: SwarmVaultQuery = serde_json::from_value(json).unwrap();
        assert_eq!(vq.kind, "read");
        assert_eq!(vq.query, "notes/design.md");
    }

    #[tokio::test]
    async fn test_resolve_swarm_vault_context_search() {
        use crate::config::VaultConfig;
        use super::super::vault::VaultManager;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("auth-guide.md"),
            "# Auth Guide\n\nUse JWT tokens for auth patterns.\n",
        )
        .await
        .unwrap();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&cfg).unwrap();

        let queries = vec![SwarmVaultQuery {
            kind: "search".to_string(),
            query: "auth patterns".to_string(),
        }];

        let result = resolve_swarm_vault_context(&vm, &queries).await;
        assert!(result.contains("## Project Knowledge Base"));
        assert!(result.contains("auth-guide.md"));
    }

    #[tokio::test]
    async fn test_resolve_swarm_vault_context_read() {
        use crate::config::VaultConfig;
        use super::super::vault::VaultManager;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let root = dir.path();
        tokio::fs::write(
            root.join("conventions.md"),
            "# Rust Conventions\n\nUse snake_case.\n",
        )
        .await
        .unwrap();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&cfg).unwrap();

        let queries = vec![SwarmVaultQuery {
            kind: "read".to_string(),
            query: "conventions.md".to_string(),
        }];

        let result = resolve_swarm_vault_context(&vm, &queries).await;
        assert!(result.contains("## Project Knowledge Base"));
        assert!(result.contains("Use snake_case."));
    }

    #[tokio::test]
    async fn test_resolve_swarm_vault_context_empty_on_no_results() {
        use crate::config::VaultConfig;
        use super::super::vault::VaultManager;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let cfg = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&cfg).unwrap();

        let queries = vec![SwarmVaultQuery {
            kind: "search".to_string(),
            query: "nonexistent-term-xyz".to_string(),
        }];

        let result = resolve_swarm_vault_context(&vm, &queries).await;
        assert!(result.is_empty());
    }

}
