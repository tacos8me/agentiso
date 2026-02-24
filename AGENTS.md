# agentiso Team Agents

## Active Sprint: Frontend Dashboard

**Plan**: `docs/plans/2026-02-21-frontend-kanban-design.md`

### Phases 1-3 COMPLETE — Frontend Dashboard Shipped

**Phase 3 agents** (all complete):
| Agent name | Type | Deliverables |
|------------|------|-------------|
| `polish-bundle` | general-purpose | Code splitting (9 chunks, 143KB gzip initial), lazy-loaded views, loading skeletons, font optimization |
| `streaming-ws` | general-purpose | Event-driven WebSocket (10s fallback), SSE exec streaming, Ctrl+C cancellation, live terminal output |
| `polish-ui` | general-purpose | Responsive layout, full ARIA accessibility, custom kanban boards, vault graph filtering, empty states |
| `embed-build` | general-purpose | rust-embed binary embedding, MIME types, cache headers, SPA fallback, build-release.sh script |

### Activation

```
TeamCreate("frontend-phase2")
Task(name="integration-kanban", team_name="frontend-phase2", subagent_type="general-purpose")
Task(name="integration-vault", team_name="frontend-phase2", subagent_type="general-purpose")
Task(name="integration-terminal", team_name="frontend-phase2", subagent_type="general-purpose")
Task(name="integration-realtime", team_name="frontend-phase2", subagent_type="general-purpose")
```

---

## Backend Swarm (maintenance mode)

To reactivate the backend swarm, create a team named `agentiso-swarm` and spawn teammates using the Task tool. Each agent should be `subagent_type: "general-purpose"`.

Agent names:
1. `guest-agent`
2. `vm-engine`
3. `storage-net`
4. `workspace-core`
5. `mcp-server`
6. `doc-weenie`

Dependency chain: `guest-agent` -> `vm-engine` + `storage-net` (parallel) -> `workspace-core` -> `mcp-server`

## Backend Team Structure (5-agent swarm)

### 1. `guest-agent`
**Role**: In-VM Guest Agent & Protocol Types
**Scope**: `protocol/`, `guest-agent/`, `agentiso/src/guest/`, `images/`
**Responsibilities**:
- Guest agent binary (vsock listener on port 5000)
- Command execution inside VM (exec with stdout/stderr/exit code, background jobs, ExecKill by job_id)
- File read/write/upload/download inside VM
- vsock protocol implementation (length-prefixed JSON)
- Readiness handshake with host
- Protocol type definitions in shared `agentiso-protocol` crate (`protocol/`), consumed by both host and guest agent
- Alpine base image build script
- Custom kernel build script

**Key files**: `protocol/src/lib.rs`, `guest-agent/src/main.rs`, `agentiso/src/guest/mod.rs`, `agentiso/src/guest/protocol.rs`, `images/build-alpine.sh`, `images/kernel/build-kernel.sh`

### 2. `vm-engine`
**Role**: VM Manager & QEMU Integration
**Scope**: `agentiso/src/vm/`
**Responsibilities**:
- QEMU microvm process spawning and lifecycle
- QMP (QEMU Machine Protocol) client over Unix socket with per-command 10s timeout and exponential backoff on connect
- VM crash detection (check_vm_alive) and console log diagnostics on boot failure
- vsock connection management to guest agents with reconnect for idempotent operations
- microvm command-line argument generation
- VM resource allocation (vCPUs, memory)
- Process monitoring and cleanup

**Key files**: `agentiso/src/vm/mod.rs`, `agentiso/src/vm/qemu.rs`, `agentiso/src/vm/microvm.rs`, `agentiso/src/vm/vsock.rs`

### 3. `storage-net`
**Role**: Storage (ZFS) & Networking
**Scope**: `agentiso/src/storage/`, `agentiso/src/network/`
**Responsibilities**:
- ZFS zvol creation, snapshot, clone, rollback, destroy operations
- ZFS dataset layout management (base images, workspaces, forks)
- TAP device creation and bridge attachment
- nftables rule generation for per-workspace isolation
- IP allocation from 10.99.0.0/16 pool
- Port forwarding via DNAT rules
- Bridge initialization (`br-agentiso`)

**Key files**: `agentiso/src/storage/mod.rs`, `agentiso/src/storage/zfs.rs`, `agentiso/src/network/mod.rs`, `agentiso/src/network/bridge.rs`, `agentiso/src/network/nftables.rs`, `agentiso/src/network/dhcp.rs`

