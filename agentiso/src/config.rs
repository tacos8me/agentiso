use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level configuration for the agentiso daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub storage: StorageConfig,
    pub network: NetworkConfig,
    pub vm: VmConfig,
    pub server: ServerConfig,
    pub resources: DefaultResourceLimits,
    pub pool: PoolConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            storage: StorageConfig::default(),
            network: NetworkConfig::default(),
            vm: VmConfig::default(),
            server: ServerConfig::default(),
            resources: DefaultResourceLimits::default(),
            pool: PoolConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file.
    pub fn load(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading config: {}", path.display()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| format!("parsing config: {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate configuration values.
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.resources.max_vcpus >= 1,
            "resources.max_vcpus must be >= 1"
        );
        anyhow::ensure!(
            self.resources.max_memory_mb >= 64,
            "resources.max_memory_mb must be >= 64"
        );
        anyhow::ensure!(
            self.resources.max_disk_gb >= 1,
            "resources.max_disk_gb must be >= 1"
        );
        anyhow::ensure!(
            self.resources.max_workspaces >= 1,
            "resources.max_workspaces must be >= 1"
        );
        anyhow::ensure!(
            self.network.subnet_prefix >= 8 && self.network.subnet_prefix <= 30,
            "network.subnet_prefix must be between 8 and 30"
        );
        anyhow::ensure!(
            !self.network.dns_servers.is_empty(),
            "network.dns_servers must not be empty"
        );
        for (i, dns) in self.network.dns_servers.iter().enumerate() {
            anyhow::ensure!(
                dns.parse::<Ipv4Addr>().is_ok(),
                "network.dns_servers[{}] is not a valid IPv4 address: {}",
                i,
                dns
            );
        }
        if self.pool.enabled {
            anyhow::ensure!(
                self.pool.min_size <= self.pool.max_size,
                "pool.min_size must be <= pool.max_size"
            );
            anyhow::ensure!(
                self.pool.target_free <= self.pool.max_size,
                "pool.target_free must be <= pool.max_size"
            );
        }
        Ok(())
    }
}

/// ZFS storage configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// ZFS pool name (e.g. "agentiso").
    pub zfs_pool: String,
    /// Dataset prefix under the pool (e.g. "agentiso").
    pub dataset_prefix: String,
    /// Base image dataset name (relative to pool/prefix/base/).
    pub base_image: String,
    /// Snapshot name on the base image to clone from.
    pub base_snapshot: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            zfs_pool: "agentiso".into(),
            dataset_prefix: "agentiso".into(),
            base_image: "alpine-dev".into(),
            base_snapshot: "latest".into(),
        }
    }
}

#[allow(dead_code)]
impl StorageConfig {
    /// Full dataset path for the base image, e.g. "tank/agentiso/base/alpine-dev".
    pub fn base_dataset(&self) -> String {
        format!(
            "{}/{}/base/{}",
            self.zfs_pool, self.dataset_prefix, self.base_image
        )
    }

    /// Full snapshot path for the base image, e.g. "tank/agentiso/base/alpine-dev@latest".
    pub fn base_snapshot_path(&self) -> String {
        format!("{}@{}", self.base_dataset(), self.base_snapshot)
    }

    /// Dataset path prefix for workspaces, e.g. "tank/agentiso/workspaces".
    pub fn workspaces_dataset(&self) -> String {
        format!("{}/{}/workspaces", self.zfs_pool, self.dataset_prefix)
    }

    /// Full dataset path for a specific workspace zvol, e.g. "tank/agentiso/workspaces/ws-{id}".
    pub fn workspace_dataset(&self, workspace_id: &str) -> String {
        format!("{}/ws-{}", self.workspaces_dataset(), workspace_id)
    }
}

/// Network configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Bridge device name.
    pub bridge_name: String,
    /// Gateway IP on the bridge (host side).
    pub gateway_ip: Ipv4Addr,
    /// Subnet prefix length (e.g. 16 for /16).
    pub subnet_prefix: u8,
    /// Whether to allow internet access by default for new workspaces.
    pub default_allow_internet: bool,
    /// Whether to allow inter-VM traffic by default.
    pub default_allow_inter_vm: bool,
    /// DNS servers to configure inside guest VMs.
    /// Default: ["1.1.1.1", "8.8.8.8"]
    #[serde(default = "default_dns_servers")]
    pub dns_servers: Vec<String>,
}

