pub mod zfs;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tracing::{info, instrument, warn};

pub use zfs::{Zfs, ZfsDatasetInfo, ZfsSnapshotInfo, ZvolInfo};

/// High-level storage manager for workspace lifecycle operations.
///
/// Wraps the low-level [`Zfs`] commands and provides workspace-oriented operations
/// like creating workspaces from base images, snapshotting, forking, and cleanup.
pub struct StorageManager {
    zfs: Zfs,
}

/// Result of creating a new workspace's storage.
#[derive(Debug, Clone)]
pub struct WorkspaceStorage {
    /// ZFS dataset name (e.g. "tank/agentiso/workspaces/ws-abc12345")
    pub dataset: String,
    /// Path to the zvol block device (e.g. "/dev/zvol/tank/agentiso/workspaces/ws-abc12345")
    pub zvol_path: PathBuf,
}

/// Result of forking a workspace from a snapshot.
#[derive(Debug, Clone)]
pub struct ForkedStorage {
    /// ZFS dataset name for the fork
    pub dataset: String,
    /// Path to the zvol block device for the fork
    pub zvol_path: PathBuf,
}

#[allow(dead_code)]
impl StorageManager {
    /// Create a new StorageManager.
    ///
    /// `pool_root` is the ZFS dataset root, e.g. "tank/agentiso".
    pub fn new(pool_root: String) -> Self {
        Self {
            zfs: Zfs::new(pool_root),
        }
    }

    /// Access the underlying ZFS operations.
    pub fn zfs(&self) -> &Zfs {
        &self.zfs
    }

    /// Ensure the ZFS pool layout exists (base/, workspaces/, forks/ datasets).
    ///
    /// Should be called once at startup.
    pub async fn init(&self) -> Result<()> {
        self.zfs
            .ensure_pool_layout()
            .await
            .context("failed to initialize ZFS pool layout")?;
        info!("storage pool layout initialized");
        Ok(())
    }

    /// Minimum available pool space (in bytes) below which workspace creation fails.
    const MIN_POOL_BYTES: u64 = 1_073_741_824; // 1 GB
    /// Available pool space threshold (in bytes) that triggers a warning.
    const WARN_POOL_BYTES: u64 = 10_737_418_240; // 10 GB

    /// Create storage for a new workspace by cloning the base image.
    ///
    /// When `disk_gb` is provided, sets the zvol's `volsize` property to enforce
    /// a disk size limit. Before cloning, checks available pool space and fails
    /// if there is insufficient room for the workspace's volsize plus a 1 GB buffer.
    ///
    /// Returns the dataset name and zvol block device path.
    #[instrument(skip(self))]
    pub async fn create_workspace(
        &self,
        base_image: &str,
        base_snapshot: &str,
        workspace_id: &str,
        disk_gb: Option<u32>,
    ) -> Result<WorkspaceStorage> {
        // Check available pool space before cloning
        self.check_pool_space(disk_gb).await?;

        let dataset = self
            .zfs
            .clone_from_base(base_image, base_snapshot, workspace_id, disk_gb)
            .await
            .context("failed to clone base image for workspace")?;

        let zvol_path = self.zfs.zvol_path(workspace_id);

        info!(
            dataset = %dataset,
            zvol = %zvol_path.display(),
            disk_gb = ?disk_gb,
            "workspace storage created"
        );

        Ok(WorkspaceStorage { dataset, zvol_path })
    }

    /// Destroy all storage for a workspace (dataset + all snapshots).
    #[instrument(skip(self))]
    pub async fn destroy_workspace(&self, workspace_id: &str) -> Result<()> {
        let dataset = self.zfs.workspace_dataset(workspace_id);

        // Check if it exists first to provide a nicer error
        if !self.zfs.dataset_exists(&dataset).await? {
            // Also try the forks path
            let fork_ds = self.zfs.fork_dataset(workspace_id);
            if self.zfs.dataset_exists(&fork_ds).await? {
                self.zfs.destroy(&fork_ds).await?;
                info!(dataset = %fork_ds, "forked workspace storage destroyed");
                return Ok(());
            }
            anyhow::bail!("workspace dataset not found: {}", dataset);
        }

        self.zfs.destroy(&dataset).await?;
        info!(dataset = %dataset, "workspace storage destroyed");
        Ok(())
    }

