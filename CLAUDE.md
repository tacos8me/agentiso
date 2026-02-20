# agentiso

QEMU microvm workspace manager for AI agents, exposed via MCP tools.

## Tech Stack

- **Language**: Rust (2021 edition)
- **Async**: tokio
- **Serialization**: serde + serde_json
- **MCP**: rmcp crate (Rust MCP SDK) with stdio transport
- **VM**: QEMU `-M microvm` with host kernel (bzImage + initrd)
- **Guest communication**: vsock (virtio-socket), not SSH
- **Storage**: ZFS zvols for workspaces, snapshots, and clones
- **Networking**: TAP devices on `br-agentiso` bridge, nftables for isolation
- **Guest OS**: Alpine Linux with dev tools pre-installed
- **Guest agent**: Separate Rust binary (`agentiso-guest`), statically linked with musl

## Project Structure

- `src/` — Main agentiso binary (MCP server + VM manager)
  - `src/mcp/` — MCP server, tool definitions, auth, vault tools, team tools
  - `src/mcp/vault.rs` — VaultManager for Obsidian-style markdown knowledge base
  - `src/mcp/team_tools.rs` — Team MCP tool handler (create/destroy/status/list/message/receive)
  - `src/mcp/git_tools.rs` — Git MCP tool handlers (clone, status, commit, push, diff)
  - `src/team/` — Team lifecycle (TeamManager, AgentCard, RoleDef, TaskBoard, MessageRelay)
  - `src/vm/` — QEMU process management, QMP client, vsock
  - `src/storage/` — ZFS operations (snapshot, clone, destroy)
  - `src/network/` — TAP/bridge setup, nftables rules, IP allocation
  - `src/workspace/` — Workspace lifecycle state machine, snapshot tree, orchestration
  - `src/guest/` — Guest agent protocol types (re-exports from `agentiso-protocol`)
- `protocol/` — Shared `agentiso-protocol` crate (protocol types used by both host and guest agent)
- `guest-agent/` — Separate crate for the in-VM guest agent binary
- `images/` — Scripts to build Alpine base image and custom kernel
- `tests/` — Unit and integration tests

## Conventions

- Use `anyhow` for error handling in binaries, `thiserror` for library-style error types
- All async code uses tokio runtime
- State is managed in-memory with periodic persistence to a JSON state file
- ZFS operations shell out to `zfs`/`zpool` CLI commands (no libzfs bindings)
- Network operations shell out to `ip` and `nft` commands
- QEMU management via QMP JSON protocol over Unix socket
- Guest agent protocol: length-prefixed (4-byte big-endian) JSON messages over vsock
- Workspace IDs are UUIDs, shortened to first 8 chars for TAP device names and paths
- IP allocation is sequential from 10.99.0.0/16 pool

## Host Environment

- **ZFS pool**: `agentiso` on `/mnt/nvme-2` (3TB)
- **Dataset layout**: `agentiso/agentiso/{base,workspaces,forks}`
- **QEMU**: `qemu-system-x86_64` via apt
- **KVM**: `/dev/kvm` available
- **Kernel**: host bzImage at `/var/lib/agentiso/vmlinuz` + initrd at `/var/lib/agentiso/initrd.img`
- **Bridge**: `br-agentiso` at `10.99.0.1/16`

## Build & Run

```bash
# Build main binary (agentiso-protocol crate builds automatically as a workspace dependency)
cargo build --release

# Build guest agent (musl static; also pulls in agentiso-protocol)
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# One-shot environment setup (ZFS pool, bridge, kernel, config, verification)
sudo ./target/release/agentiso init

# Or manual setup (equivalent to agentiso init)
sudo ./scripts/setup-e2e.sh

# Run as MCP server (stdio)
./target/release/agentiso serve

# Run with config (see config.example.toml for all options)
./target/release/agentiso serve --config config.toml
```

## Test

```bash
# Unit + integration tests (no root needed) — 806 tests
cargo test

# E2E test (needs root for QEMU/KVM/TAP/ZFS) — 19 steps
# Requires setup-e2e.sh to have been run first
sudo ./scripts/e2e-test.sh

# MCP integration test (needs root) — 95 steps (full tool coverage incl. Phases 5-6)
sudo ./scripts/test-mcp-integration.sh
```

## Swarm Team

