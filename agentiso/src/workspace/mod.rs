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
}

/// Persisted state (serialized to JSON on disk).
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PersistedState {
    pub workspaces: HashMap<Uuid, Workspace>,
    pub next_vsock_cid: u32,
    /// CIDs returned from destroyed workspaces, available for reuse.
    #[serde(default)]
    pub free_vsock_cids: Vec<u32>,
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
    pub async fn load_state(&self) -> Result<()> {
        let state_path = &self.config.server.state_file;
        if !state_path.exists() {
            info!(path = %state_path.display(), "no persisted state file, starting fresh");
            return Ok(());
        }

        let data = tokio::fs::read_to_string(state_path)
            .await
            .with_context(|| format!("reading state file: {}", state_path.display()))?;

        let persisted: PersistedState = serde_json::from_str(&data)
            .with_context(|| format!("parsing state file: {}", state_path.display()))?;

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
        *self.workspaces.write().await = workspaces;
        *self.next_vsock_cid.write().await = persisted.next_vsock_cid;
        *self.free_vsock_cids.write().await = persisted.free_vsock_cids;

        info!(count, "loaded persisted workspace state");
        Ok(())
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

        tokio::fs::write(state_path, data)
            .await
            .with_context(|| format!("writing state file: {}", state_path.display()))?;

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

        // Enforce total workspace count limit
        {
            let count = self.workspaces.read().await.len() as u32;
            if count >= limits.max_workspaces {
                bail!(
                    "workspace limit reached: {} of {} maximum",
                    count, limits.max_workspaces
                );
            }
        }

        // Fast path: try to claim a warm VM from the pool
        if self.pool.enabled() {
            if let Some(warm_vm) = self.pool.claim().await {
                return self.assign_warm_vm(warm_vm, id, name, &NetworkPolicy {
                    allow_internet: self.config.network.default_allow_internet,
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
            allow_internet: self.config.network.default_allow_internet,
            allow_inter_vm: self.config.network.default_allow_inter_vm,
            allowed_ports: Vec::new(),
        };

        let (storage_result, network_result) = tokio::join!(
            self.storage.create_workspace(&base_image, &self.config.storage.base_snapshot, &short_id),
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
                // Rollback network and storage
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
        {
            let mut vm = self.vm.write().await;
            let vsock = vm.vsock_client(&id)?;
            if let Err(e) = vsock.configure_workspace(
                &net_setup.guest_ip.to_string(),
                &net_setup.gateway_ip.to_string(),
                vec!["1.1.1.1".to_string()],
                &name,
            ).await {
                warn!(workspace_id = %id, error = %e, "failed to configure guest workspace");
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
        self.save_state().await.ok();

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
                .with_context(|| format!("workspace not found: {}", workspace_id))?
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
        self.save_state().await.ok();

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
                .with_context(|| format!("workspace not found: {}", workspace_id))?;
            if ws.state != WorkspaceState::Running {
                bail!("workspace {} is not running (state: {})", workspace_id, ws.state);
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

        self.save_state().await.ok();
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
                .with_context(|| format!("workspace not found: {}", workspace_id))?
        };

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

        // Configure guest network again
        let gateway_ip = self.network.read().await.gateway_ip();
        {
            let mut vm = self.vm.write().await;
            if let Ok(vsock) = vm.vsock_client(&workspace_id) {
                let net_config = protocol::NetworkConfig {
                    ip_address: format!("{}/16", ws.network.ip),
                    gateway: gateway_ip.to_string(),
                    dns: vec!["1.1.1.1".to_string()],
                };
                if let Err(e) = vsock.configure_network(net_config).await {
                    warn!(workspace_id = %workspace_id, error = %e, "failed to configure guest network on start");
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

        self.save_state().await.ok();
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
                .with_context(|| format!("workspace not found: {}", workspace_id))?;
            if ws.state != WorkspaceState::Running {
                bail!("workspace {} is not running (state: {})", workspace_id, ws.state);
            }
        }

        self.vm.write().await.pause(&workspace_id).await?;

        {
            let mut workspaces = self.workspaces.write().await;
            if let Some(ws) = workspaces.get_mut(&workspace_id) {
                ws.state = WorkspaceState::Suspended;
            }
        }

        self.save_state().await.ok();
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
                .with_context(|| format!("workspace not found: {}", workspace_id))?;
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

        self.save_state().await.ok();
        info!(workspace_id = %workspace_id, "workspace resumed");
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
            .with_context(|| format!("workspace not found: {}", workspace_id))
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
                .with_context(|| format!("workspace not found: {}", workspace_id))?;
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

        self.save_state().await.ok();
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
                .with_context(|| format!("workspace not found: {}", workspace_id))?;

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

        self.save_state().await.ok();
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
                .with_context(|| format!("workspace not found: {}", workspace_id))?;

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

        self.save_state().await.ok();
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
                .with_context(|| format!("workspace not found: {}", source_workspace_id))?
        };

        // Verify snapshot exists
        source_ws
            .snapshots
            .get_by_name(snapshot_name)
            .with_context(|| format!("snapshot not found: {}", snapshot_name))?;

        // Enforce total workspace count limit
        {
            let limits = &self.config.resources;
            let count = self.workspaces.read().await.len() as u32;
            if count >= limits.max_workspaces {
                bail!(
                    "workspace limit reached: {} of {} maximum",
                    count, limits.max_workspaces
                );
            }
        }

        let new_id = Uuid::new_v4();
        let new_short_id = new_id.to_string()[..8].to_string();
        let name = new_name.unwrap_or_else(|| format!("fork-{}", &new_short_id));

        // 1. ZFS clone from snapshot
        let forked = self
            .storage
            .fork_workspace(&source_ws.short_id(), snapshot_name, &new_short_id)
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
                self.network.write().await.cleanup_workspace(&new_short_id, net_setup.guest_ip).await.ok();
                self.storage.destroy_workspace(&new_short_id).await.ok();
                return Err(e.context("failed to launch forked VM"));
            }
        };

        // Configure guest workspace (single vsock RTT instead of two)
        {
            let mut vm = self.vm.write().await;
            if let Ok(vsock) = vm.vsock_client(&new_id) {
                if let Err(e) = vsock.configure_workspace(
                    &net_setup.guest_ip.to_string(),
                    &net_setup.gateway_ip.to_string(),
                    vec!["1.1.1.1".to_string()],
                    &name,
                ).await {
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
        self.save_state().await.ok();

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

        self.save_state().await.ok();
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

        self.save_state().await.ok();
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

        self.save_state().await.ok();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Shutdown
    // -----------------------------------------------------------------------

    /// Gracefully shut down all running workspaces.
    pub async fn shutdown_all(&self) {
        info!("shutting down all workspaces");

        // Drain warm pool VMs
        let pool_vms = self.pool.drain().await;
        for warm_vm in pool_vms {
            info!(pool_id = %warm_vm.id, "destroying warm pool VM");
            // Kill QEMU process
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(warm_vm.qemu_pid.to_string())
                .output();
            // Destroy ZFS dataset
            if let Err(e) = self.storage.destroy_dataset(&warm_vm.zfs_dataset).await {
                warn!(error = %e, "failed to destroy warm pool VM storage");
            }
        }

        self.vm.write().await.shutdown_all().await;

        // Mark all as stopped
        {
            let mut workspaces = self.workspaces.write().await;
            for ws in workspaces.values_mut() {
                ws.state = WorkspaceState::Stopped;
                ws.qemu_pid = None;
            }
        }

        self.save_state().await.ok();
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Ensure a workspace is in the Running state.
    async fn ensure_running(&self, workspace_id: Uuid) -> Result<()> {
        let workspaces = self.workspaces.read().await;
        let ws = workspaces
            .get(&workspace_id)
            .with_context(|| format!("workspace not found: {}", workspace_id))?;

        if ws.state != WorkspaceState::Running {
            bail!(
                "workspace {} is not running (state: {})",
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
        {
            let mut vm = self.vm.write().await;
            if let Ok(vsock) = vm.vsock_client_by_cid(warm_vm.vsock_cid) {
                if let Err(e) = vsock.configure_workspace(
                    &net_setup.guest_ip.to_string(),
                    &net_setup.gateway_ip.to_string(),
                    vec!["1.1.1.1".to_string()],
                    &name,
                ).await {
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
        self.save_state().await.ok();

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
            workspaces,
            next_vsock_cid: 105,
            free_vsock_cids: vec![100, 101],
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let deserialized: PersistedState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.workspaces.len(), 1);
        assert_eq!(deserialized.next_vsock_cid, 105);
        assert_eq!(deserialized.free_vsock_cids, vec![100, 101]);
    }
}
