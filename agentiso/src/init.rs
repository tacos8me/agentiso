//! `agentiso init` — One-shot environment setup.
//!
//! Replaces the multi-step manual setup process with a single idempotent command
//! that checks prerequisites, creates directories, sets up ZFS, networking,
//! installs kernel/initrd, builds the guest agent, writes default config, and
//! verifies everything works.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// Options for `agentiso init`, parsed from CLI flags.
pub struct InitOptions {
    /// Skip interactive confirmations.
    pub yes: bool,
    /// Path to use for ZFS pool backing (sparse file or block device).
    pub pool_path: Option<PathBuf>,
    /// Custom kernel path (instead of host /boot/vmlinuz-$(uname -r)).
    pub kernel: Option<PathBuf>,
    /// Custom initrd path (instead of host /boot/initrd.img-$(uname -r)).
    pub initrd: Option<PathBuf>,
}

/// Run the full `agentiso init` sequence.
pub fn run_init(opts: &InitOptions) -> Result<()> {
    println!("agentiso init - One-shot environment setup\n");

    // Step 1: Check prerequisites
    println!("=== Step 1/8: Checking prerequisites ===\n");
    let prereqs_ok = check_prerequisites()?;
    if !prereqs_ok {
        anyhow::bail!(
            "Critical prerequisites are missing. Install them and re-run `agentiso init`."
        );
    }
    println!();

    // Step 2: Create directories
    println!("=== Step 2/8: Creating directories ===\n");
    create_directories()?;
    println!();

    // Step 3: Set up ZFS pool
    println!("=== Step 3/8: Setting up ZFS pool ===\n");
    setup_zfs_pool(opts)?;
    println!();

    // Step 4: Set up networking
    println!("=== Step 4/8: Setting up networking ===\n");
    setup_networking(opts)?;
    println!();

    // Step 5: Install kernel + initrd
    println!("=== Step 5/8: Installing kernel + initrd ===\n");
    install_kernel_initrd(opts)?;
    println!();

    // Step 6: Build and install guest agent
    println!("=== Step 6/8: Building and installing guest agent ===\n");
    build_guest_agent(opts)?;
    println!();

    // Step 7: Create default config
    println!("=== Step 7/8: Creating default config ===\n");
    create_default_config()?;
    println!();

    // Step 8: Verification
    println!("=== Step 8/8: Verifying setup ===\n");
    run_verification()?;

    println!();
    println!("=== agentiso init complete! ===");
    println!();
    println!("Next steps:");
    println!("  1. Build the base image:  sudo ./images/build-alpine.sh");
    println!("  2. Import into ZFS:       See images/README.md for instructions");
    println!("  3. Start the server:      agentiso serve --config /etc/agentiso/config.toml");
    println!();
    println!("Or run the full e2e setup (builds image + imports):");
    println!("  sudo ./scripts/setup-e2e.sh");

    Ok(())
}

// ---------------------------------------------------------------------------
// Step 1: Prerequisites
// ---------------------------------------------------------------------------

