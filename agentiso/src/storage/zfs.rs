use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tokio::process::Command;
use tracing::{debug, instrument, warn};

/// Information about a ZFS snapshot.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ZfsSnapshotInfo {
    /// Full ZFS snapshot name (e.g. "tank/agentiso/workspaces/ws-abc12345@checkpoint-1")
    pub full_name: String,
    /// Short snapshot name after the '@' (e.g. "checkpoint-1")
    pub snap_name: String,
    /// Creation timestamp from ZFS (seconds since epoch)
    pub creation: u64,
    /// Referenced size in bytes
    pub referenced: u64,
}

/// Information about a ZFS dataset.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ZfsDatasetInfo {
    pub name: String,
    pub used: u64,
    pub available: u64,
    pub referenced: u64,
    pub mountpoint: Option<String>,
}

/// Zvol usage information (volsize and used bytes).
#[derive(Debug, Clone)]
pub struct ZvolInfo {
    /// The configured maximum size of the zvol in bytes.
    pub volsize: u64,
    /// Bytes actually used by the zvol (including snapshots).
    pub used: u64,
}

/// Low-level ZFS command wrapper. All operations shell out to the `zfs` CLI.
#[derive(Debug, Clone)]
pub struct Zfs {
    /// Pool root for agentiso datasets (e.g. "tank/agentiso")
    pool_root: String,
}

#[allow(dead_code)]
impl Zfs {
    pub fn new(pool_root: String) -> Self {
        Self { pool_root }
    }

    /// Returns the pool root path (e.g. "tank/agentiso").
    pub fn pool_root(&self) -> &str {
        &self.pool_root
    }

    /// Full dataset path for a workspace zvol.
    pub fn workspace_dataset(&self, workspace_id: &str) -> String {
        format!("{}/workspaces/ws-{}", self.pool_root, workspace_id)
    }

    /// Full dataset path for a forked workspace zvol.
    pub fn fork_dataset(&self, workspace_id: &str) -> String {
        format!("{}/forks/ws-{}", self.pool_root, workspace_id)
    }

    /// Full dataset path for a warm pool zvol.
    pub fn pool_dataset(&self, pool_id: &str) -> String {
        format!("{}/pool/warm-{}", self.pool_root, pool_id)
    }

    /// Path to the zvol block device for a warm pool VM.
    pub fn pool_zvol_path(&self, pool_id: &str) -> PathBuf {
        PathBuf::from(format!(
            "/dev/zvol/{}/pool/warm-{}",
            self.pool_root, pool_id
        ))
    }

    /// Full snapshot name for a workspace.
    pub fn snapshot_name(&self, workspace_id: &str, snap_name: &str) -> String {
        format!("{}@{}", self.workspace_dataset(workspace_id), snap_name)
    }

    /// Path to the zvol block device for a workspace.
    pub fn zvol_path(&self, workspace_id: &str) -> PathBuf {
        PathBuf::from(format!(
            "/dev/zvol/{}/workspaces/ws-{}",
            self.pool_root, workspace_id
        ))
    }

    /// Path to the zvol block device for a forked workspace.
    pub fn fork_zvol_path(&self, workspace_id: &str) -> PathBuf {
        PathBuf::from(format!(
            "/dev/zvol/{}/forks/ws-{}",
            self.pool_root, workspace_id
        ))
    }

    /// Clone the base image snapshot to create a new workspace zvol.
    ///
    /// Runs: `zfs clone [-o compression=lz4] {pool}/base/{base_image}@{snapshot} {pool}/workspaces/ws-{id}`
    ///
    /// After cloning, if `disk_gb` is provided, sets the zvol's `volsize` property
    /// to enforce a disk size limit. Zvol clones inherit their parent's volsize
    /// by default; this overrides it.
    #[instrument(skip(self))]
    pub async fn clone_from_base(
        &self,
        base_image: &str,
        base_snapshot: &str,
        workspace_id: &str,
        disk_gb: Option<u32>,
    ) -> Result<String> {
        let source = format!("{}/base/{}@{}", self.pool_root, base_image, base_snapshot);
        let target = self.workspace_dataset(workspace_id);

        debug!(source = %source, target = %target, "cloning base image");

        let args: Vec<&str> = vec!["clone", "-o", "compression=lz4", &source, &target];

        run_zfs(&args)
            .await
            .with_context(|| format!("failed to clone {} -> {}", source, target))?;

        if let Some(gb) = disk_gb {
            self.set_volsize(&target, gb).await
                .with_context(|| format!("failed to set volsize on {}", target))?;
        }

        Ok(target)
    }

