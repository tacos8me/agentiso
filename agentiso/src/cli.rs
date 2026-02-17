//! CLI-only subcommand implementations: `check` and `status`.
//!
//! These commands do not start the daemon. They run without root and are
//! useful for debugging the host environment before/after running `serve`.

use std::path::PathBuf;

use anyhow::Result;

use crate::config::Config;

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

struct Check {
    label: &'static str,
    ok: bool,
    detail: String,
    fix: Option<String>,
}

impl Check {
    fn pass(label: &'static str, detail: impl Into<String>) -> Self {
        Self { label, ok: true, detail: detail.into(), fix: None }
    }

    fn fail(label: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self { label, ok: false, detail: detail.into(), fix: Some(fix.into()) }
    }
}

/// Run `agentiso check`. Returns `Ok(())` if all checks pass, `Err` otherwise.
pub fn run_check(config: &Config) -> Result<()> {
    println!("Checking prerequisites...\n");

    let mut checks: Vec<Check> = Vec::new();

    // 1. KVM
    checks.push(check_kvm());

    // 2. QEMU
    checks.push(check_qemu());

    // 3. ZFS
    checks.push(check_zfs());

    // 4. nftables
    checks.push(check_nftables());

    // 5. Kernel
    checks.push(check_kernel(config));

    // 6. Initrd
    checks.push(check_initrd(config));

    // 7. ZFS pool root
    checks.push(check_zfs_pool(config));

    // 8. Base image
    checks.push(check_base_image(config));

    // 9. Base snapshot
    checks.push(check_base_snapshot(config));

    // 10. Bridge
    checks.push(check_bridge(config));

    // 11. State directory
    checks.push(check_state_dir(config));

    // 12. Transfer directory
    checks.push(check_transfer_dir(config));

    let all_pass = checks.iter().all(|c| c.ok);

    for c in &checks {
        let icon = if c.ok { "\u{2713}" } else { "\u{2717}" };
        println!("  {} {} ({})", icon, c.label, c.detail);
        if !c.ok {
            if let Some(fix) = &c.fix {
                println!("    Fix: {}", fix);
            }
        }
    }

    println!();
    if all_pass {
        let config_hint = match std::env::current_exe() {
            Ok(exe) => format!("{} serve --config config.toml", exe.display()),
            Err(_) => "agentiso serve --config config.toml".to_string(),
        };
        println!("All checks passed. Run: {}", config_hint);
        Ok(())
    } else {
        let failed = checks.iter().filter(|c| !c.ok).count();
        anyhow::bail!("{} check(s) failed", failed)
    }
}

fn check_kvm() -> Check {
    let path = "/dev/kvm";
    match std::fs::metadata(path) {
        Ok(meta) => {
            // Verify it's a character device (not some imposter file)
            use std::os::unix::fs::FileTypeExt;
            if meta.file_type().is_char_device() {
                Check::pass("KVM device", path)
            } else {
                Check::fail(
                    "KVM device",
                    format!("{} exists but is not a character device", path),
                    "Load the KVM kernel module: modprobe kvm_intel (or kvm_amd)",
                )
            }
        }
        Err(e) => Check::fail(
            "KVM device",
            format!("{}: {}", path, e),
            "Load the KVM kernel module: modprobe kvm_intel (or kvm_amd)",
        ),
    }
}

fn check_qemu() -> Check {
    match std::process::Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version
                .lines()
                .next()
                .unwrap_or("unknown")
                .trim()
                .to_string();
            Check::pass("QEMU", version)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Check::fail(
                "QEMU",
                format!("qemu-system-x86_64 exited with error: {}", stderr.trim()),
                "Install QEMU: apt install qemu-system-x86",
            )
        }
        Err(e) => Check::fail(
            "QEMU",
            format!("qemu-system-x86_64 not found: {}", e),
            "Install QEMU: apt install qemu-system-x86",
        ),
    }
}

fn check_zfs() -> Check {
    match std::process::Command::new("zfs").arg("version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version
                .lines()
                .next()
                .unwrap_or("unknown")
                .trim()
                .to_string();
            Check::pass("ZFS", version)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Check::fail(
                "ZFS",
                format!("zfs exited with error: {}", stderr.trim()),
                "Install ZFS: apt install zfsutils-linux",
            )
        }
        Err(e) => Check::fail(
            "ZFS",
            format!("zfs not found: {}", e),
            "Install ZFS: apt install zfsutils-linux",
        ),
    }
}

