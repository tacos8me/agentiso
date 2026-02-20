use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::workspace::{PersistedState, TeamLifecycleState, WorkspaceState};

/// All data needed to render the dashboard.
pub struct DashboardData {
    pub workspaces: Vec<WorkspaceEntry>,
    pub system: SystemInfo,
    pub logs: Vec<String>,
    pub error: Option<String>,
}

/// A workspace as displayed in the table.
pub struct WorkspaceEntry {
    pub id: String,
    pub name: String,
    pub state: String,
    pub state_alive: bool,
    pub ip: String,
    pub base_image: String,
    pub memory: String,
    pub disk: String,
    pub vcpus: u32,
    pub age: String,
    pub snapshot_names: Vec<String>,
    pub vsock_cid: u32,
    pub tap_device: String,
    pub allow_internet: bool,
    pub allow_inter_vm: bool,
    pub port_forwards: Vec<(u16, u16)>,
    pub qemu_pid: Option<u32>,
}

/// System-level info for the header bar.
pub struct SystemInfo {
    pub total: usize,
    pub running: usize,
    pub zfs_free: String,
    pub bridge_name: String,
    pub bridge_up: bool,
    pub server_running: bool,
    pub total_memory_mb: u64,
    pub total_disk_gb: u64,
    pub total_vcpus: u32,
}

impl DashboardData {
    /// Load all dashboard data from the state file and system. Never panics.
    pub fn load(config: &Config) -> Self {
        match Self::load_inner(config) {
            Ok(data) => data,
            Err(e) => DashboardData {
                workspaces: Vec::new(),
                system: SystemInfo {
                    total: 0,
                    running: 0,
                    zfs_free: "N/A".to_string(),
                    bridge_name: config.network.bridge_name.clone(),
                    bridge_up: false,
                    server_running: check_server_lock(config),
                    total_memory_mb: 0,
                    total_disk_gb: 0,
                    total_vcpus: 0,
                },
                logs: Vec::new(),
                error: Some(format!("{}", e)),
            },
        }
    }

    fn load_inner(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        let state_file = &config.server.state_file;

        if !state_file.exists() {
            return Ok(DashboardData {
                workspaces: Vec::new(),
                system: build_system_info(config, 0, 0, 0, 0, 0),
                logs: Vec::new(),
                error: Some(format!(
                    "No state file at {}. Is agentiso running?",
                    state_file.display()
                )),
            });
        }

        let data = std::fs::read_to_string(state_file)?;
        let state: PersistedState = serde_json::from_str(&data)?;

        // Sort workspaces by created_at (oldest first)
        let mut ws_list: Vec<_> = state.workspaces.values().collect();
        ws_list.sort_by_key(|w| w.created_at);

        let mut entries = Vec::with_capacity(ws_list.len());
        let mut running = 0usize;
        let mut total_memory_mb: u64 = 0;
        let mut total_disk_gb: u64 = 0;
        let mut total_vcpus: u32 = 0;

        for ws in &ws_list {
            // State display string
            let state_str = match ws.state {
                WorkspaceState::Running => "Running".to_string(),
                WorkspaceState::Stopped => "Stopped".to_string(),
                WorkspaceState::Suspended => "Suspended".to_string(),
            };

            // Count running
            if ws.state == WorkspaceState::Running {
                running += 1;
            }

            // PID liveness check for Running workspaces
            let state_alive = if ws.state == WorkspaceState::Running {
                match ws.qemu_pid {
                    Some(pid) => {
                        let proc_path = format!("/proc/{}", pid);
                        std::path::Path::new(&proc_path).exists()
                    }
                    None => false,
                }
            } else {
                false
            };

            // IP address
            let ip = ws.network.ip.to_string();

            // Format memory
            let memory = if ws.resources.memory_mb >= 1024 {
                let gb = ws.resources.memory_mb as f64 / 1024.0;
                if (gb - gb.round()).abs() < 0.01 {
                    format!("{} GB", gb.round() as u32)
                } else {
                    format!("{:.1} GB", gb)
                }
            } else {
                format!("{} MB", ws.resources.memory_mb)
            };

            // Format disk
            let disk = format!("{} GB", ws.resources.disk_gb);

            // Age
            let age = format_age(ws.created_at);

            // Snapshots (sorted by creation time via list())
            let snap_list = ws.snapshots.list();
            let snapshot_names: Vec<String> = snap_list.iter().map(|s| s.name.clone()).collect();

            let short_id = ws.id.to_string()[..8].to_string();

            // Accumulate resource totals
            total_memory_mb += ws.resources.memory_mb as u64;
            total_disk_gb += ws.resources.disk_gb as u64;
            total_vcpus += ws.resources.vcpus;

            // Port forwards as (host_port, guest_port) tuples
            let port_forwards: Vec<(u16, u16)> = ws
                .network
                .port_forwards
                .iter()
                .map(|pf| (pf.host_port, pf.guest_port))
                .collect();

            entries.push(WorkspaceEntry {
                id: short_id,
                name: ws.name.clone(),
                state: state_str,
                state_alive,
                ip,
                base_image: ws.base_image.clone(),
                memory,
                disk,
                vcpus: ws.resources.vcpus,
                age,
                snapshot_names,
                vsock_cid: ws.vsock_cid,
                tap_device: ws.tap_device.clone(),
                allow_internet: ws.network.allow_internet,
                allow_inter_vm: ws.network.allow_inter_vm,
                port_forwards,
                qemu_pid: ws.qemu_pid,
            });
        }

        let total = entries.len();

        Ok(DashboardData {
            workspaces: entries,
            system: build_system_info(config, total, running, total_memory_mb, total_disk_gb, total_vcpus),
            logs: Vec::new(),
            error: None,
        })
    }