    /// Clone the base image snapshot for a warm pool VM.
    ///
    /// Runs: `zfs clone [-o compression=lz4] {pool}/base/{base_image}@{snapshot} {pool}/pool/warm-{id}`
    ///
    /// After cloning, if `disk_gb` is provided, sets the zvol's `volsize` property.
    #[instrument(skip(self))]
    pub async fn clone_for_pool(
        &self,
        base_image: &str,
        base_snapshot: &str,
        pool_id: &str,
        disk_gb: Option<u32>,
    ) -> Result<String> {
        let source = format!("{}/base/{}@{}", self.pool_root, base_image, base_snapshot);
        let target = self.pool_dataset(pool_id);

        debug!(source = %source, target = %target, "cloning base image for warm pool");

        let args: Vec<&str> = vec!["clone", "-o", "compression=lz4", &source, &target];

        run_zfs(&args)
            .await
            .with_context(|| format!("failed to clone {} -> {}", source, target))?;

        if let Some(gb) = disk_gb {
            self.set_volsize(&target, gb).await
                .with_context(|| format!("failed to set volsize on {}", target))?;
        }

        Ok(target)
    }

    /// Create a snapshot of a workspace dataset.
    ///
    /// Runs: `zfs snapshot {dataset}@{snap_name}`
    #[instrument(skip(self))]
    pub async fn create_snapshot(
        &self,
        workspace_id: &str,
        snap_name: &str,
    ) -> Result<String> {
        let full_snap = self.snapshot_name(workspace_id, snap_name);
        debug!(snapshot = %full_snap, "creating snapshot");

        run_zfs(&["snapshot", &full_snap])
            .await
            .with_context(|| format!("failed to create snapshot {}", full_snap))?;

        Ok(full_snap)
    }

    /// Rollback a workspace dataset to a named snapshot.
    ///
    /// Runs: `zfs rollback -r {dataset}@{snap_name}`
    /// The `-r` flag destroys any newer snapshots.
    #[instrument(skip(self))]
    pub async fn rollback(
        &self,
        workspace_id: &str,
        snap_name: &str,
    ) -> Result<()> {
        let full_snap = self.snapshot_name(workspace_id, snap_name);
        debug!(snapshot = %full_snap, "rolling back to snapshot");

        run_zfs(&["rollback", "-r", &full_snap])
            .await
            .with_context(|| format!("failed to rollback to {}", full_snap))?;

        Ok(())
    }

    /// Clone a snapshot to create a forked workspace.
    ///
    /// Runs: `zfs clone [-o compression=lz4] {source_dataset}@{snap_name} {pool}/forks/ws-{new_id}`
    ///
    /// After cloning, if `disk_gb` is provided, sets the zvol's `volsize` property.
    #[instrument(skip(self))]
    pub async fn clone_snapshot(
        &self,
        source_workspace_id: &str,
        snap_name: &str,
        new_workspace_id: &str,
        disk_gb: Option<u32>,
    ) -> Result<String> {
        let source_snap = self.snapshot_name(source_workspace_id, snap_name);
        let target = self.fork_dataset(new_workspace_id);

        debug!(source = %source_snap, target = %target, "cloning snapshot for fork");

        let args: Vec<&str> = vec!["clone", "-o", "compression=lz4", &source_snap, &target];

        run_zfs(&args)
            .await
            .with_context(|| format!("failed to clone {} -> {}", source_snap, target))?;

        if let Some(gb) = disk_gb {
            self.set_volsize(&target, gb).await
                .with_context(|| format!("failed to set volsize on {}", target))?;
        }

        Ok(target)
    }