fn check_nftables() -> Check {
    match std::process::Command::new("nft").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version
                .lines()
                .next()
                .unwrap_or("unknown")
                .trim()
                .to_string();
            Check::pass("nftables", version)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Check::fail(
                "nftables",
                format!("nft exited with error: {}", stderr.trim()),
                "Install nftables: apt install nftables",
            )
        }
        Err(e) => Check::fail(
            "nftables",
            format!("nft not found: {}", e),
            "Install nftables: apt install nftables",
        ),
    }
}

fn check_kernel(config: &Config) -> Check {
    let path = &config.vm.kernel_path;
    match std::fs::metadata(path) {
        Ok(meta) => {
            let size_mb = meta.len() / (1024 * 1024);
            Check::pass("Kernel", format!("{}, {}MB", path.display(), size_mb))
        }
        Err(e) => Check::fail(
            "Kernel",
            format!("{}: {}", path.display(), e),
            format!(
                "Copy host kernel: cp /boot/vmlinuz-$(uname -r) {}",
                path.display()
            ),
        ),
    }
}

fn check_initrd(config: &Config) -> Check {
    match &config.vm.initrd_path {
        None => Check::pass("Initrd", "not configured (skipped)"),
        Some(path) => match std::fs::metadata(path) {
            Ok(meta) => {
                let size_mb = meta.len() / (1024 * 1024);
                Check::pass("Initrd", format!("{}, {}MB", path.display(), size_mb))
            }
            Err(e) => Check::fail(
                "Initrd",
                format!("{}: {}", path.display(), e),
                format!(
                    "Copy host initrd: cp /boot/initrd.img-$(uname -r) {}",
                    path.display()
                ),
            ),
        },
    }
}

fn check_zfs_pool(config: &Config) -> Check {
    let pool_root = format!(
        "{}/{}",
        config.storage.zfs_pool, config.storage.dataset_prefix
    );
    match std::process::Command::new("zfs")
        .arg("list")
        .arg(&pool_root)
        .output()
    {
        Ok(out) if out.status.success() => Check::pass("ZFS pool", pool_root),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Check::fail(
                "ZFS pool",
                format!("{} not found: {}", pool_root, stderr.trim()),
                format!("Create ZFS pool or dataset: zfs create {}", pool_root),
            )
        }
        Err(e) => Check::fail(
            "ZFS pool",
            format!("zfs list {} failed: {}", pool_root, e),
            format!("Create ZFS pool or dataset: zfs create {}", pool_root),
        ),
    }
}

fn check_base_image(config: &Config) -> Check {
    let base_dataset = config.storage.base_dataset();
    match std::process::Command::new("zfs")
        .arg("list")
        .arg(&base_dataset)
        .output()
    {
        Ok(out) if out.status.success() => Check::pass("Base image", base_dataset),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Check::fail(
                "Base image",
                format!("{} not found: {}", base_dataset, stderr.trim()),
                "Run: sudo ./scripts/setup-e2e.sh to build and import the base image",
            )
        }
        Err(e) => Check::fail(
            "Base image",
            format!("zfs list {} failed: {}", base_dataset, e),
            "Run: sudo ./scripts/setup-e2e.sh to build and import the base image",
        ),
    }
}

fn check_base_snapshot(config: &Config) -> Check {
    let snap_path = config.storage.base_snapshot_path();
    match std::process::Command::new("zfs")
        .arg("list")
        .arg("-t")
        .arg("snapshot")
        .arg(&snap_path)
        .output()
    {
        Ok(out) if out.status.success() => Check::pass("Base snapshot", snap_path),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Check::fail(
                "Base snapshot",
                format!("{} not found: {}", snap_path, stderr.trim()),
                format!(
                    "Create snapshot: zfs snapshot {}",
                    snap_path
                ),
            )
        }
        Err(e) => Check::fail(
            "Base snapshot",
            format!("zfs list {} failed: {}", snap_path, e),
            format!("Create snapshot: zfs snapshot {}", snap_path),
        ),
    }
}

fn check_bridge(config: &Config) -> Check {
    let bridge = &config.network.bridge_name;
    let sysfs_path = format!("/sys/class/net/{}", bridge);
    if std::path::Path::new(&sysfs_path).exists() {
        Check::pass("Bridge", bridge.as_str())
    } else {
        Check::fail(
            "Bridge",
            format!("{} — not found in /sys/class/net/", bridge),
            format!(
                "ip link add {} type bridge && ip link set {} up && ip addr add {}/{} dev {}",
                bridge,
                bridge,
                config.network.gateway_ip,
                config.network.subnet_prefix,
                bridge,
            ),
        )
    }
}