### 4. `workspace-core`
**Role**: Workspace Lifecycle & Snapshot Management
**Scope**: `agentiso/src/workspace/`, `agentiso/src/config.rs`, `agentiso/src/main.rs`
**Responsibilities**:
- Workspace CRUD operations
- Lifecycle state machine (Stopped -> Running -> Suspended, etc.)
- Snapshot tree management (create, restore, delete, list)
- Fork/clone workspace from snapshot
- State persistence (JSON state file)
- Orchestrating VM, storage, and network managers during lifecycle transitions
- Main binary entry point and CLI argument parsing
- Configuration struct and TOML loading

**Key files**: `agentiso/src/workspace/mod.rs`, `agentiso/src/workspace/snapshot.rs`, `agentiso/src/config.rs`, `agentiso/src/main.rs`

### 5. `mcp-server`
**Role**: MCP Server & Tool Definitions
**Scope**: `agentiso/src/mcp/`
**Responsibilities**:
- MCP server setup (stdio transport via rmcp)
- All MCP tool definitions and JSON schemas (31 tools, including workspace_logs, bundled snapshot/vault/exec_background/port_forward/workspace_fork/file_transfer/workspace_adopt/team tools, orchestration tools, git tools, workspace_merge)
- Tool handler dispatch to workspace manager
- Session-based access controls and ownership enforcement
- Resource quota enforcement
- Request validation and error responses

**Key files**: `agentiso/src/mcp/mod.rs`, `agentiso/src/mcp/tools.rs`, `agentiso/src/mcp/auth.rs`, `agentiso/src/mcp/team_tools.rs`, `agentiso/src/mcp/git_tools.rs`

### 6. `doc-weenie`
**Role**: Documentation Specialist
**Scope**: `CLAUDE.md`, `AGENTS.md`, `.claude/agents/*.md`, `docs/plans/*.md`, memory files
**Responsibilities**:
- Keep CLAUDE.md accurate (test counts, tool counts, feature lists, config references)
- Update agent skill files after every code change
- Create design docs for major features
- Maintain MEMORY.md for cross-session persistence
- Verify claims by reading actual source code before documenting
- Continuous audit: test counts, tool count, integration step count, config sections

**Key files**: `CLAUDE.md`, `AGENTS.md`, `.claude/agents/*.md`, `docs/plans/*.md`, `/home/ian/.claude/projects/-mnt-nvme-1-projects-agentiso/memory/MEMORY.md`

## Dependency Order

```
guest-agent (protocol types) <- vm-engine (vsock client)
storage-net (ZFS + network)  <- workspace-core (orchestration)
vm-engine                    <- workspace-core (orchestration)
workspace-core               <- mcp-server (tool handlers call workspace manager)
```

Build order: `guest-agent` -> `vm-engine` + `storage-net` (parallel) -> `workspace-core` -> `mcp-server`

## Shared Interfaces

All agents should agree on these trait interfaces early:

- `VmManager` struct (vm-engine exposes, workspace-core consumes)
- `StorageManager` struct (storage-net exposes, workspace-core consumes)
- `NetworkManager` struct (storage-net exposes, workspace-core consumes)
- `GuestRequest`/`GuestResponse` types (defined in shared `agentiso-protocol` crate, used by host and guest-agent; `agentiso/src/guest/protocol.rs` re-exports from crate)
- `Workspace` / `Snapshot` structs (workspace-core defines, everyone uses)

## Multi-Agent Teams (Phases 2-4, complete)

**Design doc**: `docs/plans/2026-02-19-teams-design.md`

Phase 2: Agent Cards + Team Lifecycle MCP tools.
Phase 3: Vault-Backed Task Board with dependency resolution.
Phase 4: Inter-Agent Messaging (host relay + guest relay + MCP tools).

| Component | Files | Description |
|-----------|-------|-------------|
| Team module | `agentiso/src/team/mod.rs`, `agentiso/src/team/agent_card.rs` | TeamManager, AgentCard, RoleDef, TeamStatusReport |
| Message relay | `agentiso/src/team/message_relay.rs` | MessageRelay: bounded inbox, team-scoped keys, broadcast |
| Task board | `agentiso/src/team/task_board.rs` | TaskBoard, BoardTask, claim/lifecycle/deps, INDEX.md auto-gen |
| MCP tool | `agentiso/src/mcp/team_tools.rs` | Bundled `team` tool with create/destroy/status/list/message/receive actions |
| Protocol | `protocol/src/lib.rs` | TeamMessage/TeamReceive/TeamMessageEnvelope + TaskClaim types |
| Schema v3 | `agentiso/src/workspace/mod.rs` | `team_id` field on Workspace, TeamState persistence |
| Nftables | `agentiso/src/network/nftables.rs` | Intra-team communication rules |
| Dual vsock | `agentiso/src/vm/mod.rs` | Port 5001 relay channel on VmHandle (connect_relay) |
| Guest relay | `guest-agent/src/main.rs` | Relay listener on port 5001, HTTP API on port 8080 |
| Global fork semaphore | `agentiso/src/workspace/mod.rs` | Concurrency limit for fork operations |

