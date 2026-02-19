pub mod microvm;
pub mod qemu;
pub mod vsock;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::vm::qemu::read_tail;

use crate::config::InitMode;
use crate::vm::microvm::VmConfig;
use crate::vm::qemu::{QmpClient, VmStatus};
use crate::vm::vsock::VsockClient;

/// Handle to a running VM, holding its process, QMP connection, and vsock client.
#[allow(dead_code)] // Fields accessed by WorkspaceManager via pub accessors
pub struct VmHandle {
    /// Workspace ID this VM belongs to.
    pub workspace_id: Uuid,
    /// QEMU child process.
    pub process: tokio::process::Child,
    /// QMP client for VM management commands.
    pub qmp: QmpClient,
    /// vsock client for guest agent communication.
    pub vsock: VsockClient,
    /// The configuration used to launch this VM.
    pub config: VmConfig,
    /// QEMU process PID (cached from spawn).
    pub pid: u32,
}

/// Global configuration for the VM manager.
#[derive(Debug, Clone)]
pub struct VmManagerConfig {
    /// Path to the kernel binary (bzImage or vmlinux).
    pub kernel_path: PathBuf,
    /// Optional path to the initramfs image.
    pub initrd_path: Option<PathBuf>,
    /// Base directory for QEMU runtime files (sockets, pid files).
    /// Each workspace gets a subdirectory: {run_dir}/{workspace_short_id}/
    pub run_dir: PathBuf,
    /// Default kernel command line.
    pub kernel_cmdline: String,
    /// Init mode: Fast or OpenRC.
    pub init_mode: InitMode,
    /// Optional path to the fast initrd (used when init_mode = Fast).
    pub initrd_fast_path: Option<PathBuf>,
    /// Timeout for QMP socket to appear after QEMU spawn.
    pub qmp_connect_timeout: std::time::Duration,
    /// Timeout for guest agent readiness handshake.
    pub guest_ready_timeout: std::time::Duration,
    /// vsock port where the guest agent listens.
    pub guest_agent_port: u32,
}

impl Default for VmManagerConfig {
    fn default() -> Self {
        Self {
            kernel_path: PathBuf::from("/var/lib/agentiso/vmlinuz"),
            initrd_path: Some(PathBuf::from("/var/lib/agentiso/initrd.img")),
            run_dir: PathBuf::from("/run/agentiso"),
            kernel_cmdline: "console=ttyS0 root=/dev/vda rw quiet".into(),
            init_mode: InitMode::default(),
            initrd_fast_path: None,
            qmp_connect_timeout: std::time::Duration::from_secs(5),
            guest_ready_timeout: std::time::Duration::from_secs(30),
            guest_agent_port: 5000,
        }
    }
}

/// Manages the lifecycle of QEMU microvm instances.
///
/// Handles spawning, connecting, pausing, resuming, and destroying VMs.
/// Each VM is tracked by its workspace UUID.
pub struct VmManager {
    config: VmManagerConfig,
    /// Active VM handles, keyed by workspace UUID.
    vms: HashMap<Uuid, VmHandle>,
}

#[allow(dead_code)] // Public API consumed by WorkspaceManager
impl VmManager {
    /// Create a new VM manager with the given configuration.
    pub fn new(config: VmManagerConfig) -> Self {
        Self {
            config,
            vms: HashMap::new(),
        }
    }

