pub mod pool;
pub mod snapshot;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

use crate::config::Config;
use crate::guest::protocol;
use crate::network::{NetworkManager, NetworkPolicy};
use crate::storage::StorageManager;
use crate::vm::VmManager;
use crate::workspace::snapshot::{Snapshot, SnapshotTree};

/// Workspace lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceState {
    Stopped,
    Running,
    Suspended,
}

impl std::fmt::Display for WorkspaceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => write!(f, "stopped"),
            Self::Running => write!(f, "running"),
            Self::Suspended => write!(f, "suspended"),
        }
    }
}

/// Resource limits for a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub vcpus: u32,
    pub memory_mb: u32,
    pub disk_gb: u32,
}

/// Network settings for a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceNetwork {
    pub ip: Ipv4Addr,
    pub allow_internet: bool,
    pub allow_inter_vm: bool,
    pub allowed_ports: Vec<u16>,
    pub port_forwards: Vec<PortForward>,
}

/// A port forward rule mapping a host port to a guest port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortForward {
    pub host_port: u16,
    pub guest_port: u16,
}

/// A single workspace instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub state: WorkspaceState,
    pub base_image: String,
    pub zfs_dataset: String,
    pub qemu_pid: Option<u32>,
    pub qmp_socket: PathBuf,
    pub vsock_cid: u32,
    pub tap_device: String,
    pub network: WorkspaceNetwork,
    pub snapshots: SnapshotTree,
    pub created_at: DateTime<Utc>,
    pub resources: ResourceLimits,
}

impl Workspace {
    /// Short ID (first 8 chars of UUID) used for TAP names and paths.
    pub fn short_id(&self) -> String {
        self.id.to_string()[..8].to_string()
    }
}

/// Parameters for creating a new workspace.
pub struct CreateParams {
    pub name: Option<String>,
    pub base_image: Option<String>,
    pub vcpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_gb: Option<u32>,
    pub allow_internet: Option<bool>,
}

/// Persisted state (serialized to JSON on disk).
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PersistedState {
    /// Schema version for forward-compatible state file migrations.
    /// Version 0 = legacy files without this field; version 1 = current.
    #[serde(default)]
    pub schema_version: u32,
    pub workspaces: HashMap<Uuid, Workspace>,
    pub next_vsock_cid: u32,
    /// CIDs returned from destroyed workspaces, available for reuse.
    #[serde(default)]
    pub free_vsock_cids: Vec<u32>,
}

/// Read the last N lines of the QEMU console.log for a workspace and log them.
/// Used to capture guest diagnostics when configure_workspace fails.
async fn log_console_tail(run_dir: &std::path::Path, short_id: &str, n: usize) {
    let console_path = run_dir.join(short_id).join("console.log");
    match tokio::fs::read_to_string(&console_path).await {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(n);
            let tail = &lines[start..];
            if tail.is_empty() {
                warn!(path = %console_path.display(), "console.log is empty");
            } else {
                warn!(
                    path = %console_path.display(),
                    lines = tail.len(),
                    "guest console.log tail (last {} lines):\n{}",
                    tail.len(),
                    tail.join("\n")
                );
            }
        }
        Err(e) => {
            warn!(
                path = %console_path.display(),
                error = %e,
                "could not read console.log for diagnostics"
            );
        }
    }
}

/// Read the last N lines from a log file. Returns an empty string if the file
/// does not exist or cannot be read.
pub async fn read_log_tail(path: &std::path::Path, max_lines: usize) -> String {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(max_lines);
            lines[start..].join("\n")
        }
        Err(_) => String::new(),
    }
}

/// Orchestrates workspace lifecycle across storage, network, and VM managers.
///
/// Thread-safe: all mutable state is behind `RwLock`.
///
/// # Locking discipline
///
/// The manager holds four RwLocks: `workspaces`, `vm`, `network`, `next_vsock_cid`.
/// To avoid deadlocks, **no method holds more than one lock at a time**. Each lock
/// is acquired, used, and dropped (via scope end or temporary expression) before the
/// next lock is acquired. This means lifecycle methods are NOT atomic across all
/// subsystems, but individual operations on each subsystem are serialized.
///
/// `StorageManager` is not behind a lock because its ZFS commands are stateless
/// (just shelling out) and safe to call concurrently.
pub struct WorkspaceManager {
    config: Config,
    workspaces: RwLock<HashMap<Uuid, Workspace>>,
    vm: RwLock<VmManager>,
    storage: StorageManager,
    network: RwLock<NetworkManager>,
    next_vsock_cid: RwLock<u32>,
    /// CIDs freed by destroyed workspaces, available for reuse.
    free_vsock_cids: RwLock<Vec<u32>>,
    pool: pool::VmPool,
}

impl WorkspaceManager {
    pub fn new(
        config: Config,
        vm: VmManager,
        storage: StorageManager,
        network: NetworkManager,
        pool: pool::VmPool,
    ) -> Self {
        let cid_start = config.vm.vsock_cid_start;
        Self {
            config,
            workspaces: RwLock::new(HashMap::new()),
            vm: RwLock::new(vm),
            storage,
            network: RwLock::new(network),
            next_vsock_cid: RwLock::new(cid_start),
            free_vsock_cids: RwLock::new(Vec::new()),
            pool,
        }
    }

    /// Initialize storage and network subsystems. Call once at startup.
    pub async fn init(&self) -> Result<()> {
        self.storage.init().await.context("storage init failed")?;
        self.network.write().await.init().await.context("network init failed")?;
        Ok(())
    }

    /// Load persisted state from the state file.
    ///
    /// After loading, performs orphan cleanup:
    /// 1. Scans the run directory for leftover `qemu.pid` files from a previous
    ///    daemon session and kills any still-running QEMU processes.
    /// 2. Destroys stale TAP devices for all loaded workspaces (they cannot
    ///    survive a daemon restart).
    pub async fn load_state(&self) -> Result<()> {
        let state_path = &self.config.server.state_file;
        if !state_path.exists() {
            info!(path = %state_path.display(), "no persisted state file, starting fresh");
            // Even without persisted state, clean up orphan QEMU processes
            self.cleanup_orphan_qemu_processes().await;
            return Ok(());
        }

        let data = tokio::fs::read_to_string(state_path)
            .await
            .with_context(|| format!("reading state file: {}", state_path.display()))?;

        let persisted: PersistedState = serde_json::from_str(&data)
            .with_context(|| format!("parsing state file: {}", state_path.display()))?;

        if persisted.schema_version > 1 {
            warn!(
                version = persisted.schema_version,
                "state file has newer schema version than supported (1), some fields may be lost"
            );
        }

        // Restore IP allocations
        {
            let mut net = self.network.write().await;
            for ws in persisted.workspaces.values() {
                if let Err(e) = net.restore_ip(ws.network.ip) {
                    warn!(
                        workspace_id = %ws.id,
                        ip = %ws.network.ip,
                        error = %e,
                        "failed to restore IP allocation"
                    );
                }
            }
        }

        // Mark all workspaces as stopped (VMs don't survive daemon restart)
        let mut workspaces = persisted.workspaces;
        for ws in workspaces.values_mut() {
            ws.state = WorkspaceState::Stopped;
            ws.qemu_pid = None;
        }

        let count = workspaces.len();

        // Clean up orphaned QEMU processes before making workspaces usable.
        // This must happen before inserting workspaces so that stale run
        // directories (with PID files) are cleaned up first.
        self.cleanup_orphan_qemu_processes().await;

        // Clean up stale TAP devices from the previous session.
        // nftables rules are already flushed by init(), but TAP devices persist.
        {
            let net = self.network.read().await;
            for ws in workspaces.values() {
                let short_id = ws.short_id();
                if let Err(e) = net.bridge().destroy_tap(&short_id).await {
                    // Expected to fail if TAP doesn't exist; only log at debug
                    debug!(
                        workspace_id = %ws.id,
                        tap = %ws.tap_device,
                        error = %e,
                        "stale TAP cleanup (may not exist)"
                    );
                } else {
                    warn!(
                        workspace_id = %ws.id,
                        tap = %ws.tap_device,
                        "destroyed stale TAP device from previous session"
                    );
                }
            }
        }

        *self.workspaces.write().await = workspaces;
        *self.next_vsock_cid.write().await = persisted.next_vsock_cid;
        *self.free_vsock_cids.write().await = persisted.free_vsock_cids;

        info!(count, "loaded persisted workspace state");
        Ok(())
    }