## Git Merge + Nested Teams (Phase 5, complete)

**Design doc**: `docs/plans/2026-02-20-phase5-6-plan.md`

| Component | Files | Description |
|-----------|-------|-------------|
| workspace_merge | `agentiso/src/mcp/git_tools.rs` | 3 strategies: sequential, branch-per-source, cherry-pick |
| Nested teams | `agentiso/src/team/mod.rs` | parent_team, budget inheritance, cascade destroy |
| Nesting rules | `agentiso/src/network/nftables.rs` | Bidirectional parent-child nftables rules |
| Config caps | `agentiso/src/config.rs` | max_total_vms, max_vms_per_team, max_nesting_depth |
| Protocol | `protocol/src/lib.rs` | CreateSubTeam/SubTeamCreated + SubTeamRoleDef |
| Guest handler | `guest-agent/src/main.rs` | CreateSubTeam vsock handler |

## CLI + Observability (Phase 6, complete)

| Component | Files | Description |
|-----------|-------|-------------|
| team-status CLI | `agentiso/src/main.rs` | Team overview with member IPs, workspace state |
| Team DAG orchestrate | `agentiso/src/workspace/orchestrate.rs` | TeamPlan, depends_on ordering, Kahn's topo sort |
| Dashboard team pane | `agentiso/src/dashboard/{data,ui,mod}.rs` | Team table + detail view, 't' toggle |
| Team metrics | `agentiso/src/mcp/metrics.rs` | teams_total, team_messages_total, merge_total, merge_duration |

## MCP Bridge — OpenCode-to-OpenCode Swarm (complete)

**Design doc**: `docs/plans/2026-02-21-mcp-bridge-design.md`

| Component | Files | Description |
|-----------|-------|-------------|
| HTTP MCP transport | `agentiso/src/mcp/mod.rs` | axum server on bridge interface (10.99.0.1:3100) |
| Per-workspace auth | `agentiso/src/mcp/auth.rs` | Token generation, workspace-scoped tool access |
| ConfigureMcpBridge | `protocol/src/lib.rs` | Vsock protocol message: bridge_url + auth_token |
| Guest handler | `guest-agent/src/main.rs` | Writes /root/.config/opencode/config.jsonc |
| VM bridge config | `agentiso/src/vm/mod.rs`, `agentiso/src/vm/vsock.rs` | VmManager.configure_mcp_bridge(), fresh vsock per call |
| nftables rules | `agentiso/src/network/nftables.rs` | INPUT rules for VM→host MCP traffic on bridge port |
| swarm_run bridge | `agentiso/src/workspace/mod.rs` | mcp_bridge=true mode: token gen → ConfigureMcpBridge → exec |
| McpBridgeConfig | `agentiso/src/config.rs` | `[mcp_bridge]` section: enabled, bind_addr, port, ollama_port |
| Integration tests | `scripts/test-mcp-integration.sh` | Phase 8: steps 92-100 (bridge endpoint, auth, swarm_run bridge) |

## Frontend Dashboard (Phases 1-3, complete)

**Design doc**: `docs/plans/2026-02-21-frontend-kanban-design.md`

| Component | Files | Description |
|-----------|-------|-------------|
| Dashboard server | `agentiso/src/dashboard/web/mod.rs` | DashboardState, router, server startup |
| Auth middleware | `agentiso/src/dashboard/web/auth.rs` | Admin token middleware |
| Workspace API | `agentiso/src/dashboard/web/workspaces.rs` | 29 workspace REST handlers + SSE exec streaming |
| Team API | `agentiso/src/dashboard/web/teams.rs` | 14 team REST handlers |
| Vault API | `agentiso/src/dashboard/web/vault.rs` | 11 vault REST handlers |
| System API | `agentiso/src/dashboard/web/system.rs` | 3 system REST handlers |
| Batch API | `agentiso/src/dashboard/web/batch.rs` | 2 batch REST handlers |
| WebSocket | `agentiso/src/dashboard/web/ws.rs` | BroadcastHub + event-driven + 10s fallback polling |
| Embedded assets | `agentiso/src/dashboard/web/embedded.rs` | rust-embed static file serving with MIME types |
| React frontend | `frontend/src/` | 58 source files: kanban, vault, terminal, layout, stores, API clients |

