# agentiso

QEMU microvm workspace manager for AI agents. Exposes isolated, snapshotable Linux VMs as MCP tools over stdio transport. AI agents create workspaces, execute commands, read and write files, snapshot state, and fork environments — all without Docker or SSH.

## Architecture

- **Host kernel (bzImage) + Alpine Linux rootfs in ZFS zvol** — each workspace is a ZFS clone of the base image, giving instant creation and copy-on-write isolation
- **vsock (AF_VSOCK) for host-guest communication** — the in-VM guest agent (`agentiso-guest`, statically linked musl) listens on vsock port 5000; no SSH, no port forwarding needed for exec or file ops
- **ZFS for CoW snapshots and workspace isolation** — named checkpoints, rollback, and fork/clone are all near-instant ZFS operations at the block level
- **nftables for per-workspace network isolation** — each VM gets its own TAP device on `br-agentiso`; inter-VM traffic is blocked by default; internet access and port forwards are configurable per workspace

## Prerequisites

All of the following must be present on the host before running agentiso.

**KVM access**

```bash
ls -la /dev/kvm
# Should show crw-rw---- with kvm group
# If not accessible: sudo adduser $USER kvm && newgrp kvm
```

**QEMU**

```bash
sudo apt install qemu-system-x86_64
```

**ZFS**

```bash
sudo apt install zfsutils-linux
```

**nftables**

```bash
sudo apt install nftables
```

**Bridge: `br-agentiso` at `10.42.0.1/16`**

```bash
sudo ip link add br-agentiso type bridge
sudo ip addr add 10.42.0.1/16 dev br-agentiso
sudo ip link set br-agentiso up
# To persist across reboots, add to /etc/network/interfaces or use netplan
```

**Rust toolchain**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

**musl target (for guest agent)**

```bash
rustup target add x86_64-unknown-linux-musl
# Also needs the musl linker:
sudo apt install musl-tools
```

## Installation

```bash
# 1. Clone the repository
git clone https://github.com/example/agentiso && cd agentiso

# 2. Build the host binary
cargo build --release

# 3. Build the guest agent (static musl binary baked into Alpine image)
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# 4. Set up the environment
#    This script: copies the host kernel+initrd, creates ZFS dataset layout,
#    downloads Alpine minirootfs, installs guest agent, and creates the
#    base image zvol with an @latest snapshot.
sudo ./scripts/setup-e2e.sh

# 5. Copy and edit the config
cp config.toml /etc/agentiso/config.toml   # or use it in place for dev
# Edit zfs_pool, kernel_path, etc. as needed for your environment
```

## Running

**Verify prerequisites:**

```bash
./target/release/agentiso check
# Or with a custom config:
./target/release/agentiso check --config config.toml
```

Checks KVM, QEMU, ZFS, nftables, kernel, initrd, ZFS pool, base image, bridge, and runtime directories. Exits 0 if all pass, 1 if any fail.

**Show workspace state (while server is running):**

```bash
./target/release/agentiso status
./target/release/agentiso status --config config.toml
```

Reads the state file and prints a table of all workspaces with their state, IP, and snapshot count.

**As an MCP server (stdio transport, for Claude Code):**

```bash
./target/release/agentiso serve --config config.toml
```

The server reads MCP protocol from stdin and writes to stdout. It is normally launched by the MCP client (Claude Code), not directly.

**As a system daemon:**

```bash
# Install binary, config, and service (run as root)
sudo ./deploy/install.sh
sudo systemctl enable --now agentiso
```

**View logs:**

```bash
sudo journalctl -u agentiso -f
```

**Run e2e tests (requires setup-e2e.sh to have been run first):**

```bash
sudo ./scripts/e2e-test.sh
```

## Claude Code Integration

Add agentiso as an MCP server in your Claude Code config. The config file location depends on your platform:

- **Linux**: `~/.config/claude-code/claude_desktop_config.json`
- **macOS**: `~/Library/Application Support/claude-code/claude_desktop_config.json`

Add or merge this into that file:

```json
{
  "mcpServers": {
    "agentiso": {
      "command": "/usr/local/bin/agentiso",
      "args": ["serve", "--config", "/etc/agentiso/config.toml"],
      "env": {}
    }
  }
}
```

The `command` must be an absolute path. Adjust `--config` to point to your actual config file. After adding this, restart Claude Code.

See `deploy/README-claude-code.md` for full setup instructions and `deploy/claude-code-mcp.json` for the exact snippet.

## Configuration

Configuration is a TOML file. All sections and fields are optional — unset fields use the defaults shown below.

### `[storage]`

| Key | Default | Description |
|-----|---------|-------------|
| `zfs_pool` | `"agentiso"` | ZFS pool name |
| `dataset_prefix` | `"agentiso"` | Dataset prefix under the pool |
| `base_image` | `"alpine-dev"` | Base image dataset name (under `pool/prefix/base/`) |
| `base_snapshot` | `"latest"` | Snapshot on the base image to clone workspaces from |

### `[network]`

| Key | Default | Description |
|-----|---------|-------------|
| `bridge_name` | `"br-agentiso"` | Bridge device name |
| `gateway_ip` | `"10.42.0.1"` | Host-side gateway IP on the bridge |
| `subnet_prefix` | `16` | Subnet prefix length |
| `default_allow_internet` | `false` | Allow outbound internet for new workspaces |
| `default_allow_inter_vm` | `false` | Allow traffic between workspace VMs |