    /// Destroy a dataset and all its snapshots recursively.
    ///
    /// Runs: `zfs destroy -r {dataset}`
    ///
    /// Safety: refuses to destroy datasets outside the `workspaces/`, `forks/`,
    /// or `pool/` hierarchy to prevent accidental destruction of parent datasets.
    #[instrument(skip(self))]
    pub async fn destroy(&self, dataset: &str) -> Result<()> {
        // Safety: refuse to destroy parent datasets or anything outside workspace/fork/pool hierarchy
        let valid_prefixes = [
            format!("{}/workspaces/", self.pool_root),
            format!("{}/forks/", self.pool_root),
            format!("{}/pool/", self.pool_root),
        ];
        if !valid_prefixes.iter().any(|p| dataset.starts_with(p)) {
            bail!(
                "refusing to destroy dataset '{}': not under workspaces/, forks/, or pool/ of '{}'",
                dataset,
                self.pool_root
            );
        }

        debug!(dataset = %dataset, "destroying dataset recursively");

        run_zfs(&["destroy", "-r", dataset])
            .await
            .with_context(|| format!("failed to destroy {}", dataset))?;

        Ok(())
    }

    /// Destroy a single snapshot.
    ///
    /// Runs: `zfs destroy {dataset}@{snap_name}`
    #[instrument(skip(self))]
    pub async fn destroy_snapshot(
        &self,
        workspace_id: &str,
        snap_name: &str,
    ) -> Result<()> {
        let full_snap = self.snapshot_name(workspace_id, snap_name);
        debug!(snapshot = %full_snap, "destroying snapshot");

        run_zfs(&["destroy", &full_snap])
            .await
            .with_context(|| format!("failed to destroy snapshot {}", full_snap))?;

        Ok(())
    }

    /// List all snapshots for a workspace dataset.
    ///
    /// Runs: `zfs list -t snapshot -H -o name,creation,referenced -s creation -r {dataset}`
    #[instrument(skip(self))]
    pub async fn list_snapshots(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ZfsSnapshotInfo>> {
        let dataset = self.workspace_dataset(workspace_id);
        debug!(dataset = %dataset, "listing snapshots");

        let output = run_zfs_output(&[
            "list",
            "-t", "snapshot",
            "-H",
            "-p", // parseable (exact byte values)
            "-o", "name,creation,referenced",
            "-s", "creation",
            "-r", &dataset,
        ])
        .await
        .with_context(|| format!("failed to list snapshots for {}", dataset))?;

        let mut snapshots = Vec::new();
        for line in output.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 3 {
                warn!(line = %line, "skipping malformed snapshot line");
                continue;
            }

            let full_name = parts[0].to_string();
            let snap_name = match full_name.split_once('@') {
                Some((_, name)) => name.to_string(),
                None => {
                    warn!(name = %full_name, "snapshot name missing '@'");
                    continue;
                }
            };
            let creation = parts[1].trim().parse::<u64>().unwrap_or(0);
            let referenced = parts[2].trim().parse::<u64>().unwrap_or(0);

            snapshots.push(ZfsSnapshotInfo {
                full_name,
                snap_name,
                creation,
                referenced,
            });
        }

        Ok(snapshots)
    }

    /// Check if a dataset exists.
    ///
    /// Runs: `zfs list -H -o name {dataset}`
    pub async fn dataset_exists(&self, dataset: &str) -> Result<bool> {
        let output = Command::new("zfs")
            .args(["list", "-H", "-o", "name", dataset])
            .output()
            .await
            .context("failed to run zfs list")?;

        Ok(output.status.success())
    }