    /// Create a named snapshot of a workspace.
    ///
    /// Returns the full ZFS snapshot name.
    #[instrument(skip(self))]
    pub async fn create_snapshot(
        &self,
        workspace_id: &str,
        snap_name: &str,
    ) -> Result<String> {
        let full_snap = self
            .zfs
            .create_snapshot(workspace_id, snap_name)
            .await
            .context("failed to create workspace snapshot")?;

        info!(snapshot = %full_snap, "snapshot created");
        Ok(full_snap)
    }

    /// Restore a workspace to a named snapshot.
    ///
    /// The VM must be stopped before calling this.
    #[instrument(skip(self))]
    pub async fn restore_snapshot(
        &self,
        workspace_id: &str,
        snap_name: &str,
    ) -> Result<()> {
        self.zfs
            .rollback(workspace_id, snap_name)
            .await
            .context("failed to rollback workspace to snapshot")?;

        info!(workspace_id = %workspace_id, snapshot = %snap_name, "snapshot restored");
        Ok(())
    }

    /// Delete a single snapshot from a workspace.
    #[instrument(skip(self))]
    pub async fn delete_snapshot(
        &self,
        workspace_id: &str,
        snap_name: &str,
    ) -> Result<()> {
        self.zfs
            .destroy_snapshot(workspace_id, snap_name)
            .await
            .context("failed to delete snapshot")?;

        info!(workspace_id = %workspace_id, snapshot = %snap_name, "snapshot deleted");
        Ok(())
    }