This project is built and maintained by a 5-agent swarm. To reactivate for further work, spawn a team named `agentiso-swarm` with these members:

| Agent name | Type | Scope | Files owned |
|------------|------|-------|-------------|
| `guest-agent` | general-purpose | Protocol types, guest binary, image scripts | `protocol/`, `agentiso/src/guest/`, `guest-agent/`, `images/` |
| `vm-engine` | general-purpose | QEMU microvm, QMP, vsock | `agentiso/src/vm/` |
| `storage-net` | general-purpose | ZFS storage, TAP/bridge, nftables | `agentiso/src/storage/`, `agentiso/src/network/` |
| `workspace-core` | general-purpose | Lifecycle orchestration, config, main | `agentiso/src/workspace/`, `agentiso/src/config.rs`, `agentiso/src/main.rs` |
| `mcp-server` | general-purpose | MCP server, tools, auth | `agentiso/src/mcp/` |

Dependency chain: `guest-agent` -> `vm-engine` + `storage-net` (parallel) -> `workspace-core` -> `mcp-server`

See `AGENTS.md` for full role descriptions and shared interfaces.

## Current Status

**806 unit tests passing** (717 agentiso + 56 protocol + 33 guest), 4 ignored, 0 warnings.

**Core platform (complete)**:
- 95/95 MCP integration test steps passing (full tool coverage including team lifecycle + task board + messaging + workspace_merge + nested teams)
- 10/10 state persistence tests passing
- Guest agent: vsock listener, exec, file ops, process group isolation, hardened (32 MiB limit, hostname/IP validation, exec timeout kill, ENV/BASH_ENV blocklist, output truncation)
- 31 MCP tools with name-or-UUID workspace lookup and contextual error messages
- CLI: `check`, `status`, `logs`, `dashboard` (ratatui TUI), `team-status`
- Deploy: systemd unit, install script, OpenCode MCP config

**Production hardening (P0-P1 sprint, complete)**:
- Orphan reconciliation on server restart (stale QEMU/TAP/ZFS cleanup)
- Session token persistence across restart
- Per-workspace ZFS volsize quotas with pool space hard-fail
- cgroup v2 memory+CPU limits (best-effort, `agentiso.slice`)
- Parallel VM shutdown via JoinSet (11s worst-case vs N*11s)
- Exec timeout default 30s→120s
- `git_clone` MCP tool with URL validation

**Reliability (Waves 4-5)**:
- Per-QMP-command 10s timeout, exponential backoff on connect
- VM crash detection, console log diagnostics on boot failure
- Vsock reconnect for idempotent operations
- ExecKill protocol + `exec_background(action="kill")` MCP tool
- `workspace_logs` MCP tool
- DNS reconfiguration on `network_policy` toggle — guest DNS updated via vsock `ConfigureNetwork` message when internet access is enabled/disabled

**Security**:
- Guest: file size limits, hostname/IP validation, exec timeout kill
- VM: HMP tag sanitization, stderr to log file
- MCP/storage: UTF-8 safe truncation, path traversal prevention, dataset hierarchy guard
- Token-bucket rate limiting (create 5/min, exec 60/min, default 120/min)
- ZFS volsize enforcement on workspace create/fork with atomic quota check
- Per-interface ip_forward (scoped to br-agentiso, not global)
- Init.rs security: SUDO_USER validation, shell escaping, secure tempfile
- Credential redaction in git_push
- PID reuse verification in auto-adopt
- Internet access disabled by default (`default_allow_internet = false`)
- `network_policy` reconfigures guest DNS via vsock when toggling internet access (prevents stale `/etc/resolv.conf`)

**OpenCode integration sprint (complete)**:
- SetEnv guest RPC for secure API key injection (env vars via vsock, never on disk)
- Alpine-opencode base image script (musl binary v1.2.6) + rust/python/node toolchain images
- `workspace_prepare` MCP tool: create golden workspace (clone repo, install deps, snapshot)
- `workspace_fork` with `count` param: fork N workers from snapshot in parallel (JoinSet, 1-20)
- OpenCode run wrapper (`vm/opencode.rs`): exec `opencode run` in VM, parse JSON output
- `agentiso orchestrate` CLI: TOML task file → fork workers → inject keys → run OpenCode → collect results
- Prometheus metrics (`/metrics`) + health endpoint (`/healthz`) via `--metrics-port`
- `set_env` MCP tool for secure API key injection into VMs
- 31 MCP tools total (snapshot, vault, exec_background, port_forward, workspace_fork, file_transfer, workspace_adopt, team bundled; git tools + workspace_merge + exec_parallel + swarm_run added)