    /// Load console log lines for a specific workspace.
    /// Reads from {run_dir}/{short_id}/console.log
    pub fn load_workspace_logs(config: &Config, short_id: &str, max_lines: usize) -> Vec<String> {
        let log_path = config.vm.run_dir.join(short_id).join("console.log");
        match std::fs::read_to_string(&log_path) {
            Ok(content) => {
                content.lines()
                    .rev()
                    .take(max_lines)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|s| s.to_string())
                    .collect()
            }
            Err(_) => vec![],
        }
    }

    /// Load console.log lines for a specific workspace (test helper).
    #[cfg(test)]
    pub fn load_logs(config: &Config, workspace_id: &str) -> Vec<String> {
        let ws_run_dir = config.vm.run_dir.join(workspace_id);
        let console_path = ws_run_dir.join("console.log");

        match std::fs::read_to_string(&console_path) {
            Ok(content) => content.lines().map(String::from).collect(),
            Err(_) => vec!["(no console.log found)".to_string()],
        }
    }
}

/// Build the SystemInfo struct by querying ZFS and the bridge interface.
fn build_system_info(
    config: &Config,
    total: usize,
    running: usize,
    total_memory_mb: u64,
    total_disk_gb: u64,
    total_vcpus: u32,
) -> SystemInfo {
    let zfs_free = query_zfs_free(config);
    let bridge_up = check_bridge_up(&config.network.bridge_name);
    let server_running = check_server_lock(config);

    SystemInfo {
        total,
        running,
        zfs_free,
        bridge_name: config.network.bridge_name.clone(),
        bridge_up,
        server_running,
        total_memory_mb,
        total_disk_gb,
        total_vcpus,
    }
}

/// Query ZFS available space for the pool root dataset.
fn query_zfs_free(config: &Config) -> String {
    let pool_root = format!(
        "{}/{}",
        config.storage.zfs_pool, config.storage.dataset_prefix
    );

    let output = std::process::Command::new("zfs")
        .args(["get", "-Hp", "-o", "value", "available", &pool_root])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            match s.trim().parse::<u64>() {
                Ok(bytes) => format_bytes(bytes),
                Err(_) => "N/A".to_string(),
            }
        }
        _ => "N/A".to_string(),
    }
}

/// Check if the MCP server is running by probing the instance lock file.
///
/// The server acquires an exclusive flock on `agentiso.lock` at startup.
/// If we can acquire the lock, no server is running; if we get EWOULDBLOCK,
/// a server holds it.
pub(crate) fn check_server_lock(config: &Config) -> bool {
    let lock_path = config
        .server
        .state_file
        .parent()
        .map(|p| p.join("agentiso.lock"))
        .unwrap_or_else(|| std::path::PathBuf::from("/var/lib/agentiso/agentiso.lock"));

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(_) => return false,
    };

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        // We got the lock → no server running. Release immediately.
        unsafe { libc::flock(fd, libc::LOCK_UN) };
        false
    } else {
        // EWOULDBLOCK → lock is held → server is running
        true
    }
}

/// Check if the bridge interface is up by reading its operstate from sysfs.
fn check_bridge_up(bridge_name: &str) -> bool {
    let path = format!("/sys/class/net/{}/operstate", bridge_name);
    match std::fs::read_to_string(&path) {
        Ok(content) => content.trim() == "up",
        Err(_) => false,
    }
}