fn default_dns_servers() -> Vec<String> {
    vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()]
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bridge_name: "br-agentiso".into(),
            gateway_ip: Ipv4Addr::new(10, 42, 0, 1),
            subnet_prefix: 16,
            default_allow_internet: false,
            default_allow_inter_vm: false,
            dns_servers: default_dns_servers(),
        }
    }
}

/// Init system mode for guest VMs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InitMode {
    /// Standard Alpine OpenRC init.
    OpenRC,
    /// Fast init shim (init-fast binary).
    Fast,
}

impl Default for InitMode {
    fn default() -> Self {
        InitMode::OpenRC
    }
}

impl std::fmt::Display for InitMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitMode::OpenRC => write!(f, "openrc"),
            InitMode::Fast => write!(f, "fast"),
        }
    }
}

/// VM / QEMU configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VmConfig {
    /// Path to the kernel image (bzImage or vmlinux).
    pub kernel_path: PathBuf,
    /// Optional path to the initramfs image (needed for distro kernels with virtio modules).
    pub initrd_path: Option<PathBuf>,
    /// Path to the qemu-system-x86_64 binary.
    pub qemu_binary: PathBuf,
    /// Runtime directory for QMP sockets and PID files.
    pub run_dir: PathBuf,
    /// Kernel boot arguments.
    pub kernel_append: String,
    /// Starting vsock CID (incremented per workspace).
    pub vsock_cid_start: u32,
    /// Vsock port the guest agent listens on.
    pub guest_agent_port: u32,
    /// Timeout in seconds for guest agent readiness handshake.
    pub boot_timeout_secs: u64,
    /// Init mode: Fast for init-fast shim, OpenRC for standard Alpine init.
    pub init_mode: InitMode,
    /// Optional path to the fast initrd image (used when init_mode = "fast").
    pub initrd_fast_path: Option<PathBuf>,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            kernel_path: PathBuf::from("/var/lib/agentiso/vmlinuz"),
            initrd_path: Some(PathBuf::from("/var/lib/agentiso/initrd.img")),
            qemu_binary: PathBuf::from("qemu-system-x86_64"),
            run_dir: PathBuf::from("/run/agentiso"),
            kernel_append: "console=ttyS0 root=/dev/vda rw quiet".into(),
            vsock_cid_start: 100,
            guest_agent_port: 5000,
            boot_timeout_secs: 30,
            init_mode: InitMode::default(),
            initrd_fast_path: None,
        }
    }
}

/// Server / daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Path to persist workspace state as JSON.
    pub state_file: PathBuf,
    /// Interval in seconds for periodic state persistence.
    pub state_persist_interval_secs: u64,
    /// Allowed directory for host-side file transfers (upload/download).
    /// All host_path values in file_upload and file_download must resolve
    /// within this directory. Prevents path-traversal attacks.
    pub transfer_dir: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            state_file: PathBuf::from("/var/lib/agentiso/state.json"),
            state_persist_interval_secs: 30,
            transfer_dir: PathBuf::from("/var/lib/agentiso/transfers"),
        }
    }
}

/// Default resource limits for workspaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultResourceLimits {
    /// Default vCPUs per workspace.
    pub default_vcpus: u32,
    /// Default memory in MB per workspace.
    pub default_memory_mb: u32,
    /// Default disk size in GB per workspace.
    pub default_disk_gb: u32,
    /// Maximum vCPUs a single workspace can request.
    pub max_vcpus: u32,
    /// Maximum memory in MB a single workspace can request.
    pub max_memory_mb: u32,
    /// Maximum disk in GB a single workspace can request.
    pub max_disk_gb: u32,
    /// Maximum concurrent workspaces.
    pub max_workspaces: u32,
}

impl Default for DefaultResourceLimits {
    fn default() -> Self {
        Self {
            default_vcpus: 2,
            default_memory_mb: 512,
            default_disk_gb: 10,
            max_vcpus: 8,
            max_memory_mb: 8192,
            max_disk_gb: 100,
            max_workspaces: 20,
        }
    }
}