fn check_prerequisites() -> Result<bool> {
    let mut all_ok = true;

    // Root check
    if unsafe { libc::geteuid() } != 0 {
        println!("  \u{2717} Not running as root");
        println!("    agentiso init requires root privileges for ZFS, networking, and file installation.");
        println!("    Re-run with: sudo agentiso init");
        return Ok(false);
    }
    println!("  \u{2713} Running as root");

    // QEMU
    match Command::new("qemu-system-x86_64").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version.lines().next().unwrap_or("unknown").trim();
            println!("  \u{2713} qemu-system-x86_64 ({})", version);
        }
        _ => {
            println!("  \u{2717} qemu-system-x86_64 not found");
            println!("    Install: sudo apt install qemu-system-x86");
            all_ok = false;
        }
    }

    // ZFS
    match Command::new("zfs").arg("version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version.lines().next().unwrap_or("unknown").trim();
            println!("  \u{2713} zfs ({})", version);
        }
        _ => {
            println!("  \u{2717} zfs not found");
            println!("    Install: sudo apt install zfsutils-linux");
            all_ok = false;
        }
    }

    // nftables
    match Command::new("nft").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version.lines().next().unwrap_or("unknown").trim();
            println!("  \u{2713} nft ({})", version);
        }
        _ => {
            println!("  \u{2717} nft not found");
            println!("    Install: sudo apt install nftables");
            all_ok = false;
        }
    }

    // KVM
    match std::fs::metadata("/dev/kvm") {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if meta.file_type().is_char_device() {
                println!("  \u{2713} /dev/kvm available");
            } else {
                println!("  \u{2717} /dev/kvm exists but is not a character device");
                println!("    Load the KVM kernel module: modprobe kvm_intel (or kvm_amd)");
                all_ok = false;
            }
        }
        Err(_) => {
            println!("  \u{2717} /dev/kvm not found");
            println!("    Load the KVM kernel module: modprobe kvm_intel (or kvm_amd)");
            all_ok = false;
        }
    }

    // cargo (needed for building guest agent)
    match Command::new("cargo").arg("--version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version.lines().next().unwrap_or("unknown").trim();
            println!("  \u{2713} cargo ({})", version);
        }
        _ => {
            println!("  \u{2717} cargo not found");
            println!("    Install: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh");
            all_ok = false;
        }
    }

    // musl target (needed for static guest agent build)
    let has_musl = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|l| l.contains("x86_64-unknown-linux-musl"))
        })
        .unwrap_or(false);
    if has_musl {
        println!("  \u{2713} x86_64-unknown-linux-musl target installed");
    } else {
        println!("  \u{2717} x86_64-unknown-linux-musl target not installed");
        println!("    Install: rustup target add x86_64-unknown-linux-musl");
        all_ok = false;
    }

    Ok(all_ok)
}

// ---------------------------------------------------------------------------
// Step 2: Directories
// ---------------------------------------------------------------------------