## Security Review (2026-02-24)

A 6-agent security audit was conducted across the full codebase. Report: `docs/security/SECURITY-REVIEW.md`.

**Findings**: 2 Critical, 13 High, 24 Medium, 16 Low, 18 Info — all Critical and High fixed.

### Security Auditors

| Agent name | Type | Scope | Skill card |
|------------|------|-------|------------|
| `sec-network` | security auditor | Network isolation, nftables, bridge, MCP bridge | `.claude/agents/sec-network.md` |
| `sec-vm-boundary` | security auditor | VM boundary, vsock, guest agent, QEMU config | `.claude/agents/sec-vm-boundary.md` |
| `sec-auth-secrets` | security auditor | Auth, tokens, sessions, secrets, credential handling | `.claude/agents/sec-auth-secrets.md` |
| `sec-input-validation` | security auditor | Command injection, path traversal, input validation | `.claude/agents/sec-input-validation.md` |
| `sec-privilege` | security auditor | Privilege escalation, resource abuse, cgroup limits | `.claude/agents/sec-privilege.md` |
| `sec-frontend` | security auditor | Dashboard XSS, CSRF, CORS, WebSocket auth | `.claude/agents/sec-frontend.md` |

### Key fixes by area:
- **Dashboard**: Restrictive CORS, security headers, WebSocket auth, constant-time token comparison, SSE connection limits
- **Network**: Anti-spoofing per-TAP, DNAT scoped to bridge/localhost, input chain policy drop, typed IPs in team rules, reserved port blocklist
- **Guest agent**: HTTP API on 127.0.0.1, per-request env blocklist, bounded exec reads, removed oom_score_adj=-1000
- **Auth**: 256-bit bridge tokens, token logging reduction, bridge tool whitelist, expanded git credential redaction
- **Config**: cgroup_required, config validation, MAX_MESSAGE_SIZE 4MiB, snapshot limit (20/workspace), regex pattern length limit

## Current Status (All phases complete + MCP bridge + frontend dashboard)

**872 unit tests passing** (775 agentiso + 60 protocol + 37 guest), 4 ignored, 0 warnings.
**100 MCP integration test steps** (136 assertions; full lifecycle + team lifecycle + messaging + task board + workspace_merge + nested teams + orchestration tools + MCP bridge).
**31 MCP tools total. 60 REST endpoints + WebSocket.**

**Completed (Phases 1-6 + MCP Bridge)**:
- Full workspace lifecycle: create, destroy, start, stop, snapshot, fork, adopt
- Guest agent: vsock listener, exec, file ops, background jobs, security hardening, CreateSubTeam handler
- 31 MCP tools: workspace, exec, file, snapshot, fork, vault, exec_background, port_forward, file_transfer, workspace_adopt, team (bundled), workspace_merge; git (clone, status, commit, push, diff); workspace_prepare, workspace_logs, set_env, network_policy, exec_parallel, swarm_run
- Team lifecycle: TeamManager (create/destroy/status/list), AgentCard, intra-team nftables rules
- Inter-agent messaging: MessageRelay (host-side), team-scoped agent keys, bounded inbox (100/agent), 256 KiB content limit, direct + broadcast, pull-based receive via MCP, rate limited (300/min)
- Dual vsock relay: port 5001 for message delivery, guest relay listener, guest HTTP API (axum on 8080)
- TaskBoard: vault-backed task board with YAML frontmatter markdown, full lifecycle (create/claim/start/complete/fail/release), Kahn's topological sort for dependency resolution, auto-generated INDEX.md
- Vault integration: 11 sub-actions (read, search, list, write, frontmatter, tags, replace, delete, move, batch_read, stats)
- OpenCode integration: orchestrate CLI, workspace_prepare, workspace_fork with count param
- Production hardening: rate limiting, ZFS quotas, cgroup limits, state persistence, instance lock
- Security: path traversal prevention, credential redaction, scoped ip_forward, ENV blocklist
- Git merge: workspace_merge with sequential/branch-per-source/cherry-pick strategies
- Nested teams: CreateSubTeam, budget inheritance, nesting depth limits, cascade destroy, nested nftables
- CLI: team-status command, team DAG orchestration with depends_on + cycle detection
- Dashboard: team pane with table + detail view, 't' key toggle
- Prometheus: team_created/destroyed, team_messages_total, merge_total, merge_duration_seconds