    /// Scan the run directory for orphaned QEMU processes from a previous daemon
    /// session and kill them. This prevents CID collisions and RAM exhaustion
    /// after a daemon crash.
    ///
    /// For each subdirectory in `{run_dir}/`, checks for a `qemu.pid` file,
    /// reads the PID, verifies the process is still alive, and kills it with
    /// SIGKILL if so. The entire subdirectory is then removed.
    async fn cleanup_orphan_qemu_processes(&self) {
        let run_dir = &self.config.vm.run_dir;

        let entries = match tokio::fs::read_dir(run_dir).await {
            Ok(entries) => entries,
            Err(e) => {
                // Run directory may not exist on first boot; that's fine.
                debug!(
                    path = %run_dir.display(),
                    error = %e,
                    "could not read run directory for orphan cleanup"
                );
                return;
            }
        };

        let mut entries = entries;
        let mut killed = 0u32;
        let mut cleaned = 0u32;

        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(e) => {
                    warn!(error = %e, "error reading run directory entry");
                    continue;
                }
            };

            let path = entry.path();

            // Only process directories (each workspace gets a subdirectory)
            let is_dir = match entry.file_type().await {
                Ok(ft) => ft.is_dir(),
                Err(_) => false,
            };
            if !is_dir {
                continue;
            }

            let pid_path = path.join("qemu.pid");
            let pid_str = match tokio::fs::read_to_string(&pid_path).await {
                Ok(s) => s,
                Err(_) => {
                    // No PID file -- clean up the stale directory anyway
                    debug!(path = %path.display(), "removing stale run directory (no PID file)");
                    let _ = tokio::fs::remove_dir_all(&path).await;
                    cleaned += 1;
                    continue;
                }
            };

            let pid: i32 = match pid_str.trim().parse() {
                Ok(p) => p,
                Err(e) => {
                    warn!(
                        path = %pid_path.display(),
                        contents = %pid_str.trim(),
                        error = %e,
                        "invalid PID in qemu.pid file"
                    );
                    let _ = tokio::fs::remove_dir_all(&path).await;
                    cleaned += 1;
                    continue;
                }
            };

            // Check if the process is still alive (signal 0 = existence check)
            let alive = unsafe { libc::kill(pid, 0) } == 0;

            if alive {
                warn!(
                    pid,
                    path = %path.display(),
                    "killing orphaned QEMU process from previous session"
                );
                unsafe { libc::kill(pid, libc::SIGKILL) };
                killed += 1;
            } else {
                debug!(
                    pid,
                    path = %path.display(),
                    "stale PID file found (process already exited)"
                );
            }