    /// Get info about a dataset.
    ///
    /// Runs: `zfs list -H -p -o name,used,avail,refer,mountpoint {dataset}`
    pub async fn dataset_info(&self, dataset: &str) -> Result<ZfsDatasetInfo> {
        let output = run_zfs_output(&[
            "list", "-H", "-p",
            "-o", "name,used,avail,refer,mountpoint",
            dataset,
        ])
        .await
        .with_context(|| format!("failed to get info for {}", dataset))?;

        let line = output.lines().next().context("empty output from zfs list")?;
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 5 {
            bail!("unexpected zfs list output: {}", line);
        }

        Ok(ZfsDatasetInfo {
            name: parts[0].to_string(),
            used: parts[1].trim().parse().unwrap_or(0),
            available: parts[2].trim().parse().unwrap_or(0),
            referenced: parts[3].trim().parse().unwrap_or(0),
            mountpoint: {
                let mp = parts[4].trim();
                if mp == "-" || mp == "none" {
                    None
                } else {
                    Some(mp.to_string())
                }
            },
        })
    }

    /// Get the available bytes on the pool root dataset.
    ///
    /// Runs: `zfs get -Hp -o value available {pool_root}`
    pub async fn pool_available_bytes(&self) -> Result<u64> {
        let output = run_zfs_output(&[
            "get", "-Hp", "-o", "value", "available", &self.pool_root,
        ])
        .await
        .with_context(|| format!("failed to get available space for {}", self.pool_root))?;

        let bytes: u64 = output
            .trim()
            .parse()
            .with_context(|| format!("failed to parse available bytes: {:?}", output.trim()))?;

        Ok(bytes)
    }

    /// List direct children of a given parent dataset.
    ///
    /// Runs: `zfs list -H -o name -t volume,filesystem -r -d 1 {parent}`
    /// Returns dataset names (excluding the parent itself).
    pub async fn list_children(&self, parent: &str) -> Result<Vec<String>> {
        let output = run_zfs_output(&[
            "list", "-H", "-o", "name", "-t", "volume,filesystem",
            "-r", "-d", "1", parent,
        ])
        .await
        .with_context(|| format!("failed to list children of {}", parent))?;

        Ok(output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && l != parent)
            .collect())
    }

    /// Set the volsize on a zvol dataset to enforce a disk size limit.
    ///
    /// Runs: `zfs set volsize={size_gb}G {dataset}`
    ///
    /// Note: volsize can only be increased, not decreased below the current used
    /// space. ZFS enforces this automatically and will return an error.
    #[instrument(skip(self))]
    pub async fn set_volsize(&self, dataset: &str, size_gb: u32) -> Result<()> {
        let volsize = format!("volsize={}G", size_gb);
        debug!(dataset = %dataset, size_gb, "setting zvol volsize");

        run_zfs(&["set", &volsize, dataset])
            .await
            .with_context(|| format!("failed to set volsize={}G on {}", size_gb, dataset))?;

        Ok(())
    }

    /// Get zvol usage information (volsize and used bytes).
    ///
    /// Runs: `zfs get -Hp -o value volsize,used {dataset}`
    pub async fn get_zvol_info(&self, dataset: &str) -> Result<ZvolInfo> {
        let output = run_zfs_output(&[
            "get", "-Hp", "-o", "value", "volsize,used", dataset,
        ])
        .await
        .with_context(|| format!("failed to get zvol info for {}", dataset))?;

        let lines: Vec<&str> = output.lines().collect();
        if lines.len() < 2 {
            bail!("unexpected zfs get output for {}: expected 2 lines, got {}", dataset, lines.len());
        }

        let volsize: u64 = lines[0].trim().parse()
            .with_context(|| format!("failed to parse volsize: {:?}", lines[0].trim()))?;
        let used: u64 = lines[1].trim().parse()
            .with_context(|| format!("failed to parse used: {:?}", lines[1].trim()))?;

        Ok(ZvolInfo { volsize, used })
    }

    /// Ensure the parent datasets exist (base, workspaces, forks).
    ///
    /// Runs `zfs create -p` for each.
    pub async fn ensure_pool_layout(&self) -> Result<()> {
        for sub in ["base", "workspaces", "forks", "pool"] {
            let ds = format!("{}/{}", self.pool_root, sub);
            if !self.dataset_exists(&ds).await? {
                debug!(dataset = %ds, "creating parent dataset");
                run_zfs(&["create", "-p", "-o", "compression=lz4", &ds])
                    .await
                    .with_context(|| format!("failed to create {}", ds))?;
            }
        }
        Ok(())
    }
}

