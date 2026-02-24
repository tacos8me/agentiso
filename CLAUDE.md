# agentiso

QEMU microvm workspace manager for AI agents, exposed via MCP tools.

## Frontend Dashboard (Complete)

**Plan**: `docs/plans/2026-02-21-frontend-kanban-design.md`

React 19 + Vite + TailwindCSS 4 dashboard embedded in agentiso binary via rust-embed. Espresso dark theme (#0A0A0A bg, #5C4033 accent). Inter + JetBrains Mono fonts.

**Key features**: Multi-board kanban (workspaces + tasks + custom), Obsidian-like vault browser (Tiptap editor, backlinks, force-graph), interactive xterm.js terminals with grid/split view into any VM, team chat (real-time messaging), workspace detail pane (info + embedded terminal), mosaic split panes, command palette (Ctrl+K), SSE exec streaming, event-driven WebSocket.

**Backend**: 60 REST endpoints + WebSocket on axum (default port 7070). `agentiso/src/dashboard/web/` (9 files). Deps: tower-http, rust-embed, mime_guess, tokio-stream, async-stream.

**Frontend**: `frontend/` directory (65+ source files). React + Zustand + @hello-pangea/dnd + Tiptap + react-force-graph-2d + xterm.js + @xterm/addon-search + lucide-react + react-resizable-panels + @tanstack/react-virtual.

All 3 phases complete + post-ship polish (v1-v3). 872 Rust tests (775 agentiso + 37 guest + 60 protocol), 0 TS errors, Vite build ~114KB gzip initial (14 chunks, code-split).

## Tech Stack

- **Language**: Rust (2021 edition)
- **Async**: tokio
- **Serialization**: serde + serde_json
- **MCP**: rmcp crate (Rust MCP SDK) with stdio + HTTP bridge transports
- **VM**: QEMU `-M microvm` with host kernel (bzImage + initrd)
- **Guest communication**: vsock (virtio-socket), not SSH
- **Storage**: ZFS zvols for workspaces, snapshots, and clones
- **Networking**: TAP devices on `br-agentiso` bridge, nftables for isolation
- **Guest OS**: Alpine Linux with dev tools pre-installed
- **Guest agent**: Separate Rust binary (`agentiso-guest`), statically linked with musl
- **Dashboard backend**: axum REST + WebSocket (60 endpoints, port 7070)
- **Frontend**: React 19 + Vite + TailwindCSS 4 + Zustand, embedded via rust-embed

## Project Structure

- `agentiso/src/` — Main agentiso binary (MCP server + VM manager)
  - `src/mcp/` — MCP server, tool definitions, auth, vault tools, team tools
  - `src/mcp/vault.rs` — VaultManager for Obsidian-style markdown knowledge base
  - `src/mcp/team_tools.rs` — Team MCP tool handler (create/destroy/status/list/message/receive)
  - `src/mcp/git_tools.rs` — Git MCP tool handlers (clone, status, commit, push, diff)
  - `src/team/` — Team lifecycle (TeamManager, AgentCard, RoleDef, TaskBoard, MessageRelay)
  - `src/vm/` — QEMU process management, QMP client, vsock
  - `src/storage/` — ZFS operations (snapshot, clone, destroy)
  - `src/network/` — TAP/bridge setup, nftables rules, IP allocation
  - `src/workspace/` — Workspace lifecycle state machine, snapshot tree, orchestration
  - `src/dashboard/` — Web dashboard server (axum REST + WebSocket)
    - `web/` — 9 Rust files: mod, auth, workspaces, teams, vault, system, batch, ws, embedded
  - `src/guest/` — Guest agent protocol types (re-exports from `agentiso-protocol`)
- `frontend/` — React 19 + Vite + TailwindCSS 4 dashboard UI (65+ source files)
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
# Build frontend (required before cargo build for embedded dashboard)
cd frontend && npm run build && cd ..

# Build main binary (agentiso-protocol crate builds automatically as a workspace dependency)
cargo build --release

# Build guest agent (musl static; also pulls in agentiso-protocol)
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# One-shot environment setup (ZFS pool, bridge, kernel, config, verification)
sudo ./target/release/agentiso init

# Or manual setup (equivalent to agentiso init)
sudo ./scripts/setup-e2e.sh

# Run as MCP server (stdio) — when stdin is piped from an MCP client
./target/release/agentiso serve

# Run from terminal (TTY) — shows ASCII banner + status, Ctrl+C to stop
sudo ./target/release/agentiso serve --config config.toml

# Run with config (see config.example.toml for all options)
./target/release/agentiso serve --config config.toml
```

## Test

```bash
# Unit + integration tests (no root needed) — 872 tests
cargo test

# E2E test (needs root for QEMU/KVM/TAP/ZFS) — 9 sections
# Requires setup-e2e.sh to have been run first
sudo ./scripts/e2e-test.sh

# MCP integration test (needs root) — 100 steps, 136 assertions (full tool coverage incl. Phases 5-8)
sudo ./scripts/test-mcp-integration.sh

# Frontend type check
cd frontend && npx tsc --noEmit

# Frontend build
cd frontend && npx vite build
```

## Swarm Team

This project is built and maintained by a 6-agent swarm. To reactivate for further work, spawn a team named `agentiso-swarm` with these members:

| Agent name | Type | Scope | Files owned |
|------------|------|-------|-------------|
| `guest-agent` | general-purpose | Protocol types, guest binary, image scripts | `protocol/`, `agentiso/src/guest/`, `guest-agent/`, `images/` |
| `vm-engine` | general-purpose | QEMU microvm, QMP, vsock | `agentiso/src/vm/` |
| `storage-net` | general-purpose | ZFS storage, TAP/bridge, nftables | `agentiso/src/storage/`, `agentiso/src/network/` |
| `workspace-core` | general-purpose | Lifecycle orchestration, config, main | `agentiso/src/workspace/`, `agentiso/src/config.rs`, `agentiso/src/main.rs` |
| `mcp-server` | general-purpose | MCP server, tools, auth | `agentiso/src/mcp/` |
| `doc-weenie` | general-purpose | Documentation, skill files, CLAUDE.md, memory | `CLAUDE.md`, `AGENTS.md`, `.claude/agents/*.md`, `docs/`, memory files |

Dependency chain: `guest-agent` -> `vm-engine` + `storage-net` (parallel) -> `workspace-core` -> `mcp-server`
Documentation: `doc-weenie` runs continuously, updating docs after every agent's changes

See `AGENTS.md` for full role descriptions and shared interfaces.

## Current Status

**872 unit tests passing** (775 agentiso + 60 protocol + 37 guest), 4 ignored, 0 warnings.

**Core platform (complete)**:
- 100 MCP integration test steps (136 assertions, full tool coverage including team lifecycle + task board + messaging + workspace_merge + nested teams + orchestration tools + MCP bridge)
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
- Guest: TCP fallback removed — vsock-only, no unauthenticated TCP listener
- Guest: 64-connection semaphore on both main and relay vsock accept loops
- VM: HMP tag sanitization, stderr to log file
- MCP/storage: UTF-8 safe truncation, path traversal prevention, dataset hierarchy guard
- Token-bucket rate limiting (create 5/min, exec 60/min, default 120/min)
- ZFS volsize enforcement on workspace create/fork with atomic quota check
- Per-interface ip_forward (scoped to br-agentiso, not global)
- Init.rs security: SUDO_USER validation, shell escaping, secure tempfile
- Credential redaction in git_push
- PID reuse verification in auto-adopt
- Internet access enabled by default (`default_allow_internet = true`); disable per-workspace via MCP tools
- `network_policy` reconfigures guest DNS via vsock when toggling internet access (prevents stale `/etc/resolv.conf`)
- Workspace name validation: 1-128 chars, alphanumeric/hyphen/underscore/dot only
- `shared_context` size limit: 1 MiB max in `swarm_run`
- Force-adopt quota-check ordering: quotas verified before removing previous owner (prevents orphaning)
- Session activity tracking: `last_activity` timestamp on every tool call, force-adopt blocked for sessions active within 60s
- `workspace_prepare` name reservation: RAII guard prevents concurrent duplicate creation

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
- Internet access enabled by default (`default_allow_internet = true`); override per-workspace
- Rate limiting enabled by default (`[rate_limit]` section in config)

**Vsock desync fix + connection concurrency (complete)**:
- ALL vsock operations use fresh_vsock_client() per-operation — no shared Arc<Mutex<VsockClient>> contention or protocol desync
- Added fresh_vsock_client_by_cid() for warm pool path (lookup by CID before VM re-key)
- process.wait() in all 3 launch() error paths to release vsock CIDs
- Guest agent accepts multiple concurrent connections (accept loop → tokio::spawn), semaphore-limited to 64

**Init-fast boot (complete)**:
- `init_mode = "fast"` in config.toml — sub-second VM boot bypassing OpenRC entirely
- initrd-fast.img (1.1MB) with minimal busybox + vsock modules
- /sbin/init-fast shim: mount /proc/sys/dev/tmp/run/pts/shm, load vsock modules, exec guest agent
- Bounded tmpfs: /tmp (256MB), /run (64MB) — prevents OOM from unbounded tmpfs during npm install
- /dev/pts + /dev/shm mounted for npm/node compatibility
- /etc/hosts written at boot (localhost resolution)
- boot_timeout_secs reduced from 60 to 10
- Pool VMs ready ~2-3s after server start (vs 30-60s with OpenRC)

**Guest agent hardening (complete)**:
- OOM score protection: oom_score_adj=-1000 at startup (survives guest OOM events)
- /etc/hosts written with hostname in configure_workspace (localhost + hostname resolution)
- cgroup overhead increased 64→256MB for QEMU process memory
- workspace_prepare: configurable timeout_secs param (default 300, set 600+ for npm install)
- swarm_run: unregister_workspace on cleanup failure (quota leak fix)

**Session resilience (complete)**:
- `workspace_adopt(force=true)`: transfers ownership from stale sessions (inactive >60s), blocked for active sessions
- Session `last_activity` timestamp updated on every tool call for dead-session detection
- `workspace_prepare`: idempotent — reclaims existing workspace by name, auto-suffix on collision, RAII name lock prevents concurrent duplicate creation
- `configure_workspace`: retry with 500ms delay at all 4 call sites (create, start, fork, warm pool)

**Swarm optimizations (complete)**:
- Compact JSON responses (`to_string` instead of `to_string_pretty` across all 49 MCP tool responses)
- `vault_context` on `swarm_run`: resolve vault queries and inject into worker prompts
- `shared_context` on `swarm_run`: distribute a single context string to all workers
- Parallel set_env + context injection + destroy via JoinSet in swarm_run

**MCP Bridge — OpenCode-to-OpenCode swarm (complete)**:
- HTTP MCP transport on bridge interface (10.99.0.1:3100) — slave OpenCodes in VMs connect back to host
- `ConfigureMcpBridge` vsock protocol: host sends bridge_url + auth_token to guest, guest writes OpenCode config.jsonc
- Per-workspace auth tokens: each slave OpenCode scoped to its own workspace tools only
- `[mcp_bridge]` config section: enabled, bind_addr, port
- nftables INPUT rules: per-workspace allow VM→host TCP on MCP bridge port (+ optional ollama 11434)
- swarm_run `mcp_bridge=true` mode: generate token → set_env → ConfigureMcpBridge → run OpenCode with full MCP tools
- Local model support: optional `model_provider` + `model_api_base` in ConfigureMcpBridge (for ollama/vLLM)
- Protocol types: `ConfigureMcpBridgeRequest`, `McpBridgeConfiguredResponse` in protocol/src/lib.rs
- VmManager.configure_mcp_bridge(): fresh vsock per call, &self only
- Guest handler: writes /root/.config/opencode/config.jsonc with MCP server entry + auth header
- 60 protocol tests, 37 guest tests, 775 agentiso tests (ConfigureMcpBridge coverage + bridge + dashboard integration)

**Frontend Dashboard (Phases 1-3 + post-ship polish v1-v3, complete)**:
- React 19 + Vite + TailwindCSS 4 + Zustand, embedded via rust-embed (compressed, MIME types, cache headers, SPA fallback)
- 60 REST endpoints + WebSocket on axum (port 7070, `agentiso/src/dashboard/web/` — 9 files)
- Multi-board kanban (@hello-pangea/dnd), vault browser (Tiptap + react-force-graph-2d), mosaic panes, command palette (Ctrl+K)
- Code splitting: 14 chunks, ~114KB gzip initial load
- SSE exec streaming with Ctrl+C cancellation, event-driven WebSocket (10s fallback polling)
- Responsive layout (sidebar rail <1200px, mobile overlay <768px), full ARIA accessibility
- `[dashboard]` config section: enabled, bind_addr (0.0.0.0), port (7070), admin_token, static_dir
- TTY-aware serve mode: ASCII banner + status display when running from terminal, Ctrl+C shutdown via signal handler (skips MCP stdio in TTY mode)
- **Workspace detail pane**: split view with status/resources/network/metadata + embedded xterm.js terminal, Lucide action buttons, 2-step destroy confirmation
- **Terminal grid view**: tabs/grid toggle (1-4 terminals in CSS grid), click-to-focus, double-click maximize, scrollback search (Ctrl+F via @xterm/addon-search), paste handling with multi-line confirmation, right-click context menu, floating quick toolbar, font size control (Ctrl+=/Ctrl+-/Ctrl+0)
- **Team chat**: real-time messaging wired to team API (send/receive/broadcast), grouped messages with color-coded sender badges, relative timestamps, 3s polling, from/to agent dropdowns
- **Task lifecycle UI**: CreateTaskDialog (title/description/priority/dependencies), lifecycle buttons on cards (start/complete/fail/release), 7 API endpoints fully wired, command palette + board + team detail entry points
- **Lucide icons**: all action buttons use Lucide React icons (Play, Square, Trash2, GitFork, Camera, Globe, Terminal, etc.) — no Unicode chars
- **ConfirmDialog**: reusable confirmation dialog on all destructive actions (workspace destroy)
- **Custom kanban boards**: create/select/delete user-defined boards, persisted to localStorage
- **+15% scale pass**: base font 14→16px, all components scaled proportionally
- **Accessibility**: cursor-pointer, tabIndex, Enter/Space handlers, aria-labels on all interactive elements, consistent 150ms transitions

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
- 100 MCP integration test steps (136 assertions, full tool coverage including team lifecycle, messaging, task board, workspace_merge, nested teams, orchestration tools, MCP bridge)

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
- 872 unit tests (775 agentiso + 60 protocol + 37 guest)

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
- `docs/plans/2026-02-21-mcp-bridge-design.md` — MCP bridge: HTTP transport for VM-based OpenCode swarms
- `docs/plans/2026-02-21-frontend-kanban-design.md` — Frontend dashboard: React kanban + vault + terminal UI
