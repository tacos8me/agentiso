# Troubleshooting

Common issues, fixes, and diagnostic commands for agentiso.

## Prerequisites Check

Run the built-in prerequisite checker before investigating issues:

```bash
./target/release/agentiso check --config config.toml
```

This verifies KVM, QEMU, ZFS, nftables, kernel, initrd, ZFS pool, base image,
bridge, and runtime directories. Exit code 0 means all checks pass.

## Common Issues

### KVM access / permission denied

**Symptom**: `KVM not accessible` or `Permission denied` on `/dev/kvm`.

```bash
# Check KVM device permissions
ls -la /dev/kvm
# Should show crw-rw---- with kvm group

# Fix: add your user to the kvm group
sudo adduser $USER kvm
newgrp kvm

# If running as a systemd service, ensure the service user is in the kvm group.
# Alternatively, run agentiso as root.
```

If running inside a VM (nested virtualization), ensure the hypervisor exposes
KVM to the guest. Without KVM, QEMU falls back to TCG (software emulation)
which is significantly slower and may cause boot timeouts.

### Bridge not found / TAP creation fails

**Symptom**: `Bridge br-agentiso not found` or `Cannot create TAP device`.

The bridge must exist before the daemon starts. It is not created automatically.

```bash
# Create the bridge
sudo ip link add br-agentiso type bridge
sudo ip addr add 10.99.0.1/16 dev br-agentiso
sudo ip link set br-agentiso up

# Verify
ip link show br-agentiso
ip addr show br-agentiso
```

To persist across reboots, add to `/etc/network/interfaces` or use a netplan
config. The gateway IP must match `network.gateway_ip` in your config (default
`10.99.0.1`).

TAP creation requires `CAP_NET_ADMIN` or root. If running as a non-root user,
the binary needs the capability: `sudo setcap cap_net_admin+ep ./target/release/agentiso`.

### ZFS pool not found / base image missing

**Symptom**: `ZFS pool 'agentiso' not found` or `Base image dataset not found`.

```bash
# Check if the pool exists
zpool list

# Check dataset layout
zfs list -r agentiso/agentiso

# If missing, run the setup script
sudo ./scripts/setup-e2e.sh
```

The setup script creates the ZFS dataset hierarchy, downloads the Alpine
minirootfs, installs the guest agent binary, and creates the base image zvol
with an `@latest` snapshot. The pool name and prefix must match your
`config.toml` settings (`storage.zfs_pool` and `storage.dataset_prefix`).

### Guest agent not reachable / vsock issues

**Symptom**: `Guest agent not reachable` or `vsock connection refused` or
`workspace creation timed out waiting for guest agent`.

**Check host vsock kernel modules:**

```bash
lsmod | grep vsock
# Should show vhost_vsock and related modules

# If missing:
sudo modprobe vhost_vsock
```

**Check for vsock device:**

```bash
ls -la /dev/vhost-vsock
# Must exist and be accessible to the agentiso process
```

**Check guest agent is in the base image:**

The guest agent binary (`agentiso-guest`) must be baked into the Alpine base
image and started via OpenRC on boot. Rebuild the base image if it is missing:

```bash
# Rebuild guest agent (musl static binary)
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# Rebuild base image
sudo ./scripts/setup-e2e.sh
```

**Double CIDR bug (historical):** If guest network configuration fails with
errors about invalid IP addresses, ensure the host is passing the bare IP
(e.g., `10.99.0.2`) to `ConfigureWorkspace`, not `10.99.0.2/16`. The guest
agent appends the prefix length itself.

### VM boot timeout

**Symptom**: `Workspace creation timed out` after 30 seconds (configurable via
`vm.boot_timeout_secs`).

**Check QEMU process output:**

```bash
# Run with debug logging to see QEMU stderr
RUST_LOG=debug ./target/release/agentiso serve --config config.toml

# If running as systemd service, check the journal
sudo journalctl -u agentiso -f
```

**Check console logs for a specific workspace:**

```bash
./target/release/agentiso logs <workspace-id> --config config.toml
```

**Common causes:**

- Kernel not found at `vm.kernel_path` (default `/var/lib/agentiso/vmlinuz`)
- Initrd not found at `vm.initrd_path` (default `/var/lib/agentiso/initrd.img`)
- Firecracker kernels crash under QEMU microvm -- use the host bzImage + initrd instead
- QEMU microvm requires MMIO device variants (`virtio-blk-device`, `virtio-net-device`, `vhost-vsock-device`), not PCI variants
- Insufficient memory (default 512 MB; Alpine + dev tools need at least 256 MB)
- The `x-option-roms=off` flag in `-M microvm` prevents PVH boot; use `-M microvm,rtc=on` without disabling option ROMs

### Port forwarding errors

**Symptom**: `port_forward` tool fails or forwarded ports are not reachable.

Port forwarding uses nftables DNAT rules. The rules must use `dnat ip to`
(not just `dnat to`) when the nftables table uses the `inet` family:

```
# Correct (inet family):
dnat ip to 10.99.0.2:8080

# Incorrect (will silently fail):
dnat to 10.99.0.2:8080
```

Also verify:

- The workspace VM is running (`workspace_info` shows state `running`)
- The guest service is actually listening on the forwarded port
- IP forwarding is enabled on the host: `sysctl net.ipv4.ip_forward` should be `1`
- Internet access is enabled for the workspace if forwarding external traffic

### exec_kill not working

**Symptom**: `exec_kill` returns success but the process is still running, or
child processes survive after killing the parent.