    /// Launch a new VM for a workspace.
    ///
    /// This will:
    /// 1. Build the QEMU command from the provided config parameters
    /// 2. Spawn the QEMU process
    /// 3. Wait for the QMP socket and negotiate capabilities
    /// 4. Connect to the guest agent via vsock and wait for readiness
    ///
    /// Returns the QEMU PID on success. The VM handle is stored internally.
    pub async fn launch(
        &mut self,
        workspace_id: Uuid,
        vcpus: u32,
        memory_mb: u32,
        root_disk: PathBuf,
        tap_device: String,
        vsock_cid: u32,
        guest_ip: std::net::Ipv4Addr,
    ) -> Result<u32> {
        if self.vms.contains_key(&workspace_id) {
            bail!("VM already running for workspace {}", workspace_id);
        }

        let short_id = &workspace_id.to_string()[..8];
        let run_dir = self.config.run_dir.join(short_id);
        let qmp_socket = run_dir.join("qmp.sock");

        let vm_config = VmConfig {
            vcpus,
            memory_mb,
            kernel_path: self.config.kernel_path.clone(),
            initrd_path: if self.config.init_mode == InitMode::Fast {
                self.config.initrd_fast_path.clone().or(self.config.initrd_path.clone())
            } else {
                self.config.initrd_path.clone()
            },
            kernel_cmdline: self.config.kernel_cmdline.clone(),
            init_mode: self.config.init_mode.clone(),
            root_disk,
            tap_device,
            mac_address: mac_from_ip(guest_ip),
            vsock_cid,
            qmp_socket: qmp_socket.clone(),
            run_dir: run_dir.clone(),
        };

        info!(
            workspace = %workspace_id,
            vcpus,
            memory_mb,
            cid = vsock_cid,
            "launching VM"
        );

        // Record start time for boot duration measurement
        let boot_start = Instant::now();

        // Spawn the QEMU process
        let mut child = qemu::spawn_qemu(&vm_config).await?;
        let pid = child
            .id()
            .context("QEMU process exited immediately after spawn")?;

        // Wait for QMP socket to appear
        if let Err(e) = qemu::wait_for_qmp_socket(&qmp_socket, self.config.qmp_connect_timeout).await {
            error!(workspace = %workspace_id, "QMP socket did not appear, killing QEMU");
            let console_tail = read_tail(&run_dir.join("console.log"), 30).await;
            let stderr_tail = read_tail(&run_dir.join("qemu-stderr.log"), 30).await;
            let _ = child.kill().await;
            return Err(e.context(format!(
                "QMP socket did not appear.\n--- console.log (last 30 lines) ---\n{}\n--- qemu-stderr.log (last 30 lines) ---\n{}",
                console_tail, stderr_tail
            )));
        }

        // Connect QMP client
        let qmp = match QmpClient::connect_with_retry(
            &qmp_socket,
            5,
            std::time::Duration::from_millis(200),
        )
        .await
        {
            Ok(qmp) => qmp,
            Err(e) => {
                error!(workspace = %workspace_id, "QMP connection failed, killing QEMU");
                let console_tail = read_tail(&run_dir.join("console.log"), 30).await;
                let stderr_tail = read_tail(&run_dir.join("qemu-stderr.log"), 30).await;
                let _ = child.kill().await;
                return Err(e.context(format!(
                    "failed to connect QMP after QEMU spawn.\n--- console.log (last 30 lines) ---\n{}\n--- qemu-stderr.log (last 30 lines) ---\n{}",
                    console_tail, stderr_tail
                )));
            }
        };

        // Connect vsock client and wait for guest agent readiness
        let vsock = match VsockClient::connect_and_wait(
            vsock_cid,
            self.config.guest_agent_port,
            self.config.guest_ready_timeout,
        )
        .await
        {
            Ok(vsock) => vsock,
            Err(e) => {
                error!(workspace = %workspace_id, "guest agent not ready, killing QEMU");
                let console_tail = read_tail(&run_dir.join("console.log"), 30).await;
                let stderr_tail = read_tail(&run_dir.join("qemu-stderr.log"), 30).await;
                let _ = child.kill().await;
                return Err(e.context(format!(
                    "guest agent readiness check failed.\n--- console.log (last 30 lines) ---\n{}\n--- qemu-stderr.log (last 30 lines) ---\n{}",
                    console_tail, stderr_tail
                )));
            }
        };

        let boot_elapsed = boot_start.elapsed();
        let boot_ms = boot_elapsed.as_millis();

        // Warn if boot took more than 50% of the guest_ready_timeout
        let timeout_half = self.config.guest_ready_timeout / 2;
        if boot_elapsed > timeout_half {
            warn!(
                workspace = %workspace_id,
                boot_ms,
                timeout_ms = self.config.guest_ready_timeout.as_millis(),
                "VM boot took more than 50% of timeout"
            );
        }

        info!(
            workspace = %workspace_id,
            pid,
            boot_ms,
            "VM boot complete"
        );

        self.vms.insert(
            workspace_id,
            VmHandle {
                workspace_id,
                process: child,
                qmp,
                vsock,
                config: vm_config,
                pid,
            },
        );

        Ok(pid)
    }