fn check_state_dir(config: &Config) -> Check {
    let state_file = &config.server.state_file;
    let parent = match state_file.parent() {
        Some(p) => p,
        None => {
            return Check::fail(
                "State directory",
                format!("state_file path {} has no parent", state_file.display()),
                "Set server.state_file to an absolute path",
            );
        }
    };

    if !parent.exists() {
        return Check::fail(
            "State directory",
            format!("{} does not exist", parent.display()),
            format!("mkdir -p {}", parent.display()),
        );
    }

    // Check writability by attempting to open a temp file
    let probe = parent.join(".agentiso-check");
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Check::pass("State directory", parent.display().to_string())
        }
        Err(e) => Check::fail(
            "State directory",
            format!("{} is not writable: {}", parent.display(), e),
            format!("chmod u+w {} or run agentiso as a user with write access", parent.display()),
        ),
    }
}

fn check_transfer_dir(config: &Config) -> Check {
    let dir = &config.server.transfer_dir;
    if dir.exists() {
        Check::pass("Transfer directory", dir.display().to_string())
    } else {
        // Try to create it
        match std::fs::create_dir_all(dir) {
            Ok(_) => Check::pass(
                "Transfer directory",
                format!("{} (created)", dir.display()),
            ),
            Err(e) => Check::fail(
                "Transfer directory",
                format!("{} does not exist and could not be created: {}", dir.display(), e),
                format!("mkdir -p {}", dir.display()),
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Run `agentiso status`. Always returns `Ok(())` — informational only.
pub fn run_status(config: &Config) -> Result<()> {
    use crate::workspace::PersistedState;

    let state_file = &config.server.state_file;

    if !state_file.exists() {
        println!("No state file at {}", state_file.display());
        println!(
            "Is agentiso running? Try: agentiso serve --config config.toml"
        );
        return Ok(());
    }

    // Last-modified time
    let modified_ago = match std::fs::metadata(state_file) {
        Ok(meta) => match meta.modified() {
            Ok(mtime) => {
                let elapsed = mtime
                    .elapsed()
                    .unwrap_or(std::time::Duration::from_secs(0));
                format_duration(elapsed)
            }
            Err(_) => "unknown".to_string(),
        },
        Err(_) => "unknown".to_string(),
    };

    let data = match std::fs::read_to_string(state_file) {
        Ok(d) => d,
        Err(e) => {
            println!("State file: {} (last modified: {})", state_file.display(), modified_ago);
            println!("Error reading state file: {}", e);
            return Ok(());
        }
    };

    let state: PersistedState = match serde_json::from_str(&data) {
        Ok(s) => s,
        Err(e) => {
            println!("State file: {} (last modified: {})", state_file.display(), modified_ago);
            println!("Error parsing state file: {}", e);
            return Ok(());
        }
    };

    println!(
        "State file: {} (last modified: {})\n",
        state_file.display(),
        modified_ago
    );

    let count = state.workspaces.len();
    println!("Workspaces: {}", count);

    if count > 0 {
        // Sort by creation time for deterministic output
        let mut workspaces: Vec<_> = state.workspaces.values().collect();
        workspaces.sort_by_key(|w| w.created_at);

        for ws in workspaces {
            let snap_count = ws.snapshots.len();
            println!(
                "  {:<12}  {:<9}  {:<15}  {:<16}  {} snapshot{}",
                ws.name,
                ws.state.to_string(),
                ws.network.ip.to_string(),
                ws.base_image,
                snap_count,
                if snap_count == 1 { "" } else { "s" },
            );
        }
    }

    let recycled = state.free_vsock_cids.len();
    println!(
        "\nvsock CIDs: next={}, recycled={}",
        state.next_vsock_cid, recycled
    );

    Ok(())
}

/// Format a duration as a human-friendly relative string (e.g. "3 seconds ago", "2 minutes ago").
fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        if secs == 1 {
            "1 second ago".to_string()
        } else {
            format!("{} seconds ago", secs)
        }
    } else if secs < 3600 {
        let mins = secs / 60;
        if mins == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{} minutes ago", mins)
        }
    } else if secs < 86400 {
        let hours = secs / 3600;
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else {
        let days = secs / 86400;
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", days)
        }
    }
}

/// Load a config from an optional path, falling back to defaults.
pub fn load_config(config_path: Option<PathBuf>) -> Result<Config> {
    match config_path {
        Some(path) => Config::load(&path),
        None => Ok(Config::default()),
    }
}
