//! cgroup v2 resource limits for QEMU processes.
//!
//! Places each workspace's QEMU process into a dedicated systemd transient scope
//! under `agentiso.slice`, with memory and CPU limits derived from the workspace
//! configuration.
//!
//! All operations are best-effort: if cgroup v2 is not available or any operation
//! fails, a warning is logged but the workspace continues without cgroup isolation.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::fs;
use tracing::{debug, info, warn};

/// The cgroup v2 filesystem mount point.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Parent slice for all agentiso QEMU processes.
const PARENT_SLICE: &str = "agentiso.slice";

/// Check whether cgroup v2 is available on this system.
///
/// Returns `true` if `/sys/fs/cgroup/cgroup.controllers` exists, which
/// indicates a unified (v2) cgroup hierarchy.
pub async fn is_cgroup_v2_available() -> bool {
    fs::metadata(Path::new(CGROUP_ROOT).join("cgroup.controllers"))
        .await
        .is_ok()
}

/// Ensure the parent slice `agentiso.slice` exists under the cgroup root.
///
/// Creates the directory if it does not exist. This is called once at startup.
/// Best-effort: logs a warning and returns Ok if creation fails.
pub async fn ensure_parent_slice() -> Result<()> {
    if !is_cgroup_v2_available().await {
        warn!("cgroup v2 not available, skipping cgroup setup");
        return Ok(());
    }

    let slice_path = PathBuf::from(CGROUP_ROOT).join(PARENT_SLICE);
    if !slice_path.exists() {
        match fs::create_dir_all(&slice_path).await {
            Ok(()) => {
                info!(path = %slice_path.display(), "created parent cgroup slice");
            }
            Err(e) => {
                warn!(
                    path = %slice_path.display(),
                    error = %e,
                    "failed to create parent cgroup slice, cgroup limits will be unavailable"
                );
            }
        }
    }

    Ok(())
}

/// Scope name for a workspace's QEMU process.
fn scope_name(short_id: &str) -> String {
    format!("agentiso-{}.scope", short_id)
}

/// Full path to a workspace's cgroup scope directory.
fn scope_path(short_id: &str) -> PathBuf {
    PathBuf::from(CGROUP_ROOT)
        .join(PARENT_SLICE)
        .join(scope_name(short_id))
}

/// Resource limits to apply to a workspace's cgroup.
#[derive(Debug, Clone)]
pub struct CgroupLimits {
    /// Memory limit in bytes (QEMU -m value + overhead).
    pub memory_max_bytes: u64,
    /// CPU quota as a fraction of one CPU (e.g. 2.0 = 2 full CPUs).
    /// Translated to cpu.max as `(vcpus * period) period`.
    pub cpu_vcpus: u32,
}

impl CgroupLimits {
    /// Create limits from workspace resource configuration.
    ///
    /// Adds 64 MB overhead to the QEMU memory value to account for QEMU's
    /// own memory usage beyond the guest RAM.
    pub fn from_workspace(memory_mb: u32, vcpus: u32) -> Self {
        let overhead_mb: u64 = 64;
        let memory_max_bytes = (memory_mb as u64 + overhead_mb) * 1024 * 1024;
        Self {
            memory_max_bytes,
            cpu_vcpus: vcpus,
        }
    }
}

/// Create a cgroup scope for a workspace and apply resource limits.
///
/// This creates the cgroup directory, writes memory.max and cpu.max, then
/// places the given PID into the scope.
///
/// Best-effort: returns Ok(()) on any failure after logging a warning.
pub async fn setup_cgroup(short_id: &str, pid: u32, limits: &CgroupLimits) -> Result<()> {
    if !is_cgroup_v2_available().await {
        debug!("cgroup v2 not available, skipping cgroup setup for {}", short_id);
        return Ok(());
    }

    let path = scope_path(short_id);

    // Create the scope directory
    if let Err(e) = fs::create_dir_all(&path).await {
        warn!(
            short_id,
            error = %e,
            "failed to create cgroup scope, proceeding without cgroup limits"
        );
        return Ok(());
    }

    // Set memory.max
    if let Err(e) = write_cgroup_file(&path, "memory.max", &limits.memory_max_bytes.to_string()).await {
        warn!(
            short_id,
            error = %e,
            memory_max_bytes = limits.memory_max_bytes,
            "failed to set cgroup memory.max"
        );
    }

    // Set cpu.max: "quota period" where quota = vcpus * period
    // period is 100000 (100ms), so 2 vcpus = "200000 100000"
    let cpu_period: u64 = 100_000;
    let cpu_quota = limits.cpu_vcpus as u64 * cpu_period;
    let cpu_max = format!("{} {}", cpu_quota, cpu_period);
    if let Err(e) = write_cgroup_file(&path, "cpu.max", &cpu_max).await {
        warn!(
            short_id,
            error = %e,
            cpu_max = %cpu_max,
            "failed to set cgroup cpu.max"
        );
    }

    // Place the QEMU process into the cgroup
    if let Err(e) = write_cgroup_file(&path, "cgroup.procs", &pid.to_string()).await {
        warn!(
            short_id,
            pid,
            error = %e,
            "failed to add PID to cgroup scope"
        );
    } else {
        info!(
            short_id,
            pid,
            memory_max_mb = limits.memory_max_bytes / (1024 * 1024),
            cpu_vcpus = limits.cpu_vcpus,
            "QEMU process placed in cgroup"
        );
    }

    Ok(())
}