    /// Get a mutable reference to a VM handle.
    pub fn get_mut(&mut self, workspace_id: &Uuid) -> Result<&mut VmHandle> {
        self.vms
            .get_mut(workspace_id)
            .with_context(|| format!("no running VM for workspace {}", workspace_id))
    }

    /// Get a reference to a VM handle.
    pub fn get(&self, workspace_id: &Uuid) -> Result<&VmHandle> {
        self.vms
            .get(workspace_id)
            .with_context(|| format!("no running VM for workspace {}", workspace_id))
    }

    /// Check if a VM is running for the given workspace.
    pub fn is_running(&self, workspace_id: &Uuid) -> bool {
        self.vms.contains_key(workspace_id)
    }

    /// Gracefully stop a VM by sending a guest agent Shutdown request, with
    /// fallback to ACPI powerdown and then force-kill.
    ///
    /// The shutdown sequence is:
    /// 1. Send Shutdown to guest agent via vsock (triggers `poweroff` inside VM)
    /// 2. Wait up to `timeout` for the QEMU process to exit
    /// 3. If still running, try ACPI powerdown via QMP
    /// 4. If still running after another brief wait, force-kill
    pub async fn stop(
        &mut self,
        workspace_id: &Uuid,
        timeout: std::time::Duration,
    ) -> Result<()> {
        // Early check: if the VM already exited, skip the shutdown sequence
        if !self.is_running(workspace_id) {
            debug!(workspace = %workspace_id, "VM not tracked, nothing to stop");
            return Ok(());
        }
        match self.check_vm_alive(workspace_id).await {
            Ok(false) => {
                info!(workspace = %workspace_id, "VM already exited, skipping shutdown sequence");
                return Ok(());
            }
            Err(_) => {
                // VM not tracked anymore (cleaned up by check_vm_alive or race)
                return Ok(());
            }
            Ok(true) => {
                // VM is alive, proceed with shutdown
            }
        }

        let handle = self
            .vms
            .get_mut(workspace_id)
            .with_context(|| format!("no running VM for workspace {}", workspace_id))?;

        info!(workspace = %workspace_id, "stopping VM");

        // Step 1: Try graceful shutdown via guest agent (runs `poweroff` in VM)
        match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            handle.vsock.shutdown(),
        )
        .await
        {
            Ok(Ok(())) => {
                debug!(workspace = %workspace_id, "guest agent shutdown request sent");
            }
            Ok(Err(e)) => {
                debug!(workspace = %workspace_id, error = %e, "guest agent shutdown failed (expected if already down)");
            }
            Err(_) => {
                debug!(workspace = %workspace_id, "guest agent shutdown timed out");
            }
        }

        // Step 2: Wait for the process to exit
        let wait_result = tokio::time::timeout(timeout, handle.process.wait()).await;
        match wait_result {
            Ok(Ok(status)) => {
                debug!(
                    workspace = %workspace_id,
                    exit_code = ?status.code(),
                    "VM exited gracefully via guest agent shutdown"
                );
                self.cleanup_vm(workspace_id).await;
                return Ok(());
            }
            Ok(Err(e)) => {
                warn!(workspace = %workspace_id, error = %e, "error waiting for VM exit");
            }
            Err(_) => {
                debug!(workspace = %workspace_id, "VM did not exit after guest shutdown, trying ACPI powerdown");
            }
        }

        // Step 3: Try ACPI powerdown as fallback (re-borrow handle after timeout)
        let handle = self
            .vms
            .get_mut(workspace_id)
            .with_context(|| format!("VM disappeared during stop for workspace {}", workspace_id))?;

        if let Err(e) = handle.qmp.system_powerdown().await {
            warn!(workspace = %workspace_id, error = %e, "ACPI powerdown failed, will force kill");
        } else {
            // Give ACPI a brief chance
            let acpi_timeout = std::time::Duration::from_secs(3);
            let wait_result = tokio::time::timeout(acpi_timeout, handle.process.wait()).await;
            if let Ok(Ok(status)) = wait_result {
                debug!(
                    workspace = %workspace_id,
                    exit_code = ?status.code(),
                    "VM exited via ACPI powerdown"
                );
                self.cleanup_vm(workspace_id).await;
                return Ok(());
            }
            warn!(workspace = %workspace_id, "ACPI powerdown did not stop VM, force killing");
        }