/// Format a duration since `created_at` as a compact human-readable age string.
fn format_age(created_at: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(created_at);

    let total_secs = duration.num_seconds().max(0) as u64;

    if total_secs < 60 {
        return "just now".to_string();
    }

    let total_minutes = total_secs / 60;
    if total_minutes < 60 {
        return format!("{}m", total_minutes);
    }

    let total_hours = total_minutes / 60;
    let remaining_minutes = total_minutes % 60;
    if total_hours < 24 {
        if remaining_minutes == 0 {
            return format!("{}h", total_hours);
        }
        return format!("{}h {}m", total_hours, remaining_minutes);
    }

    let total_days = total_hours / 24;
    let remaining_hours = total_hours % 24;
    if total_days < 7 {
        if remaining_hours == 0 {
            return format!("{}d", total_days);
        }
        return format!("{}d {}h", total_days, remaining_hours);
    }

    format!("{}d", total_days)
}

/// Format a byte count as a human-readable size string.
fn format_bytes(bytes: u64) -> String {
    const TB: u64 = 1_099_511_627_776;
    const GB: u64 = 1_073_741_824;
    const MB: u64 = 1_048_576;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Team dashboard data (loaded when in team mode).
#[derive(Debug, Default)]
pub struct TeamDashboardData {
    pub teams: Vec<TeamSummary>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct TeamSummary {
    pub name: String,
    pub state: String,
    pub member_count: usize,
    pub max_vms: u32,
    pub created_at: String,
}

impl TeamDashboardData {
    /// Load team data from the state file. Never panics.
    pub fn load(config: &Config) -> Self {
        match Self::load_inner(config) {
            Ok(data) => data,
            Err(e) => TeamDashboardData {
                teams: Vec::new(),
                error: Some(format!("{}", e)),
            },
        }
    }

    fn load_inner(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        let state_file = &config.server.state_file;

        if !state_file.exists() {
            return Ok(TeamDashboardData {
                teams: Vec::new(),
                error: Some("No state file found".to_string()),
            });
        }

        let data = std::fs::read_to_string(state_file)?;
        let state: PersistedState = serde_json::from_str(&data)?;

        let mut teams: Vec<TeamSummary> = state
            .teams
            .values()
            .map(|t| {
                let state_str = match t.state {
                    TeamLifecycleState::Creating => "Creating",
                    TeamLifecycleState::Ready => "Ready",
                    TeamLifecycleState::Working => "Working",
                    TeamLifecycleState::Completing => "Completing",
                    TeamLifecycleState::Destroyed => "Destroyed",
                };
                TeamSummary {
                    name: t.name.clone(),
                    state: state_str.to_string(),
                    member_count: t.member_workspace_ids.len(),
                    max_vms: t.max_vms,
                    created_at: format_age(t.created_at),
                }
            })
            .collect();

        // Sort by name for stable ordering
        teams.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(TeamDashboardData {
            teams,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_age_just_now() {
        let now = Utc::now();
        assert_eq!(format_age(now), "just now");
    }

    #[test]
    fn test_format_age_minutes() {
        let t = Utc::now() - chrono::Duration::minutes(5);
        assert_eq!(format_age(t), "5m");

        let t = Utc::now() - chrono::Duration::minutes(45);
        assert_eq!(format_age(t), "45m");
    }

    #[test]
    fn test_format_age_hours() {
        let t = Utc::now() - chrono::Duration::hours(2);
        assert_eq!(format_age(t), "2h");

        let t = Utc::now() - chrono::Duration::minutes(90);
        assert_eq!(format_age(t), "1h 30m");
    }

    #[test]
    fn test_format_age_days() {
        let t = Utc::now() - chrono::Duration::hours(50);
        assert_eq!(format_age(t), "2d 2h");

        let t = Utc::now() - chrono::Duration::days(3);
        assert_eq!(format_age(t), "3d");
    }

    #[test]
    fn test_format_age_weeks() {
        let t = Utc::now() - chrono::Duration::days(10);
        assert_eq!(format_age(t), "10d");
    }

    #[test]
    fn test_format_bytes_tb() {
        assert_eq!(format_bytes(2_800_000_000_000), "2.5 TB");
        assert_eq!(format_bytes(1_099_511_627_776), "1.0 TB");
    }

    #[test]
    fn test_format_bytes_gb() {
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
        assert_eq!(format_bytes(500_000_000_000), "465.7 GB");
    }

    #[test]
    fn test_format_bytes_mb() {
        assert_eq!(format_bytes(104_857_600), "100 MB");
        assert_eq!(format_bytes(1_048_576), "1 MB");
    }

    #[test]
    fn test_format_bytes_small() {
        assert_eq!(format_bytes(1024), "1024 B");
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn test_format_age_future_clamps_to_just_now() {
        // If created_at is in the future (clock skew), should not panic
        let future = Utc::now() + chrono::Duration::hours(1);
        assert_eq!(format_age(future), "just now");
    }

    #[test]
    fn test_load_logs_missing_file() {
        let config = Config::default();
        let logs = DashboardData::load_logs(&config, "nonexistent");
        assert_eq!(logs, vec!["(no console.log found)"]);
    }
}
