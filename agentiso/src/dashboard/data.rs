use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::workspace::{PersistedState, WorkspaceState};

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
    pub full_id: String,
    pub name: String,
    pub state: String,
    pub state_alive: bool,
    pub ip: String,
    pub base_image: String,
    pub memory: String,
    pub disk: String,
    pub vcpus: u32,
    pub age: String,
    pub snapshot_count: usize,
    pub snapshot_names: Vec<String>,
    pub vsock_cid: u32,
    pub tap_device: String,
}

/// System-level info for the header bar.
pub struct SystemInfo {
    pub total: usize,
    pub running: usize,
    pub stopped: usize,
    pub zfs_free: String,
    pub bridge_name: String,
    pub bridge_up: bool,
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
                    stopped: 0,
                    zfs_free: "N/A".to_string(),
                    bridge_name: config.network.bridge_name.clone(),
                    bridge_up: false,
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
                system: build_system_info(config, 0, 0, 0),
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
        let mut stopped = 0usize;

        for ws in &ws_list {
            // State display string
            let state_str = match ws.state {
                WorkspaceState::Running => "Running".to_string(),
                WorkspaceState::Stopped => "Stopped".to_string(),
                WorkspaceState::Suspended => "Suspended".to_string(),
            };

            // Count running/stopped
            match ws.state {
                WorkspaceState::Running => running += 1,
                WorkspaceState::Stopped => stopped += 1,
                WorkspaceState::Suspended => {}
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
            let snapshot_count = snap_list.len();
            let snapshot_names: Vec<String> = snap_list.iter().map(|s| s.name.clone()).collect();

            let short_id = ws.id.to_string()[..8].to_string();

            entries.push(WorkspaceEntry {
                id: short_id,
                full_id: ws.id.to_string(),
                name: ws.name.clone(),
                state: state_str,
                state_alive,
                ip,
                base_image: ws.base_image.clone(),
                memory,
                disk,
                vcpus: ws.resources.vcpus,
                age,
                snapshot_count,
                snapshot_names,
                vsock_cid: ws.vsock_cid,
                tap_device: ws.tap_device.clone(),
            });
        }

        let total = entries.len();

        Ok(DashboardData {
            workspaces: entries,
            system: build_system_info(config, total, running, stopped),
            logs: Vec::new(),
            error: None,
        })
    }

    /// Load console.log lines for a specific workspace.
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
fn build_system_info(config: &Config, total: usize, running: usize, stopped: usize) -> SystemInfo {
    let zfs_free = query_zfs_free(config);
    let bridge_up = check_bridge_up(&config.network.bridge_name);

    SystemInfo {
        total,
        running,
        stopped,
        zfs_free,
        bridge_name: config.network.bridge_name.clone(),
        bridge_up,
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