    /// List all snapshots for a workspace.
    #[instrument(skip(self))]
    pub async fn list_snapshots(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ZfsSnapshotInfo>> {
        self.zfs
            .list_snapshots(workspace_id)
            .await
            .context("failed to list workspace snapshots")
    }

    /// Fork a workspace from a snapshot, creating a new independent workspace.
    ///
    /// When `disk_gb` is provided, sets the forked zvol's `volsize` property.
    ///
    /// Returns the new dataset and zvol path.
    #[instrument(skip(self))]
    pub async fn fork_workspace(
        &self,
        source_workspace_id: &str,
        snap_name: &str,
        new_workspace_id: &str,
        disk_gb: Option<u32>,
    ) -> Result<ForkedStorage> {
        // Check available pool space before cloning
        self.check_pool_space(disk_gb).await?;

        let dataset = self
            .zfs
            .clone_snapshot(source_workspace_id, snap_name, new_workspace_id, disk_gb)
            .await
            .context("failed to fork workspace from snapshot")?;

        let zvol_path = self.zfs.fork_zvol_path(new_workspace_id);

        info!(
            source = %source_workspace_id,
            snapshot = %snap_name,
            new_dataset = %dataset,
            disk_gb = ?disk_gb,
            "workspace forked"
        );

        Ok(ForkedStorage { dataset, zvol_path })
    }

    /// Create storage for a warm pool VM by cloning the base image.
    ///
    /// When `disk_gb` is provided, sets the zvol's `volsize` property.
    #[instrument(skip(self))]
    pub async fn create_pool_vm(
        &self,
        base_image: &str,
        base_snapshot: &str,
        pool_id: &str,
        disk_gb: Option<u32>,
    ) -> Result<WorkspaceStorage> {
        // Check available pool space before cloning
        self.check_pool_space(disk_gb).await?;

        let dataset = self
            .zfs
            .clone_for_pool(base_image, base_snapshot, pool_id, disk_gb)
            .await
            .context("failed to clone base image for pool VM")?;

        let zvol_path = self.zfs.pool_zvol_path(pool_id);

        info!(
            dataset = %dataset,
            zvol = %zvol_path.display(),
            disk_gb = ?disk_gb,
            "pool VM storage created"
        );

        Ok(WorkspaceStorage { dataset, zvol_path })
    }

    /// Check available pool space, warning if low and failing if critically low.
    ///
    /// When `required_gb` is provided, fails if available space < required_gb + 1 GB buffer.
    /// Otherwise falls back to the static 1 GB minimum. Always warns if < 10 GB.
    async fn check_pool_space(&self, required_gb: Option<u32>) -> Result<()> {
        match self.zfs.pool_available_bytes().await {
            Ok(available) => {
                let min_required = if let Some(gb) = required_gb {
                    // Required workspace size + 1 GB buffer
                    (gb as u64) * 1_073_741_824 + Self::MIN_POOL_BYTES
                } else {
                    Self::MIN_POOL_BYTES
                };

                if available < min_required {
                    bail!(
                        "insufficient pool space: {:.1} GB available, but {:.1} GB required \
                         (workspace size + 1 GB buffer). \
                         Free space by destroying unused workspaces or snapshots.",
                        available as f64 / 1_073_741_824.0,
                        min_required as f64 / 1_073_741_824.0,
                    );
                }
                if available < Self::WARN_POOL_BYTES {
                    warn!(
                        available_bytes = available,
                        "pool space is low: {:.1} GB available (< 10 GB). \
                         Consider cleaning up unused workspaces or snapshots.",
                        available as f64 / 1_073_741_824.0,
                    );
                }
            }
            Err(e) => {
                // Don't fail workspace creation if we can't check space —
                // the ZFS clone itself will fail if there's truly no space.
                warn!(error = %e, "failed to check pool available space, proceeding anyway");
            }
        }
        Ok(())
    }

    /// List all workspace datasets (under workspaces/ and forks/).
    ///
    /// Returns full dataset names for each child.
    pub async fn list_all_workspace_datasets(&self) -> Result<Vec<String>> {
        let mut datasets = Vec::new();

        let workspaces_parent = format!("{}/workspaces", self.zfs.pool_root());
        match self.zfs.list_children(&workspaces_parent).await {
            Ok(children) => datasets.extend(children),
            Err(e) => {
                warn!(error = %e, "failed to list workspace datasets");
            }
        }

        let forks_parent = format!("{}/forks", self.zfs.pool_root());
        match self.zfs.list_children(&forks_parent).await {
            Ok(children) => datasets.extend(children),
            Err(e) => {
                warn!(error = %e, "failed to list fork datasets");
            }
        }

        Ok(datasets)
    }

    /// Destroy storage for a pool VM or workspace by dataset path.
    #[instrument(skip(self))]
    pub async fn destroy_dataset(&self, dataset: &str) -> Result<()> {
        self.zfs.destroy(dataset).await?;
        info!(dataset = %dataset, "dataset destroyed");
        Ok(())
    }

    /// Get info about a workspace's dataset.
    #[instrument(skip(self))]
    pub async fn workspace_info(
        &self,
        workspace_id: &str,
    ) -> Result<ZfsDatasetInfo> {
        let dataset = self.zfs.workspace_dataset(workspace_id);
        self.zfs
            .dataset_info(&dataset)
            .await
            .context("failed to get workspace dataset info")
    }

    /// Get zvol info (volsize + used) for a workspace by dataset path.
    ///
    /// Accepts the full dataset path (e.g. from `Workspace.zfs_dataset`)
    /// so it works for both regular workspaces and forks.
    #[instrument(skip(self))]
    pub async fn zvol_info(&self, dataset: &str) -> Result<ZvolInfo> {
        self.zfs
            .get_zvol_info(dataset)
            .await
            .context("failed to get zvol info")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_manager_zfs_accessor() {
        let sm = StorageManager::new("tank/agentiso".to_string());
        assert_eq!(sm.zfs().pool_root(), "tank/agentiso");
    }

    #[test]
    fn test_storage_manager_workspace_paths() {
        let sm = StorageManager::new("tank/agentiso".to_string());
        let zfs = sm.zfs();

        assert_eq!(
            zfs.workspace_dataset("abc12345"),
            "tank/agentiso/workspaces/ws-abc12345"
        );
        assert_eq!(
            zfs.zvol_path("abc12345"),
            PathBuf::from("/dev/zvol/tank/agentiso/workspaces/ws-abc12345")
        );
    }

    #[test]
    fn test_storage_manager_fork_paths() {
        let sm = StorageManager::new("tank/agentiso".to_string());
        let zfs = sm.zfs();

        assert_eq!(
            zfs.fork_dataset("def67890"),
            "tank/agentiso/forks/ws-def67890"
        );
        assert_eq!(
            zfs.fork_zvol_path("def67890"),
            PathBuf::from("/dev/zvol/tank/agentiso/forks/ws-def67890")
        );
    }
}
