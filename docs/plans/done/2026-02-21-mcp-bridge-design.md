# MCP Bridge Design — HTTP Transport for VM-Based OpenCode Swarms

**Date**: 2026-02-21
**Status**: Complete
**Sprint**: MCP Bridge (opencode-swarm)

## Problem

When running OpenCode inside workspace VMs via `swarm_run`, the OpenCode instances need access to agentiso's MCP tools (exec, file ops, snapshot, etc.) to be productive. Without MCP tool access, each worker can only execute shell commands — it cannot manage its own workspace, create snapshots, or coordinate with other workers.

## Solution: Dual MCP Transport

Add an HTTP MCP transport alongside the existing stdio transport. OpenCode instances inside VMs connect back to the host over the bridge network (`10.99.0.1:3100`) using per-workspace authentication tokens.

### Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        Host (agentiso)                            │
│                                                                   │
│  ┌─────────────┐    ┌───────────────────────────────────────┐    │
│  │ stdio MCP   │    │ HTTP MCP Bridge (axum, 10.99.0.1:3100)│    │
│  │ (primary    │    │                                        │    │
│  │  client)    │    │  Per-session AgentisoServer             │    │
│  └──────┬──────┘    │  Bearer token → (session_id, ws_id)    │    │
│         │           └──────────────┬────────────────────────┘    │
│         │                          │                              │
│         ▼                          ▼                              │
│  ┌─────────────────────────────────────────┐                     │
│  │       AgentisoServer (tool handlers)     │                     │
│  │  - Same 31 tools on both transports      │                     │
│  │  - Ownership checks enforce scoping      │                     │
│  └─────────────────────────────────────────┘                     │
│                                                                   │
│  ┌──────────┐  vsock   ┌────────────────┐                        │
│  │VmManager │ ───────> │  Guest Agent    │                        │
│  │          │  port    │  (port 5000)    │                        │
│  │configure │  5000    │                 │                        │
│  │_mcp_     │          │ Writes config   │                        │
│  │bridge()  │          │ .jsonc with     │                        │
│  └──────────┘          │ MCP server URL  │                        │
│                        └────────────────┘                        │
│                               │                                   │
│              ┌────────────────┘                                   │
│              ▼                                                    │
│  ┌────────────────────────────────────┐                          │
│  │     Workspace VM (10.99.0.x)       │                          │
│  │                                     │                          │
│  │  OpenCode reads config.jsonc        │                          │
│  │  → connects to 10.99.0.1:3100/mcp  │                          │
│  │  → Bearer token in auth header      │                          │
│  │  → Full MCP tool access (scoped)    │                          │
│  └────────────────────────────────────┘                          │
└──────────────────────────────────────────────────────────────────┘
```

## Data Flow

### swarm_run with mcp_bridge=true

1. **Orchestrator** calls `swarm_run(mcp_bridge=true, ...)`
2. For each worker:
   a. Fork workspace from golden snapshot
   b. Generate per-workspace bridge token via `AuthManager::generate_bridge_token()`
   c. Inject API keys via `set_env`
   d. Send `ConfigureMcpBridge` vsock message with `bridge_url` + `auth_token`
   e. Guest agent writes `/root/.config/opencode/config.jsonc` with MCP server entry
   f. Execute `opencode run` — OpenCode discovers MCP tools from config.jsonc
   g. OpenCode connects to `http://10.99.0.1:3100/mcp` with Bearer token
   h. Each tool call is authenticated and scoped to the worker's own workspace
3. On cleanup: revoke bridge tokens, destroy worker workspaces

## Protocol: ConfigureMcpBridge

### Request (host → guest via vsock port 5000)

```json
{
  "type": "ConfigureMcpBridge",
  "bridge_url": "http://10.99.0.1:3100/mcp",
  "auth_token": "tok-abc123...",
  "model_provider": null,
  "model_api_base": null
}
```

### Response (guest → host)

```json
{
  "type": "McpBridgeConfigured",
  "config_path": "/root/.config/opencode/config.jsonc",
  "local_model_configured": false
}
```

### Protocol types (protocol/src/lib.rs)

- `ConfigureMcpBridgeRequest { bridge_url, auth_token, model_provider?, model_api_base? }`
- `McpBridgeConfiguredResponse { config_path, local_model_configured }`

### Guest handler behavior

The guest agent writes `/root/.config/opencode/config.jsonc` containing:

```jsonc
{
  "provider": "anthropic",
  "mcpServers": {
    "agentiso": {
      "type": "streamable-http",
      "url": "http://10.99.0.1:3100/mcp",
      "headers": {
        "Authorization": "Bearer tok-abc123..."
      }
    }
  }
}
```