The guest agent uses process group isolation for reliable kill. Each background
exec spawns in its own process group via `setsid`. `ExecKill` sends `SIGKILL`
to the negative PID (killing the entire process group), then waits for death.

If processes still escape:

- The process may have called `setsid()` itself, creating a new session
- Check with `exec` running `ps auxf` inside the workspace to see the process tree
- As a last resort, `workspace_stop` + `workspace_start` will kill everything

### Workspaces missing after restart

**Symptom**: Workspaces disappeared or show as stopped after a daemon restart.

The server now **auto-adopts** running workspaces on startup. If the QEMU
process was still alive when the server restarted, the workspace is
automatically re-attached and remains running.

If a workspace was stopped during the restart (e.g., the host rebooted),
it will appear in `stopped` state. Use `workspace_start` to re-boot it.
Session ownership is also restored automatically -- no `workspace_adopt`
call is needed for auto-adopted workspaces.

### Cannot delete snapshot

**Symptom**: `snapshot(action="delete")` fails with an error about dependent clones.

The snapshot has forked workspaces that depend on it. ZFS cannot delete a
snapshot that has active clones. Destroy the forked workspace first, then
delete the snapshot.

To find which workspaces depend on a snapshot, check `workspace_info` on
your forked workspaces -- the `forked_from` field shows the source
snapshot.

### Workspace destroy failed

**Symptom**: `workspace_destroy` returns an error instead of succeeding.

Storage errors during destroy are now reported rather than silently
ignored. Common causes:

- The VM is still running and cannot be stopped (check `workspace_info`)
- The ZFS dataset has dependent clones (destroy the forked workspaces first)
- ZFS pool I/O errors (check `zpool status`)

### State persistence: workspace_adopt_all after restart

**Symptom**: After a daemon restart, the MCP client cannot operate on
previously created workspaces (`not owned by this session`).

Most workspaces are now auto-adopted on startup. If you still see orphaned
workspaces (e.g., the workspace's VM was not running when the server
restarted):

1. Call `workspace_list` to see all workspaces (with `"owned": false`)
2. Call `workspace_adopt_all` to claim all orphaned workspaces into the
   current session
3. Or call `workspace_adopt` with a specific workspace ID

Previously-stopped workspaces remain in `stopped` state after restart.
Use `workspace_start` to re-boot them.

### Build issues: musl target for guest agent

**Symptom**: `cargo build` fails for the guest agent with linker errors.

The guest agent must be statically linked with musl for the Alpine VM:

```bash
# Install the musl target
rustup target add x86_64-unknown-linux-musl

# Install the musl linker
sudo apt install musl-tools

# Build
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest
```

If you get OpenSSL-related errors, the guest agent should not depend on
OpenSSL. Check `guest-agent/Cargo.toml` for unexpected dependencies.

### cgroup errors on startup

**Symptom**: Warnings about cgroup v2 limits failing to apply.

agentiso attempts best-effort cgroup v2 memory+CPU limits under
`agentiso.slice`. These are non-fatal -- the daemon continues without
resource isolation if cgroups are not available.

```bash
# Check cgroup v2 availability
mount | grep cgroup2

# Check if the slice exists
systemctl status agentiso.slice
```

### Workspace creation hangs

If workspace creation hangs (no timeout, no error), check:

1. ZFS pool has free space: `zpool list`
2. The bridge is up: `ip link show br-agentiso`
3. QEMU process actually started: `ps aux | grep qemu`
4. QMP socket was created: `ls /run/agentiso/*/qmp.sock`

## Diagnostic Commands

```bash
# Check all prerequisites
./target/release/agentiso check --config config.toml

# Show workspace table (reads state file)
./target/release/agentiso status --config config.toml

# View console/stderr logs for a workspace
./target/release/agentiso logs <workspace-id> --config config.toml

# Interactive TUI dashboard
./target/release/agentiso dashboard --config config.toml

# Check QEMU processes
ps aux | grep qemu-system-x86_64

# Check TAP devices on the bridge
ip link show master br-agentiso

# Check nftables rules
sudo nft list ruleset

# Check ZFS datasets
zfs list -r agentiso/agentiso

# Check vsock module
lsmod | grep vsock

# Check host daemon logs
sudo journalctl -u agentiso -f
RUST_LOG=debug ./target/release/agentiso serve --config config.toml
```

## Test Commands

### Unit tests (no root required)

```bash
cargo test
```

505 unit tests across the workspace (agentiso + guest-agent + protocol crates).
No KVM, ZFS, or network access needed.

### E2E tests (root required)

```bash
sudo ./scripts/e2e-test.sh
```

26 tests covering ZFS clones, TAP networking, QEMU microvm boot, QMP protocol,
vsock guest agent ping/pong, ZFS snapshots/forks, and QMP shutdown. Requires
`setup-e2e.sh` to have been run first.

### MCP integration tests (root required)

```bash
sudo ./scripts/test-mcp-integration.sh
```

26 steps driving the full MCP server over stdio: create workspace, exec,
file_write, file_read, snapshot (create/list), workspace_info,
workspace_ip, port_forward, network_policy, workspace_fork, exec_kill,
workspace_logs, workspace_adopt, and destroy.

### State persistence tests (root required)

```bash
sudo ./scripts/test-state-persistence.sh
```

10 tests verifying state file write/load, schema versioning, orphan
reconciliation, session token persistence across daemon restart, and
workspace re-adoption.
