pub mod zfs;

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{info, instrument};

pub use zfs::{Zfs, ZfsDatasetInfo, ZfsSnapshotInfo};

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

    /// Create storage for a new workspace by cloning the base image.
    ///
    /// Returns the dataset name and zvol block device path.
    #[instrument(skip(self))]
    pub async fn create_workspace(
        &self,
        base_image: &str,
        base_snapshot: &str,
        workspace_id: &str,
    ) -> Result<WorkspaceStorage> {
        let dataset = self
            .zfs
            .clone_from_base(base_image, base_snapshot, workspace_id)
            .await
            .context("failed to clone base image for workspace")?;

        let zvol_path = self.zfs.zvol_path(workspace_id);

        info!(
            dataset = %dataset,
            zvol = %zvol_path.display(),
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
    /// Returns the new dataset and zvol path.
    #[instrument(skip(self))]
    pub async fn fork_workspace(
        &self,
        source_workspace_id: &str,
        snap_name: &str,
        new_workspace_id: &str,
    ) -> Result<ForkedStorage> {
        let dataset = self
            .zfs
            .clone_snapshot(source_workspace_id, snap_name, new_workspace_id)
            .await
            .context("failed to fork workspace from snapshot")?;

        let zvol_path = self.zfs.fork_zvol_path(new_workspace_id);

        info!(
            source = %source_workspace_id,
            snapshot = %snap_name,
            new_dataset = %dataset,
            "workspace forked"
        );

        Ok(ForkedStorage { dataset, zvol_path })
    }

    /// Create storage for a warm pool VM by cloning the base image.
    #[instrument(skip(self))]
    pub async fn create_pool_vm(
        &self,
        base_image: &str,
        base_snapshot: &str,
        pool_id: &str,
    ) -> Result<WorkspaceStorage> {
        let dataset = self
            .zfs
            .clone_for_pool(base_image, base_snapshot, pool_id)
            .await
            .context("failed to clone base image for pool VM")?;

        let zvol_path = self.zfs.pool_zvol_path(pool_id);

        info!(
            dataset = %dataset,
            zvol = %zvol_path.display(),
            "pool VM storage created"
        );

        Ok(WorkspaceStorage { dataset, zvol_path })
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