If `model_provider` is set (e.g., `@ai-sdk/openai-compatible`), the provider field is overridden for local model support (ollama/vLLM).

## Authentication

- **Token generation**: `AuthManager::generate_bridge_token()` creates a UUID token mapped to `(session_id, workspace_id)`
- **Token validation**: HTTP middleware extracts `Authorization: Bearer <token>`, looks up the mapping
- **Scoping**: Each bridge session gets its own `AgentisoServer` with a unique session ID. The workspace is adopted into that session, so existing ownership checks enforce access control.
- **Revocation**: `AuthManager::revoke_bridge_token()` called on cleanup (both normal and error paths)
- **No shared tokens**: Each workspace gets its own unique token

## Network: nftables Rules

When `[mcp_bridge]` is enabled, per-workspace nftables INPUT rules are added:

```
# Allow VM (10.99.0.x) to connect to host's MCP bridge port
nft add rule inet agentiso input ip saddr 10.99.0.x tcp dport 3100 accept
```

- Rules are INPUT chain (not FORWARD) because VMs talk to the host's own bridge interface
- Added on workspace create, removed on workspace destroy
- Optional: if `ollama_port` is configured, an additional rule allows VM→host on port 11434
- Generated by `generate_mcp_bridge_rules()` in `network/nftables.rs`

## Config: [mcp_bridge] Section

```toml
[mcp_bridge]
enabled = true           # Enable HTTP MCP transport for VM-based OpenCode
bind_addr = "10.99.0.1"  # Listen on bridge interface
port = 3100              # MCP bridge port
# ollama_port = 11434    # Optional: allow VM→host ollama access
```

Default: `enabled = false`, `bind_addr = "10.99.0.1"`, `port = 3100`

Defined as `McpBridgeConfig` in `config.rs`.

## Local Model Support

Optional fields in `ConfigureMcpBridge` allow using local models (ollama, vLLM) instead of cloud APIs:

- `model_provider`: e.g., `"@ai-sdk/openai-compatible"` for ollama
- `model_api_base`: e.g., `"http://10.99.0.1:11434/v1"`

When set, the guest handler overrides the default provider in config.jsonc. Combined with `ollama_port` in the nftables config, this enables fully local (no internet) swarm execution.

## Files Changed

| File | Changes |
|------|---------|
| `agentiso/src/mcp/bridge.rs` | New: HTTP MCP bridge server (axum + rmcp Streamable HTTP) |
| `agentiso/src/mcp/mod.rs` | Export bridge module |
| `agentiso/src/mcp/auth.rs` | generate_bridge_token, revoke_bridge_token, token→workspace mapping |
| `agentiso/src/mcp/tools.rs` | swarm_run mcp_bridge=true path |
| `agentiso/src/config.rs` | McpBridgeConfig struct + tests |
| `agentiso/src/main.rs` | start_bridge() call when config enabled |
| `agentiso/src/vm/mod.rs` | VmManager::configure_mcp_bridge() |
| `agentiso/src/vm/vsock.rs` | VsockClient::configure_mcp_bridge() |
| `agentiso/src/workspace/mod.rs` | WorkspaceManager::configure_mcp_bridge() |
| `agentiso/src/network/nftables.rs` | generate_mcp_bridge_rules, apply_mcp_bridge_rules |
| `agentiso/src/storage/mod.rs` | (no changes for bridge) |
| `protocol/src/lib.rs` | ConfigureMcpBridgeRequest, McpBridgeConfiguredResponse |
| `guest-agent/src/main.rs` | ConfigureMcpBridge handler, writes config.jsonc |
| `scripts/test-mcp-integration.sh` | Phase 8: steps 92-100 (bridge endpoint, auth, swarm_run bridge) |
| `config.toml` | `[mcp_bridge]` section |

## Test Coverage

- 4 protocol roundtrip tests (ConfigureMcpBridge request/response serialization)
- 2 guest-agent tests (ConfigureMcpBridge validation)
- 3 config.rs tests (defaults, TOML parsing, serde roundtrip)
- nftables generate_mcp_bridge_rules test
- orchestrate.rs mcp_bridge plan parsing tests (3)
- Integration test Phase 8: steps 92-100
  - Step 92: HTTP endpoint listening
  - Step 93: Unauthenticated request handling
  - Step 94: Invalid token rejection
  - Step 95: swarm_run with mcp_bridge=true (end-to-end)
  - Step 96: ConfigureMcpBridge writes config.jsonc in workspace
  - Step 97-100: Cleanup