        // Step 4: Force kill
        let handle = self
            .vms
            .get_mut(workspace_id)
            .with_context(|| format!("VM disappeared during stop for workspace {}", workspace_id))?;
        let pid = handle.pid;
        if let Err(e) = handle.process.kill().await {
            warn!(workspace = %workspace_id, error = %e, "failed to kill QEMU child");
            let _ = qemu::kill_qemu(pid).await;
        }

        self.cleanup_vm(workspace_id).await;
        Ok(())
    }

    /// Force-kill a VM immediately without graceful shutdown.
    pub async fn force_kill(&mut self, workspace_id: &Uuid) -> Result<()> {
        let handle = self
            .vms
            .get_mut(workspace_id)
            .with_context(|| format!("no running VM for workspace {}", workspace_id))?;

        warn!(workspace = %workspace_id, "force-killing VM");

        let pid = handle.pid;
        if let Err(e) = handle.process.kill().await {
            warn!(workspace = %workspace_id, error = %e, "kill via child handle failed");
            qemu::kill_qemu(pid).await?;
        }

        self.cleanup_vm(workspace_id).await;
        Ok(())
    }

    /// Pause (suspend) a running VM.
    pub async fn pause(&mut self, workspace_id: &Uuid) -> Result<()> {
        let handle = self.get_mut(workspace_id)?;
        handle.qmp.stop().await?;
        info!(workspace = %workspace_id, "VM paused");
        Ok(())
    }

    /// Resume a paused VM.
    pub async fn resume(&mut self, workspace_id: &Uuid) -> Result<()> {
        let handle = self.get_mut(workspace_id)?;
        handle.qmp.cont().await?;
        info!(workspace = %workspace_id, "VM resumed");
        Ok(())
    }

    /// Query the VM status via QMP.
    pub async fn query_status(&mut self, workspace_id: &Uuid) -> Result<VmStatus> {
        let handle = self.get_mut(workspace_id)?;
        handle.qmp.query_status().await
    }

    /// Check whether the QEMU process for a workspace is still alive.
    ///
    /// Uses `try_wait()` on the child process handle to check for exit without
    /// blocking. If the process has exited, logs the exit code, cleans up the
    /// VM handle, and returns `false`. Returns `true` if the process is still
    /// running. Returns an error only if the workspace has no VM tracked.
    pub async fn check_vm_alive(&mut self, workspace_id: &Uuid) -> Result<bool> {
        let handle = self
            .vms
            .get_mut(workspace_id)
            .with_context(|| format!("no running VM for workspace {}", workspace_id))?;

        match handle.process.try_wait() {
            Ok(Some(status)) => {
                warn!(
                    workspace = %workspace_id,
                    exit_code = ?status.code(),
                    "VM process already exited (crashed or stopped)"
                );
                self.cleanup_vm(workspace_id).await;
                Ok(false)
            }
            Ok(None) => {
                // Process is still running
                Ok(true)
            }
            Err(e) => {
                warn!(
                    workspace = %workspace_id,
                    error = %e,
                    "failed to check VM process status"
                );
                // Conservatively assume it's dead and clean up
                self.cleanup_vm(workspace_id).await;
                Ok(false)
            }
        }
    }

    /// Save VM memory state for a live snapshot.
    ///
    /// NOTE: This uses QEMU's internal `savevm` command, which requires a
    /// writable block device that supports internal snapshots (e.g., qcow2).
    /// Raw block devices (such as ZFS zvols) do NOT support this. Callers
    /// should check the disk format before calling, or handle the error.
    pub async fn save_vm_state(&mut self, workspace_id: &Uuid, tag: &str) -> Result<()> {
        let handle = self.get_mut(workspace_id)?;
        // Raw zvols don't support QEMU internal snapshots. Check the drive format.
        if handle.config.root_disk.to_string_lossy().contains("/dev/zvol/")
            || handle.config.root_disk.to_string_lossy().contains("/dev/zd")
        {
            bail!(
                "VM memory snapshots (savevm) are not supported with raw block devices. \
                 Use disk-only snapshots via ZFS instead (include_memory=false)."
            );
        }
        handle.qmp.savevm(tag).await?;
        info!(workspace = %workspace_id, tag, "VM state saved");
        Ok(())
    }

    /// Restore VM memory state from a snapshot.
    ///
    /// NOTE: Requires qcow2 disk format. See `save_vm_state` for details.
    pub async fn load_vm_state(&mut self, workspace_id: &Uuid, tag: &str) -> Result<()> {
        let handle = self.get_mut(workspace_id)?;
        if handle.config.root_disk.to_string_lossy().contains("/dev/zvol/")
            || handle.config.root_disk.to_string_lossy().contains("/dev/zd")
        {
            bail!(
                "VM memory restore (loadvm) is not supported with raw block devices. \
                 Use disk-only snapshot restore via ZFS instead."
            );
        }
        handle.qmp.loadvm(tag).await?;
        info!(workspace = %workspace_id, tag, "VM state restored");
        Ok(())
    }

    /// Delete a saved VM state.
    ///
    /// NOTE: Requires qcow2 disk format. See `save_vm_state` for details.
    pub async fn delete_vm_state(&mut self, workspace_id: &Uuid, tag: &str) -> Result<()> {
        let handle = self.get_mut(workspace_id)?;
        if handle.config.root_disk.to_string_lossy().contains("/dev/zvol/")
            || handle.config.root_disk.to_string_lossy().contains("/dev/zd")
        {
            bail!(
                "VM memory state deletion (delvm) is not supported with raw block devices."
            );
        }
        handle.qmp.delvm(tag).await?;
        info!(workspace = %workspace_id, tag, "VM state deleted");
        Ok(())
    }

    /// Get the vsock client for a workspace's guest agent.
    pub fn vsock_client(&mut self, workspace_id: &Uuid) -> Result<&mut VsockClient> {
        let handle = self.get_mut(workspace_id)?;
        Ok(&mut handle.vsock)
    }

    /// Get a vsock client by CID (for warm pool VMs not yet assigned to a workspace).
    pub fn vsock_client_by_cid(&mut self, cid: u32) -> Result<&mut VsockClient> {
        for handle in self.vms.values_mut() {
            if handle.config.vsock_cid == cid {
                return Ok(&mut handle.vsock);
            }
        }
        bail!("no VM with vsock CID {}", cid)
    }

    /// Remove a VM from tracking and clean up runtime files.
    async fn cleanup_vm(&mut self, workspace_id: &Uuid) {
        if let Some(_handle) = self.vms.remove(workspace_id) {
            let short_id = &workspace_id.to_string()[..8];
            let run_dir = self.config.run_dir.join(short_id);

            // Best-effort cleanup of runtime directory
            if let Err(e) = tokio::fs::remove_dir_all(&run_dir).await {
                warn!(
                    workspace = %workspace_id,
                    path = %run_dir.display(),
                    error = %e,
                    "failed to clean up VM run directory"
                );
            }

            debug!(workspace = %workspace_id, "VM cleaned up");
        }
    }

    /// Shut down all running VMs. Used during daemon shutdown.
    pub async fn shutdown_all(&mut self) {
        let workspace_ids: Vec<Uuid> = self.vms.keys().copied().collect();
        let timeout = std::time::Duration::from_secs(5);

        for workspace_id in workspace_ids {
            if let Err(e) = self.stop(&workspace_id, timeout).await {
                warn!(
                    workspace = %workspace_id,
                    error = %e,
                    "failed to stop VM during shutdown"
                );
            }
        }
    }

    /// Get the list of workspace IDs with running VMs.
    pub fn running_workspaces(&self) -> Vec<Uuid> {
        self.vms.keys().copied().collect()
    }

    /// Get the PID of a workspace's QEMU process.
    pub fn pid(&self, workspace_id: &Uuid) -> Result<u32> {
        Ok(self.get(workspace_id)?.pid)
    }
}

