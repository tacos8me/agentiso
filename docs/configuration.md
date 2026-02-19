# Configuration Reference

agentiso is configured via a TOML file; all sections and fields are optional and fall back to sensible defaults when omitted.

## Minimal Example

```toml
[storage]
zfs_pool = "agentiso"

[vm]
kernel_path = "/var/lib/agentiso/vmlinuz"
initrd_path = "/var/lib/agentiso/initrd.img"
```

This is enough to get started on a host where the ZFS pool, bridge, and kernel/initrd are already in place. Everything else uses built-in defaults.

## `[server]`

Daemon-level settings: state persistence, transfer directory.

| Key | Default | Description |
|-----|---------|-------------|
| `state_file` | `"/var/lib/agentiso/state.json"` | Path to persist workspace state as JSON. Written periodically and on shutdown; read by CLI commands (`status`, `logs`). |
| `state_persist_interval_secs` | `30` | How often (in seconds) to write in-memory state to the state file. |
| `transfer_dir` | `"/var/lib/agentiso/transfers"` | Allowed directory for host-side file transfers (`file_upload` / `file_download`). All `host_path` values must resolve within this directory. Prevents path-traversal attacks. |

## `[storage]`

ZFS pool and dataset layout.

| Key | Default | Description |
|-----|---------|-------------|
| `zfs_pool` | `"agentiso"` | ZFS pool name. |
| `dataset_prefix` | `"agentiso"` | Dataset prefix under the pool. Full layout: `{zfs_pool}/{dataset_prefix}/base/`, `{zfs_pool}/{dataset_prefix}/workspaces/`, `{zfs_pool}/{dataset_prefix}/forks/`. |
| `base_image` | `"alpine-dev"` | Base image dataset name (under `{pool}/{prefix}/base/`). |
| `base_snapshot` | `"latest"` | Snapshot on the base image that new workspaces are cloned from. |

## `[network]`

Bridge, subnet, and default isolation policy for new workspaces.

| Key | Default | Description |
|-----|---------|-------------|
| `bridge_name` | `"br-agentiso"` | Bridge device name. Must exist before the server starts. |
| `gateway_ip` | `"10.99.0.1"` | Host-side gateway IP on the bridge. This is the default gateway inside each VM. |
| `subnet_prefix` | `16` | Subnet prefix length (e.g. 16 for a /16 network = 65534 addresses). Must be between 8 and 30. |
| `default_allow_internet` | `false` | Whether new workspaces can reach the internet by default. Can be overridden per-workspace via the `network_policy` tool. |
| `default_allow_inter_vm` | `false` | Whether new workspaces can communicate with other VMs by default. Can be overridden per-workspace via the `network_policy` tool. |
| `dns_servers` | `["1.1.1.1", "8.8.8.8"]` | DNS servers written to `/etc/resolv.conf` inside guest VMs. |

## `[vm]`

QEMU process settings, kernel/initrd paths, vsock configuration, and boot behavior.

| Key | Default | Description |
|-----|---------|-------------|
| `kernel_path` | `"/var/lib/agentiso/vmlinuz"` | Path to the kernel image (bzImage or vmlinux). |
| `initrd_path` | `"/var/lib/agentiso/initrd.img"` | Path to the initramfs image. Required when the kernel has virtio drivers compiled as modules rather than built-in. |
| `initrd_fast_path` | _(none)_ | Optional path to a fast initrd image, used when `init_mode = "fast"`. Should be a minimal initramfs that boots directly into the guest agent. |
| `qemu_binary` | `"qemu-system-x86_64"` | QEMU binary path or name. |
| `run_dir` | `"/run/agentiso"` | Runtime directory for per-workspace QMP sockets, PID files, and logs. |
| `kernel_append` | `"console=ttyS0 root=/dev/vda rw quiet"` | Kernel boot arguments passed to QEMU via `-append`. |
| `vsock_cid_start` | `100` | Starting vsock CID. Each new workspace gets the next available CID. Must be >= 3 (0-2 are reserved by the vsock protocol). |
| `guest_agent_port` | `5000` | vsock port the guest agent listens on inside each VM. |
| `boot_timeout_secs` | `30` | Timeout in seconds for the guest agent readiness handshake after VM boot. If the guest agent does not respond within this time, workspace creation fails. |
| `init_mode` | `"openrc"` | Init mode inside the guest VM. `"openrc"` uses standard Alpine OpenRC init (slower, full service manager). `"fast"` uses a minimal init shim that launches the guest agent directly (faster boot). |

