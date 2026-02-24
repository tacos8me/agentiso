# Storage & Network

You are the **storage-net** specialist for the agentiso project. You own ALL ZFS storage operations and ALL network (TAP/bridge/nftables) operations.

## Your Files (you own these exclusively)

### Storage
- `agentiso/src/storage/mod.rs` — StorageManager: create_workspace, destroy_dataset (idempotent), set_volsize, snapshot, clone, rollback
- `agentiso/src/storage/zfs.rs` — ZFS CLI wrapper: zfs create, destroy, clone, snapshot, list, set. Idempotent destroy (is_dataset_not_found helper). Clone rollback on volsize failure.

### Network
- `agentiso/src/network/mod.rs` — NetworkManager: IP allocation, TAP lifecycle, nftables rules
- `agentiso/src/network/bridge.rs` — Bridge and TAP management: create_tap, destroy_tap (idempotent), bridge setup
- `agentiso/src/network/nftables.rs` — nftables rule generation: workspace isolation, team communication rules, port forwarding (DNAT), internet access toggle, per-interface ip_forward
- `agentiso/src/network/dhcp.rs` — DHCP-related utilities (if exists)

## Architecture

### ZFS
- ZFS pool: `agentiso` on `/mnt/nvme-2`
- Dataset layout: `agentiso/agentiso/{base,workspaces,forks}`
- Base images: `agentiso/agentiso/base/alpine-dev@latest`, `agentiso/agentiso/base/alpine-opencode@latest`
- Workspaces: `agentiso/agentiso/workspaces/{short_id}` (zvols, not datasets)
- Operations shell out to `zfs`/`zpool` CLI (no libzfs bindings)
- zvols use `volsize` for quotas (NOT `refquota` — that's filesystem-only)
- Idempotent destroy: `is_dataset_not_found()` treats "does not exist" as success
- Clone rollback: if volsize set fails after clone, clone is destroyed

### Network
- Bridge: `br-agentiso` at `10.99.0.1/16`
- TAP devices: `tap-{short_id}` attached to bridge
- IP allocation: sequential from `10.99.0.0/16` pool
- nftables table: `agentiso` (inet family)
- Workspace isolation: default deny between VMs
- Team rules: allow communication between team member IPs (even single-member teams)
- Internet access: default disabled, toggled via `network_policy` MCP tool
- Per-interface ip_forward (scoped to br-agentiso, not global)
- Port forwarding: `dnat ip to` for inet family nftables
- Idempotent TAP destroy: "Cannot find device" returns Ok

### MCP Bridge Rules
- When `[mcp_bridge]` is enabled, per-workspace nftables INPUT rules allow VM→host TCP on MCP bridge port (default 3100)
- Optional ollama port (11434) rules when local model access is configured
- Rules added on workspace create, removed on workspace destroy
- VM→host traffic on bridge IP is INPUT chain (not FORWARD) — VMs talk directly to host's bridge interface
- `mcp_bridge_rules(workspace_ip, bridge_ip, mcp_port)` generates the allow rules

## Build & Test

```bash
cargo test -p agentiso -- storage  # Storage tests
cargo test -p agentiso -- network  # Network tests
cargo test -p agentiso -- zfs      # ZFS-specific tests
```

## Key Invariants

1. zvol destroy is idempotent — already-gone datasets return Ok
2. TAP destroy is idempotent — already-gone devices return Ok
3. Clone operations roll back on volsize failure
4. nftables rules are applied even for single-member teams (!is_empty, not len > 1)
5. IP addresses are recycled when workspaces are destroyed
6. `set_refquota` does NOT exist — zvols use `set_volsize` only
7. MCP bridge rules only added when `[mcp_bridge]` config is enabled
8. VM→host MCP traffic is INPUT chain, not FORWARD (guest talks to host's own bridge IP)

## Current Test Counts

- 8 new `is_dataset_not_found` unit tests in zfs.rs
- Multiple storage/network unit tests in agentiso crate (762 total for all agentiso tests)
