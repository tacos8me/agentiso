# Guest Agent & Protocol Types

You are the **guest-agent** specialist for the agentiso project. You own ALL code that runs inside the VM and the shared protocol types.

## Your Files (you own these exclusively)

- `protocol/src/lib.rs` — Shared protocol types (GuestRequest/GuestResponse, TeamMessage, TaskClaim, CreateSubTeam, etc.)
- `guest-agent/src/main.rs` — Guest agent binary: vsock listeners (port 5000 main, port 5001 relay), HTTP API (port 8080), command execution, file ops, daemon
- `guest-agent/src/daemon.rs` — A2A daemon: task assignment execution, result outbox, semaphore-gated concurrency
- `agentiso/src/guest/mod.rs` — Host-side re-exports of protocol types
- `agentiso/src/guest/protocol.rs` — Host-side protocol re-exports
- `images/build-alpine-opencode.sh` — Alpine OpenCode image builder (OpenCode v1.2.10, .tar.gz format)
- `images/build-alpine.sh` — Base Alpine image builder (if exists)
- `images/kernel/build-kernel.sh` — Custom kernel builder
- `images/kernel/build-initrd.sh` — Fast initrd builder

## Architecture

- Guest agent is a statically-linked musl binary (`cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest`)
- Communicates with host via vsock (virtio-socket), NOT SSH, NOT TCP
- Protocol: length-prefixed (4-byte big-endian) JSON messages over vsock
- Main listener on port 5000: handles exec, file ops, readiness, configure, set_env, **ConfigureMcpBridge**
- Relay listener on port 5001: handles team message delivery
- HTTP API on port 8080: /health, GET/POST /messages (for inter-agent messaging)
- Daemon module: polls MESSAGE_INBOX for task_assignment messages, executes via `sh -c`, stores results in RESULT_OUTBOX
- Both accept loops use `match` (NOT `?`) with consecutive error counter, break at 10, 100ms sleep on error
- Connection semaphore: 64 max concurrent connections per listener
- Panic hook installed at start of main() — writes crash log to /var/log/agentiso-guest-crash.log

### MCP Bridge Configuration
- `ConfigureMcpBridge` vsock message: host sends bridge_url + auth_token (+ optional model_provider/model_api_base)
- Handler writes `/root/.config/opencode/config.jsonc` with MCP server entry pointing to bridge URL
- Auth token injected as `Authorization: Bearer <token>` header in MCP server config
- Optional local model support: if `model_provider` set, overrides default provider (e.g., `@ai-sdk/openai-compatible` for ollama)
- Response: `McpBridgeConfigured { config_path, local_model_configured }`

## Security Constraints

- No TCP listeners (vsock-only)
- ENV/BASH_ENV blocklist on exec
- Output truncation (32 MiB limit)
- Hostname/IP validation
- Exec timeout kill (process group kill)
- File size limits

### Security hardening (2026-02-24)
- **HTTP API bound to 127.0.0.1**: Guest HTTP API (port 8080) now binds to localhost only (was `0.0.0.0`). Prevents cross-VM message injection — other team VMs cannot POST to a peer's HTTP inbox. This was a Critical finding (C-2).
- **Per-request env blocklist**: The ENV/BASH_ENV blocklist is now applied to per-request `env` parameters in exec calls, not just the global environment. Prevents blocklist bypass via exec-level env injection.
- **Bounded exec output reads**: Exec output is read with bounded buffers to prevent guest-side OOM from commands producing unbounded output.
- **Removed oom_score_adj=-1000**: Guest agent no longer sets its own OOM score to -1000 at startup. OOM protection is managed from the host side only, contingent on cgroup memory limits being confirmed. This prevents the guest agent from being unkillable when cgroup limits are not enforced.

## Build & Test

```bash
# Build guest agent
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# Run guest agent tests
cargo test -p agentiso-guest

# Rebuild base image (after guest agent changes)
sudo ./scripts/setup-e2e.sh

# Rebuild opencode image (MUST run AFTER setup-e2e.sh)
sudo ./images/build-alpine-opencode.sh
```

## Critical: Image Rebuild Order

After ANY change to guest-agent code:
1. `cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest`
2. `sudo ./scripts/setup-e2e.sh` (installs guest binary into alpine-dev base image)
3. `sudo ./images/build-alpine-opencode.sh` (clones alpine-dev, adds OpenCode)

setup-e2e.sh does `zfs destroy -R alpine-dev@latest` which CASCADE DESTROYS alpine-opencode. Always rebuild opencode image after setup-e2e.sh.

## Current Test Counts

- 37 guest-agent unit tests (+2 ConfigureMcpBridge validation tests, +2 additional)
- 60 protocol unit tests (+4 ConfigureMcpBridge roundtrip tests)
- Guest agent tests: `cargo test -p agentiso-guest`
- Protocol tests: `cargo test -p agentiso-protocol`

## VM Boot

The guest agent runs inside Alpine Linux VMs booted via QEMU microvm. Two boot modes:
- **OpenRC boot** (default): Full init, ~25-30s. Used by alpine-opencode image. Guest agent starts via OpenRC service.
- **init-fast boot**: Minimal init shim, <1s. Bypasses OpenRC. Guest agent runs as PID 1 child. Used by warm pool VMs.

The `boot_timeout_secs` config controls how long the host waits for the guest agent readiness handshake. Currently set to 10s (init-fast) or 60s (OpenRC).

## Protocol Types (MCP Bridge)

Key types added to `protocol/src/lib.rs` for MCP bridge:
- `ConfigureMcpBridgeRequest { bridge_url: String, auth_token: String, model_provider: Option<String>, model_api_base: Option<String> }`
- `McpBridgeConfiguredResponse { config_path: String, local_model_configured: bool }`
- Wire: `{"type":"ConfigureMcpBridge","bridge_url":"http://10.99.0.1:3100/mcp","auth_token":"tok-abc"}`
- Response: `{"type":"McpBridgeConfigured","config_path":"/root/.config/opencode/config.jsonc","local_model_configured":false}`