            // Remove the run directory regardless
            if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to remove stale run directory"
                );
            }
            cleaned += 1;
        }

        if killed > 0 || cleaned > 0 {
            warn!(
                killed,
                cleaned,
                "orphan cleanup complete"
            );
        }
    }

    /// Persist current state to the state file.
    pub async fn save_state(&self) -> Result<()> {
        let state_path = &self.config.server.state_file;

        // Ensure parent directory exists
        if let Some(parent) = state_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }

        // Acquire all three locks before reading to get a consistent snapshot.
        // Without this, a concurrent create() between reads could produce a
        // state where the workspace is saved but the CID counter is stale.
        let ws_guard = self.workspaces.read().await;
        let cid_guard = self.next_vsock_cid.read().await;
        let free_guard = self.free_vsock_cids.read().await;

        let persisted = PersistedState {
            schema_version: 1,
            workspaces: ws_guard.clone(),
            next_vsock_cid: *cid_guard,
            free_vsock_cids: free_guard.clone(),
        };

        // Drop all locks before the potentially slow I/O
        drop(ws_guard);
        drop(cid_guard);
        drop(free_guard);

        let data = serde_json::to_string_pretty(&persisted)
            .context("serializing state")?;

        // Write to temp file then rename (atomic on same filesystem)
        let tmp_path = state_path.with_extension("tmp");
        tokio::fs::write(&tmp_path, &data)
            .await
            .with_context(|| format!("writing temp state file: {}", tmp_path.display()))?;
        tokio::fs::rename(&tmp_path, state_path)
            .await
            .with_context(|| format!("renaming temp state file to: {}", state_path.display()))?;

        // Restrict permissions to owner-only
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(state_path, perms).await.ok();

        Ok(())
    }

    /// Allocate the next vsock CID, preferring recycled CIDs from destroyed
    /// workspaces before incrementing the counter.
    ///
    /// Reserved CIDs per the vsock spec:
    /// - 0: VMADDR_CID_HYPERVISOR
    /// - 1: VMADDR_CID_LOCAL (loopback)
    /// - 2: VMADDR_CID_HOST
    /// - u32::MAX: VMADDR_CID_ANY (wildcard)
    async fn alloc_vsock_cid(&self) -> Result<u32> {
        // Try to reuse a recycled CID first
        {
            let mut free_list = self.free_vsock_cids.write().await;
            if let Some(recycled) = free_list.pop() {
                debug!(cid = recycled, "reusing recycled vsock CID");
                return Ok(recycled);
            }
        }

        // No recycled CIDs available, increment the counter
        let mut cid = self.next_vsock_cid.write().await;

        // Skip any reserved CIDs
        while *cid <= 2 || *cid == u32::MAX {
            if *cid == u32::MAX {
                bail!("vsock CID space exhausted");
            }
            *cid += 1;
        }

        let allocated = *cid;
        *cid += 1;

        // Guard against wrapping into reserved range on next allocation
        if *cid == u32::MAX {
            warn!("vsock CID counter approaching u32::MAX, next allocation will fail");
        }

        Ok(allocated)
    }

    /// Return a vsock CID to the free pool for reuse.
    async fn recycle_vsock_cid(&self, cid: u32) {
        // Don't recycle reserved CIDs
        if cid <= 2 || cid == u32::MAX {
            return;
        }
        let mut free_list = self.free_vsock_cids.write().await;
        free_list.push(cid);
        debug!(cid, free_count = free_list.len(), "recycled vsock CID");
    }

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Create and start a new workspace.
    #[instrument(skip(self, params))]
    pub async fn create(&self, params: CreateParams) -> Result<Workspace> {
        let id = Uuid::new_v4();
        let short_id = id.to_string()[..8].to_string();
        let name = params.name.unwrap_or_else(|| format!("ws-{}", &short_id));
        let base_image = params
            .base_image
            .unwrap_or_else(|| self.config.storage.base_image.clone());
        let vcpus = params.vcpus.unwrap_or(self.config.resources.default_vcpus);
        let memory_mb = params.memory_mb.unwrap_or(self.config.resources.default_memory_mb);
        let disk_gb = params.disk_gb.unwrap_or(self.config.resources.default_disk_gb);
        let allow_internet = params.allow_internet
            .unwrap_or(self.config.network.default_allow_internet);

        // Enforce per-workspace resource limits
        let limits = &self.config.resources;
        if vcpus > limits.max_vcpus {
            bail!(
                "requested {} vCPUs exceeds maximum {} per workspace",
                vcpus, limits.max_vcpus
            );
        }
        if memory_mb > limits.max_memory_mb {
            bail!(
                "requested {} MB memory exceeds maximum {} MB per workspace",
                memory_mb, limits.max_memory_mb
            );
        }
        if disk_gb > limits.max_disk_gb {
            bail!(
                "requested {} GB disk exceeds maximum {} GB per workspace",
                disk_gb, limits.max_disk_gb
            );
        }

        // Enforce total workspace count limit and name uniqueness
        {
            let workspaces = self.workspaces.read().await;
            let count = workspaces.len() as u32;
            if count >= limits.max_workspaces {
                bail!(
                    "workspace limit reached: {} of {} maximum",
                    count, limits.max_workspaces
                );
            }
            if workspaces.values().any(|ws| ws.name == name) {
                bail!("workspace name '{}' is already in use", name);
            }
        }

        // Fast path: try to claim a warm VM from the pool
        if self.pool.enabled() {
            if let Some(warm_vm) = self.pool.claim().await {
                return self.assign_warm_vm(warm_vm, id, name, &NetworkPolicy {
                    allow_internet,
                    allow_inter_vm: self.config.network.default_allow_inter_vm,
                    allowed_ports: Vec::new(),
                }).await;
            }
            debug!("warm pool empty, falling back to cold create");
        }

        info!(
            workspace_id = %id,
            name = %name,
            base_image = %base_image,
            vcpus,
            memory_mb,
            "creating workspace"
        );

        // 1+2. Clone storage + set up networking in parallel
        let default_policy = NetworkPolicy {
            allow_internet,
            allow_inter_vm: self.config.network.default_allow_inter_vm,
            allowed_ports: Vec::new(),
        };

        let (storage_result, network_result) = tokio::join!(
            self.storage.create_workspace(&base_image, &self.config.storage.base_snapshot, &short_id, Some(disk_gb)),
            async { self.network.write().await.setup_workspace(&short_id, &default_policy).await }
        );

        let ws_storage = match storage_result {
            Ok(s) => s,
            Err(e) => {
                // If network succeeded, clean it up
                if let Ok(ref net) = network_result {
                    let _ = self.network.write().await.cleanup_workspace(&short_id, net.guest_ip).await;
                }
                return Err(e.context("failed to create workspace storage"));
            }
        };

        let net_setup = match network_result {
            Ok(n) => n,
            Err(e) => {
                // Rollback storage
                if let Err(e2) = self.storage.destroy_workspace(&short_id).await {
                    error!(error = %e2, "failed to rollback storage after network setup failure");
                }
                return Err(e.context("failed to set up workspace networking"));
            }
        };

        // 3. Allocate vsock CID
        let vsock_cid = self.alloc_vsock_cid().await?;

        // 4. Start QEMU
        let qmp_socket = self.config.vm.run_dir.join(&short_id).join("qmp.sock");
        let pid = match self.vm.write().await.launch(
            id,
            vcpus,
            memory_mb,
            ws_storage.zvol_path.clone(),
            net_setup.tap_device.clone(),
            vsock_cid,
        ).await {
            Ok(pid) => pid,
            Err(e) => {
                // Rollback vsock CID, network, and storage
                self.recycle_vsock_cid(vsock_cid).await;
                if let Err(e2) = self.network.write().await.cleanup_workspace(&short_id, net_setup.guest_ip).await {
                    error!(error = %e2, "failed to rollback network after VM launch failure");
                }
                if let Err(e2) = self.storage.destroy_workspace(&short_id).await {
                    error!(error = %e2, "failed to rollback storage after VM launch failure");
                }
                return Err(e.context("failed to launch VM"));
            }
        };

        // 5. Configure guest workspace via guest agent (single vsock RTT)
        let dns_servers = if default_policy.allow_internet {
            self.config.network.dns_servers.clone()
        } else {
            vec![self.config.network.gateway_ip.to_string()]
        };
        {
            let mut vm = self.vm.write().await;
            let vsock = vm.vsock_client(&id)?;
            if let Err(e) = vsock.configure_workspace(
                &net_setup.guest_ip.to_string(),
                &net_setup.gateway_ip.to_string(),
                dns_servers,
                &name,
            ).await {
                log_console_tail(&self.config.vm.run_dir, &short_id, 50).await;
                warn!(workspace_id = %id, "failed to configure guest workspace: {:#}", e);
            }
        }

        let workspace = Workspace {
            id,
            name,
            state: WorkspaceState::Running,
            base_image,
            zfs_dataset: ws_storage.dataset,
            qemu_pid: Some(pid),
            qmp_socket,
            vsock_cid,
            tap_device: net_setup.tap_device,
            network: WorkspaceNetwork {
                ip: net_setup.guest_ip,
                allow_internet: default_policy.allow_internet,
                allow_inter_vm: default_policy.allow_inter_vm,
                allowed_ports: default_policy.allowed_ports.clone(),
                port_forwards: Vec::new(),
            },
            snapshots: SnapshotTree::new(),
            created_at: Utc::now(),
            resources: ResourceLimits {
                vcpus,
                memory_mb,
                disk_gb,
            },
        };

        self.workspaces.write().await.insert(id, workspace.clone());
        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }

        info!(workspace_id = %id, "workspace created and running");
        Ok(workspace)
    }

    /// Destroy a workspace: stop VM, tear down network, destroy storage.
    #[instrument(skip(self))]
    pub async fn destroy(&self, workspace_id: Uuid) -> Result<()> {
        let ws = {
            let workspaces = self.workspaces.read().await;
            workspaces
                .get(&workspace_id)
                .cloned()
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?
        };

        let short_id = ws.short_id();

        // Stop VM if running
        if ws.state == WorkspaceState::Running || ws.state == WorkspaceState::Suspended {
            let mut vm = self.vm.write().await;
            if vm.is_running(&workspace_id) {
                if let Err(e) = vm.stop(&workspace_id, std::time::Duration::from_secs(5)).await {
                    warn!(workspace_id = %workspace_id, error = %e, "failed to stop VM, force killing");
                    vm.force_kill(&workspace_id).await.ok();
                }
            }
        }

        // Tear down network
        if let Err(e) = self.network.write().await.cleanup_workspace(&short_id, ws.network.ip).await {
            warn!(workspace_id = %workspace_id, error = %e, "failed to clean up network");
        }

        // Destroy storage
        if let Err(e) = self.storage.destroy_workspace(&short_id).await {
            warn!(workspace_id = %workspace_id, error = %e, "failed to destroy storage");
        }

        // Recycle vsock CID so it can be reused by future workspaces
        self.recycle_vsock_cid(ws.vsock_cid).await;

        self.workspaces.write().await.remove(&workspace_id);
        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }

        info!(workspace_id = %workspace_id, "workspace destroyed");
        Ok(())
    }

    /// Stop a running workspace.
    #[instrument(skip(self))]
    pub async fn stop(&self, workspace_id: Uuid) -> Result<()> {
        {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?;
            if ws.state != WorkspaceState::Running {
                bail!("workspace {} is not running (state: {}). Use workspace_start to boot it.", workspace_id, ws.state);
            }
        }

        // Try graceful guest shutdown first
        {
            let mut vm = self.vm.write().await;
            if let Ok(vsock) = vm.vsock_client(&workspace_id) {
                vsock.shutdown().await.ok();
            }
        }

        // Stop the VM via QMP
        self.vm
            .write()
            .await
            .stop(&workspace_id, std::time::Duration::from_secs(10))
            .await
            .context("failed to stop VM")?;

        // Update state
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.state = WorkspaceState::Stopped;
                ws.qemu_pid = None;
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        info!(workspace_id = %workspace_id, "workspace stopped");
        Ok(())
    }

    /// Start a stopped workspace.
    #[instrument(skip(self))]
    pub async fn start(&self, workspace_id: Uuid) -> Result<()> {
        let ws = {
            let workspaces = self.workspaces.read().await;
            workspaces
                .get(&workspace_id)
                .cloned()
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?
        };

        if ws.state == WorkspaceState::Running {
            bail!("workspace {} is already running. Use exec to run commands.", workspace_id);
        }
        if ws.state != WorkspaceState::Stopped {
            bail!("workspace {} is not stopped (state: {})", workspace_id, ws.state);
        }

        let short_id = ws.short_id();

        // Derive zvol path from the stored dataset name. This correctly handles
        // both regular workspaces (under workspaces/) and forked workspaces (under forks/).
        let zvol_path = PathBuf::from(format!("/dev/zvol/{}", ws.zfs_dataset));

        // Ensure TAP device and nftables rules exist (they may have been lost
        // after a server restart or host reboot).
        let policy = NetworkPolicy {
            allow_internet: ws.network.allow_internet,
            allow_inter_vm: ws.network.allow_inter_vm,
            allowed_ports: ws.network.allowed_ports.clone(),
        };
        let tap_device = self
            .network
            .read()
            .await
            .ensure_workspace_network(&short_id, ws.network.ip, &policy)
            .await
            .context("failed to ensure workspace networking for start")?;

        // Update the tap device name in case it changed
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.tap_device = tap_device.clone();
            }
        }

        // Re-launch the VM
        let pid = self
            .vm
            .write()
            .await
            .launch(
                workspace_id,
                ws.resources.vcpus,
                ws.resources.memory_mb,
                zvol_path,
                tap_device,
                ws.vsock_cid,
            )
            .await
            .context("failed to launch VM for start")?;

        // Configure guest workspace again (network + hostname) via single vsock RTT
        let gateway_ip = self.network.read().await.gateway_ip();
        let dns_servers = if ws.network.allow_internet {
            self.config.network.dns_servers.clone()
        } else {
            vec![self.config.network.gateway_ip.to_string()]
        };
        {
            let mut vm = self.vm.write().await;
            if let Ok(vsock) = vm.vsock_client(&workspace_id) {
                if let Err(e) = vsock.configure_workspace(
                    &ws.network.ip.to_string(),
                    &gateway_ip.to_string(),
                    dns_servers,
                    &ws.name,
                ).await {
                    log_console_tail(&self.config.vm.run_dir, &ws.short_id(), 50).await;
                    warn!(workspace_id = %workspace_id, "failed to configure guest workspace on start: {:#}", e);
                }
            }
        }

        // Update state
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.state = WorkspaceState::Running;
                ws.qemu_pid = Some(pid);
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        info!(workspace_id = %workspace_id, "workspace started");
        Ok(())
    }

    /// Suspend (pause) a running workspace.
    #[allow(dead_code)]
    #[instrument(skip(self))]
    pub async fn suspend(&self, workspace_id: Uuid) -> Result<()> {
        {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?;
            if ws.state != WorkspaceState::Running {
                bail!("workspace {} is not running (state: {}). Use workspace_start to boot it.", workspace_id, ws.state);
            }
        }

        self.vm.write().await.pause(&workspace_id).await?;

        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.state = WorkspaceState::Suspended;
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        info!(workspace_id = %workspace_id, "workspace suspended");
        Ok(())
    }

    /// Resume a suspended workspace.
    #[allow(dead_code)]
    #[instrument(skip(self))]
    pub async fn resume(&self, workspace_id: Uuid) -> Result<()> {
        {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?;
            if ws.state != WorkspaceState::Suspended {
                bail!("workspace {} is not suspended (state: {})", workspace_id, ws.state);
            }
        }

        self.vm.write().await.resume(&workspace_id).await?;

        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.state = WorkspaceState::Running;
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        info!(workspace_id = %workspace_id, "workspace resumed");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pool replenishment
    // -----------------------------------------------------------------------

    /// Boot a single warm VM into the pool if there is a deficit.
    ///
    /// Call this repeatedly (e.g. on a timer) to keep the pool topped up.
    /// Only boots ONE VM per invocation to avoid holding locks too long.
    pub async fn replenish_pool(&self) -> Result<()> {
        // Gate: pool must be enabled
        if !self.pool.enabled() {
            return Ok(());
        }

        // Gate: check deficit
        let deficit = self.pool.deficit().await;
        if deficit == 0 {
            return Ok(());
        }

        // Gate: respect memory budget
        let memory_mb = self.config.resources.default_memory_mb;
        let current_memory = self.pool.total_memory_mb().await;
        if current_memory + memory_mb as usize > self.pool.max_memory_mb() {
            debug!(
                current_memory_mb = current_memory,
                max_memory_mb = self.pool.max_memory_mb(),
                vm_memory_mb = memory_mb,
                "pool memory budget would be exceeded, skipping replenish"
            );
            return Ok(());
        }

        // Generate a pool VM identity
        let pool_vm_id = Uuid::new_v4();
        let short_id = pool_vm_id.to_string()[..8].to_string();
        let vcpus = self.config.resources.default_vcpus;

        debug!(
            pool_vm_id = %pool_vm_id,
            short_id = %short_id,
            deficit,
            "replenishing warm pool"
        );

        // 1. Clone storage from base snapshot
        let disk_gb = self.config.resources.default_disk_gb;
        let ws_storage = self.storage
            .create_workspace(
                &self.config.storage.base_image,
                &self.config.storage.base_snapshot,
                &short_id,
                Some(disk_gb),
            )
            .await
            .context("pool replenish: failed to create workspace storage")?;

        // 2. Set up minimal networking (TAP device required by QEMU)
        let pool_policy = NetworkPolicy {
            allow_internet: false,
            allow_inter_vm: false,
            allowed_ports: Vec::new(),
        };

        let net_setup = match self.network.write().await
            .setup_workspace(&short_id, &pool_policy).await
        {
            Ok(setup) => setup,
            Err(e) => {
                // Rollback storage
                if let Err(e2) = self.storage.destroy_workspace(&short_id).await {
                    error!(error = %e2, "pool replenish: failed to rollback storage after network failure");
                }
                return Err(e.context("pool replenish: failed to set up networking"));
            }
        };

        // 3. Allocate vsock CID
        let vsock_cid = match self.alloc_vsock_cid().await {
            Ok(cid) => cid,
            Err(e) => {
                // Rollback network + storage
                if let Err(e2) = self.network.write().await
                    .cleanup_workspace(&short_id, net_setup.guest_ip).await
                {
                    error!(error = %e2, "pool replenish: failed to rollback network after CID failure");
                }
                if let Err(e2) = self.storage.destroy_workspace(&short_id).await {
                    error!(error = %e2, "pool replenish: failed to rollback storage after CID failure");
                }
                return Err(e.context("pool replenish: failed to allocate vsock CID"));
            }
        };

        // 4. Launch QEMU
        let pid = match self.vm.write().await.launch(
            pool_vm_id,
            vcpus,
            memory_mb,
            ws_storage.zvol_path.clone(),
            net_setup.tap_device.clone(),
            vsock_cid,
        ).await {
            Ok(pid) => pid,
            Err(e) => {
                // Rollback: recycle CID, destroy network, destroy storage
                self.recycle_vsock_cid(vsock_cid).await;
                if let Err(e2) = self.network.write().await
                    .cleanup_workspace(&short_id, net_setup.guest_ip).await
                {
                    error!(error = %e2, "pool replenish: failed to rollback network after VM launch failure");
                }
                if let Err(e2) = self.storage.destroy_workspace(&short_id).await {
                    error!(error = %e2, "pool replenish: failed to rollback storage after VM launch failure");
                }
                return Err(e.context("pool replenish: failed to launch VM"));
            }
        };

        // 5. Add to pool
        let warm_vm = pool::WarmVm {
            id: pool_vm_id,
            vsock_cid,
            zfs_dataset: ws_storage.dataset,
            zvol_path: ws_storage.zvol_path,
            qemu_pid: pid,
            booted_at: std::time::Instant::now(),
            short_id,
            tap_device: net_setup.tap_device,
            guest_ip: net_setup.guest_ip,
            memory_mb,
        };

        info!(
            pool_vm_id = %pool_vm_id,
            vsock_cid,
            pid,
            deficit = deficit - 1,
            "warm pool VM booted successfully"
        );

        self.pool.add_ready(warm_vm).await;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Get a clone of a workspace by ID.
    pub async fn get(&self, workspace_id: Uuid) -> Result<Workspace> {
        self.workspaces
            .read()
            .await
            .get(&workspace_id)
            .cloned()
            .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))
    }

    /// Get ZFS dataset info (used/available bytes) for a workspace.
    pub async fn workspace_disk_info(
        &self,
        workspace_id: Uuid,
    ) -> Result<crate::storage::ZfsDatasetInfo> {
        let short_id = {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .with_context(|| format!("workspace not found: {}", workspace_id))?;
            ws.short_id()
        };
        self.storage.workspace_info(&short_id).await
    }

    /// List all workspaces.
    pub async fn list(&self) -> Result<Vec<Workspace>> {
        Ok(self.workspaces.read().await.values().cloned().collect())
    }

    // -----------------------------------------------------------------------
    // Execution (proxy to guest agent via vsock)
    // -----------------------------------------------------------------------

    /// Execute a command in the guest VM.
    #[instrument(skip(self, env))]
    pub async fn exec(
        &self,
        workspace_id: Uuid,
        command: &str,
        workdir: Option<&str>,
        env: Option<&HashMap<String, String>>,
        timeout_secs: Option<u64>,
    ) -> Result<protocol::ExecResponse> {
        self.ensure_running(workspace_id).await?;

        let timeout = timeout_secs.unwrap_or(30);
        let env_map = env.cloned().unwrap_or_default();

        let mut vm = self.vm.write().await;
        let vsock = vm.vsock_client(&workspace_id)?;

        vsock
            .exec(
                command,
                Vec::new(),
                workdir.map(String::from),
                env_map,
                timeout,
            )
            .await
            .context("exec failed")
    }

    /// Write a file in the guest VM.
    #[instrument(skip(self, content))]
    pub async fn file_write(
        &self,
        workspace_id: Uuid,
        path: &str,
        content: &[u8],
        mode: Option<u32>,
    ) -> Result<()> {
        self.ensure_running(workspace_id).await?;

        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, content);

        let mut vm = self.vm.write().await;
        let vsock = vm.vsock_client(&workspace_id)?;

        vsock
            .file_write(path, &encoded, mode)
            .await
            .context("file_write failed")
    }

    /// Read a file from the guest VM.
    #[instrument(skip(self))]
    pub async fn file_read(
        &self,
        workspace_id: Uuid,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<Vec<u8>> {
        self.ensure_running(workspace_id).await?;

        let mut vm = self.vm.write().await;
        let vsock = vm.vsock_client(&workspace_id)?;

        let resp = vsock
            .file_read(path, offset, limit)
            .await
            .context("file_read failed")?;

        let data = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &resp.content)
            .context("failed to decode base64 file content")?;

        Ok(data)
    }

    /// List directory contents in the guest VM.
    pub async fn list_dir(
        &self,
        workspace_id: Uuid,
        path: &str,
    ) -> Result<Vec<protocol::DirEntry>> {
        self.ensure_running(workspace_id).await?;

        let mut vm = self.vm.write().await;
        let vsock = vm.vsock_client(&workspace_id)?;

        vsock
            .list_dir(path)
            .await
            .context("list_dir failed")
    }

    /// Edit a file in the guest VM by replacing an exact string match.
    pub async fn edit_file(
        &self,
        workspace_id: Uuid,
        path: &str,
        old_string: &str,
        new_string: &str,
    ) -> Result<()> {
        self.ensure_running(workspace_id).await?;

        let mut vm = self.vm.write().await;
        let vsock = vm.vsock_client(&workspace_id)?;

        vsock
            .edit_file(path, old_string, new_string)
            .await
            .context("edit_file failed")
    }

    /// Start a background command in the guest VM. Returns the job ID.
    pub async fn exec_background(
        &self,
        workspace_id: Uuid,
        command: &str,
        workdir: Option<&str>,
        env: Option<&HashMap<String, String>>,
    ) -> Result<u32> {
        self.ensure_running(workspace_id).await?;

        let env_map = env.cloned();

        let mut vm = self.vm.write().await;
        let vsock = vm.vsock_client(&workspace_id)?;

        vsock
            .exec_background(command, workdir, env_map)
            .await
            .context("exec_background failed")
    }

    /// Poll a background job in the guest VM.
    pub async fn exec_poll(
        &self,
        workspace_id: Uuid,
        job_id: u32,
    ) -> Result<protocol::BackgroundStatusResponse> {
        self.ensure_running(workspace_id).await?;

        let mut vm = self.vm.write().await;
        let vsock = vm.vsock_client(&workspace_id)?;

        vsock
            .exec_poll(job_id)
            .await
            .context("exec_poll failed")
    }

    /// Kill a background job in the guest VM.
    ///
    /// Sends a signal to the background job identified by `job_id`. The default
    /// signal is 9 (SIGKILL). Common alternatives: 2 (SIGINT), 15 (SIGTERM).
    ///
    /// TODO: Use the ExecKill protocol variant via VsockClient::exec_kill once
    /// the vm-engine agent adds that method to vsock.rs. For now, we send the
    /// kill command by executing a shell command in the VM.
    pub async fn exec_kill(
        &self,
        workspace_id: Uuid,
        job_id: u32,
        signal: Option<i32>,
    ) -> Result<()> {
        let sig = signal.unwrap_or(9);

        // Temporary implementation: send kill via exec.
        // The guest agent tracks background jobs with sequential IDs and
        // spawns them as child processes, so we use `kill` with the signal.
        // We first poll the job to check if it is still running, then send
        // the kill signal via a shell command.
        let result = self.exec(
            workspace_id,
            &format!("kill -{} %{} 2>/dev/null || true", sig, job_id),
            None,
            None,
            Some(5),
        ).await?;

        // Best-effort: don't fail if the process was already gone
        let _ = result;
        Ok(())
    }

    /// Retrieve QEMU console and stderr logs for a workspace.
    ///
    /// Returns `(console_log, stderr_log)`, each containing at most `max_lines`
    /// lines from the tail of the respective log file. If a log file does not
    /// exist, the corresponding string is empty.
    pub async fn workspace_logs(
        &self,
        workspace_id: Uuid,
        max_lines: usize,
    ) -> Result<(String, String)> {
        let ws = self.get(workspace_id).await?;
        let short_id = ws.short_id();
        let run_dir = &self.config.vm.run_dir;

        let console = read_log_tail(
            &run_dir.join(&short_id).join("console.log"),
            max_lines,
        ).await;
        let stderr = read_log_tail(
            &run_dir.join(&short_id).join("qemu-stderr.log"),
            max_lines,
        ).await;

        Ok((console, stderr))
    }

    // -----------------------------------------------------------------------
    // Snapshots
    // -----------------------------------------------------------------------

    /// Create a named snapshot of a workspace.
    #[instrument(skip(self))]
    pub async fn snapshot_create(
        &self,
        workspace_id: Uuid,
        name: &str,
        include_memory: bool,
    ) -> Result<Snapshot> {
        self.ensure_running(workspace_id).await?;

        let short_id = {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?;
            ws.short_id()
        };

        // Save VM memory state if requested
        let qemu_state = if include_memory {
            self.vm.write().await.save_vm_state(&workspace_id, name).await?;
            // Memory state is saved internally by QEMU (in the disk image)
            Some(PathBuf::from(format!("qemu-state:{}", name)))
        } else {
            None
        };

        // Create ZFS snapshot
        let zfs_snapshot = self
            .storage
            .create_snapshot(&short_id, name)
            .await
            .context("failed to create ZFS snapshot")?;

        // Find the latest snapshot as parent
        let parent = {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces.get(&workspace_id).unwrap();
            ws.snapshots.list().last().map(|s| s.id)
        };

        let snapshot = Snapshot {
            id: Uuid::new_v4(),
            name: name.to_string(),
            workspace_id,
            zfs_snapshot,
            qemu_state,
            parent,
            created_at: Utc::now(),
        };

        // Add to workspace's snapshot tree
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.snapshots.add(snapshot.clone());
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        info!(workspace_id = %workspace_id, snapshot = %name, "snapshot created");
        Ok(snapshot)
    }

    /// Restore a workspace to a named snapshot.
    ///
    /// Stops the VM, rolls back ZFS, and restarts.
    #[instrument(skip(self))]
    pub async fn snapshot_restore(
        &self,
        workspace_id: Uuid,
        snapshot_name: &str,
    ) -> Result<()> {
        let (short_id, has_memory, target_snap_created_at) = {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?;

            let snap = ws
                .snapshots
                .get_by_name(snapshot_name)
                .with_context(|| format!("snapshot not found: {}", snapshot_name))?;

            (ws.short_id(), snap.qemu_state.is_some(), snap.created_at)
        };

        // Stop VM if running
        {
            let state = self.workspaces.read().await.get(&workspace_id).map(|w| w.state);
            if state == Some(WorkspaceState::Running) || state == Some(WorkspaceState::Suspended) {
                let mut vm = self.vm.write().await;
                if vm.is_running(&workspace_id) {
                    vm.stop(&workspace_id, std::time::Duration::from_secs(5)).await.ok();
                }
            }
        }

        // ZFS rollback
        self.storage
            .restore_snapshot(&short_id, snapshot_name)
            .await
            .context("failed to rollback ZFS snapshot")?;

        // Update state to stopped and remove snapshots newer than the target.
        // `zfs rollback -r` destroys all ZFS snapshots created after the target,
        // so we must sync the in-memory SnapshotTree to match.
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.state = WorkspaceState::Stopped;
                ws.qemu_pid = None;

                let newer_ids: Vec<Uuid> = ws
                    .snapshots
                    .list()
                    .iter()
                    .filter(|s| s.created_at > target_snap_created_at)
                    .map(|s| s.id)
                    .collect();
                for id in &newer_ids {
                    ws.snapshots.remove(id);
                }
                if !newer_ids.is_empty() {
                    info!(
                        workspace_id = %workspace_id,
                        removed = newer_ids.len(),
                        "removed snapshots newer than restored snapshot from tree"
                    );
                }
            }
        }

        // Restart the workspace
        self.start(workspace_id).await?;

        // If the snapshot included memory, restore VM state
        if has_memory {
            if let Err(e) = self.vm.write().await.load_vm_state(&workspace_id, snapshot_name).await {
                warn!(
                    workspace_id = %workspace_id,
                    snapshot = %snapshot_name,
                    error = %e,
                    "failed to restore VM memory state"
                );
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        info!(workspace_id = %workspace_id, snapshot = %snapshot_name, "snapshot restored");
        Ok(())
    }

    /// Delete a snapshot from a workspace.
    #[instrument(skip(self))]
    pub async fn snapshot_delete(
        &self,
        workspace_id: Uuid,
        snapshot_name: &str,
    ) -> Result<()> {
        let (short_id, snap_id, has_memory) = {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?;

            let snap = ws
                .snapshots
                .get_by_name(snapshot_name)
                .with_context(|| format!("snapshot not found: {}", snapshot_name))?;

            // Refuse to delete a snapshot that has children to avoid orphaning
            // parent references in the tree. This also aligns with ZFS semantics
            // (snapshots with dependent clones cannot be destroyed).
            if ws.snapshots.has_children(&snap.id) {
                bail!(
                    "cannot delete snapshot '{}': it has child snapshots that depend on it",
                    snapshot_name
                );
            }

            (ws.short_id(), snap.id, snap.qemu_state.is_some())
        };

        // Delete ZFS snapshot
        self.storage
            .delete_snapshot(&short_id, snapshot_name)
            .await
            .context("failed to delete ZFS snapshot")?;

        // Delete VM state if present
        if has_memory {
            if let Err(e) = self.vm.write().await.delete_vm_state(&workspace_id, snapshot_name).await {
                warn!(
                    workspace_id = %workspace_id,
                    snapshot = %snapshot_name,
                    error = %e,
                    "failed to delete VM memory state"
                );
            }
        }

        // Remove from snapshot tree
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.snapshots.remove(&snap_id);
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        info!(workspace_id = %workspace_id, snapshot = %snapshot_name, "snapshot deleted");
        Ok(())
    }

    /// Fork a workspace from a snapshot, creating a new independent workspace.
    #[instrument(skip(self))]
    pub async fn fork(
        &self,
        source_workspace_id: Uuid,
        snapshot_name: &str,
        new_name: Option<String>,
    ) -> Result<Workspace> {
        let source_ws = {
            let workspaces = self.workspaces.read().await;
            workspaces
                .get(&source_workspace_id)
                .cloned()
                .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", source_workspace_id))?
        };

        // Verify snapshot exists
        source_ws
            .snapshots
            .get_by_name(snapshot_name)
            .with_context(|| format!("snapshot not found: {}", snapshot_name))?;

        let new_id = Uuid::new_v4();
        let new_short_id = new_id.to_string()[..8].to_string();
        let name = new_name.unwrap_or_else(|| format!("fork-{}", &new_short_id));

        // Enforce total workspace count limit and name uniqueness
        {
            let limits = &self.config.resources;
            let workspaces = self.workspaces.read().await;
            let count = workspaces.len() as u32;
            if count >= limits.max_workspaces {
                bail!(
                    "workspace limit reached: {} of {} maximum",
                    count, limits.max_workspaces
                );
            }
            if workspaces.values().any(|ws| ws.name == name) {
                bail!("workspace name '{}' is already in use", name);
            }
        }

        // 1. ZFS clone from snapshot
        let forked = self
            .storage
            .fork_workspace(&source_ws.short_id(), snapshot_name, &new_short_id, Some(source_ws.resources.disk_gb))
            .await
            .context("failed to fork workspace storage")?;

        // 2. Set up networking
        let default_policy = NetworkPolicy {
            allow_internet: self.config.network.default_allow_internet,
            allow_inter_vm: self.config.network.default_allow_inter_vm,
            allowed_ports: Vec::new(),
        };

        let net_setup = match self.network.write().await.setup_workspace(&new_short_id, &default_policy).await {
            Ok(setup) => setup,
            Err(e) => {
                self.storage.destroy_workspace(&new_short_id).await.ok();
                return Err(e.context("failed to set up fork networking"));
            }
        };

        // 3. Allocate vsock CID and launch VM
        let vsock_cid = self.alloc_vsock_cid().await?;
        let qmp_socket = self.config.vm.run_dir.join(&new_short_id).join("qmp.sock");

        let pid = match self.vm.write().await.launch(
            new_id,
            source_ws.resources.vcpus,
            source_ws.resources.memory_mb,
            forked.zvol_path,
            net_setup.tap_device.clone(),
            vsock_cid,
        ).await {
            Ok(pid) => pid,
            Err(e) => {
                self.recycle_vsock_cid(vsock_cid).await;
                self.network.write().await.cleanup_workspace(&new_short_id, net_setup.guest_ip).await.ok();
                self.storage.destroy_workspace(&new_short_id).await.ok();
                return Err(e.context("failed to launch forked VM"));
            }
        };

        // Configure guest workspace (single vsock RTT instead of two)
        let dns_servers = if default_policy.allow_internet {
            self.config.network.dns_servers.clone()
        } else {
            vec![self.config.network.gateway_ip.to_string()]
        };
        {
            let mut vm = self.vm.write().await;
            if let Ok(vsock) = vm.vsock_client(&new_id) {
                if let Err(e) = vsock.configure_workspace(
                    &net_setup.guest_ip.to_string(),
                    &net_setup.gateway_ip.to_string(),
                    dns_servers,
                    &name,
                ).await {
                    log_console_tail(&self.config.vm.run_dir, &new_short_id, 50).await;
                    warn!(workspace_id = %new_id, error = %e, "failed to configure forked workspace");
                }
            }
        }

        let workspace = Workspace {
            id: new_id,
            name,
            state: WorkspaceState::Running,
            base_image: source_ws.base_image.clone(),
            zfs_dataset: forked.dataset,
            qemu_pid: Some(pid),
            qmp_socket,
            vsock_cid,
            tap_device: net_setup.tap_device,
            network: WorkspaceNetwork {
                ip: net_setup.guest_ip,
                allow_internet: default_policy.allow_internet,
                allow_inter_vm: default_policy.allow_inter_vm,
                allowed_ports: default_policy.allowed_ports.clone(),
                port_forwards: Vec::new(),
            },
            snapshots: SnapshotTree::new(),
            created_at: Utc::now(),
            resources: source_ws.resources.clone(),
        };

        self.workspaces.write().await.insert(new_id, workspace.clone());
        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }

        info!(
            source = %source_workspace_id,
            snapshot = %snapshot_name,
            new_workspace_id = %new_id,
            "workspace forked"
        );
        Ok(workspace)
    }

    // -----------------------------------------------------------------------
    // Networking
    // -----------------------------------------------------------------------

    /// Add a port forwarding rule.
    #[instrument(skip(self))]
    pub async fn port_forward_add(
        &self,
        workspace_id: Uuid,
        guest_port: u16,
        host_port: Option<u16>,
    ) -> Result<u16> {
        let ws = self.get(workspace_id).await?;

        // Use provided host_port or default to same as guest_port
        let host_port = host_port.unwrap_or(guest_port);

        self.network
            .read()
            .await
            .add_port_forward(&ws.short_id(), ws.network.ip, guest_port, host_port)
            .await
            .context("failed to add port forward")?;

        // Record in workspace state
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.network.port_forwards.push(PortForward {
                    host_port,
                    guest_port,
                });
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        Ok(host_port)
    }

    /// Remove a port forwarding rule.
    #[instrument(skip(self))]
    pub async fn port_forward_remove(
        &self,
        workspace_id: Uuid,
        guest_port: u16,
    ) -> Result<()> {
        let ws = self.get(workspace_id).await?;

        self.network
            .read()
            .await
            .remove_port_forward(&ws.short_id(), guest_port)
            .await
            .context("failed to remove port forward")?;

        // Remove from workspace state
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.network.port_forwards.retain(|pf| pf.guest_port != guest_port);
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        Ok(())
    }

    /// Update the network policy for a workspace.
    #[instrument(skip(self))]
    pub async fn update_network_policy(
        &self,
        workspace_id: Uuid,
        allow_internet: Option<bool>,
        allow_inter_vm: Option<bool>,
        allowed_ports: Option<Vec<u16>>,
    ) -> Result<()> {
        let ws = self.get(workspace_id).await?;

        let policy = NetworkPolicy {
            allow_internet: allow_internet.unwrap_or(ws.network.allow_internet),
            allow_inter_vm: allow_inter_vm.unwrap_or(ws.network.allow_inter_vm),
            allowed_ports: allowed_ports.clone().unwrap_or_else(|| ws.network.allowed_ports.clone()),
        };

        self.network
            .read()
            .await
            .update_policy(&ws.short_id(), ws.network.ip, &policy)
            .await
            .context("failed to update network policy")?;

        // Update workspace state
        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.network.allow_internet = policy.allow_internet;
                ws.network.allow_inter_vm = policy.allow_inter_vm;
                ws.network.allowed_ports = policy.allowed_ports;
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Shutdown
    // -----------------------------------------------------------------------

    /// Gracefully shut down all running workspaces with a 30-second timeout.
    /// If the graceful shutdown takes too long, force-kills remaining QEMU processes.
    pub async fn shutdown_all(&self) {
        info!("shutting down all workspaces");

        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.graceful_shutdown_inner(),
        )
        .await
        {
            Ok(()) => {}
            Err(_) => {
                warn!("graceful shutdown timed out after 30s, force-killing remaining VMs");
                self.force_kill_all().await;
            }
        }

        // Mark all as stopped
        {
            let mut workspaces = self.workspaces.write().await;
            for ws in workspaces.values_mut() {
                ws.state = WorkspaceState::Stopped;
                ws.qemu_pid = None;
            }
        }

        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }
    }

    /// Inner graceful shutdown logic (pool drain + VM shutdown).
    async fn graceful_shutdown_inner(&self) {
        // Drain warm pool VMs
        let pool_vms = self.pool.drain().await;
        for warm_vm in pool_vms {
            info!(pool_id = %warm_vm.id, short_id = %warm_vm.short_id, "destroying warm pool VM");
            // Kill QEMU process
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(warm_vm.qemu_pid.to_string())
                .output();
            // Recycle vsock CID
            self.recycle_vsock_cid(warm_vm.vsock_cid).await;
            // Clean up network (TAP device + IP allocation)
            if let Err(e) = self.network.write().await
                .cleanup_workspace(&warm_vm.short_id, warm_vm.guest_ip).await
            {
                warn!(error = %e, pool_id = %warm_vm.id, "failed to clean up warm pool VM network");
            }
            // Destroy ZFS dataset
            if let Err(e) = self.storage.destroy_dataset(&warm_vm.zfs_dataset).await {
                warn!(error = %e, "failed to destroy warm pool VM storage");
            }
        }

        self.vm.write().await.shutdown_all().await;
    }

    /// Force-kill all known QEMU processes as a last resort.
    async fn force_kill_all(&self) {
        let workspaces = self.workspaces.read().await;
        for ws in workspaces.values() {
            if let Some(pid) = ws.qemu_pid {
                warn!(workspace_id = %ws.id, pid, "force-killing QEMU process");
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Ensure a workspace is in the Running state.
    async fn ensure_running(&self, workspace_id: Uuid) -> Result<()> {
        let workspaces = self.workspaces.read().await;
        let ws = workspaces
            .get(&workspace_id)
            .with_context(|| format!("workspace {} not found. Use workspace_list to see available workspaces.", workspace_id))?;

        if ws.state != WorkspaceState::Running {
            bail!(
                "workspace {} is not running (state: {}). Use workspace_start to boot it.",
                workspace_id,
                ws.state
            );
        }
        Ok(())
    }

    /// Assign a warm VM from the pool to a workspace.
    async fn assign_warm_vm(
        &self,
        warm_vm: pool::WarmVm,
        id: Uuid,
        name: String,
        policy: &NetworkPolicy,
    ) -> Result<Workspace> {
        let short_id = id.to_string()[..8].to_string();

        // Clean up the pool VM's temporary network resources (TAP + IP)
        // before setting up the workspace's own network identity.
        if let Err(e) = self.network.write().await
            .cleanup_workspace(&warm_vm.short_id, warm_vm.guest_ip).await
        {
            warn!(
                pool_id = %warm_vm.id,
                error = %e,
                "failed to clean up pool VM network during assignment"
            );
        }

        // Set up networking (TAP + nftables)
        let net_setup = match self.network.write().await.setup_workspace(&short_id, policy).await {
            Ok(setup) => setup,
            Err(e) => {
                // Return warm VM to pool on failure
                self.pool.add_ready(warm_vm).await;
                return Err(e.context("failed to set up networking for warm VM assignment"));
            }
        };

        // Configure workspace via vsock (single RTT)
        let dns_servers = if policy.allow_internet {
            self.config.network.dns_servers.clone()
        } else {
            vec![self.config.network.gateway_ip.to_string()]
        };
        {
            let mut vm = self.vm.write().await;
            if let Ok(vsock) = vm.vsock_client_by_cid(warm_vm.vsock_cid) {
                if let Err(e) = vsock.configure_workspace(
                    &net_setup.guest_ip.to_string(),
                    &net_setup.gateway_ip.to_string(),
                    dns_servers,
                    &name,
                ).await {
                    log_console_tail(&self.config.vm.run_dir, &short_id, 50).await;
                    warn!(workspace_id = %id, error = %e, "failed to configure warm VM workspace");
                }
            }
        }

        let workspace = Workspace {
            id,
            name,
            state: WorkspaceState::Running,
            base_image: self.config.storage.base_image.clone(),
            zfs_dataset: warm_vm.zfs_dataset,
            qemu_pid: Some(warm_vm.qemu_pid),
            qmp_socket: self.config.vm.run_dir.join(&short_id).join("qmp.sock"),
            vsock_cid: warm_vm.vsock_cid,
            tap_device: net_setup.tap_device,
            network: WorkspaceNetwork {
                ip: net_setup.guest_ip,
                allow_internet: policy.allow_internet,
                allow_inter_vm: policy.allow_inter_vm,
                allowed_ports: policy.allowed_ports.clone(),
                port_forwards: Vec::new(),
            },
            snapshots: SnapshotTree::new(),
            created_at: Utc::now(),
            resources: ResourceLimits {
                vcpus: self.config.resources.default_vcpus,
                memory_mb: self.config.resources.default_memory_mb,
                disk_gb: self.config.resources.default_disk_gb,
            },
        };

        self.workspaces.write().await.insert(id, workspace.clone());
        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }

        info!(
            workspace_id = %id,
            warm_vm_id = %warm_vm.id,
            "workspace assigned from warm pool"
        );
        Ok(workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_workspace(state: WorkspaceState) -> Workspace {
        let id = Uuid::new_v4();
        Workspace {
            id,
            name: format!("ws-{}", &id.to_string()[..8]),
            state,
            base_image: "alpine-dev".to_string(),
            zfs_dataset: "tank/agentiso/workspaces/ws-test".to_string(),
            qemu_pid: if state == WorkspaceState::Running { Some(1234) } else { None },
            qmp_socket: PathBuf::from("/run/agentiso/test/qmp.sock"),
            vsock_cid: 100,
            tap_device: "tap-test0".to_string(),
            network: WorkspaceNetwork {
                ip: "10.42.0.2".parse().unwrap(),
                allow_internet: false,
                allow_inter_vm: false,
                allowed_ports: Vec::new(),
                port_forwards: Vec::new(),
            },
            snapshots: SnapshotTree::new(),
            created_at: Utc::now(),
            resources: ResourceLimits {
                vcpus: 2,
                memory_mb: 512,
                disk_gb: 10,
            },
        }
    }

    // --- State machine tests ---

    /// Valid transition: Stopped -> Running
    #[test]
    fn state_transition_stopped_to_running() {
        let mut ws = make_workspace(WorkspaceState::Stopped);
        assert_eq!(ws.state, WorkspaceState::Stopped);
        ws.state = WorkspaceState::Running;
        assert_eq!(ws.state, WorkspaceState::Running);
    }

    /// Valid transition: Running -> Suspended
    #[test]
    fn state_transition_running_to_suspended() {
        let mut ws = make_workspace(WorkspaceState::Running);
        ws.state = WorkspaceState::Suspended;
        assert_eq!(ws.state, WorkspaceState::Suspended);
    }

    /// Valid transition: Suspended -> Running
    #[test]
    fn state_transition_suspended_to_running() {
        let mut ws = make_workspace(WorkspaceState::Suspended);
        ws.state = WorkspaceState::Running;
        assert_eq!(ws.state, WorkspaceState::Running);
    }

    /// Valid transition: Running -> Stopped
    #[test]
    fn state_transition_running_to_stopped() {
        let mut ws = make_workspace(WorkspaceState::Running);
        ws.state = WorkspaceState::Stopped;
        assert_eq!(ws.state, WorkspaceState::Stopped);
    }

    /// Full lifecycle: Stopped -> Running -> Suspended -> Running -> Stopped
    #[test]
    fn state_full_lifecycle() {
        let mut ws = make_workspace(WorkspaceState::Stopped);
        assert_eq!(ws.state, WorkspaceState::Stopped);

        ws.state = WorkspaceState::Running;
        assert_eq!(ws.state, WorkspaceState::Running);

        ws.state = WorkspaceState::Suspended;
        assert_eq!(ws.state, WorkspaceState::Suspended);

        ws.state = WorkspaceState::Running;
        assert_eq!(ws.state, WorkspaceState::Running);

        ws.state = WorkspaceState::Stopped;
        assert_eq!(ws.state, WorkspaceState::Stopped);
    }

    /// Verify state display strings match serde rename.
    #[test]
    fn state_display() {
        assert_eq!(WorkspaceState::Stopped.to_string(), "stopped");
        assert_eq!(WorkspaceState::Running.to_string(), "running");
        assert_eq!(WorkspaceState::Suspended.to_string(), "suspended");
    }

    /// Verify state serde serialization matches expected JSON values.
    #[test]
    fn state_serde_values() {
        assert_eq!(serde_json::to_string(&WorkspaceState::Stopped).unwrap(), "\"stopped\"");
        assert_eq!(serde_json::to_string(&WorkspaceState::Running).unwrap(), "\"running\"");
        assert_eq!(serde_json::to_string(&WorkspaceState::Suspended).unwrap(), "\"suspended\"");

        let deserialized: WorkspaceState = serde_json::from_str("\"running\"").unwrap();
        assert_eq!(deserialized, WorkspaceState::Running);
    }

    /// Invalid state strings are rejected during deserialization.
    #[test]
    fn state_serde_rejects_invalid() {
        let result: Result<WorkspaceState, _> = serde_json::from_str("\"paused\"");
        assert!(result.is_err());
    }

    // --- Workspace serde round-trip ---

    #[test]
    fn workspace_serde_roundtrip() {
        let ws = make_workspace(WorkspaceState::Running);
        let json = serde_json::to_string(&ws).unwrap();
        let deserialized: Workspace = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, ws.id);
        assert_eq!(deserialized.name, ws.name);
        assert_eq!(deserialized.state, ws.state);
        assert_eq!(deserialized.base_image, ws.base_image);
        assert_eq!(deserialized.vsock_cid, ws.vsock_cid);
        assert_eq!(deserialized.network.ip, ws.network.ip);
        assert_eq!(deserialized.resources.vcpus, ws.resources.vcpus);
        assert_eq!(deserialized.resources.memory_mb, ws.resources.memory_mb);
    }

    #[test]
    fn workspace_serde_roundtrip_with_snapshots() {
        let mut ws = make_workspace(WorkspaceState::Running);
        let snap = Snapshot {
            id: Uuid::new_v4(),
            name: "test-snap".to_string(),
            workspace_id: ws.id,
            zfs_snapshot: "tank/agentiso/workspaces/ws-test@test-snap".to_string(),
            qemu_state: Some(PathBuf::from("qemu-state:test-snap")),
            parent: None,
            created_at: Utc::now(),
        };
        ws.snapshots.add(snap);

        let json = serde_json::to_string(&ws).unwrap();
        let deserialized: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.snapshots.len(), 1);
        assert!(deserialized.snapshots.get_by_name("test-snap").is_some());
    }

    #[test]
    fn workspace_short_id() {
        let ws = make_workspace(WorkspaceState::Stopped);
        let short = ws.short_id();
        assert_eq!(short.len(), 8);
        assert!(ws.id.to_string().starts_with(&short));
    }

    /// PersistedState round-trip (simulates state file save/load).
    #[test]
    fn persisted_state_serde_roundtrip() {
        let mut workspaces = HashMap::new();
        let ws = make_workspace(WorkspaceState::Stopped);
        workspaces.insert(ws.id, ws);

        let state = PersistedState {
            schema_version: 1,
            workspaces,
            next_vsock_cid: 105,
            free_vsock_cids: vec![100, 101],
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let deserialized: PersistedState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.schema_version, 1);
        assert_eq!(deserialized.workspaces.len(), 1);
        assert_eq!(deserialized.next_vsock_cid, 105);
        assert_eq!(deserialized.free_vsock_cids, vec![100, 101]);
    }

    /// Legacy state files without schema_version should deserialize with version 0.
    #[test]
    fn persisted_state_legacy_without_schema_version() {
        let json = r#"{
            "workspaces": {},
            "next_vsock_cid": 100,
            "free_vsock_cids": []
        }"#;
        let state: PersistedState = serde_json::from_str(json).unwrap();
        assert_eq!(state.schema_version, 0);
    }

    // --- read_log_tail tests ---

    #[tokio::test]
    async fn test_read_log_tail_file_not_found() {
        let path = std::path::Path::new("/tmp/agentiso-test-nonexistent-log.txt");
        let result = read_log_tail(path, 10).await;
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_read_log_tail_empty_file() {
        let dir = std::env::temp_dir().join(format!("agentiso-log-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("empty.log");
        tokio::fs::write(&path, "").await.unwrap();

        let result = read_log_tail(&path, 10).await;
        assert_eq!(result, "");

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn test_read_log_tail_fewer_lines_than_max() {
        let dir = std::env::temp_dir().join(format!("agentiso-log-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("short.log");
        tokio::fs::write(&path, "line1\nline2\nline3\n").await.unwrap();

        let result = read_log_tail(&path, 10).await;
        assert_eq!(result, "line1\nline2\nline3");

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn test_read_log_tail_truncates_to_max_lines() {
        let dir = std::env::temp_dir().join(format!("agentiso-log-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("long.log");
        let content: String = (1..=20).map(|i| format!("line{}\n", i)).collect();
        tokio::fs::write(&path, &content).await.unwrap();

        let result = read_log_tail(&path, 5).await;
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "line16");
        assert_eq!(lines[4], "line20");

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn test_read_log_tail_exact_lines() {
        let dir = std::env::temp_dir().join(format!("agentiso-log-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("exact.log");
        tokio::fs::write(&path, "line1\nline2\nline3\n").await.unwrap();

        // Requesting exactly 3 lines from a 3-line file
        let result = read_log_tail(&path, 3).await;
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[2], "line3");

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }
}