**Vault integration (Phase 1, complete)**:
- 1 bundled `vault` MCP tool with 11 sub-actions: read, search, list, write, frontmatter, tags, replace, delete, move, batch_read, stats
- VaultManager in `src/mcp/vault.rs` with path traversal prevention, frontmatter parsing, regex search
- `VaultConfig` in config.toml (`[vault]` section: enabled, path, extensions, exclude_dirs)
- `vault_context` in orchestration TOML: per-task `[[tasks.vault_context]]` with kind="search"/"read"
- Orchestrator resolves vault queries and injects `## Project Knowledge Base` into worker prompts
- Pure Rust implementation (no Node.js, no Obsidian desktop) using `ignore` + `serde_yaml` crates

**Hardening sprint (P0-P3, complete)**:
- 53 new unit tests: guest-agent (24), workspace lifecycle (12), MCP dispatch (15), orchestrate (4)
- Fork concurrency limit (semaphore gates both fork + execution phases)
- Fork error details in TaskResult.stderr (was generic message)
- SIGINT handler in CLI orchestrate (destroys workers before exit)
- Instance lock for `agentiso orchestrate` CLI (prevents concurrent runs)
- Parallel vault_context resolution via JoinSet
- Vault write size limit (10 MiB)
- exec_background kill action: process group isolation + group kill + wait-for-death
- port_forward: nftables `dnat ip to` fix for inet family
- Guest agent security hardening: ENV/BASH_ENV blocklist, output truncation
- State persistence fully tested (10/10 integration tests)

**Runtime defaults**:
- Warm pool enabled by default (size=2), auto-replenish on claim
- Auto-adopt running workspaces on server restart
- Internet access disabled by default (`default_allow_internet = false`, secure by default)
- Rate limiting enabled by default (`[rate_limit]` section in config)

**Vsock connection concurrency (complete)**:
- Fresh vsock connection per long-running exec/opencode call (no longer holds shared mutex)
- Guest agent already accepts multiple concurrent connections (accept loop → tokio::spawn)
- Short-lived operations (file_read, exec_background, ping) use shared connection (fast, no contention)
- Fixes MCP transport-level timeouts (-32001) when exec blocks exec_background/file_read/etc.

**Session resilience (complete)**:
- `workspace_adopt(force=true)`: transfers ownership from dead sessions
- `workspace_prepare`: idempotent — reclaims existing workspace by name, auto-suffix on collision ({name}-2 through {name}-6)
- `configure_workspace`: retry with 500ms delay at all 4 call sites (create, start, fork, warm pool)

**Swarm optimizations (complete)**:
- Compact JSON responses (`to_string` instead of `to_string_pretty` across all 49 MCP tool responses)
- `vault_context` on `swarm_run`: resolve vault queries and inject into worker prompts
- `shared_context` on `swarm_run`: distribute a single context string to all workers
- Parallel set_env + context injection + destroy via JoinSet in swarm_run

**Known limitations**:
- Graceful VM shutdown may time out; falls back to SIGKILL

