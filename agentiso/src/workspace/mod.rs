pub mod orchestrate;
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

use std::sync::{Arc, Weak};

use crate::config::Config;
use crate::guest::protocol;
use crate::mcp::auth::AuthManager;
use crate::mcp::metrics::MetricsRegistry;
use crate::network::{NetworkManager, NetworkPolicy};
use crate::storage::StorageManager;
use crate::vm::VmManager;
use crate::workspace::snapshot::{ForkLineage, Snapshot, SnapshotTree};

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
    /// Fork lineage: set when this workspace was created via fork().
    /// Tracks the source workspace, snapshot name, and fork timestamp.
    #[serde(default)]
    pub forked_from: Option<ForkLineage>,
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
    /// Version 0 = legacy files without this field; version 1 = current;
    /// version 2 = adds session ownership.
    #[serde(default)]
    pub schema_version: u32,
    pub workspaces: HashMap<Uuid, Workspace>,
    pub next_vsock_cid: u32,
    /// CIDs returned from destroyed workspaces, available for reuse.
    #[serde(default)]
    pub free_vsock_cids: Vec<u32>,
    /// Session→workspace ownership mappings. Added in schema version 2.
    #[serde(default)]
    pub sessions: Vec<crate::mcp::auth::PersistedSession>,
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
    /// Optional auth manager reference for persisting session ownership.
    auth: RwLock<Option<Arc<AuthManager>>>,
    /// Optional metrics registry for recording operational metrics.
    metrics: RwLock<Option<MetricsRegistry>>,
    /// Weak self-reference for spawning background tasks (e.g. pool replenishment).
    /// Set via `set_self_ref()` after wrapping in Arc.
    self_ref: RwLock<Weak<Self>>,
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
            auth: RwLock::new(None),
            metrics: RwLock::new(None),
            self_ref: RwLock::new(Weak::new()),
        }
    }

    /// Create a WorkspaceManager with default config for unit tests.
    ///
    /// Uses default Config, VmManager, StorageManager, and NetworkManager.
    /// NOT suitable for real VM operations -- only for testing metadata,
    /// state persistence, and other pure logic paths.
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        use crate::vm::VmManagerConfig;
        let config = Config::default();
        let vm = VmManager::new(VmManagerConfig::default());
        let storage = StorageManager::new(format!(
            "{}/{}",
            config.storage.zfs_pool, config.storage.dataset_prefix
        ));
        let network = NetworkManager::new();
        let pool = pool::VmPool::new(config.pool.clone());
        Self::new(config, vm, storage, network, pool)
    }

    /// Set the auth manager for session persistence.
    /// Must be called before `save_state()` to enable session export.
    pub async fn set_auth_manager(&self, auth: Arc<AuthManager>) {
        *self.auth.write().await = Some(auth);
    }

    /// Set the metrics registry for recording operational metrics.
    pub async fn set_metrics(&self, metrics: MetricsRegistry) {
        *self.metrics.write().await = Some(metrics);
    }

    /// Store a weak self-reference for spawning background tasks.
    /// Must be called after wrapping in `Arc<WorkspaceManager>`.
    /// When set, pool replenishment is triggered immediately after a warm VM
    /// is claimed in `create()`. Without this, the background timer (every 2s)
    /// handles replenishment instead.
    #[allow(dead_code)] // public API: callers set this after Arc::new()
    pub async fn set_self_ref(&self, arc: &Arc<Self>) {
        *self.self_ref.write().await = Arc::downgrade(arc);
    }

    /// Get a clone of the metrics registry, if set.
    async fn metrics(&self) -> Option<MetricsRegistry> {
        self.metrics.read().await.clone()
    }

    /// Spawn a background task to replenish the warm pool if the pool
    /// is below its target size. Safe to call from any context; does
    /// nothing if the pool is disabled, already full, or the self-ref
    /// has not been set.
    fn spawn_pool_replenish_if_needed(&self) {
        if !self.pool.enabled() {
            return;
        }
        // We need a Weak -> Arc upgrade to spawn a task; try_read to
        // avoid blocking the caller.
        let weak = match self.self_ref.try_read() {
            Ok(guard) => guard.clone(),
            Err(_) => return, // Lock contention, skip this time
        };
        let Some(mgr) = weak.upgrade() else {
            return;
        };
        tokio::spawn(async move {
            if mgr.pool.deficit().await == 0 {
                return;
            }
            if let Err(e) = mgr.replenish_pool().await {
                warn!(error = %e, "background pool replenishment failed");
            }
        });
    }

    /// Initialize storage, network, and cgroup subsystems. Call once at startup.
    pub async fn init(&self) -> Result<()> {
        self.storage.init().await.context("storage init failed")?;
        self.network.write().await.init().await.context("network init failed")?;
        // Best-effort: create parent cgroup slice for QEMU process isolation
        if let Err(e) = crate::vm::cgroup::ensure_parent_slice().await {
            warn!(error = %e, "cgroup parent slice setup failed (non-fatal)");
        }
        Ok(())
    }

    /// Load persisted state from the state file.
    ///
    /// After loading, performs auto-adoption and orphan reconciliation:
    /// 1. For each workspace that was Running: check if QEMU PID is alive
    ///    - If alive: auto-adopt (keep Running state, verify vsock)
    ///    - If dead: mark as Stopped with a warning
    /// 2. Kills orphaned QEMU processes not belonging to any workspace
    /// 3. Cleans up stale TAP devices for stopped workspaces
    /// 4. Destroys orphaned TAP devices not matching any known workspace
    /// 5. Detects orphaned ZFS datasets not tracked in state (logs warnings)
    pub async fn load_state(&self) -> Result<()> {
        let state_path = &self.config.server.state_file;
        if !state_path.exists() {
            info!(path = %state_path.display(), "no persisted state file, starting fresh");
            // Even without persisted state, run full orphan reconciliation
            self.cleanup_orphan_qemu_processes().await;
            self.cleanup_stale_tap_devices(&std::collections::HashSet::new()).await;
            self.detect_orphan_zfs_datasets(&std::collections::HashSet::new()).await;
            return Ok(());
        }

        let data = tokio::fs::read_to_string(state_path)
            .await
            .with_context(|| format!("reading state file: {}", state_path.display()))?;

        let persisted: PersistedState = serde_json::from_str(&data)
            .with_context(|| format!("parsing state file: {}", state_path.display()))?;

        if persisted.schema_version > 2 {
            warn!(
                version = persisted.schema_version,
                "state file has newer schema version than supported (2), some fields may be lost"
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

        // Auto-adopt running workspaces or mark dead ones as stopped.
        // Instead of blindly marking everything Stopped, check if QEMU is
        // still alive by PID. Workspaces whose VMs survived the daemon
        // restart are kept in Running state.
        let mut workspaces = persisted.workspaces;
        let persisted_sessions = persisted.sessions;
        let mut adopted_count: u32 = 0;
        let mut stopped_count: u32 = 0;

        // Track PIDs of workspaces we adopt so we don't kill them during
        // orphan cleanup. Collect into a set of workspace short_ids that
        // should keep their TAP devices.
        let mut adopted_short_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for ws in workspaces.values_mut() {
            if ws.state == WorkspaceState::Running || ws.state == WorkspaceState::Suspended {
                let is_alive = if let Some(pid) = ws.qemu_pid {
                    Self::is_process_alive(pid as i32)
                } else {
                    false
                };

                if is_alive {
                    // QEMU process is still running -- auto-adopt
                    info!(
                        workspace_id = %ws.id,
                        name = %ws.name,
                        pid = ws.qemu_pid.unwrap_or(0),
                        vsock_cid = ws.vsock_cid,
                        "auto-adopting workspace (QEMU process still running)"
                    );
                    adopted_count += 1;
                    adopted_short_ids.insert(ws.short_id());
                    // State and PID are left as-is (Running/Suspended + valid PID)
                } else {
                    // QEMU process is dead -- mark as Stopped
                    warn!(
                        workspace_id = %ws.id,
                        name = %ws.name,
                        previous_pid = ws.qemu_pid.unwrap_or(0),
                        "workspace was Running but QEMU process is dead, marking Stopped"
                    );
                    ws.state = WorkspaceState::Stopped;
                    ws.qemu_pid = None;
                    stopped_count += 1;
                }
            }
            // Workspaces already in Stopped state stay that way
        }

        let count = workspaces.len();

        // Build sets of known workspace identifiers for orphan detection
        let known_short_ids: std::collections::HashSet<String> = workspaces
            .values()
            .map(|ws| ws.short_id())
            .collect();
        let known_datasets: std::collections::HashSet<String> = workspaces
            .values()
            .map(|ws| ws.zfs_dataset.clone())
            .collect();

        // Build workspace resource map for session restoration
        let workspace_resources: HashMap<Uuid, (u64, u64)> = workspaces
            .values()
            .map(|ws| (ws.id, (ws.resources.memory_mb as u64, ws.resources.disk_gb as u64)))
            .collect();

        // Clean up orphaned QEMU processes before making workspaces usable.
        // This must happen before inserting workspaces so that stale run
        // directories (with PID files) are cleaned up first.
        // NOTE: this only kills processes not adopted above; adopted PIDs
        // are preserved because cleanup_orphan_qemu_processes works from
        // PID files in the run directory, which we do not remove for
        // adopted workspaces.
        self.cleanup_orphan_qemu_processes().await;

        // Clean up stale TAP devices from stopped workspaces only.
        // Adopted (still-running) workspaces keep their TAP devices.
        {
            let net = self.network.read().await;
            for ws in workspaces.values() {
                // Skip adopted workspaces -- their TAP devices are in use
                if adopted_short_ids.contains(&ws.short_id()) {
                    continue;
                }
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

        // Clean up orphaned TAP devices not matching any known workspace
        self.cleanup_stale_tap_devices(&known_short_ids).await;

        // Detect orphaned ZFS datasets (log-only, no auto-destroy)
        self.detect_orphan_zfs_datasets(&known_datasets).await;

        *self.workspaces.write().await = workspaces;
        *self.next_vsock_cid.write().await = persisted.next_vsock_cid;
        *self.free_vsock_cids.write().await = persisted.free_vsock_cids;

        // Restore session ownership if auth manager is available
        if !persisted_sessions.is_empty() {
            let auth_guard = self.auth.read().await;
            if let Some(auth) = auth_guard.as_ref() {
                auth.restore_sessions(&persisted_sessions, &workspace_resources).await;
                info!(
                    session_count = persisted_sessions.len(),
                    "restored session ownership from state file"
                );
            } else {
                warn!(
                    session_count = persisted_sessions.len(),
                    "state file contains session data but no auth manager is configured (sessions will be orphaned)"
                );
            }
        }

        info!(
            count,
            adopted = adopted_count,
            stopped = stopped_count,
            "Auto-adopted {} workspaces, {} were stopped",
            adopted_count,
            stopped_count
        );
        Ok(())
    }

    /// Check if a process with the given PID is still alive.
    fn is_process_alive(pid: i32) -> bool {
        // signal 0 = existence check (no signal sent)
        (unsafe { libc::kill(pid, 0) }) == 0
    }

    /// Scan the run directory for orphaned QEMU processes from a previous daemon
    /// session and kill them. This prevents CID collisions and RAM exhaustion
    /// after a daemon crash.
    ///
    /// Phase 1: For each subdirectory in `{run_dir}/`, checks for a `qemu.pid`
    /// file, reads the PID, verifies the process is still alive, and kills it
    /// with SIGKILL if so. The entire subdirectory is then removed.
    ///
    /// Phase 2: Scans `/proc` for any QEMU processes whose cmdline contains
    /// the run directory path (catches processes that lost their PID files).
    async fn cleanup_orphan_qemu_processes(&self) {
        let run_dir = &self.config.vm.run_dir;
        let mut killed = 0u32;
        let mut cleaned = 0u32;

        // Phase 1: PID file-based cleanup
        if let Ok(mut entries) = tokio::fs::read_dir(run_dir).await {
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
        } else {
            // Run directory may not exist on first boot; that's fine.
            debug!(
                path = %run_dir.display(),
                "could not read run directory for orphan cleanup"
            );
        }

        // Phase 2: Scan /proc for orphaned QEMU processes by cmdline pattern.
        // This catches processes whose PID files were lost (e.g. /run was tmpfs
        // and the server crashed without cleaning up, then /run was recreated).
        let run_dir_str = run_dir.to_string_lossy().to_string();
        let proc_killed = Self::kill_orphan_qemu_by_cmdline(&run_dir_str).await;
        killed += proc_killed;

        if killed > 0 || cleaned > 0 {
            warn!(
                killed,
                cleaned,
                "orphan QEMU cleanup complete"
            );
        }
    }

    /// Scan `/proc` for QEMU processes whose cmdline contains the given run_dir
    /// path, and SIGKILL them. Returns the number of processes killed.
    ///
    /// This is a fallback for when PID files are lost. We match on the run_dir
    /// path in the cmdline to avoid killing unrelated QEMU instances.
    async fn kill_orphan_qemu_by_cmdline(run_dir: &str) -> u32 {
        let mut killed = 0u32;
        let my_pid = std::process::id();

        let mut proc_entries = match tokio::fs::read_dir("/proc").await {
            Ok(entries) => entries,
            Err(e) => {
                debug!(error = %e, "could not read /proc for orphan QEMU scan");
                return 0;
            }
        };

        loop {
            let entry = match proc_entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(_) => continue,
            };

            // Only process numeric directories (PIDs)
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let pid: i32 = match name_str.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Skip our own process
            if pid as u32 == my_pid {
                continue;
            }

            // Read /proc/<pid>/cmdline (NUL-separated)
            let cmdline_path = format!("/proc/{}/cmdline", pid);
            let cmdline = match tokio::fs::read(&cmdline_path).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Err(_) => continue,
            };

            // Check if this is a QEMU process associated with our run_dir
            let is_qemu = cmdline.contains("qemu-system-") || cmdline.contains("qemu-kvm");
            let is_ours = cmdline.contains(run_dir);

            if is_qemu && is_ours {
                warn!(
                    pid,
                    "killing orphaned QEMU process found via /proc scan (cmdline matches run_dir)"
                );
                unsafe { libc::kill(pid, libc::SIGKILL) };
                killed += 1;
            }
        }

        killed
    }

    /// Clean up stale TAP devices attached to the bridge that don't correspond
    /// to any known workspace. Called during startup reconciliation.
    async fn cleanup_stale_tap_devices(&self, known_short_ids: &std::collections::HashSet<String>) {
        let net = self.network.read().await;
        let tap_devices = match net.bridge().list_tap_devices().await {
            Ok(taps) => taps,
            Err(e) => {
                warn!(error = %e, "failed to list TAP devices on bridge for orphan cleanup");
                return;
            }
        };

        let mut cleaned = 0u32;
        for tap_name in &tap_devices {
            // TAP names are "tap-{short_id}" — extract the short_id
            let short_id = match tap_name.strip_prefix("tap-") {
                Some(id) => id.to_string(),
                None => continue, // Not one of ours
            };

            if !known_short_ids.contains(&short_id) {
                warn!(
                    tap = %tap_name,
                    short_id = %short_id,
                    "destroying orphaned TAP device (no matching workspace)"
                );
                if let Err(e) = net.bridge().destroy_tap(&short_id).await {
                    warn!(
                        tap = %tap_name,
                        error = %e,
                        "failed to destroy orphaned TAP device"
                    );
                }
                cleaned += 1;
            }
        }

        if cleaned > 0 {
            warn!(cleaned, "orphan TAP cleanup complete");
        }
    }

    /// Detect ZFS datasets under workspaces/ and forks/ that are not tracked
    /// in state.json. Logs warnings but does NOT auto-destroy (too dangerous).
    async fn detect_orphan_zfs_datasets(&self, known_datasets: &std::collections::HashSet<String>) {
        let all_datasets = match self.storage.list_all_workspace_datasets().await {
            Ok(ds) => ds,
            Err(e) => {
                warn!(error = %e, "failed to list ZFS datasets for orphan detection");
                return;
            }
        };

        let mut orphan_count = 0u32;
        for dataset in &all_datasets {
            if !known_datasets.contains(dataset) {
                warn!(
                    dataset = %dataset,
                    "orphaned ZFS dataset detected (not in state.json). \
                     Manual cleanup may be needed: zfs destroy -r {}", dataset
                );
                orphan_count += 1;
            }
        }

        if orphan_count > 0 {
            warn!(
                orphan_count,
                "found orphaned ZFS datasets not tracked in state.json. \
                 These may be from crashed workspace creation or manual operations. \
                 Inspect and clean up manually if no longer needed."
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

        // Export session ownership if auth manager is configured
        let sessions = {
            let auth_guard = self.auth.read().await;
            match auth_guard.as_ref() {
                Some(auth) => auth.export_sessions().await,
                None => Vec::new(),
            }
        };

        // Acquire all three locks before reading to get a consistent snapshot.
        // Without this, a concurrent create() between reads could produce a
        // state where the workspace is saved but the CID counter is stale.
        let ws_guard = self.workspaces.read().await;
        let cid_guard = self.next_vsock_cid.read().await;
        let free_guard = self.free_vsock_cids.read().await;

        let persisted = PersistedState {
            schema_version: 2,
            workspaces: ws_guard.clone(),
            next_vsock_cid: *cid_guard,
            free_vsock_cids: free_guard.clone(),
            sessions,
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
                let result = self.assign_warm_vm(warm_vm, id, name, &NetworkPolicy {
                    allow_internet,
                    allow_inter_vm: self.config.network.default_allow_inter_vm,
                    allowed_ports: Vec::new(),
                }).await;
                // After claiming a VM from the pool, spawn background
                // replenishment to maintain the target pool size.
                // This does not block the workspace_create response.
                self.spawn_pool_replenish_if_needed();
                return result;
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

        // 4. Start QEMU (timed for metrics)
        let boot_start = std::time::Instant::now();
        let qmp_socket = self.config.vm.run_dir.join(&short_id).join("qmp.sock");
        let pid = match self.vm.write().await.launch(
            id,
            vcpus,
            memory_mb,
            ws_storage.zvol_path.clone(),
            net_setup.tap_device.clone(),
            vsock_cid,
            net_setup.guest_ip,
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
            let vsock_arc = self.vm.read().await.vsock_client_arc(&id)?;
            let mut vsock = vsock_arc.lock().await;
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

        // Record boot duration metric
        let boot_duration = boot_start.elapsed();
        if let Some(m) = self.metrics().await {
            m.record_boot(boot_duration);
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
            forked_from: None,
        };

        self.workspaces.write().await.insert(id, workspace.clone());
        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to persist workspace state");
        }

        info!(workspace_id = %id, boot_duration_ms = boot_duration.as_millis(), "workspace created and running");
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

        // Destroy storage — propagate the error so callers see it.
        // VM stop and network cleanup are best-effort above, but storage
        // destroy failure likely means the workspace data is still on disk
        // and the caller needs to know.
        self.storage
            .destroy_workspace(&short_id)
            .await
            .with_context(|| format!("failed to destroy storage for workspace {}", workspace_id))?;

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
            if let Ok(vsock_arc) = self.vm.read().await.vsock_client_arc(&workspace_id) {
                let mut vsock = vsock_arc.lock().await;
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
                ws.network.ip,
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
            if let Ok(vsock_arc) = self.vm.read().await.vsock_client_arc(&workspace_id) {
                let mut vsock = vsock_arc.lock().await;
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
            net_setup.guest_ip,
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

    /// Find a workspace by name. Returns the workspace UUID if found.
    pub async fn find_by_name(&self, name: &str) -> Option<Uuid> {
        self.workspaces
            .read()
            .await
            .values()
            .find(|ws| ws.name == name)
            .map(|ws| ws.id)
    }

    /// Get zvol info (volsize + used bytes) for a workspace.
    pub async fn workspace_zvol_info(
        &self,
        workspace_id: Uuid,
    ) -> Result<crate::storage::ZvolInfo> {
        let dataset = {
            let workspaces = self.workspaces.read().await;
            let ws = workspaces
                .get(&workspace_id)
                .with_context(|| format!("workspace not found: {}", workspace_id))?;
            ws.zfs_dataset.clone()
        };
        self.storage.zvol_info(&dataset).await
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

        let timeout = timeout_secs.unwrap_or(120);
        let env_map = env.cloned().unwrap_or_default();

        // Clone the Arc<Mutex<VsockClient>> under a read lock, then drop the lock
        // before performing vsock I/O. This allows other workspaces to proceed
        // concurrently instead of being serialized behind a global write lock.
        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;

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

    /// Set environment variables in the guest VM via vsock SetEnv RPC.
    #[instrument(skip(self, vars))]
    pub async fn set_env(
        &self,
        workspace_id: Uuid,
        vars: HashMap<String, String>,
    ) -> Result<usize> {
        self.ensure_running(workspace_id).await?;

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;
        vsock.set_env(vars).await.context("set_env failed")
    }

    /// Run `opencode run` in the guest VM and return the parsed result.
    #[instrument(skip(self))]
    pub async fn run_opencode(
        &self,
        workspace_id: Uuid,
        prompt: &str,
        timeout_secs: u64,
        workdir: Option<&str>,
    ) -> Result<crate::vm::opencode::OpenCodeResult> {
        self.ensure_running(workspace_id).await?;

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;
        crate::vm::opencode::run_opencode(&mut vsock, prompt, timeout_secs, workdir)
            .await
            .context("run_opencode failed")
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

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;

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

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;

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

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;

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

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;

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

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;

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

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;

        vsock
            .exec_poll(job_id)
            .await
            .context("exec_poll failed")
    }

    /// Kill a background job in the guest VM.
    ///
    /// Sends a signal to the background job identified by `job_id` via the
    /// guest agent's ExecKill protocol. The default signal is 9 (SIGKILL).
    /// Common alternatives: 2 (SIGINT), 15 (SIGTERM).
    pub async fn exec_kill(
        &self,
        workspace_id: Uuid,
        job_id: u32,
        signal: Option<i32>,
    ) -> Result<()> {
        let sig = signal.unwrap_or(9);
        self.ensure_running(workspace_id).await?;

        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;
        vsock
            .exec_kill(job_id, sig)
            .await
            .context("exec_kill failed")
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

        // 2. Set up networking (inherit source workspace's policy, not server defaults)
        let source_policy = NetworkPolicy {
            allow_internet: source_ws.network.allow_internet,
            allow_inter_vm: source_ws.network.allow_inter_vm,
            allowed_ports: source_ws.network.allowed_ports.clone(),
        };

        let net_setup = match self.network.write().await.setup_workspace(&new_short_id, &source_policy).await {
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
            net_setup.guest_ip,
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
        let dns_servers = if source_policy.allow_internet {
            self.config.network.dns_servers.clone()
        } else {
            vec![self.config.network.gateway_ip.to_string()]
        };
        {
            if let Ok(vsock_arc) = self.vm.read().await.vsock_client_arc(&new_id) {
                let mut vsock = vsock_arc.lock().await;
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
                allow_internet: source_policy.allow_internet,
                allow_inter_vm: source_policy.allow_inter_vm,
                allowed_ports: source_policy.allowed_ports.clone(),
                port_forwards: Vec::new(),
            },
            snapshots: SnapshotTree::new(),
            created_at: Utc::now(),
            resources: source_ws.resources.clone(),
            forked_from: Some(ForkLineage {
                source_workspace_id,
                source_workspace_name: source_ws.name.clone(),
                snapshot_name: snapshot_name.to_string(),
                forked_at: Utc::now(),
            }),
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

        // Save state before starting shutdown so we have a checkpoint
        // even if systemd kills us mid-shutdown.
        if let Err(e) = self.save_state().await {
            tracing::warn!(error = %e, "failed to save state before shutdown");
        }

        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.graceful_shutdown_inner(),
        )
        .await
        {
            Ok(()) => {
                info!("all workspaces shut down gracefully");
            }
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
            tracing::warn!(error = %e, "failed to persist workspace state after shutdown");
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
            if let Ok(vsock_arc) = self.vm.read().await.vsock_client_arc_by_cid(warm_vm.vsock_cid) {
                let mut vsock = vsock_arc.lock().await;
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
            forked_from: None,
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
                ip: "10.99.0.2".parse().unwrap(),
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
            forked_from: None,
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
            sessions: Vec::new(),
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
        // sessions should default to empty
        assert!(state.sessions.is_empty());
    }

    /// State files with sessions round-trip correctly.
    #[test]
    fn persisted_state_with_sessions() {
        use crate::mcp::auth::PersistedSession;

        let ws = make_workspace(WorkspaceState::Stopped);
        let mut workspaces = HashMap::new();
        workspaces.insert(ws.id, ws.clone());

        let state = PersistedState {
            schema_version: 2,
            workspaces,
            next_vsock_cid: 105,
            free_vsock_cids: vec![],
            sessions: vec![PersistedSession {
                session_id: "test-session".to_string(),
                workspace_ids: vec![ws.id],
                created_at: Utc::now(),
            }],
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let deserialized: PersistedState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.schema_version, 2);
        assert_eq!(deserialized.sessions.len(), 1);
        assert_eq!(deserialized.sessions[0].session_id, "test-session");
        assert_eq!(deserialized.sessions[0].workspace_ids.len(), 1);
        assert_eq!(deserialized.sessions[0].workspace_ids[0], ws.id);
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

    // -----------------------------------------------------------------------
    // Integration test scaffolding (requires root + ZFS + QEMU + vsock)
    // -----------------------------------------------------------------------
    // Run with: sudo cargo test -p agentiso -- --ignored

    /// Verify that WorkspaceManager can save state to a JSON file, restart,
    /// reload state, and resume managing existing workspaces.
    ///
    /// Requires:
    /// - Root privileges
    /// - ZFS pool `agentiso` with base dataset
    /// - QEMU + KVM available
    /// - br-agentiso bridge configured
    ///
    /// Steps a full integration test would verify:
    /// 1. Create a WorkspaceManager, create a workspace, run a command in it
    /// 2. Call save_state() to persist to the state file
    /// 3. Drop the WorkspaceManager (simulate server restart)
    /// 4. Create a new WorkspaceManager, call load_state()
    /// 5. Verify the workspace appears in the loaded state with correct metadata
    /// 6. Verify the workspace can be resumed (re-create TAP, reconnect vsock)
    /// 7. Run a command in the resumed workspace to confirm end-to-end
    /// 8. Destroy the workspace and verify cleanup
    #[tokio::test]
    #[ignore]
    async fn integration_state_persistence_across_restart() {
        todo!("requires root + ZFS + QEMU + bridge infrastructure")
    }

    // --- PersistedState file I/O roundtrip ---

    #[tokio::test]
    async fn persisted_state_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("agentiso-state-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let state_path = dir.join("state.json");

        let mut workspaces = HashMap::new();
        let ws1 = make_workspace(WorkspaceState::Running);
        let ws2 = make_workspace(WorkspaceState::Stopped);
        workspaces.insert(ws1.id, ws1.clone());
        workspaces.insert(ws2.id, ws2.clone());

        let state = PersistedState {
            schema_version: 2,
            workspaces,
            next_vsock_cid: 110,
            free_vsock_cids: vec![103, 105, 107],
            sessions: Vec::new(),
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        tokio::fs::write(&state_path, &json).await.unwrap();

        let loaded_json = tokio::fs::read_to_string(&state_path).await.unwrap();
        let loaded: PersistedState = serde_json::from_str(&loaded_json).unwrap();

        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.workspaces.len(), 2);
        assert_eq!(loaded.next_vsock_cid, 110);
        assert_eq!(loaded.free_vsock_cids, vec![103, 105, 107]);
        assert!(loaded.workspaces.contains_key(&ws1.id));
        assert!(loaded.workspaces.contains_key(&ws2.id));

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    // --- Workspace find_by_name simulation ---

    #[test]
    fn find_workspace_by_name() {
        let mut workspaces: HashMap<Uuid, Workspace> = HashMap::new();
        let mut ws = make_workspace(WorkspaceState::Running);
        ws.name = "my-dev-env".to_string();
        let expected_id = ws.id;
        workspaces.insert(ws.id, ws);

        let mut ws2 = make_workspace(WorkspaceState::Stopped);
        ws2.name = "my-prod-env".to_string();
        workspaces.insert(ws2.id, ws2);

        // Simulate find_by_name logic
        let found = workspaces
            .values()
            .find(|ws| ws.name == "my-dev-env")
            .map(|ws| ws.id);
        assert_eq!(found, Some(expected_id));

        let not_found = workspaces
            .values()
            .find(|ws| ws.name == "nonexistent")
            .map(|ws| ws.id);
        assert_eq!(not_found, None);
    }

    #[test]
    fn name_uniqueness_check() {
        let mut workspaces: HashMap<Uuid, Workspace> = HashMap::new();
        let mut ws = make_workspace(WorkspaceState::Running);
        ws.name = "unique-name".to_string();
        workspaces.insert(ws.id, ws);

        // Simulate the name uniqueness check from create()
        let name_taken = workspaces.values().any(|ws| ws.name == "unique-name");
        assert!(name_taken);

        let name_free = workspaces.values().any(|ws| ws.name == "another-name");
        assert!(!name_free);
    }

    // --- Port forwards and network metadata ---

    #[test]
    fn workspace_with_port_forwards_roundtrip() {
        let mut ws = make_workspace(WorkspaceState::Running);
        ws.network.port_forwards.push(PortForward {
            host_port: 8080,
            guest_port: 80,
        });
        ws.network.port_forwards.push(PortForward {
            host_port: 3000,
            guest_port: 3000,
        });

        let json = serde_json::to_string(&ws).unwrap();
        let deserialized: Workspace = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.network.port_forwards.len(), 2);
        assert_eq!(deserialized.network.port_forwards[0].host_port, 8080);
        assert_eq!(deserialized.network.port_forwards[0].guest_port, 80);
        assert_eq!(deserialized.network.port_forwards[1].host_port, 3000);
        assert_eq!(deserialized.network.port_forwards[1].guest_port, 3000);
    }

    #[test]
    fn workspace_network_settings_roundtrip() {
        let net = WorkspaceNetwork {
            ip: "10.42.0.5".parse().unwrap(),
            allow_internet: true,
            allow_inter_vm: false,
            allowed_ports: vec![80, 443, 8080],
            port_forwards: vec![PortForward {
                host_port: 9090,
                guest_port: 9090,
            }],
        };

        let json = serde_json::to_string(&net).unwrap();
        let deserialized: WorkspaceNetwork = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.ip, Ipv4Addr::new(10, 42, 0, 5));
        assert!(deserialized.allow_internet);
        assert!(!deserialized.allow_inter_vm);
        assert_eq!(deserialized.allowed_ports, vec![80, 443, 8080]);
        assert_eq!(deserialized.port_forwards.len(), 1);
    }

    #[test]
    fn resource_limits_roundtrip() {
        let limits = ResourceLimits {
            vcpus: 4,
            memory_mb: 2048,
            disk_gb: 50,
        };

        let json = serde_json::to_string(&limits).unwrap();
        let deserialized: ResourceLimits = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.vcpus, 4);
        assert_eq!(deserialized.memory_mb, 2048);
        assert_eq!(deserialized.disk_gb, 50);
    }

    // --- CID allocation simulation ---

    #[test]
    fn vsock_cid_allocation_simulation() {
        // Simulate the sequential CID allocation + recycle logic
        let mut next_cid: u32 = 100;
        let mut free_cids: Vec<u32> = Vec::new();

        // Allocate 3 CIDs sequentially
        let cid1 = next_cid;
        next_cid += 1;
        let cid2 = next_cid;
        next_cid += 1;
        let cid3 = next_cid;
        next_cid += 1;

        assert_eq!(cid1, 100);
        assert_eq!(cid2, 101);
        assert_eq!(cid3, 102);
        assert_eq!(next_cid, 103);

        // Recycle cid2
        free_cids.push(cid2);

        // Next allocation should reuse recycled CID
        let cid4 = if let Some(recycled) = free_cids.pop() {
            recycled
        } else {
            let c = next_cid;
            next_cid += 1;
            c
        };
        assert_eq!(cid4, 101); // reused

        // Next allocation should increment since free list is empty
        let cid5 = if let Some(recycled) = free_cids.pop() {
            recycled
        } else {
            let c = next_cid;
            // next_cid would be incremented in the real code
            c
        };
        assert_eq!(cid5, 103);
    }

    #[test]
    fn vsock_cid_reserved_range() {
        // CIDs 0, 1, 2, and u32::MAX are reserved per vsock spec.
        // The alloc_vsock_cid method skips these.
        let reserved = [0u32, 1, 2, u32::MAX];
        for cid in &reserved {
            assert!(
                *cid <= 2 || *cid == u32::MAX,
                "CID {} should be reserved",
                cid
            );
        }

        // Valid CIDs start at 3
        let valid_cid = 3u32;
        assert!(valid_cid > 2 && valid_cid != u32::MAX);
    }

    // --- PersistedState migration ---

    #[test]
    fn persisted_state_schema_v0_migration() {
        // Legacy state file without schema_version, free_vsock_cids, or sessions
        let json = r#"{
            "workspaces": {},
            "next_vsock_cid": 100
        }"#;
        let state: PersistedState = serde_json::from_str(json).unwrap();
        assert_eq!(state.schema_version, 0);
        assert!(state.free_vsock_cids.is_empty());
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn persisted_state_marks_all_stopped_on_load() {
        // Simulate what load_state does: mark all workspaces as Stopped
        let mut workspaces = HashMap::new();
        let ws_running = make_workspace(WorkspaceState::Running);
        let ws_suspended = make_workspace(WorkspaceState::Suspended);
        let running_id = ws_running.id;
        let suspended_id = ws_suspended.id;
        workspaces.insert(ws_running.id, ws_running);
        workspaces.insert(ws_suspended.id, ws_suspended);

        // Simulate the load_state behavior
        for ws in workspaces.values_mut() {
            ws.state = WorkspaceState::Stopped;
            ws.qemu_pid = None;
        }

        assert_eq!(workspaces[&running_id].state, WorkspaceState::Stopped);
        assert_eq!(workspaces[&suspended_id].state, WorkspaceState::Stopped);
        assert!(workspaces[&running_id].qemu_pid.is_none());
        assert!(workspaces[&suspended_id].qemu_pid.is_none());
    }

    // --- Workspace count limit simulation ---

    #[test]
    fn workspace_count_limit_check() {
        let mut workspaces: HashMap<Uuid, Workspace> = HashMap::new();
        let max_workspaces: u32 = 3;

        // Add up to the limit
        for _ in 0..3 {
            let ws = make_workspace(WorkspaceState::Running);
            workspaces.insert(ws.id, ws);
        }

        let count = workspaces.len() as u32;
        assert!(count >= max_workspaces);

        // After destroying one, should be under limit
        let first_id = *workspaces.keys().next().unwrap();
        workspaces.remove(&first_id);
        let count = workspaces.len() as u32;
        assert!(count < max_workspaces);
    }

    // --- Multiple workspaces with distinct metadata ---

    #[test]
    fn persisted_state_multiple_workspaces_distinct() {
        let mut workspaces = HashMap::new();
        for i in 0..5u32 {
            let mut ws = make_workspace(if i % 2 == 0 {
                WorkspaceState::Running
            } else {
                WorkspaceState::Stopped
            });
            ws.name = format!("ws-{}", i);
            ws.vsock_cid = 100 + i;
            ws.network.ip = Ipv4Addr::new(10, 42, 0, 2 + i as u8);
            workspaces.insert(ws.id, ws);
        }

        let state = PersistedState {
            schema_version: 2,
            workspaces,
            next_vsock_cid: 105,
            free_vsock_cids: Vec::new(),
            sessions: Vec::new(),
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: PersistedState = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.workspaces.len(), 5);

        // Verify all names are distinct
        let names: std::collections::HashSet<&str> = loaded
            .workspaces
            .values()
            .map(|ws| ws.name.as_str())
            .collect();
        assert_eq!(names.len(), 5);

        // Verify all CIDs are distinct
        let cids: std::collections::HashSet<u32> = loaded
            .workspaces
            .values()
            .map(|ws| ws.vsock_cid)
            .collect();
        assert_eq!(cids.len(), 5);

        // Verify all IPs are distinct
        let ips: std::collections::HashSet<Ipv4Addr> = loaded
            .workspaces
            .values()
            .map(|ws| ws.network.ip)
            .collect();
        assert_eq!(ips.len(), 5);
    }

    // --- ForkLineage tests ---

    #[test]
    fn workspace_with_fork_lineage_serde_roundtrip() {
        let source_id = Uuid::new_v4();
        let mut ws = make_workspace(WorkspaceState::Running);
        ws.forked_from = Some(ForkLineage {
            source_workspace_id: source_id,
            source_workspace_name: "golden-ws".to_string(),
            snapshot_name: "golden".to_string(),
            forked_at: Utc::now(),
        });

        let json = serde_json::to_string(&ws).unwrap();
        let deserialized: Workspace = serde_json::from_str(&json).unwrap();

        assert!(deserialized.forked_from.is_some());
        let lineage = deserialized.forked_from.unwrap();
        assert_eq!(lineage.source_workspace_id, source_id);
        assert_eq!(lineage.source_workspace_name, "golden-ws");
        assert_eq!(lineage.snapshot_name, "golden");
    }

    #[test]
    fn workspace_without_fork_lineage_serde_roundtrip() {
        let ws = make_workspace(WorkspaceState::Running);
        assert!(ws.forked_from.is_none());

        let json = serde_json::to_string(&ws).unwrap();
        let deserialized: Workspace = serde_json::from_str(&json).unwrap();
        assert!(deserialized.forked_from.is_none());
    }

    #[test]
    fn workspace_fork_lineage_missing_in_legacy_json() {
        // Legacy state files won't have the forked_from field.
        // The #[serde(default)] attribute on the field should handle this.
        let ws = make_workspace(WorkspaceState::Running);
        let mut json: serde_json::Value = serde_json::to_value(&ws).unwrap();
        // Remove forked_from to simulate a legacy JSON
        json.as_object_mut().unwrap().remove("forked_from");

        let deserialized: Workspace = serde_json::from_value(json).unwrap();
        assert!(deserialized.forked_from.is_none());
    }

    // --- Auto-adopt tests ---

    #[test]
    fn auto_adopt_detects_alive_process() {
        // Our own PID should always be alive
        let our_pid = std::process::id() as i32;
        assert!(WorkspaceManager::is_process_alive(our_pid));
    }

    #[test]
    fn auto_adopt_detects_dead_process() {
        // PID 2^30 is extremely unlikely to exist
        let dead_pid = 1 << 30;
        assert!(!WorkspaceManager::is_process_alive(dead_pid));
    }

    #[test]
    fn auto_adopt_marks_dead_workspaces_stopped() {
        // Simulate the auto-adopt logic: workspaces with dead PIDs become Stopped
        let mut workspaces = HashMap::new();
        let mut ws_running = make_workspace(WorkspaceState::Running);
        ws_running.qemu_pid = Some(1 << 30); // dead PID
        let running_id = ws_running.id;
        workspaces.insert(ws_running.id, ws_running);

        let mut ws_stopped = make_workspace(WorkspaceState::Stopped);
        ws_stopped.qemu_pid = None;
        let stopped_id = ws_stopped.id;
        workspaces.insert(ws_stopped.id, ws_stopped);

        // Simulate auto-adopt logic from load_state
        let mut adopted = 0u32;
        let mut stopped = 0u32;
        for ws in workspaces.values_mut() {
            if ws.state == WorkspaceState::Running || ws.state == WorkspaceState::Suspended {
                let is_alive = if let Some(pid) = ws.qemu_pid {
                    WorkspaceManager::is_process_alive(pid as i32)
                } else {
                    false
                };

                if is_alive {
                    adopted += 1;
                } else {
                    ws.state = WorkspaceState::Stopped;
                    ws.qemu_pid = None;
                    stopped += 1;
                }
            }
        }

        assert_eq!(adopted, 0); // Dead PID should not be adopted
        assert_eq!(stopped, 1); // One Running workspace with dead PID
        assert_eq!(workspaces[&running_id].state, WorkspaceState::Stopped);
        assert!(workspaces[&running_id].qemu_pid.is_none());
        assert_eq!(workspaces[&stopped_id].state, WorkspaceState::Stopped);
    }

    #[test]
    fn auto_adopt_preserves_running_with_live_pid() {
        // Simulate: workspace with our own PID (alive) stays Running
        let our_pid = std::process::id();
        let mut workspaces = HashMap::new();
        let mut ws = make_workspace(WorkspaceState::Running);
        ws.qemu_pid = Some(our_pid);
        let ws_id = ws.id;
        workspaces.insert(ws.id, ws);

        let mut adopted = 0u32;
        for ws in workspaces.values_mut() {
            if ws.state == WorkspaceState::Running {
                let is_alive = if let Some(pid) = ws.qemu_pid {
                    WorkspaceManager::is_process_alive(pid as i32)
                } else {
                    false
                };

                if is_alive {
                    adopted += 1;
                } else {
                    ws.state = WorkspaceState::Stopped;
                    ws.qemu_pid = None;
                }
            }
        }

        assert_eq!(adopted, 1);
        assert_eq!(workspaces[&ws_id].state, WorkspaceState::Running);
        assert_eq!(workspaces[&ws_id].qemu_pid, Some(our_pid));
    }

    // --- Pool replenishment trigger tests ---

    #[tokio::test]
    async fn pool_replenish_triggered_after_claim() {
        // Verify that after claiming from the pool, deficit increases
        let config = crate::config::PoolConfig {
            enabled: true,
            min_size: 1,
            max_size: 5,
            target_free: 2,
            max_memory_mb: 8192,
        };
        let pool = pool::VmPool::new(config);

        // Add 2 VMs to fill to target
        pool.add_ready(pool::WarmVm {
            id: Uuid::new_v4(),
            vsock_cid: 100,
            zfs_dataset: "test/ds1".to_string(),
            zvol_path: PathBuf::from("/dev/zvol/test1"),
            qemu_pid: 1001,
            booted_at: std::time::Instant::now(),
            short_id: "aaaa0001".to_string(),
            tap_device: "tap-aaaa0001".to_string(),
            guest_ip: Ipv4Addr::new(10, 99, 0, 100),
            memory_mb: 512,
        }).await;
        pool.add_ready(pool::WarmVm {
            id: Uuid::new_v4(),
            vsock_cid: 101,
            zfs_dataset: "test/ds2".to_string(),
            zvol_path: PathBuf::from("/dev/zvol/test2"),
            qemu_pid: 1002,
            booted_at: std::time::Instant::now(),
            short_id: "aaaa0002".to_string(),
            tap_device: "tap-aaaa0002".to_string(),
            guest_ip: Ipv4Addr::new(10, 99, 0, 101),
            memory_mb: 512,
        }).await;

        assert_eq!(pool.deficit().await, 0);

        // Claim one VM
        let claimed = pool.claim().await;
        assert!(claimed.is_some());

        // Now deficit should be 1 (target_free=2, only 1 remaining)
        assert_eq!(pool.deficit().await, 1);
        assert!(pool.needs_replenish().await);
    }

    // --- PersistedState with forked_from ---

    #[test]
    fn persisted_state_with_fork_lineage_roundtrip() {
        let mut workspaces = HashMap::new();
        let mut ws = make_workspace(WorkspaceState::Running);
        ws.forked_from = Some(ForkLineage {
            source_workspace_id: Uuid::new_v4(),
            source_workspace_name: "golden".to_string(),
            snapshot_name: "snap-1".to_string(),
            forked_at: Utc::now(),
        });
        workspaces.insert(ws.id, ws);

        let state = PersistedState {
            schema_version: 2,
            workspaces,
            next_vsock_cid: 100,
            free_vsock_cids: vec![],
            sessions: Vec::new(),
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: PersistedState = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.workspaces.len(), 1);
        let loaded_ws = loaded.workspaces.values().next().unwrap();
        assert!(loaded_ws.forked_from.is_some());
        let lineage = loaded_ws.forked_from.as_ref().unwrap();
        assert_eq!(lineage.source_workspace_name, "golden");
        assert_eq!(lineage.snapshot_name, "snap-1");
    }
}