/// Generate a unique MAC address from a guest IP.
///
/// Uses the QEMU OUI prefix `52:54:00` (locally administered) followed by
/// `00` and the last two octets of the IP address. This ensures each VM
/// on the bridge has a distinct MAC.
///
/// Example: 10.99.0.5 → 52:54:00:00:00:05
///          10.99.1.3 → 52:54:00:00:01:03
fn mac_from_ip(ip: std::net::Ipv4Addr) -> String {
    let octets = ip.octets();
    format!(
        "52:54:00:00:{:02x}:{:02x}",
        octets[2], octets[3]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mac_from_ip() {
        let ip: std::net::Ipv4Addr = "10.99.0.2".parse().unwrap();
        assert_eq!(mac_from_ip(ip), "52:54:00:00:00:02");

        let ip: std::net::Ipv4Addr = "10.99.1.3".parse().unwrap();
        assert_eq!(mac_from_ip(ip), "52:54:00:00:01:03");

        let ip: std::net::Ipv4Addr = "10.99.255.255".parse().unwrap();
        assert_eq!(mac_from_ip(ip), "52:54:00:00:ff:ff");
    }

    #[test]
    fn test_vm_manager_config_defaults() {
        let config = VmManagerConfig::default();
        assert_eq!(config.kernel_path, PathBuf::from("/var/lib/agentiso/vmlinuz"));
        assert_eq!(config.initrd_path, Some(PathBuf::from("/var/lib/agentiso/initrd.img")));
        assert_eq!(config.run_dir, PathBuf::from("/run/agentiso"));
        assert_eq!(config.kernel_cmdline, "console=ttyS0 root=/dev/vda rw quiet");
        assert_eq!(config.init_mode, InitMode::OpenRC);
        assert!(config.initrd_fast_path.is_none());
        assert_eq!(config.qmp_connect_timeout, std::time::Duration::from_secs(5));
        assert_eq!(config.guest_ready_timeout, std::time::Duration::from_secs(30));
        assert_eq!(config.guest_agent_port, 5000);
    }

    #[test]
    fn test_vm_manager_config_clone() {
        let config = VmManagerConfig::default();
        let cloned = config.clone();
        assert_eq!(config.kernel_path, cloned.kernel_path);
        assert_eq!(config.run_dir, cloned.run_dir);
        assert_eq!(config.guest_agent_port, cloned.guest_agent_port);
    }

    #[test]
    fn test_vm_manager_new_starts_empty() {
        let manager = VmManager::new(VmManagerConfig::default());
        assert!(manager.running_workspaces().is_empty());
    }

    #[test]
    fn test_vm_manager_is_running_false_for_unknown() {
        let manager = VmManager::new(VmManagerConfig::default());
        let id = Uuid::new_v4();
        assert!(!manager.is_running(&id));
    }

    #[test]
    fn test_vm_manager_get_fails_for_unknown() {
        let manager = VmManager::new(VmManagerConfig::default());
        let id = Uuid::new_v4();
        let result = manager.get(&id);
        assert!(result.is_err());
        let err_msg = format!("{}", result.err().unwrap());
        assert!(err_msg.contains("no running VM"));
    }

    #[test]
    fn test_vm_manager_get_mut_fails_for_unknown() {
        let mut manager = VmManager::new(VmManagerConfig::default());
        let id = Uuid::new_v4();
        let result = manager.get_mut(&id);
        assert!(result.is_err());
    }

    #[test]
    fn test_vm_manager_pid_fails_for_unknown() {
        let manager = VmManager::new(VmManagerConfig::default());
        let id = Uuid::new_v4();
        let result = manager.pid(&id);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_check_vm_alive_fails_for_unknown() {
        let mut manager = VmManager::new(VmManagerConfig::default());
        let id = Uuid::new_v4();
        let result = manager.check_vm_alive(&id).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.err().unwrap());
        assert!(err_msg.contains("no running VM"));
    }

    #[tokio::test]
    async fn test_stop_returns_ok_for_unknown_workspace() {
        // If a workspace is not tracked at all, stop should succeed trivially
        let mut manager = VmManager::new(VmManagerConfig::default());
        let id = Uuid::new_v4();
        let result = manager.stop(&id, std::time::Duration::from_secs(1)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_read_tail_integration() {
        // Verify read_tail is accessible from mod.rs via the re-export
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.log");
        std::fs::write(&file_path, "line1\nline2\nline3\nline4\nline5").unwrap();

        let tail = read_tail(&file_path, 3).await;
        assert!(tail.contains("line3"));
        assert!(tail.contains("line4"));
        assert!(tail.contains("line5"));
        assert!(!tail.contains("line2"));
    }

    #[tokio::test]
    async fn test_read_tail_missing_file_in_context() {
        let path = std::path::PathBuf::from("/tmp/agentiso-nonexistent-file-test.log");
        let tail = read_tail(&path, 30).await;
        assert!(tail.starts_with("[could not read"));
    }
}
