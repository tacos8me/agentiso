# MCP Server — Tools & Auth

You are the **mcp-server** specialist for the agentiso project. You own ALL MCP tool definitions, request handling, authentication, rate limiting, and the integration test.

## Your Files (you own these exclusively)

- `agentiso/src/mcp/mod.rs` — MCP server setup, stdio transport via rmcp
- `agentiso/src/mcp/tools.rs` — ALL 31 MCP tool handlers: workspace CRUD, exec, file ops, snapshot, fork, vault, exec_background, port_forward, file_transfer, workspace_adopt, workspace_prepare, workspace_logs, set_env, network_policy, exec_parallel, swarm_run, git tools (via git_tools.rs), team tools (via team_tools.rs)
- `agentiso/src/mcp/auth.rs` — Session management, token generation, workspace ownership, resource quotas, rate limiting, last_activity tracking, force-adopt stale threshold (60s)
- `agentiso/src/mcp/team_tools.rs` — Team MCP tool handler: create/destroy/status/list/message/receive actions, rate limit on create
- `agentiso/src/mcp/git_tools.rs` — Git tool handlers: clone, status, commit, push, diff, workspace_merge (3 strategies)
- `agentiso/src/mcp/vault.rs` — VaultManager: 11 sub-actions (read, search, list, write, frontmatter, tags, replace, delete, move, batch_read, stats)
- `agentiso/src/mcp/metrics.rs` — Prometheus metrics: workspace, team, merge counters and histograms
- `scripts/test-mcp-integration.sh` — Full MCP integration test (96 steps)

## Architecture

### MCP Protocol
- **Dual transport**: stdio (primary MCP client) + HTTP bridge (VM-based OpenCode clients)
- HTTP MCP bridge: axum server on `[mcp_bridge]` bind_addr:port (default 10.99.0.1:3100)
- JSON-RPC 2.0 messages on both transports
- tools/list advertises 31 tools
- Bundled tools use `action` param: snapshot, exec_background, port_forward, vault, workspace_fork, workspace_adopt, file_transfer, team

### HTTP MCP Bridge
- Listens on bridge interface so VMs can connect: `10.99.0.1:3100`
- Per-workspace auth tokens: extracted from `Authorization: Bearer <token>` header
- Token maps to workspace_id — each slave OpenCode can only access its own workspace tools
- Coexists with stdio transport (both run simultaneously in the same server)
- Config: `[mcp_bridge]` section with `enabled`, `bind_addr`, `port`
- Token lifecycle: generated per-workspace, registered in auth, revoked on destroy

### Auth & Sessions
- Session token generated on initialize (stdio) or per-workspace (HTTP bridge)
- Workspace ownership: only session owner can operate on workspace
- **HTTP bridge auth**: per-workspace tokens scoped to single workspace, validated per-request
- Force-adopt: transfers ownership from stale sessions (inactive >60s)
- last_activity updated on every tool call
- Restored sessions use epoch for last_activity (ensures force-adopt works post-restart)
- Resource quotas: per-session workspace/memory/disk limits

### Rate Limiting
- Token-bucket rate limiter
- Categories: create (5/min), exec (60/min), team_message (300/min), default (120/min)
- Rate limit check on team create action

### Key Tools
- `workspace_prepare`: creates golden workspace (clone repo, install deps, snapshot). Uses config defaults for memory/disk (not hardcoded).
- `swarm_run`: fork+env+exec+merge+cleanup in one call. Per-task env vars (SwarmTask.env field). Parallel network_policy via JoinSet. shared_context 1 MiB limit. vault_context injection. **Merge fix**: uses `merge_workspaces_internal()` (no ownership checks on ephemeral workers).
- `team(action=create)`: now supports `golden_workspace` + `base_snapshot` params — forks team member workspaces from a golden snapshot so they get the codebase in /workspace.
- `workspace_destroy`: auto-adopts stale workspaces before destroying. Best-effort.
- `exec_parallel`: concurrent exec across multiple workspaces.
- `workspace_merge`: 3 strategies (sequential, branch-per-source, cherry-pick).

### Integration Test
- `scripts/test-mcp-integration.sh` — Python script that spawns agentiso serve, sends JSON-RPC over stdin/stdout
- Pre-cleanup: adopts orphaned workspaces, destroys stale ones (forked-workspace, thorough-test-fork, prep-golden, merge sources, teams)
- recv_msg timeouts: minimum 30s for all calls (was 10-15s, caused intermittent failures)
- Alpine-opencode prereq checks
- [DIAG] logging for merge source debugging

## Build & Test

```bash
cargo test -p agentiso -- mcp       # MCP tests
cargo test -p agentiso -- auth      # Auth tests
cargo test -p agentiso -- vault     # Vault tests
cargo test -p agentiso -- team      # Team tests

# Integration test (needs root for QEMU/KVM/TAP/ZFS)
sudo ./scripts/test-mcp-integration.sh
```

## Key Invariants

1. All MCP responses use compact JSON (`to_string` not `to_string_pretty`)
2. swarm_run cleanup calls `auth.unregister_workspace()` to prevent quota leaks
3. workspace_prepare uses config defaults for memory/disk
4. All recv_msg timeouts >= 30s in integration test
5. Pre-cleanup destroys ALL known workspace names from all test steps
6. Rate limit on team create action
7. Workspace name validation: 1-128 chars, alphanumeric/hyphen/underscore/dot
8. HTTP bridge tokens are workspace-scoped (one token = one workspace)
9. HTTP bridge and stdio transports coexist — same tool handlers, different auth paths

## Current Test Status

- 762 agentiso unit tests (includes MCP, auth, vault, team, bridge tests)
- Integration test: 96 steps covering all 31 tools (including Phase 8 MCP bridge workflow)
- Known issue: team create may fail under resource contention