/// Warm VM pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PoolConfig {
    /// Enable the warm VM pool.
    pub enabled: bool,
    /// Minimum VMs to keep in the pool.
    pub min_size: usize,
    /// Maximum VMs in the pool.
    pub max_size: usize,
    /// Target number of free (ready) VMs.
    pub target_free: usize,
    /// Total memory budget in MB for pool VMs.
    pub max_memory_mb: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_size: 2,
            max_size: 10,
            target_free: 3,
            max_memory_mb: 8192,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn config_defaults() {
        let config = Config::default();
        assert_eq!(config.storage.zfs_pool, "agentiso");
        assert_eq!(config.storage.dataset_prefix, "agentiso");
        assert_eq!(config.storage.base_image, "alpine-dev");
        assert_eq!(config.network.bridge_name, "br-agentiso");
        assert_eq!(config.network.gateway_ip, Ipv4Addr::new(10, 42, 0, 1));
        assert_eq!(config.network.subnet_prefix, 16);
        assert!(!config.network.default_allow_internet);
        assert!(!config.network.default_allow_inter_vm);
        assert_eq!(config.vm.vsock_cid_start, 100);
        assert_eq!(config.vm.guest_agent_port, 5000);
        assert_eq!(config.resources.default_vcpus, 2);
        assert_eq!(config.resources.default_memory_mb, 512);
        assert_eq!(config.resources.max_workspaces, 20);
    }

    #[test]
    fn config_default_validates() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn config_load_from_toml() {
        let toml_content = r#"
[storage]
zfs_pool = "rpool"
base_image = "ubuntu-dev"

[network]
bridge_name = "br-test"
subnet_prefix = 24

[resources]
default_vcpus = 4
max_memory_mb = 16384
"#;
        let mut tmpfile = tempfile();
        tmpfile.write_all(toml_content.as_bytes()).unwrap();
        let path = tmpfile.path();

        let config = Config::load(path).unwrap();
        assert_eq!(config.storage.zfs_pool, "rpool");
        assert_eq!(config.storage.base_image, "ubuntu-dev");
        // Unset fields use defaults
        assert_eq!(config.storage.dataset_prefix, "agentiso");
        assert_eq!(config.network.bridge_name, "br-test");
        assert_eq!(config.network.subnet_prefix, 24);
        assert_eq!(config.resources.default_vcpus, 4);
        assert_eq!(config.resources.max_memory_mb, 16384);
    }

    #[test]
    fn config_validation_rejects_zero_vcpus() {
        let mut config = Config::default();
        config.resources.max_vcpus = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_validation_rejects_low_memory() {
        let mut config = Config::default();
        config.resources.max_memory_mb = 32;
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_validation_rejects_bad_subnet_prefix() {
        let mut config = Config::default();
        config.network.subnet_prefix = 31;
        assert!(config.validate().is_err());

        config.network.subnet_prefix = 7;
        assert!(config.validate().is_err());
    }

    #[test]
    fn storage_config_dataset_paths() {
        let sc = StorageConfig::default();
        assert_eq!(sc.base_dataset(), "agentiso/agentiso/base/alpine-dev");
        assert_eq!(sc.base_snapshot_path(), "agentiso/agentiso/base/alpine-dev@latest");
        assert_eq!(sc.workspaces_dataset(), "agentiso/agentiso/workspaces");
        assert_eq!(sc.workspace_dataset("abcd1234"), "agentiso/agentiso/workspaces/ws-abcd1234");
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.storage.zfs_pool, config.storage.zfs_pool);
        assert_eq!(deserialized.network.gateway_ip, config.network.gateway_ip);
        assert_eq!(deserialized.vm.vsock_cid_start, config.vm.vsock_cid_start);
        assert_eq!(deserialized.resources.max_workspaces, config.resources.max_workspaces);
    }

    /// Helper: create a named temporary file that auto-deletes.
    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl std::io::Write for TempFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?
                .write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn tempfile() -> TempFile {
        let path = std::env::temp_dir().join(format!("agentiso-test-{}.toml", uuid::Uuid::new_v4()));
        TempFile { path }
    }

    #[test]
    fn config_pool_defaults() {
        let config = Config::default();
        assert!(!config.pool.enabled);
        assert_eq!(config.pool.min_size, 2);
        assert_eq!(config.pool.max_size, 10);
        assert_eq!(config.pool.target_free, 3);
        assert_eq!(config.pool.max_memory_mb, 8192);
    }

    #[test]
    fn config_pool_validation_min_exceeds_max() {
        let mut config = Config::default();
        config.pool.enabled = true;
        config.pool.min_size = 15;
        config.pool.max_size = 10;
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_vm_init_mode_default() {
        let config = Config::default();
        assert_eq!(config.vm.init_mode, InitMode::OpenRC);
        assert!(config.vm.initrd_fast_path.is_none());
    }
}