/// Run a `zfs` command and check for success.
async fn run_zfs(args: &[&str]) -> Result<()> {
    debug!(args = ?args, "running zfs command");

    let output = Command::new("zfs")
        .args(args)
        .output()
        .await
        .context("failed to execute zfs command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("zfs {} failed: {}", args.first().unwrap_or(&""), stderr.trim());
    }

    Ok(())
}

/// Run a `zfs` command and return its stdout.
#[allow(dead_code)]
async fn run_zfs_output(args: &[&str]) -> Result<String> {
    debug!(args = ?args, "running zfs command");

    let output = Command::new("zfs")
        .args(args)
        .output()
        .await
        .context("failed to execute zfs command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("zfs {} failed: {}", args.first().unwrap_or(&""), stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Parse a tab-separated `zfs list` line into a ZfsDatasetInfo.
///
/// Expected format: name\tused\tavail\trefer\tmountpoint
#[allow(dead_code)]
pub(crate) fn parse_dataset_info_line(line: &str) -> Result<ZfsDatasetInfo> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < 5 {
        bail!("unexpected zfs list output: {}", line);
    }

    Ok(ZfsDatasetInfo {
        name: parts[0].to_string(),
        used: parts[1].trim().parse().unwrap_or(0),
        available: parts[2].trim().parse().unwrap_or(0),
        referenced: parts[3].trim().parse().unwrap_or(0),
        mountpoint: {
            let mp = parts[4].trim();
            if mp == "-" || mp == "none" {
                None
            } else {
                Some(mp.to_string())
            }
        },
    })
}

/// Parse tab-separated `zfs list -t snapshot` output into ZfsSnapshotInfo entries.
///
/// Expected format per line: full_name\tcreation\treferenced
#[allow(dead_code)]
pub(crate) fn parse_snapshot_list(output: &str) -> Vec<ZfsSnapshotInfo> {
    let mut snapshots = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }

        let full_name = parts[0].to_string();
        let snap_name = match full_name.split_once('@') {
            Some((_, name)) => name.to_string(),
            None => continue,
        };
        let creation = parts[1].trim().parse::<u64>().unwrap_or(0);
        let referenced = parts[2].trim().parse::<u64>().unwrap_or(0);

        snapshots.push(ZfsSnapshotInfo {
            full_name,
            snap_name,
            creation,
            referenced,
        });
    }
    snapshots
}

