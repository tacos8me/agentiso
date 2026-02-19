use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Serialize;
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

/// Space usage information for a ZFS snapshot.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SnapshotSizeInfo {
    /// Bytes uniquely consumed by this snapshot (would be freed on destroy).
    pub used: u64,
    /// Total bytes referenced (shared with the active dataset).
    pub referenced: u64,
}

/// A single entry from `zfs diff` output.
///
/// Note: `zfs diff` only works on mounted filesystem datasets. Since agentiso
/// uses zvols (block devices), this is currently not usable on workspace zvols.
/// See [`Zfs::snapshot_diff`] for details.
#[derive(Debug, Clone, Serialize)]
pub struct DiffEntry {
    /// The type of change: '+' added, '-' removed, 'M' modified, 'R' renamed.
    pub change_type: char,
    /// The filesystem path that was changed.
    pub path: String,
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
    ///
    /// If the destroy fails because the zvol is busy (e.g. still open by QEMU),
    /// the error message will include a hint to stop the VM first.
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

        let output = Command::new("zfs")
            .args(["destroy", "-r", dataset])
            .output()
            .await
            .context("failed to execute zfs destroy command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr_trimmed = stderr.trim();

            // Check for "busy" errors which typically mean the zvol block device
            // is still open (e.g. by a QEMU process).
            if stderr_trimmed.contains("busy") || stderr_trimmed.contains("dataset is busy") {
                bail!(
                    "cannot destroy dataset '{}': the zvol is busy (likely still open by QEMU). \
                     Stop the VM before destroying the workspace. ZFS error: {}",
                    dataset,
                    stderr_trimmed,
                );
            }

            bail!(
                "failed to destroy dataset '{}': {}",
                dataset,
                stderr_trimmed,
            );
        }

