use std::path::{Path, PathBuf};

/// Resource limits for a VM.
#[derive(Debug, Clone)]
pub struct VmConfig {
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Memory in megabytes.
    pub memory_mb: u32,
    /// Path to the kernel binary (bzImage or vmlinux).
    pub kernel_path: PathBuf,
    /// Optional path to the initramfs image.
    pub initrd_path: Option<PathBuf>,
    /// Kernel command line arguments.
    pub kernel_cmdline: String,
    /// Init mode: "fast" appends init=/sbin/init-fast, "openrc" uses default init.
    pub init_mode: String,
    /// Path to the root disk (ZFS zvol block device).
    pub root_disk: PathBuf,
    /// TAP device name for networking.
    pub tap_device: String,
    /// vsock context ID for host<->guest communication.
    pub vsock_cid: u32,
    /// Path for the QMP control socket.
    pub qmp_socket: PathBuf,
    /// Working directory for QEMU (for PID file, logs, etc).
    pub run_dir: PathBuf,
}

impl VmConfig {
    /// Build the QEMU command line arguments for a microvm launch.
    pub fn build_command(&self) -> QemuCommand {
        let mut args = Vec::new();

        // Machine type: microvm with minimal features
        args.push("-M".into());
        args.push("microvm,rtc=on".into());

        // CPU and KVM acceleration
        args.push("-cpu".into());
        args.push("host".into());
        args.push("-enable-kvm".into());

        // Memory
        args.push("-m".into());
        args.push(format!("{}", self.memory_mb));

        // SMP (virtual CPUs)
        args.push("-smp".into());
        args.push(format!("{}", self.vcpus));

        // Kernel
        args.push("-kernel".into());
        args.push(self.kernel_path.to_string_lossy().into_owned());

        // Initramfs (needed when kernel has virtio as modules, e.g. distro kernels)
        if let Some(ref initrd) = self.initrd_path {
            args.push("-initrd".into());
            args.push(initrd.to_string_lossy().into_owned());
        }

        // Kernel command line
        args.push("-append".into());
        let mut cmdline = self.kernel_cmdline.clone();
        if self.init_mode == "fast" {
            cmdline.push_str(" init=/sbin/init-fast");
        }
        args.push(cmdline);

        // Root disk (raw block device from ZFS zvol)
        args.push("-drive".into());
        args.push(format!(
            "id=root,file={},format=raw,if=none",
            self.root_disk.display()
        ));
        args.push("-device".into());
        args.push("virtio-blk-device,drive=root".into());

        // Networking via TAP device
        args.push("-netdev".into());
        args.push(format!(
            "tap,id=net0,ifname={},script=no,downscript=no",
            self.tap_device
        ));
        args.push("-device".into());
        args.push("virtio-net-device,netdev=net0".into());

        // vsock for host<->guest communication
        args.push("-device".into());
        args.push(format!("vhost-vsock-device,guest-cid={}", self.vsock_cid));

        // QMP control socket
        args.push("-chardev".into());
        args.push(format!(
            "socket,id=qmp,path={},server=on,wait=off",
            self.qmp_socket.display()
        ));
        args.push("-mon".into());
        args.push("chardev=qmp,mode=control".into());

        // Serial console for kernel messages (matches console=ttyS0 in kernel cmdline)
        args.push("-chardev".into());
        args.push(format!(
            "file,id=serial0,path={}/console.log",
            self.run_dir.display()
        ));
        args.push("-device".into());
        args.push("isa-serial,chardev=serial0".into());

        // No graphics, no default devices
        args.push("-nographic".into());
        args.push("-nodefaults".into());

        QemuCommand {
            binary: "qemu-system-x86_64".into(),
            args,
        }
    }
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            vcpus: 2,
            memory_mb: 512,
            kernel_path: PathBuf::from("/var/lib/agentiso/vmlinuz"),
            initrd_path: Some(PathBuf::from("/var/lib/agentiso/initrd.img")),
            kernel_cmdline: "console=ttyS0 root=/dev/vda rw quiet".into(),
            init_mode: "openrc".into(),
            root_disk: PathBuf::new(),
            tap_device: String::new(),
            vsock_cid: 0,
            qmp_socket: PathBuf::new(),
            run_dir: PathBuf::new(),
        }
    }
}

/// A fully-resolved QEMU command ready to be spawned.
#[derive(Debug, Clone)]
pub struct QemuCommand {
    /// Path to the QEMU binary.
    pub binary: String,
    /// Command line arguments.
    pub args: Vec<String>,
}