fn create_directories() -> Result<()> {
    let dirs = [
        "/etc/agentiso",
        "/var/lib/agentiso",
        "/var/lib/agentiso/state",
        "/var/lib/agentiso/transfers",
        "/run/agentiso",
    ];

    for dir in &dirs {
        let path = Path::new(dir);
        if path.exists() {
            println!("  \u{2713} {} (already exists)", dir);
        } else {
            std::fs::create_dir_all(path)
                .with_context(|| format!("creating directory: {}", dir))?;
            println!("  \u{2713} {} (created)", dir);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Step 3: ZFS pool
// ---------------------------------------------------------------------------

fn setup_zfs_pool(opts: &InitOptions) -> Result<()> {
    let pool_name = "agentiso";
    let dataset_prefix = "agentiso";

    // Check if pool already exists
    let pool_exists = Command::new("zpool")
        .args(["list", pool_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if pool_exists {
        println!("  \u{2713} ZFS pool '{}' already exists", pool_name);
    } else {
        // Need a path for the pool
        let pool_path = match &opts.pool_path {
            Some(p) => p.clone(),
            None => {
                if opts.yes {
                    anyhow::bail!(
                        "ZFS pool '{}' does not exist and --pool-path was not specified.\n\
                         Use --pool-path to specify a disk device or file path for the pool.",
                        pool_name
                    );
                }
                // Interactive: ask user
                print!(
                    "  ZFS pool '{}' does not exist. Enter path for pool backing \
                     (disk device or file path): ",
                    pool_name
                );
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let input = input.trim().to_string();
                if input.is_empty() {
                    anyhow::bail!("No path provided for ZFS pool. Cannot continue.");
                }
                PathBuf::from(input)
            }
        };

        // Determine if the path is a block device or needs to be a sparse file
        let pool_path_str = pool_path.to_string_lossy().to_string();
        let is_block_device = std::fs::metadata(&pool_path)
            .map(|m| {
                use std::os::unix::fs::FileTypeExt;
                m.file_type().is_block_device()
            })
            .unwrap_or(false);

        if is_block_device {
            println!("  Using block device: {}", pool_path_str);
        } else if pool_path.exists() {
            // Existing file — use it as a vdev
            println!("  Using existing file: {}", pool_path_str);
        } else {
            // Create a sparse file
            let parent = pool_path.parent().unwrap_or(Path::new("/"));
            if !parent.exists() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent directory: {}", parent.display()))?;
            }

            // Default to 100GB sparse file
            let size_gb: u64 = 100;
            println!(
                "  Creating {}GB sparse file at {}...",
                size_gb, pool_path_str
            );
            let file = std::fs::File::create(&pool_path)
                .with_context(|| format!("creating pool file: {}", pool_path_str))?;
            file.set_len(size_gb * 1024 * 1024 * 1024)
                .with_context(|| format!("setting pool file size: {}", pool_path_str))?;
        }

        // Confirm before creating pool (unless --yes)
        if !opts.yes {
            print!(
                "  Create ZFS pool '{}' on '{}'? [Y/n] ",
                pool_name, pool_path_str
            );
            io::stdout().flush()?;
            let mut confirm = String::new();
            io::stdin().read_line(&mut confirm)?;
            let confirm = confirm.trim().to_lowercase();
            if confirm == "n" || confirm == "no" {
                println!("  Skipping ZFS pool creation.");
                return Ok(());
            }
        }

        // Create the pool
        let status = Command::new("zpool")
            .args(["create", "-f", pool_name, &pool_path_str])
            .status()
            .context("running zpool create")?;
        if !status.success() {
            anyhow::bail!("Failed to create ZFS pool '{}'", pool_name);
        }
        println!("  \u{2713} Created ZFS pool '{}'", pool_name);
    }

    // Create datasets
    let pool_root = format!("{}/{}", pool_name, dataset_prefix);
    let datasets = [
        pool_root.clone(),
        format!("{}/base", pool_root),
        format!("{}/workspaces", pool_root),
        format!("{}/forks", pool_root),
        format!("{}/pool", pool_root),
    ];

    for ds in &datasets {
        let exists = Command::new("zfs")
            .args(["list", ds])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if exists {
            println!("  \u{2713} Dataset '{}' already exists", ds);
        } else {
            let status = Command::new("zfs")
                .args(["create", "-p", ds])
                .status()
                .with_context(|| format!("creating dataset: {}", ds))?;
            if !status.success() {
                anyhow::bail!("Failed to create ZFS dataset '{}'", ds);
            }
            println!("  \u{2713} Created dataset '{}'", ds);
        }
    }

    // Set compression=lz4 on the root dataset
    let status = Command::new("zfs")
        .args(["set", "compression=lz4", &pool_root])
        .status()
        .context("setting compression on ZFS dataset")?;
    if status.success() {
        println!("  \u{2713} Set compression=lz4 on '{}'", pool_root);
    } else {
        println!(
            "  \u{2717} Warning: failed to set compression=lz4 on '{}'",
            pool_root
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Step 4: Networking
// ---------------------------------------------------------------------------

fn setup_networking(opts: &InitOptions) -> Result<()> {
    let bridge = "br-agentiso";
    let gateway_ip = "10.99.0.1";
    let subnet_prefix = "16";

    // Check if bridge exists
    let bridge_sysfs = format!("/sys/class/net/{}", bridge);
    if Path::new(&bridge_sysfs).exists() {
        println!("  \u{2713} Bridge '{}' already exists", bridge);
    } else {
        // Create bridge
        let status = Command::new("ip")
            .args(["link", "add", bridge, "type", "bridge"])
            .status()
            .context("creating bridge device")?;
        if !status.success() {
            anyhow::bail!("Failed to create bridge '{}'", bridge);
        }

        // Bring it up
        let status = Command::new("ip")
            .args(["link", "set", bridge, "up"])
            .status()
            .context("bringing bridge up")?;
        if !status.success() {
            anyhow::bail!("Failed to bring bridge '{}' up", bridge);
        }

        // Assign IP
        let addr = format!("{}/{}", gateway_ip, subnet_prefix);
        let status = Command::new("ip")
            .args(["addr", "add", &addr, "dev", bridge])
            .status()
            .context("assigning IP to bridge")?;
        if !status.success() {
            anyhow::bail!("Failed to assign IP {} to bridge '{}'", addr, bridge);
        }

        println!("  \u{2713} Created bridge '{}' with IP {}", bridge, addr);
    }

    // Enable per-interface IP forwarding (scoped to the bridge only, not global)
    let forwarding_path = format!("/proc/sys/net/ipv4/conf/{}/forwarding", bridge);
    let current = std::fs::read_to_string(&forwarding_path).unwrap_or_default();
    if current.trim() == "1" {
        println!("  \u{2713} IP forwarding already enabled for {}", bridge);
    } else {
        std::fs::write(&forwarding_path, "1")
            .with_context(|| format!("enabling IP forwarding for {}", bridge))?;
        println!("  \u{2713} Enabled IP forwarding for {}", bridge);
    }

    // Make per-interface IP forwarding persistent via sysctl.d
    let sysctl_conf = "/etc/sysctl.d/99-agentiso.conf";
    let sysctl_key = format!("net.ipv4.conf.{}.forwarding", bridge);
    let sysctl_content = format!("{} = 1\n", sysctl_key);
    if Path::new(sysctl_conf).exists() {
        let existing = std::fs::read_to_string(sysctl_conf).unwrap_or_default();
        if existing.contains(&sysctl_key) {
            println!("  \u{2713} IP forwarding sysctl already persisted for {}", bridge);
        } else {
            std::fs::write(sysctl_conf, &sysctl_content)
                .with_context(|| format!("writing {}", sysctl_conf))?;
            println!("  \u{2713} Persisted IP forwarding to {}", sysctl_conf);
        }
    } else {
        std::fs::write(sysctl_conf, &sysctl_content)
            .with_context(|| format!("writing {}", sysctl_conf))?;
        println!("  \u{2713} Persisted IP forwarding to {}", sysctl_conf);
    }

    // Set up nftables masquerade rule for outbound traffic
    setup_nftables_masquerade(bridge, gateway_ip, subnet_prefix, opts)?;

    Ok(())
}

fn setup_nftables_masquerade(
    _bridge: &str,
    _gateway_ip: &str,
    subnet_prefix: &str,
    _opts: &InitOptions,
) -> Result<()> {
    // Check if agentiso table already exists
    let table_exists = Command::new("nft")
        .args(["list", "table", "inet", "agentiso"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if table_exists {
        println!("  \u{2713} nftables table 'inet agentiso' already exists");
        return Ok(());
    }

    // Build the subnet CIDR for masquerade rule
    let subnet_cidr = format!("10.99.0.0/{}", subnet_prefix);

    // Create the nftables ruleset
    let ruleset = format!(
        r#"table inet agentiso {{
    chain postrouting {{
        type nat hook postrouting priority srcnat; policy accept;
        ip saddr {} oifname != "br-agentiso" masquerade
    }}

    chain forward {{
        type filter hook forward priority filter; policy accept;
        iifname "br-agentiso" accept
        oifname "br-agentiso" ct state established,related accept
    }}
}}"#,
        subnet_cidr
    );

    // Write to a secure temp file and apply.
    // Uses tempfile::NamedTempFile to avoid symlink attacks on predictable paths.
    let mut nft_tmpfile = tempfile::NamedTempFile::new()
        .context("creating temp file for nftables ruleset")?;
    nft_tmpfile
        .write_all(ruleset.as_bytes())
        .context("writing nftables ruleset to temp file")?;

    let status = Command::new("nft")
        .args(["-f", &nft_tmpfile.path().to_string_lossy().into_owned()])
        .status()
        .context("applying nftables ruleset")?;

    // nft_tmpfile is automatically cleaned up when dropped

    if status.success() {
        println!("  \u{2713} Created nftables masquerade rules for VM outbound traffic");
    } else {
        println!("  \u{2717} Warning: failed to apply nftables rules");
        println!("    You may need to configure masquerade manually for VM internet access.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Step 5: Kernel + initrd
// ---------------------------------------------------------------------------

fn install_kernel_initrd(opts: &InitOptions) -> Result<()> {
    let dest_kernel = PathBuf::from("/var/lib/agentiso/vmlinuz");
    let dest_initrd = PathBuf::from("/var/lib/agentiso/initrd.img");

    // Determine source kernel path
    let kernel_release = get_kernel_release()?;
    let default_kernel = PathBuf::from(format!("/boot/vmlinuz-{}", kernel_release));
    let default_initrd = PathBuf::from(format!("/boot/initrd.img-{}", kernel_release));

    let src_kernel = opts.kernel.as_ref().unwrap_or(&default_kernel);
    let src_initrd = opts.initrd.as_ref().unwrap_or(&default_initrd);

    // Install kernel
    if dest_kernel.exists() {
        // Check if it's the same file
        if files_match(src_kernel, &dest_kernel) {
            println!(
                "  \u{2713} Kernel already installed and up-to-date ({})",
                dest_kernel.display()
            );
        } else {
            std::fs::copy(src_kernel, &dest_kernel).with_context(|| {
                format!(
                    "copying kernel from {} to {}",
                    src_kernel.display(),
                    dest_kernel.display()
                )
            })?;
            println!(
                "  \u{2713} Updated kernel: {} -> {}",
                src_kernel.display(),
                dest_kernel.display()
            );
        }
    } else {
        if !src_kernel.exists() {
            anyhow::bail!(
                "Source kernel not found at {}.\n\
                 Use --kernel to specify a custom kernel path.",
                src_kernel.display()
            );
        }
        std::fs::copy(src_kernel, &dest_kernel).with_context(|| {
            format!(
                "copying kernel from {} to {}",
                src_kernel.display(),
                dest_kernel.display()
            )
        })?;
        println!(
            "  \u{2713} Installed kernel: {} -> {}",
            src_kernel.display(),
            dest_kernel.display()
        );
    }

    // Install initrd
    if src_initrd.exists() {
        if dest_initrd.exists() && files_match(src_initrd, &dest_initrd) {
            println!(
                "  \u{2713} Initrd already installed and up-to-date ({})",
                dest_initrd.display()
            );
        } else {
            std::fs::copy(src_initrd, &dest_initrd).with_context(|| {
                format!(
                    "copying initrd from {} to {}",
                    src_initrd.display(),
                    dest_initrd.display()
                )
            })?;
            println!(
                "  \u{2713} Installed initrd: {} -> {}",
                src_initrd.display(),
                dest_initrd.display()
            );
        }
    } else if opts.initrd.is_some() {
        // User explicitly specified an initrd that doesn't exist
        anyhow::bail!(
            "Specified initrd not found at {}",
            src_initrd.display()
        );
    } else {
        println!(
            "  \u{26A0} No initrd found at {} (distro kernels with virtio as modules may need this)",
            src_initrd.display()
        );
    }

    // Print sizes
    if let Ok(meta) = std::fs::metadata(&dest_kernel) {
        let size_mb = meta.len() / (1024 * 1024);
        println!("    Kernel size: {}MB", size_mb);
    }
    if let Ok(meta) = std::fs::metadata(&dest_initrd) {
        let size_mb = meta.len() / (1024 * 1024);
        println!("    Initrd size: {}MB", size_mb);
    }

    Ok(())
}

fn get_kernel_release() -> Result<String> {
    let output = Command::new("uname")
        .arg("-r")
        .output()
        .context("running uname -r")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Compare two files by size (fast check to avoid reading entire files).
fn files_match(a: &Path, b: &Path) -> bool {
    let meta_a = match std::fs::metadata(a) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let meta_b = match std::fs::metadata(b) {
        Ok(m) => m,
        Err(_) => return false,
    };
    meta_a.len() == meta_b.len()
}

// ---------------------------------------------------------------------------
// Step 6: Guest agent
// ---------------------------------------------------------------------------

fn build_guest_agent(opts: &InitOptions) -> Result<()> {
    let dest = PathBuf::from("/var/lib/agentiso/guest-agent");

    // Find the project root by looking for Cargo.toml with workspace members
    let project_root = find_project_root()?;

    println!("  Building guest agent (this may take a while on first build)...");
    println!("  Project root: {}", project_root.display());

    // Determine the user to run cargo as. When running under sudo, we should
    // build as the original user to use their cargo/rustup installation and
    // avoid polluting root's home with build artifacts.
    let sudo_user = std::env::var("SUDO_USER").ok();
    let build_status = if let Some(ref user) = sudo_user {
        // Validate SUDO_USER: must be a reasonable username (alphanumeric, dash, underscore, dot)
        if !user.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
            anyhow::bail!(
                "SUDO_USER contains invalid characters: {:?}. \
                 Expected a valid Unix username.",
                user
            );
        }
        // Run cargo as the original user via su, passing arguments safely
        // without shell interpolation to prevent command injection.
        println!("  Running cargo as user '{}' (from SUDO_USER)", user);
        Command::new("su")
            .arg("-")
            .arg(user)
            .arg("-c")
            .arg(format!(
                "cd -- {} && exec cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest",
                shell_escape(&project_root.to_string_lossy())
            ))
            .status()
            .context("running cargo build via su")?
    } else {
        Command::new("cargo")
            .args([
                "build",
                "--release",
                "--target",
                "x86_64-unknown-linux-musl",
                "-p",
                "agentiso-guest",
            ])
            .current_dir(&project_root)
            .status()
            .context("running cargo build")?
    };

    if !build_status.success() {
        anyhow::bail!(
            "Failed to build guest agent. Ensure the musl target is installed:\n\
             rustup target add x86_64-unknown-linux-musl"
        );
    }

    let built_binary = project_root
        .join("target/x86_64-unknown-linux-musl/release/agentiso-guest");

    if !built_binary.exists() {
        anyhow::bail!(
            "Guest agent binary not found at {} after build",
            built_binary.display()
        );
    }

    std::fs::copy(&built_binary, &dest).with_context(|| {
        format!(
            "copying guest agent from {} to {}",
            built_binary.display(),
            dest.display()
        )
    })?;

    // Ensure executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .context("setting guest agent permissions")?;
    }

    if let Ok(meta) = std::fs::metadata(&dest) {
        let size_mb = meta.len() as f64 / (1024.0 * 1024.0);
        println!(
            "  \u{2713} Guest agent installed: {} ({:.1}MB)",
            dest.display(),
            size_mb
        );
    } else {
        println!("  \u{2713} Guest agent installed: {}", dest.display());
    }

    // Suppress unused variable warning when not building for unix
    let _ = opts;

    Ok(())
}

/// Walk upward from the current executable or cwd to find the workspace root
/// (directory containing Cargo.toml with [workspace]).
fn find_project_root() -> Result<PathBuf> {
    // Try from the current executable location first
    let start = std::env::current_exe()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let mut dir = start.as_path();
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            // Check if it has [workspace]
            if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
                if content.contains("[workspace]") {
                    return Ok(dir.to_path_buf());
                }
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    // Fall back to cwd
    let cwd = std::env::current_dir().context("getting current directory")?;
    let mut dir = cwd.as_path();
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
                if content.contains("[workspace]") {
                    return Ok(dir.to_path_buf());
                }
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    anyhow::bail!(
        "Could not find project root (Cargo.toml with [workspace]).\n\
         Run agentiso init from within the project directory."
    )
}

// ---------------------------------------------------------------------------
// Step 7: Default config
// ---------------------------------------------------------------------------

fn create_default_config() -> Result<()> {
    let config_path = PathBuf::from("/etc/agentiso/config.toml");

    if config_path.exists() {
        println!(
            "  \u{2713} Config file already exists: {}",
            config_path.display()
        );
        println!("    (not overwriting existing config)");
        return Ok(());
    }

    let config_content = r#"# agentiso configuration
# Generated by `agentiso init`

[server]
state_file = "/var/lib/agentiso/state.json"
state_persist_interval_secs = 30
transfer_dir = "/var/lib/agentiso/transfers"

[vm]
kernel_path = "/var/lib/agentiso/vmlinuz"
initrd_path = "/var/lib/agentiso/initrd.img"
qemu_binary = "qemu-system-x86_64"
run_dir = "/run/agentiso"
kernel_append = "console=ttyS0 root=/dev/vda rw quiet"
vsock_cid_start = 100
guest_agent_port = 5000
boot_timeout_secs = 30
init_mode = "openrc"

[storage]
zfs_pool = "agentiso"
dataset_prefix = "agentiso"
base_image = "alpine-dev"
base_snapshot = "latest"

[network]
bridge_name = "br-agentiso"
gateway_ip = "10.99.0.1"
subnet_prefix = 16
default_allow_internet = true
default_allow_inter_vm = false
dns_servers = ["1.1.1.1", "8.8.8.8"]

[resources]
default_vcpus = 2
default_memory_mb = 512
default_disk_gb = 10
max_vcpus = 8
max_memory_mb = 8192
max_disk_gb = 100
max_workspaces = 20

[pool]
enabled = true
min_size = 2
max_size = 10
target_free = 2
max_memory_mb = 8192

[vault]
enabled = false
path = "/mnt/vault"
extensions = ["md"]
exclude_dirs = [".obsidian", ".trash", ".git"]
"#;

    // Ensure the parent directory exists
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating config directory: {}", parent.display()))?;
    }

    std::fs::write(&config_path, config_content)
        .with_context(|| format!("writing config file: {}", config_path.display()))?;

    println!(
        "  \u{2713} Created default config: {}",
        config_path.display()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Step 8: Verification
// ---------------------------------------------------------------------------

fn run_verification() -> Result<()> {
    let mut all_ok = true;

    // Check directories
    for dir in &[
        "/etc/agentiso",
        "/var/lib/agentiso",
        "/var/lib/agentiso/state",
        "/run/agentiso",
    ] {
        if Path::new(dir).exists() {
            println!("  \u{2713} Directory: {}", dir);
        } else {
            println!("  \u{2717} Directory missing: {}", dir);
            all_ok = false;
        }
    }

    // Check kernel
    let kernel = Path::new("/var/lib/agentiso/vmlinuz");
    if kernel.exists() {
        let size_mb = std::fs::metadata(kernel)
            .map(|m| m.len() / (1024 * 1024))
            .unwrap_or(0);
        println!("  \u{2713} Kernel: {} ({}MB)", kernel.display(), size_mb);
    } else {
        println!("  \u{2717} Kernel not found: {}", kernel.display());
        all_ok = false;
    }

    // Check initrd
    let initrd = Path::new("/var/lib/agentiso/initrd.img");
    if initrd.exists() {
        let size_mb = std::fs::metadata(initrd)
            .map(|m| m.len() / (1024 * 1024))
            .unwrap_or(0);
        println!("  \u{2713} Initrd: {} ({}MB)", initrd.display(), size_mb);
    } else {
        println!("  \u{26A0} Initrd not found: {} (may be needed for distro kernels)", initrd.display());
    }

    // Check guest agent
    let guest_agent = Path::new("/var/lib/agentiso/guest-agent");
    if guest_agent.exists() {
        let size_mb = std::fs::metadata(guest_agent)
            .map(|m| m.len() as f64 / (1024.0 * 1024.0))
            .unwrap_or(0.0);
        println!(
            "  \u{2713} Guest agent: {} ({:.1}MB)",
            guest_agent.display(),
            size_mb
        );
    } else {
        println!(
            "  \u{2717} Guest agent not found: {}",
            guest_agent.display()
        );
        all_ok = false;
    }

    // Check config
    let config = Path::new("/etc/agentiso/config.toml");
    if config.exists() {
        println!("  \u{2713} Config: {}", config.display());
    } else {
        println!("  \u{2717} Config not found: {}", config.display());
        all_ok = false;
    }

    // Check ZFS pool
    let pool_ok = Command::new("zpool")
        .args(["list", "agentiso"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if pool_ok {
        println!("  \u{2713} ZFS pool 'agentiso' exists");
    } else {
        println!("  \u{2717} ZFS pool 'agentiso' not found");
        all_ok = false;
    }

    // Check ZFS datasets
    let pool_root = "agentiso/agentiso";
    let dataset_ok = Command::new("zfs")
        .args(["list", pool_root])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if dataset_ok {
        println!("  \u{2713} ZFS dataset '{}' exists", pool_root);
    } else {
        println!("  \u{2717} ZFS dataset '{}' not found", pool_root);
        all_ok = false;
    }

    // Check bridge
    let bridge_sysfs = "/sys/class/net/br-agentiso";
    if Path::new(bridge_sysfs).exists() {
        println!("  \u{2713} Bridge 'br-agentiso' exists");
    } else {
        println!("  \u{2717} Bridge 'br-agentiso' not found");
        all_ok = false;
    }

    // Check per-interface IP forwarding on the bridge
    let bridge_fwd_path = "/proc/sys/net/ipv4/conf/br-agentiso/forwarding";
    let forwarding = std::fs::read_to_string(bridge_fwd_path).unwrap_or_default();
    if forwarding.trim() == "1" {
        println!("  \u{2713} IP forwarding enabled for br-agentiso");
    } else {
        // Also check global forwarding as a fallback (may be set by Docker, etc.)
        let global_fwd = std::fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
            .unwrap_or_default();
        if global_fwd.trim() == "1" {
            println!("  \u{2713} IP forwarding enabled (global)");
        } else {
            println!("  \u{2717} IP forwarding not enabled for br-agentiso");
            all_ok = false;
        }
    }

    // Check nftables
    let nft_ok = Command::new("nft")
        .args(["list", "table", "inet", "agentiso"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if nft_ok {
        println!("  \u{2713} nftables rules configured");
    } else {
        println!("  \u{26A0} nftables rules not found (VMs may not have internet access)");
    }

    println!();
    if all_ok {
        println!("  All verifications passed!");
    } else {
        println!("  Some verifications failed. Review the output above.");
    }

    Ok(())
}

use crate::util::shell_escape;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_project_root_from_cwd() {
        // This test only works when run from within the project tree
        // It should find the workspace Cargo.toml
        let result = find_project_root();
        // Don't assert success since CI might run from a different directory,
        // but if it succeeds, verify the Cargo.toml exists
        if let Ok(root) = result {
            assert!(root.join("Cargo.toml").exists());
        }
    }

    #[test]
    fn files_match_same_size() {
        let dir = std::env::temp_dir();
        let a = dir.join("agentiso-test-match-a");
        let b = dir.join("agentiso-test-match-b");
        std::fs::write(&a, "hello").unwrap();
        std::fs::write(&b, "world").unwrap();
        assert!(files_match(&a, &b)); // same size, different content
        std::fs::write(&b, "hi").unwrap();
        assert!(!files_match(&a, &b)); // different size
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn files_match_missing() {
        let missing = PathBuf::from("/nonexistent/file");
        assert!(!files_match(&missing, &missing));
    }
}