/// Remove the cgroup scope for a workspace.
///
/// The cgroup directory can only be removed when it has no processes.
/// Best-effort: logs a warning on failure.
pub async fn cleanup_cgroup(short_id: &str) {
    let path = scope_path(short_id);

    if !path.exists() {
        return;
    }

    // The kernel automatically removes the cgroup directory once all
    // processes have exited, but we try to clean it up explicitly in
    // case it's still lingering.
    match fs::remove_dir(&path).await {
        Ok(()) => {
            debug!(short_id, "cgroup scope removed");
        }
        Err(e) => {
            // EBUSY is expected if processes haven't fully exited yet.
            // The kernel will clean it up asynchronously.
            debug!(
                short_id,
                error = %e,
                "could not remove cgroup scope (may be cleaned up by kernel)"
            );
        }
    }
}

/// Write a value to a cgroup control file.
async fn write_cgroup_file(cgroup_path: &Path, filename: &str, value: &str) -> Result<()> {
    let file_path = cgroup_path.join(filename);
    fs::write(&file_path, value)
        .await
        .with_context(|| format!("writing {} to {}", value, file_path.display()))
}

/// Build the cpu.max string for a given number of vcpus.
///
/// Extracted as a free function for unit testing.
#[cfg(test)]
pub(crate) fn build_cpu_max(vcpus: u32) -> String {
    let period: u64 = 100_000;
    let quota = vcpus as u64 * period;
    format!("{} {}", quota, period)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_name() {
        assert_eq!(scope_name("abcd1234"), "agentiso-abcd1234.scope");
    }

    #[test]
    fn test_scope_path() {
        let path = scope_path("abcd1234");
        assert_eq!(
            path,
            PathBuf::from("/sys/fs/cgroup/agentiso.slice/agentiso-abcd1234.scope")
        );
    }

    #[test]
    fn test_cgroup_limits_from_workspace() {
        let limits = CgroupLimits::from_workspace(512, 2);
        // 512 + 64 = 576 MB
        assert_eq!(limits.memory_max_bytes, 576 * 1024 * 1024);
        assert_eq!(limits.cpu_vcpus, 2);
    }

    #[test]
    fn test_cgroup_limits_from_workspace_min() {
        let limits = CgroupLimits::from_workspace(64, 1);
        // 64 + 64 = 128 MB
        assert_eq!(limits.memory_max_bytes, 128 * 1024 * 1024);
        assert_eq!(limits.cpu_vcpus, 1);
    }

    #[test]
    fn test_cgroup_limits_from_workspace_large() {
        let limits = CgroupLimits::from_workspace(8192, 8);
        // 8192 + 64 = 8256 MB
        assert_eq!(limits.memory_max_bytes, 8256 * 1024 * 1024);
        assert_eq!(limits.cpu_vcpus, 8);
    }

    #[test]
    fn test_build_cpu_max_single_cpu() {
        assert_eq!(build_cpu_max(1), "100000 100000");
    }

    #[test]
    fn test_build_cpu_max_multi_cpu() {
        assert_eq!(build_cpu_max(2), "200000 100000");
        assert_eq!(build_cpu_max(4), "400000 100000");
        assert_eq!(build_cpu_max(8), "800000 100000");
    }

    #[test]
    fn test_parent_slice_constant() {
        assert_eq!(PARENT_SLICE, "agentiso.slice");
    }

    #[tokio::test]
    async fn test_is_cgroup_v2_available_does_not_panic() {
        // This just verifies the function doesn't panic; the result
        // depends on the test environment.
        let _available = is_cgroup_v2_available().await;
    }

    #[tokio::test]
    async fn test_cleanup_cgroup_nonexistent_is_noop() {
        // Cleaning up a cgroup that doesn't exist should not fail
        cleanup_cgroup("nonexistent-test-12345678").await;
    }
}
