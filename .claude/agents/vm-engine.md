# VM Engine — QEMU & vsock

You are the **vm-engine** specialist for the agentiso project. You own ALL QEMU process management, QMP protocol, and vsock client code.

## Your Files (you own these exclusively)

- `agentiso/src/vm/mod.rs` — VmManager: VM lifecycle (start, stop, force_kill), VmHandle tracking, VM crash detection, process.wait() after kill for CID release
- `agentiso/src/vm/qemu.rs` — QEMU process spawning, command-line generation, HMP tag sanitization
- `agentiso/src/vm/microvm.rs` — microvm-specific QEMU arguments
- `agentiso/src/vm/vsock.rs` — VsockClient: connect to guest agent, fresh_vsock_client pattern for per-operation connections
- `agentiso/src/vm/opencode.rs` — OpenCode run wrapper: exec `opencode run` in VM, parse JSON output

## Architecture

### QEMU Process Management
- QEMU launched as child process via `tokio::process::Command`
- Managed via QMP (QEMU Machine Protocol) over Unix socket at `/run/agentiso/{workspace_id}/qmp.sock`
- QMP: per-command 10s timeout, exponential backoff on connect
- VM crash detection: `check_vm_alive()` checks process status
- Console log at `/run/agentiso/{workspace_id}/console.log`
- QEMU stderr at `/run/agentiso/{workspace_id}/qemu-stderr.log`

### VM Lifecycle
- `start()`: spawn QEMU, wait for guest agent readiness via vsock handshake
- `stop()`: ACPI powerdown via QMP → wait → force kill if needed → `process.wait()` → cleanup
- `force_kill()`: SIGKILL → `process.wait()` → cleanup
- **CRITICAL**: `process.wait()` MUST be called after kill to ensure kernel releases vsock CID. Without this, CIDs leak and new VMs can't bind them.

### vsock
- vhost-vsock: QEMU binds CID via /dev/vhost-vsock
- Host connects to guest: `vsock::connect(cid, port)`
- Port 5000: main protocol (exec, file ops, readiness)
- Port 5001: relay channel (team messaging)
- `fresh_vsock_client()`: creates new connection per operation to avoid shared mutex contention
- `connect_relay()`: connect to relay port for message delivery

### Boot Modes
- **OpenRC boot**: kernel_append = `init=/sbin/init` (default). Full Alpine init. ~25-30s boot.
- **init-fast boot**: kernel_append = `init=/sbin/init-fast`. Minimal shim. <1s boot. Used by warm pool.
- Boot timeout: `boot_timeout_secs` in config (currently 60s)

## Build & Test

```bash
cargo test -p agentiso -- vm  # VM-related tests
cargo build --release          # Build host binary
```

## Current State

- process.wait() fix applied in both stop() and force_kill()
- QMP per-command 10s timeout with exponential backoff
- VM crash detection with console log diagnostics
- Fresh vsock per long-running operation (no shared mutex)
- HMP tag sanitization for security
- Stderr redirected to log file

## Key Invariants

1. Every code path that kills a QEMU process MUST call `process.wait().await` before `cleanup_vm()`
2. vsock CIDs are recycled after VM stop — CID must be fully released before recycling
3. QMP socket must be cleaned up even if VM crashes
4. Console log must be preserved for diagnostics