        Ok(())
    }

    /// Destroy a single snapshot.
    ///
    /// Checks for dependent clones before destroying. If any clones (forks)
    /// depend on this snapshot, the destroy is refused with a clear error
    /// listing the dependent clones. This prevents orphaning forked workspaces.
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

        // Guard: check for dependent clones before destroying
        let clones = self.list_snapshot_clones(&full_snap).await
            .with_context(|| format!("failed to check dependent clones for {}", full_snap))?;

        if !clones.is_empty() {
            bail!(
                "cannot destroy snapshot '{}': it has {} dependent clone(s): [{}]. \
                 Destroy or promote the dependent clone(s) first.",
                full_snap,
                clones.len(),
                clones.join(", "),
            );
        }

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

    /// Check if a snapshot has dependent clones.
    ///
    /// Runs: `zfs get -Hp -o value clones {snapshot_path}`
    ///
    /// Returns `true` if any clones depend on this snapshot. ZFS returns "-"
    /// when there are no clones, or a comma-separated list of clone dataset names.
    pub async fn has_dependent_clones(&self, snapshot_path: &str) -> Result<bool> {
        let output = run_zfs_output(&[
            "get", "-Hp", "-o", "value", "clones", snapshot_path,
        ])
        .await
        .with_context(|| format!("failed to check clones for {}", snapshot_path))?;

        Ok(parse_clones_property(output.trim()))
    }

    /// List the clone datasets that depend on a given snapshot.
    ///
    /// Runs: `zfs get -Hp -o value clones {snapshot_path}`
    ///
    /// Returns a (possibly empty) list of clone dataset names. ZFS returns "-"
    /// when there are no clones, or a comma-separated list of clone dataset names.
    pub async fn list_snapshot_clones(&self, snapshot_path: &str) -> Result<Vec<String>> {
        let output = run_zfs_output(&[
            "get", "-Hp", "-o", "value", "clones", snapshot_path,
        ])
        .await
        .with_context(|| format!("failed to list clones for {}", snapshot_path))?;

        Ok(parse_clones_list(output.trim()))
    }

    /// Get the space used by a snapshot.
    ///
    /// Runs: `zfs get -Hp -o value used,referenced {dataset}@{snap_name}`
    ///
    /// Returns `(used_bytes, referenced_bytes)`:
    /// - `used`: bytes uniquely consumed by this snapshot (freed on destroy)
    /// - `referenced`: total bytes referenced (shared with the active dataset)
    pub async fn snapshot_size(
        &self,
        workspace_id: &str,
        snap_name: &str,
    ) -> Result<(u64, u64)> {
        let full_snap = self.snapshot_name(workspace_id, snap_name);

        let output = run_zfs_output(&[
            "get", "-Hp", "-o", "value", "used,referenced", &full_snap,
        ])
        .await
        .with_context(|| format!("failed to get size for snapshot {}", full_snap))?;

        parse_snapshot_size_output(&output)
            .with_context(|| format!("failed to parse snapshot size for {}", full_snap))
    }

    /// Diff two snapshots of the same workspace dataset.
    ///
    /// **Limitation**: `zfs diff` only works on mounted filesystem datasets.
    /// Since agentiso workspaces use zvols (block devices, not mounted filesystems),
    /// this method will always return an error explaining the limitation. Zvols
    /// are accessed as raw block devices (`/dev/zvol/...`) and do not have a
    /// ZFS-visible directory tree, so `zfs diff` cannot inspect their contents.
    ///
    /// This method is provided for forward-compatibility in case agentiso ever
    /// switches to filesystem datasets, and to clearly communicate the limitation
    /// to callers rather than silently failing.
    pub async fn snapshot_diff(
        &self,
        workspace_id: &str,
        snap_a: &str,
        snap_b: &str,
    ) -> Result<Vec<DiffEntry>> {
        let dataset = self.workspace_dataset(workspace_id);
        let snap_a_full = format!("{}@{}", dataset, snap_a);
        let snap_b_full = format!("{}@{}", dataset, snap_b);

        // zvols are block devices, not mounted filesystems. `zfs diff` requires
        // a mounted filesystem dataset to inspect directory changes. Since agentiso
        // workspaces are zvols, this operation is not supported.
        bail!(
            "snapshot diff is not supported for zvol datasets. \
             '{}' is a zvol (block device), and `zfs diff` requires a mounted \
             filesystem dataset. To compare zvol contents, mount the zvol's \
             filesystem and use standard file-level diff tools instead. \
             (attempted diff between {} and {})",
            dataset,
            snap_a_full,
            snap_b_full,
        );
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

/// Parse the `clones` property value from `zfs get -Hp -o value clones`.
///
/// Returns `true` if there are any dependent clones.
/// ZFS returns "-" when there are no clones, or a comma-separated list.
pub(crate) fn parse_clones_property(value: &str) -> bool {
    !value.is_empty() && value != "-"
}

/// Parse the `clones` property value into a list of clone dataset names.
///
/// ZFS returns "-" when there are no clones, or a comma-separated list like
/// "tank/forks/ws-abc,tank/forks/ws-def".
pub(crate) fn parse_clones_list(value: &str) -> Vec<String> {
    if value.is_empty() || value == "-" {
        Vec::new()
    } else {
        value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    }
}

/// Parse `zfs get -Hp -o value used,referenced` output for a snapshot.
///
/// The output contains two lines: the first is used bytes, the second is referenced bytes.
pub(crate) fn parse_snapshot_size_output(output: &str) -> Result<(u64, u64)> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 2 {
        bail!(
            "unexpected snapshot size output: expected 2 lines, got {}",
            lines.len()
        );
    }

    let used: u64 = lines[0]
        .trim()
        .parse()
        .with_context(|| format!("failed to parse snapshot used bytes: {:?}", lines[0].trim()))?;
    let referenced: u64 = lines[1]
        .trim()
        .parse()
        .with_context(|| format!("failed to parse snapshot referenced bytes: {:?}", lines[1].trim()))?;

    Ok((used, referenced))
}