### `[vm]`

| Key | Default | Description |
|-----|---------|-------------|
| `kernel_path` | `"/var/lib/agentiso/vmlinuz"` | Path to bzImage kernel |
| `initrd_path` | `"/var/lib/agentiso/initrd.img"` | Path to initramfs (needed for virtio modules) |
| `qemu_binary` | `"qemu-system-x86_64"` | QEMU binary path or name |
| `run_dir` | `"/run/agentiso"` | Runtime directory for QMP sockets |
| `kernel_append` | `"console=ttyS0 root=/dev/vda rw quiet"` | Kernel boot arguments |
| `vsock_cid_start` | `100` | Starting vsock CID (incremented per workspace) |
| `guest_agent_port` | `5000` | vsock port the guest agent listens on |
| `boot_timeout_secs` | `30` | Timeout waiting for guest agent readiness |
| `init_mode` | `"openrc"` | Boot mode: `"openrc"` or `"fast"` |

### `[resources]`

| Key | Default | Description |
|-----|---------|-------------|
| `default_vcpus` | `2` | Default vCPUs per workspace |
| `default_memory_mb` | `512` | Default memory in MB |
| `default_disk_gb` | `10` | Default disk size in GB |
| `max_vcpus` | `8` | Maximum vCPUs a workspace can request |
| `max_memory_mb` | `8192` | Maximum memory in MB |
| `max_disk_gb` | `100` | Maximum disk in GB |
| `max_workspaces` | `20` | Maximum concurrent workspaces |

### `[pool]`

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `false` | Enable the warm VM pool (pre-booted VMs ready to assign) |
| `min_size` | `2` | Minimum VMs to keep in the pool |
| `max_size` | `10` | Maximum VMs in the pool |
| `target_free` | `3` | Target number of ready (unassigned) VMs |
| `max_memory_mb` | `8192` | Total memory budget for pool VMs in MB |

### `[server]`

| Key | Default | Description |
|-----|---------|-------------|
| `state_file` | `"/var/lib/agentiso/state.json"` | Path for persistent workspace state |
| `state_persist_interval_secs` | `30` | How often to write state to disk |
| `transfer_dir` | `"/var/lib/agentiso/transfers"` | Allowed directory for host-side file transfers |

## Available MCP Tools

### Workspace lifecycle

| Tool | Description |
|------|-------------|
| `workspace_create` | Create and start a new isolated workspace VM |
| `workspace_destroy` | Stop and permanently destroy a workspace and all its storage |
| `workspace_start` | Boot a stopped workspace VM |
| `workspace_stop` | Gracefully stop a running workspace VM |
| `workspace_list` | List workspaces owned by this session, with optional state filter |
| `workspace_info` | Get full details: resources, network config, snapshot tree |

### Execution

| Tool | Description |
|------|-------------|
| `exec` | Execute a shell command inside a running workspace; returns stdout, stderr, exit code |
| `file_write` | Write a file into a running workspace VM |
| `file_read` | Read a file from a running workspace VM |
| `file_upload` | Upload a file from the host filesystem into a workspace VM |
| `file_download` | Download a file from a workspace VM to the host filesystem |

### Snapshots

| Tool | Description |
|------|-------------|
| `snapshot_create` | Create a named checkpoint of a workspace's disk state |
| `snapshot_restore` | Roll a workspace back to a previously created snapshot |
| `snapshot_list` | List all snapshots for a workspace (with parent tree) |
| `snapshot_delete` | Delete a named snapshot |
| `workspace_fork` | Clone a new independent workspace from an existing snapshot |

### Networking

| Tool | Description |
|------|-------------|
| `port_forward` | Forward a host port to a guest port; host port auto-assigned if omitted |
| `port_forward_remove` | Remove a port forwarding rule |
| `workspace_ip` | Get the IP address of a workspace VM |
| `network_policy` | Set internet access, inter-VM communication, and allowed ports |

## Troubleshooting

**"KVM not accessible" or permission denied on `/dev/kvm`**

```bash
sudo adduser $USER kvm
newgrp kvm
# Or run agentiso as root if KVM group approach doesn't work
```

**"Bridge not found" / TAP creation fails**

```bash
sudo ip link add br-agentiso type bridge
sudo ip addr add 10.42.0.1/16 dev br-agentiso
sudo ip link set br-agentiso up
```

**"ZFS pool not found" or base image missing**

Run the setup script which creates the ZFS layout and base image:

```bash
sudo ./scripts/setup-e2e.sh
```

**"Guest agent not reachable" / vsock connection refused**

The vsock kernel module may not be loaded inside the guest:

```bash
# Check host vsock support
lsmod | grep vsock
sudo modprobe vhost_vsock

# Check dmesg for vsock errors after VM boot
dmesg | grep vsock
```

**VM boot timeout**

QEMU logs are written to stderr and captured in the journal when running as a daemon. For manual debugging:

```bash
# Run with visible console output
RUST_LOG=debug ./target/release/agentiso serve --config config.toml
```

The QEMU process stderr goes to the agentiso process's stderr. When running as a systemd service, view it with `journalctl -u agentiso`.

**Unit tests (no root required)**

```bash
cargo test
```

**E2E tests (root required, setup-e2e.sh must have been run first)**

```bash
sudo ./scripts/e2e-test.sh
```