**Multi-agent teams (Phases 2-3, complete)**:
- `team` MCP tool with 6 actions: create, destroy, status, list, message, receive
- TeamManager: create teams with named roles, each getting its own workspace VM
- AgentCard (A2A-style): stored as JSON in vault at `teams/{name}/cards/{member}.json`
- `team_id` field on Workspace (persisted in state v3) for team membership tracking
- Intra-team nftables rules: team members can communicate, non-members are isolated
- Parallel workspace teardown on team destroy via JoinSet
- Global fork semaphore for concurrency control across team + regular fork operations
- **TaskBoard**: vault-backed task board with YAML frontmatter markdown files
  - Full lifecycle: create, claim, start, complete, fail, release
  - Dependency resolution: `available_tasks`, `is_task_ready`, `dependency_order` (Kahn's topological sort with cycle detection)
  - Auto-generated INDEX.md on every task state change (grouped by status with markers)
  - TaskClaim protocol message for atomic claiming via vsock
- **Inter-Agent Messaging (Phase 4)**:
  - MessageRelay: host-side message router with bounded per-agent inbox (VecDeque), team-scoped agent keys
  - `team` tool `message`/`receive` sub-actions for send, broadcast, and pull-based retrieval
  - Protocol types: TeamMessageEnvelope, TeamMessage/TeamReceive request/response types
  - Dual vsock: port 5001 relay channel on VmHandle (connect_relay), guest relay listener
  - Guest HTTP API: axum on port 8080 with /health, GET/POST /messages endpoints
  - Rate limiting: team_message category (burst 50, 300/min)
  - Content size limit: 256 KiB per message, inbox capacity 100 messages per agent
- 95/95 MCP integration test steps (full tool coverage including team lifecycle, messaging, task board, workspace_merge, nested teams, swarm tools)

**Git merge + Nested teams (Phase 5, complete)**:
- `workspace_merge` MCP tool: merge changes from N source workspaces into a target workspace
  - 3 strategies: `sequential` (git format-patch/am), `branch-per-source` (branch + merge), `cherry-pick` (per-commit cherry-pick)
  - Per-source result reporting with conflict details
- Nested sub-teams: `CreateSubTeam` vsock RPC for creating child teams under a parent
  - Budget inheritance: child teams deduct from parent's VM budget
  - Nesting depth enforcement: configurable `max_nesting_depth` (default 3)
  - Cascade destroy: destroying a parent team destroys all sub-teams
  - Bidirectional nftables rules between parent and child team members
- Config: `max_total_vms` (100), `max_vms_per_team` (20), `max_nesting_depth` (3) in `[resources]`
- Protocol: `CreateSubTeam`/`SubTeamCreated` types with `SubTeamRoleDef`
- Guest agent: `CreateSubTeam` vsock handler (forward to host TeamManager)

**CLI + Observability (Phase 6, complete)**:
- `agentiso team-status <name>` CLI: team overview with member IPs, workspace state, agent status
- Team DAG orchestration: `TeamPlan` with `depends_on` task ordering, Kahn's topological sort, cycle detection via `parse_team_plan()` and `validate_team_dag()`
- Dashboard team pane: press 't' to toggle team view, table with Name/State/Members/Max VMs/Created, detail pane with member list
- Prometheus team metrics: `agentiso_teams_total` (gauge), `agentiso_team_messages_total` (counter), `agentiso_merge_total` (counter by strategy/result), `agentiso_merge_duration_seconds` (histogram)
- 806 unit tests (717 agentiso + 56 protocol + 33 guest)

**A2A agent daemon (complete)**:
- Guest daemon module (`guest-agent/src/daemon.rs`) with semaphore-gated execution (max 4 concurrent tasks)
- Push-based message delivery via relay vsock (port 5001): host pushes `task_assignment` messages to guest
- `PollDaemonResults` protocol for host-side collection of completed task results via main vsock (port 5000)
- `team(action="receive")` includes `daemon_results` array and `daemon_pending_tasks` in response
- Task assignment messages filtered from relay receive (daemon handles them exclusively)
- Code review hardening: semaphore-only concurrency (removed AtomicU32 dual gate), UTF-8 safe truncation, capped non-task message accumulation

## Design Docs

- `docs/plans/2026-02-16-agentiso-design.md` — Core architecture
- `docs/plans/2026-02-17-boot-latency-design.md` — Boot latency optimization design
- `docs/plans/2026-02-17-boot-latency-impl.md` — Boot latency implementation plan
- `docs/plans/2026-02-19-opencode-sprint-design.md` — OpenCode integration sprint
- `docs/plans/2026-02-19-vault-integration-design.md` — Obsidian vault integration (Phase 1)
- `docs/plans/2026-02-19-teams-design.md` — Multi-agent team coordination (Phases 2-4)
- `docs/plans/2026-02-19-teams-impl-plan.md` — Teams implementation plan (41 tasks, 6 phases)
- `docs/plans/2026-02-19-phase4-messaging-plan.md` — Phase 4 inter-agent messaging implementation
- `docs/plans/2026-02-20-phase5-6-plan.md` — Phase 5-6: git merge, nested teams, CLI, observability
- `docs/plans/2026-02-20-swarm-taming-design.md` — Swarm taming: exec_parallel + swarm_run MCP tools