impl QemuCommand {
    /// Convert to a tokio Command for spawning.
    ///
    /// Note: stderr is set to null by default. Callers that need QEMU stderr
    /// output (e.g., `spawn_qemu`) should override stderr to redirect to a
    /// log file via `cmd.stderr(Stdio::from(file))`.
    pub fn to_tokio_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.args(&self.args);
        // Detach from controlling terminal
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd
    }

    /// Return the full command line as a string (for logging).
    pub fn command_line(&self) -> String {
        let mut parts = vec![self.binary.clone()];
        parts.extend(self.args.iter().map(|a| {
            if a.contains(' ') || a.contains(',') {
                format!("'{}'", a)
            } else {
                a.clone()
            }
        }));
        parts.join(" ")
    }
}

/// Resolve the runtime directory path for a workspace.
#[allow(dead_code)] // Utility used by tests and future callers
pub fn workspace_run_dir(base_run_dir: &Path, workspace_id: &str) -> PathBuf {
    base_run_dir.join(workspace_id)
}

/// Resolve the QMP socket path for a workspace.
#[allow(dead_code)] // Utility used by tests and future callers
pub fn qmp_socket_path(base_run_dir: &Path, workspace_id: &str) -> PathBuf {
    workspace_run_dir(base_run_dir, workspace_id).join("qmp.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_command_contains_expected_args() {
        let config = VmConfig {
            vcpus: 4,
            memory_mb: 1024,
            kernel_path: PathBuf::from("/var/lib/agentiso/vmlinuz"),
            initrd_path: Some(PathBuf::from("/var/lib/agentiso/initrd.img")),
            kernel_cmdline: "console=ttyS0 root=/dev/vda rw quiet".into(),
            init_mode: "openrc".into(),
            root_disk: PathBuf::from("/dev/zvol/tank/agentiso/workspaces/ws-abcd1234"),
            tap_device: "tap-abcd1234".into(),
            vsock_cid: 3,
            qmp_socket: PathBuf::from("/run/agentiso/abcd1234/qmp.sock"),
            run_dir: PathBuf::from("/run/agentiso/abcd1234"),
        };

        let cmd = config.build_command();
        assert_eq!(cmd.binary, "qemu-system-x86_64");

        let args_str = cmd.args.join(" ");
        assert!(args_str.contains("microvm"));
        assert!(args_str.contains("-enable-kvm"));
        assert!(args_str.contains("-m 1024"));
        assert!(args_str.contains("-smp 4"));
        assert!(args_str.contains("guest-cid=3"));
        assert!(args_str.contains("tap-abcd1234"));
        assert!(args_str.contains("qmp.sock"));
        assert!(args_str.contains("-nographic"));
        assert!(args_str.contains("virtio-blk-device"));
        assert!(args_str.contains("virtio-net-device"));
        assert!(args_str.contains("isa-serial"));
        assert!(args_str.contains("console.log"));
    }

    #[test]
    fn test_command_line_formatting() {
        let cmd = QemuCommand {
            binary: "qemu-system-x86_64".into(),
            args: vec![
                "-M".into(),
                "microvm,rtc=on".into(),
                "-m".into(),
                "512".into(),
            ],
        };

        let line = cmd.command_line();
        assert!(line.starts_with("qemu-system-x86_64"));
        // Args with commas should be quoted
        assert!(line.contains("'microvm,rtc=on'"));
    }

    #[test]
    fn test_path_helpers() {
        let base = Path::new("/run/agentiso");
        assert_eq!(
            workspace_run_dir(base, "abcd1234"),
            PathBuf::from("/run/agentiso/abcd1234")
        );
        assert_eq!(
            qmp_socket_path(base, "abcd1234"),
            PathBuf::from("/run/agentiso/abcd1234/qmp.sock")
        );
    }

    #[test]
    fn test_default_vm_config() {
        let config = VmConfig::default();
        assert_eq!(config.vcpus, 2);
        assert_eq!(config.memory_mb, 512);
        assert_eq!(config.kernel_path, PathBuf::from("/var/lib/agentiso/vmlinuz"));
        assert_eq!(config.initrd_path, Some(PathBuf::from("/var/lib/agentiso/initrd.img")));
        assert_eq!(config.kernel_cmdline, "console=ttyS0 root=/dev/vda rw quiet");
        assert_eq!(config.root_disk, PathBuf::new());
        assert!(config.tap_device.is_empty());
        assert_eq!(config.vsock_cid, 0);
    }

    #[test]
    fn test_build_command_minimal_config() {
        let config = VmConfig {
            vcpus: 1,
            memory_mb: 128,
            kernel_path: PathBuf::from("/boot/vmlinuz"),
            initrd_path: None,
            kernel_cmdline: "quiet".into(),
            init_mode: "openrc".into(),
            root_disk: PathBuf::from("/dev/vda"),
            tap_device: "tap0".into(),
            vsock_cid: 100,
            qmp_socket: PathBuf::from("/tmp/qmp.sock"),
            run_dir: PathBuf::from("/tmp"),
        };

        let cmd = config.build_command();
        let args_str = cmd.args.join(" ");

        assert!(args_str.contains("-m 128"));
        assert!(args_str.contains("-smp 1"));
        assert!(args_str.contains("guest-cid=100"));
        assert!(args_str.contains("tap0"));
        assert!(args_str.contains("/boot/vmlinuz"));
        // No initrd when initrd_path is None
        assert!(!args_str.contains("-initrd"));
        assert!(args_str.contains("-append quiet"));
    }

    #[test]
    fn test_build_command_arg_pairs() {
        let config = VmConfig {
            vcpus: 2,
            memory_mb: 256,
            kernel_path: PathBuf::from("/vmlinuz"),
            initrd_path: Some(PathBuf::from("/initrd.img")),
            kernel_cmdline: "root=/dev/vda".into(),
            init_mode: "openrc".into(),
            root_disk: PathBuf::from("/dev/zvol/test"),
            tap_device: "tap-test".into(),
            vsock_cid: 5,
            qmp_socket: PathBuf::from("/run/qmp.sock"),
            run_dir: PathBuf::from("/run"),
        };

        let cmd = config.build_command();

        // Verify specific arg pairs appear in order
        let args = &cmd.args;
        let m_idx = args.iter().position(|a| a == "-m").unwrap();
        assert_eq!(args[m_idx + 1], "256");

        let smp_idx = args.iter().position(|a| a == "-smp").unwrap();
        assert_eq!(args[smp_idx + 1], "2");

        let kernel_idx = args.iter().position(|a| a == "-kernel").unwrap();
        assert_eq!(args[kernel_idx + 1], "/vmlinuz");

        let initrd_idx = args.iter().position(|a| a == "-initrd").unwrap();
        assert_eq!(args[initrd_idx + 1], "/initrd.img");

        let append_idx = args.iter().position(|a| a == "-append").unwrap();
        assert_eq!(args[append_idx + 1], "root=/dev/vda");
    }

    #[test]
    fn test_build_command_has_nodefaults() {
        let config = VmConfig::default();
        let cmd = config.build_command();
        assert!(cmd.args.contains(&"-nodefaults".to_string()));
    }

    #[test]
    fn test_build_command_has_host_cpu() {
        let config = VmConfig::default();
        let cmd = config.build_command();
        let cpu_idx = cmd.args.iter().position(|a| a == "-cpu").unwrap();
        assert_eq!(cmd.args[cpu_idx + 1], "host");
    }

    #[test]
    fn test_to_tokio_command() {
        let qemu_cmd = QemuCommand {
            binary: "qemu-system-x86_64".into(),
            args: vec!["-m".into(), "512".into()],
        };

        // Verify it creates a valid tokio Command (we can't inspect internals,
        // but we can confirm it doesn't panic)
        let _cmd = qemu_cmd.to_tokio_command();
    }

    #[test]
    fn test_command_line_spaces_quoted() {
        let cmd = QemuCommand {
            binary: "qemu".into(),
            args: vec!["-append".into(), "console=ttyS0 root=/dev/vda rw".into()],
        };
        let line = cmd.command_line();
        // Args with spaces should be quoted
        assert!(line.contains("'console=ttyS0 root=/dev/vda rw'"));
    }

    #[test]
    fn test_command_line_no_unnecessary_quotes() {
        let cmd = QemuCommand {
            binary: "qemu".into(),
            args: vec!["-m".into(), "512".into(), "-nographic".into()],
        };
        let line = cmd.command_line();
        assert_eq!(line, "qemu -m 512 -nographic");
    }

    #[test]
    fn test_qemu_command_clone() {
        let cmd = QemuCommand {
            binary: "qemu-system-x86_64".into(),
            args: vec!["-m".into(), "1024".into()],
        };
        let cloned = cmd.clone();
        assert_eq!(cmd.binary, cloned.binary);
        assert_eq!(cmd.args, cloned.args);
    }

    #[test]
    fn test_build_command_fast_init_mode() {
        let config = VmConfig {
            init_mode: "fast".into(),
            ..VmConfig::default()
        };
        let cmd = config.build_command();
        let append_idx = cmd.args.iter().position(|a| a == "-append").unwrap();
        let cmdline = &cmd.args[append_idx + 1];
        assert!(cmdline.contains("init=/sbin/init-fast"));
    }

    #[test]
    fn test_build_command_openrc_init_mode() {
        let config = VmConfig {
            init_mode: "openrc".into(),
            ..VmConfig::default()
        };
        let cmd = config.build_command();
        let append_idx = cmd.args.iter().position(|a| a == "-append").unwrap();
        let cmdline = &cmd.args[append_idx + 1];
        assert!(!cmdline.contains("init=/sbin/init-fast"));
    }
}