/// Parse `zfs diff` output into DiffEntry items.
///
/// Each line has the format: `{change_type}\t{path}`
/// where change_type is one of: + (added), - (removed), M (modified), R (renamed).
///
/// Note: This parser is provided for forward-compatibility. Currently `zfs diff`
/// does not work on zvols (which agentiso uses), so this is only testable in
/// isolation with synthetic input.
#[allow(dead_code)]
pub(crate) fn parse_diff_output(output: &str) -> Vec<DiffEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // zfs diff output: {change_type}\t{path}
        // Change type is a single character at the start of the line
        if let Some((change_str, path)) = line.split_once('\t') {
            let change_str = change_str.trim();
            if let Some(change_type) = change_str.chars().next() {
                if matches!(change_type, '+' | '-' | 'M' | 'R') {
                    entries.push(DiffEntry {
                        change_type,
                        path: path.to_string(),
                    });
                }
            }
        }
    }
    entries
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

    // --- Tests for has_dependent_clones / parse_clones_property ---

    #[test]
    fn test_parse_clones_property_no_clones() {
        // ZFS returns "-" when there are no dependent clones
        assert!(!parse_clones_property("-"));
    }

    #[test]
    fn test_parse_clones_property_empty() {
        assert!(!parse_clones_property(""));
    }

    #[test]
    fn test_parse_clones_property_single_clone() {
        assert!(parse_clones_property("tank/agentiso/forks/ws-abc12345"));
    }

    #[test]
    fn test_parse_clones_property_multiple_clones() {
        assert!(parse_clones_property(
            "tank/agentiso/forks/ws-abc,tank/agentiso/forks/ws-def"
        ));
    }

    // --- Tests for list_snapshot_clones / parse_clones_list ---

    #[test]
    fn test_parse_clones_list_no_clones() {
        let clones = parse_clones_list("-");
        assert!(clones.is_empty());
    }

    #[test]
    fn test_parse_clones_list_empty() {
        let clones = parse_clones_list("");
        assert!(clones.is_empty());
    }

    #[test]
    fn test_parse_clones_list_single_clone() {
        let clones = parse_clones_list("tank/agentiso/forks/ws-abc12345");
        assert_eq!(clones.len(), 1);
        assert_eq!(clones[0], "tank/agentiso/forks/ws-abc12345");
    }

    #[test]
    fn test_parse_clones_list_multiple_clones() {
        let clones = parse_clones_list(
            "tank/agentiso/forks/ws-abc,tank/agentiso/forks/ws-def,tank/agentiso/forks/ws-ghi"
        );
        assert_eq!(clones.len(), 3);
        assert_eq!(clones[0], "tank/agentiso/forks/ws-abc");
        assert_eq!(clones[1], "tank/agentiso/forks/ws-def");
        assert_eq!(clones[2], "tank/agentiso/forks/ws-ghi");
    }

    #[test]
    fn test_parse_clones_list_with_whitespace() {
        let clones = parse_clones_list(" tank/forks/ws-abc , tank/forks/ws-def ");
        assert_eq!(clones.len(), 2);
        assert_eq!(clones[0], "tank/forks/ws-abc");
        assert_eq!(clones[1], "tank/forks/ws-def");
    }

    // --- Tests for snapshot_size / parse_snapshot_size_output ---

    #[test]
    fn test_parse_snapshot_size_output_valid() {
        let output = "4096\n1073741824\n";
        let (used, referenced) = parse_snapshot_size_output(output).unwrap();
        assert_eq!(used, 4096);
        assert_eq!(referenced, 1_073_741_824);
    }

    #[test]
    fn test_parse_snapshot_size_output_large_values() {
        let output = "10737418240\n107374182400\n";
        let (used, referenced) = parse_snapshot_size_output(output).unwrap();
        assert_eq!(used, 10_737_418_240); // 10 GB
        assert_eq!(referenced, 107_374_182_400); // 100 GB
    }

    #[test]
    fn test_parse_snapshot_size_output_zero() {
        let output = "0\n0\n";
        let (used, referenced) = parse_snapshot_size_output(output).unwrap();
        assert_eq!(used, 0);
        assert_eq!(referenced, 0);
    }

    #[test]
    fn test_parse_snapshot_size_output_too_few_lines() {
        let output = "4096\n";
        let result = parse_snapshot_size_output(output);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expected 2 lines"));
    }

    #[test]
    fn test_parse_snapshot_size_output_empty() {
        let result = parse_snapshot_size_output("");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_snapshot_size_output_invalid_number() {
        let output = "not_a_number\n4096\n";
        let result = parse_snapshot_size_output(output);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to parse"));
    }

    // --- Tests for snapshot_diff / parse_diff_output ---

    #[test]
    fn test_parse_diff_output_all_types() {
        let output = "+\t/workspace/new_file.txt\n\
                       -\t/workspace/deleted_file.txt\n\
                       M\t/workspace/modified_file.txt\n\
                       R\t/workspace/renamed_file.txt\n";
        let entries = parse_diff_output(output);
        assert_eq!(entries.len(), 4);

        assert_eq!(entries[0].change_type, '+');
        assert_eq!(entries[0].path, "/workspace/new_file.txt");

        assert_eq!(entries[1].change_type, '-');
        assert_eq!(entries[1].path, "/workspace/deleted_file.txt");

        assert_eq!(entries[2].change_type, 'M');
        assert_eq!(entries[2].path, "/workspace/modified_file.txt");

        assert_eq!(entries[3].change_type, 'R');
        assert_eq!(entries[3].path, "/workspace/renamed_file.txt");
    }

    #[test]
    fn test_parse_diff_output_empty() {
        let entries = parse_diff_output("");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_diff_output_blank_lines() {
        let output = "\n+\t/workspace/file.txt\n\n";
        let entries = parse_diff_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].change_type, '+');
    }

    #[test]
    fn test_parse_diff_output_unknown_type_skipped() {
        // Lines with unknown change types should be skipped
        let output = "X\t/workspace/unknown.txt\n\
                       M\t/workspace/modified.txt\n";
        let entries = parse_diff_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].change_type, 'M');
    }

    #[test]
    fn test_parse_diff_output_no_tab_skipped() {
        // Lines without tab separator should be skipped
        let output = "M /workspace/no_tab.txt\n\
                       +\t/workspace/good.txt\n";
        let entries = parse_diff_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].change_type, '+');
    }

    #[test]
    fn test_parse_diff_output_path_with_spaces() {
        let output = "+\t/workspace/path with spaces/file.txt\n";
        let entries = parse_diff_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/workspace/path with spaces/file.txt");
    }

    // --- Tests for snapshot_diff returning error on zvols ---

    #[tokio::test]
    async fn test_snapshot_diff_returns_error_for_zvols() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        let result = zfs.snapshot_diff("abc12345", "snap1", "snap2").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not supported for zvol"),
            "error should mention zvol limitation: {}",
            err_msg
        );
    }

    // --- Tests for destroy_snapshot guard ---

    #[test]
    fn test_destroy_snapshot_guard_logic() {
        // Test the clone-checking logic via the parsing functions.
        // We cannot run destroy_snapshot in tests without real ZFS, but we can
        // verify the guard condition: if clones are present, the snapshot should
        // be refused for destruction.

        // No clones -> should allow destruction
        let clones = parse_clones_list("-");
        assert!(clones.is_empty(), "should allow destroy when no clones");

        // Has clones -> should refuse destruction
        let clones = parse_clones_list("tank/forks/ws-abc");
        assert!(!clones.is_empty(), "should refuse destroy when clones exist");
    }

    // --- Tests for DiffEntry serialization ---

    #[test]
    fn test_diff_entry_serialize() {
        let entry = DiffEntry {
            change_type: 'M',
            path: "/workspace/file.txt".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"change_type\":\"M\""));
        assert!(json.contains("\"path\":\"/workspace/file.txt\""));
    }

    // --- Tests for SnapshotSizeInfo ---

    #[test]
    fn test_snapshot_size_info_struct() {
        let info = SnapshotSizeInfo {
            used: 4096,
            referenced: 1_073_741_824,
        };
        assert_eq!(info.used, 4096);
        assert_eq!(info.referenced, 1_073_741_824);
    }
}