## `[resources]`

Default and maximum resource limits for workspaces.

| Key | Default | Description |
|-----|---------|-------------|
| `default_vcpus` | `2` | Default vCPUs allocated to each new workspace. |
| `default_memory_mb` | `512` | Default memory in MB allocated to each new workspace. |
| `default_disk_gb` | `10` | Default disk size in GB for each new workspace zvol. |
| `max_vcpus` | `8` | Maximum vCPUs a single workspace can request. |
| `max_memory_mb` | `8192` | Maximum memory in MB a single workspace can request. |
| `max_disk_gb` | `100` | Maximum disk in GB a single workspace can request. |
| `max_workspaces` | `20` | Maximum number of concurrent workspaces the daemon will manage. |

## `[pool]`

Warm VM pool: pre-boots VMs in the background so workspace creation can skip the boot wait.

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `true` | Enable the warm VM pool. When enabled, the daemon pre-boots VMs in the background so that workspace creation can skip the boot wait. |
| `min_size` | `2` | Minimum number of VMs to keep in the pool at all times. When a VM is claimed from the pool, a replacement is automatically created to maintain the target count. |
| `max_size` | `10` | Maximum number of VMs allowed in the pool. |
| `target_free` | `3` | Target number of free (ready) VMs the pool tries to maintain. The daemon boots new VMs when the free count drops below this. |
| `max_memory_mb` | `8192` | Total memory budget in MB for all pool VMs combined. Prevents the pool from consuming too much host RAM. |

**Pool auto-replenish:** When a warm pool VM is claimed by `workspace_create`, the pool automatically starts booting a replacement VM to maintain the target count. This happens asynchronously in the background.

**Auto-adopt on startup:** When the server starts, it auto-detects and re-adopts any running workspaces from a previous server instance. No manual `workspace_adopt` call is needed for workspaces that were running when the server stopped.

## `[vault]`

Obsidian-style markdown knowledge base accessible via vault MCP tools. When disabled, vault tools are not registered with the MCP server.

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `false` | Enable the `vault` tool (bundled: read, search, list, write, delete, frontmatter, tags, replace, move, batch_read, stats). |
| `path` | `"/mnt/vault"` | Path to the vault root directory on the host filesystem. All vault operations are confined to this directory (path traversal prevented). |
| `extensions` | `["md"]` | File extensions to include in search and list operations. |
| `exclude_dirs` | `[".obsidian", ".trash", ".git"]` | Directories to exclude from search and list operations. Matched by name at any depth in the vault tree. |

## `[rate_limit]`

Token-bucket rate limiting for MCP tool calls. Rate limits are applied per tool category, not per individual tool. Categories group tools by cost:

- **create**: `workspace_create`, `workspace_fork`, `workspace_batch_fork` (VM creation — expensive)
- **exec**: `exec`, `exec_background` (command execution)
- **default**: all other tools (lightweight reads/writes)

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `true` | Enable rate limiting. Set to `false` to disable all rate limits. |
| `create_per_minute` | `5` | Maximum create-category calls per minute (workspace_create, workspace_fork, workspace_batch_fork). |
| `exec_per_minute` | `60` | Maximum exec-category calls per minute (exec, exec_background). |
| `default_per_minute` | `120` | Maximum default-category calls per minute (all other tools). |