/// Build the argument list for a `zfs clone` command, optionally including a `refquota` property.
///
/// Returns owned strings because the refquota property is dynamically formatted.
/// This is extracted as a free function to make it unit-testable without shelling out.
/// Build the argument list for a `zfs clone` command.
///
/// Note: `refquota` is intentionally NOT included because agentiso base images
/// are zvols (block devices), and zvol clones don't support `refquota`
/// (it's a filesystem-only property). Zvols inherit their `volsize` from the
/// parent snapshot. The `disk_gb` parameter is accepted but ignored for
/// forward-compatibility if filesystem datasets are used in the future.
#[cfg(test)]
pub(crate) fn build_clone_args(
    source: &str,
    target: &str,
    _disk_gb: Option<u32>,
) -> Vec<String> {
    vec![
        "clone".to_string(),
        "-o".to_string(),
        "compression=lz4".to_string(),
        source.to_string(),
        target.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zfs_pool_root() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(zfs.pool_root(), "tank/agentiso");
    }

    #[test]
    fn test_workspace_dataset_path() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.workspace_dataset("abc12345"),
            "tank/agentiso/workspaces/ws-abc12345"
        );
    }

    #[test]
    fn test_fork_dataset_path() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.fork_dataset("def67890"),
            "tank/agentiso/forks/ws-def67890"
        );
    }

    #[test]
    fn test_snapshot_name_construction() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.snapshot_name("abc12345", "checkpoint-1"),
            "tank/agentiso/workspaces/ws-abc12345@checkpoint-1"
        );
    }

    #[test]
    fn test_zvol_path() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.zvol_path("abc12345"),
            PathBuf::from("/dev/zvol/tank/agentiso/workspaces/ws-abc12345")
        );
    }

    #[test]
    fn test_fork_zvol_path() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.fork_zvol_path("abc12345"),
            PathBuf::from("/dev/zvol/tank/agentiso/forks/ws-abc12345")
        );
    }

    #[test]
    fn test_parse_dataset_info_line() {
        let line = "tank/agentiso/workspaces/ws-abc\t1048576\t5368709120\t524288\t/mnt/ws-abc";
        let info = parse_dataset_info_line(line).unwrap();
        assert_eq!(info.name, "tank/agentiso/workspaces/ws-abc");
        assert_eq!(info.used, 1048576);
        assert_eq!(info.available, 5368709120);
        assert_eq!(info.referenced, 524288);
        assert_eq!(info.mountpoint, Some("/mnt/ws-abc".to_string()));
    }

    #[test]
    fn test_parse_dataset_info_no_mountpoint() {
        let line = "tank/agentiso/workspaces/ws-abc\t1024\t999\t512\t-";
        let info = parse_dataset_info_line(line).unwrap();
        assert!(info.mountpoint.is_none());

        let line2 = "tank/agentiso/workspaces/ws-abc\t1024\t999\t512\tnone";
        let info2 = parse_dataset_info_line(line2).unwrap();
        assert!(info2.mountpoint.is_none());
    }

    #[test]
    fn test_parse_dataset_info_malformed() {
        let line = "too\tfew\tcolumns";
        assert!(parse_dataset_info_line(line).is_err());
    }

    #[test]
    fn test_parse_snapshot_list() {
        let output = "tank/agentiso/workspaces/ws-abc@snap1\t1700000000\t4096\n\
                       tank/agentiso/workspaces/ws-abc@snap2\t1700001000\t8192\n";
        let snaps = parse_snapshot_list(output);
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].snap_name, "snap1");
        assert_eq!(snaps[0].creation, 1700000000);
        assert_eq!(snaps[0].referenced, 4096);
        assert_eq!(snaps[1].snap_name, "snap2");
        assert_eq!(snaps[1].full_name, "tank/agentiso/workspaces/ws-abc@snap2");
    }

    #[test]
    fn test_parse_snapshot_list_malformed_lines() {
        let output = "too_few_columns\n\
                       no_at_sign\t100\t200\n\
                       tank/ws@good\t300\t400\n";
        let snaps = parse_snapshot_list(output);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].snap_name, "good");
    }

    #[test]
    fn test_parse_snapshot_list_empty() {
        let snaps = parse_snapshot_list("");
        assert!(snaps.is_empty());
    }

    #[test]
    fn test_pool_dataset_path() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.pool_dataset("abc12345"),
            "tank/agentiso/pool/warm-abc12345"
        );
    }

    #[test]
    fn test_pool_zvol_path() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.pool_zvol_path("abc12345"),
            PathBuf::from("/dev/zvol/tank/agentiso/pool/warm-abc12345")
        );
    }

    #[tokio::test]
    async fn test_destroy_rejects_parent_dataset() {
        let zfs = Zfs::new("tank/agentiso".to_string());

        // Attempting to destroy the pool root should fail
        let result = zfs.destroy("tank/agentiso").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("refusing to destroy")
        );

        // Attempting to destroy the base dataset should fail
        let result = zfs.destroy("tank/agentiso/base/alpine-dev").await;
        assert!(result.is_err());

        // Attempting to destroy a completely unrelated dataset should fail
        let result = zfs.destroy("rpool/ROOT").await;
        assert!(result.is_err());

        // Attempting to destroy the workspaces parent (without trailing content) should fail
        let result = zfs.destroy("tank/agentiso/workspaces").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_destroy_accepts_valid_prefixes() {
        let zfs = Zfs::new("tank/agentiso".to_string());

        // These should pass the prefix check (will fail at the zfs command level
        // since we don't have real ZFS, but should NOT fail with "refusing to destroy")
        let result = zfs
            .destroy("tank/agentiso/workspaces/ws-abc12345")
            .await;
        assert!(result.is_err()); // fails because zfs command doesn't exist in test
        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.contains("refusing to destroy"),
            "should not be rejected by safety guard: {}",
            err_msg
        );

        let result = zfs.destroy("tank/agentiso/forks/ws-def67890").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.contains("refusing to destroy"),
            "should not be rejected by safety guard: {}",
            err_msg
        );

        let result = zfs.destroy("tank/agentiso/pool/warm-abc12345").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.contains("refusing to destroy"),
            "should not be rejected by safety guard: {}",
            err_msg
        );
    }

    #[test]
    fn test_different_pool_root() {
        let zfs = Zfs::new("rpool/vms".to_string());
        assert_eq!(
            zfs.workspace_dataset("12345678"),
            "rpool/vms/workspaces/ws-12345678"
        );
        assert_eq!(
            zfs.zvol_path("12345678"),
            PathBuf::from("/dev/zvol/rpool/vms/workspaces/ws-12345678")
        );
        assert_eq!(
            zfs.snapshot_name("12345678", "save1"),
            "rpool/vms/workspaces/ws-12345678@save1"
        );
    }

    #[test]
    fn test_build_clone_args_with_disk_gb_ignored() {
        // disk_gb is accepted but ignored (zvols don't support refquota)
        let args = build_clone_args(
            "tank/agentiso/base/alpine-dev@ready",
            "tank/agentiso/workspaces/ws-abc12345",
            Some(10),
        );
        assert_eq!(
            args,
            vec![
                "clone",
                "-o",
                "compression=lz4",
                "tank/agentiso/base/alpine-dev@ready",
                "tank/agentiso/workspaces/ws-abc12345",
            ]
        );
        // refquota should NOT be present (invalid for zvols)
        assert!(!args.iter().any(|a| a.contains("refquota")));
    }

    #[test]
    fn test_build_clone_args_without_disk_gb() {
        let args = build_clone_args(
            "tank/agentiso/base/alpine-dev@ready",
            "tank/agentiso/workspaces/ws-abc12345",
            None,
        );
        assert_eq!(
            args,
            vec![
                "clone",
                "-o",
                "compression=lz4",
                "tank/agentiso/base/alpine-dev@ready",
                "tank/agentiso/workspaces/ws-abc12345",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("refquota")));
    }

    #[test]
    fn test_build_clone_args_always_has_compression() {
        // Verify compression=lz4 is present regardless of disk_gb
        let args_none = build_clone_args("pool/base@snap", "pool/workspaces/ws-xyz", None);
        assert!(args_none.contains(&"compression=lz4".to_string()));

        let args_some = build_clone_args("pool/base@snap", "pool/workspaces/ws-xyz", Some(100));
        assert!(args_some.contains(&"compression=lz4".to_string()));
        // No refquota even when disk_gb is provided
        assert!(!args_some.iter().any(|a| a.contains("refquota")));
    }

    #[test]
    fn test_build_clone_args_fork_path() {
        let args = build_clone_args(
            "pool/base@snap",
            "pool/forks/ws-fork1",
            Some(1),
        );
        assert_eq!(
            args,
            vec![
                "clone",
                "-o",
                "compression=lz4",
                "pool/base@snap",
                "pool/forks/ws-fork1",
            ]
        );
    }

    #[test]
    fn test_zvol_info_struct() {
        let info = ZvolInfo {
            volsize: 10 * 1024 * 1024 * 1024,
            used: 256 * 1024 * 1024,
        };
        assert_eq!(info.volsize, 10_737_418_240);
        assert_eq!(info.used, 268_435_456);
    }

    #[test]
    fn test_zvol_info_clone() {
        let info = ZvolInfo {
            volsize: 5_368_709_120,
            used: 1_073_741_824,
        };
        let cloned = info.clone();
        assert_eq!(cloned.volsize, info.volsize);
        assert_eq!(cloned.used, info.used);
    }
}
