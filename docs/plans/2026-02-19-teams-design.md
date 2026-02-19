# Teams Design: Multi-Agent Team Coordination for agentiso

**Date**: 2026-02-19
**Status**: Design (pre-implementation)
**Scope**: Full A2A-style team coordination with vault-backed task board, nested teams, agent discovery, inter-VM communication, and git-based merge

---

## 1. Overview

Add first-class multi-agent team support to agentiso, enabling AI agents running in isolated VM workspaces to coordinate as teams. This builds on the existing vault, orchestration engine, and workspace primitives.

### Design decisions (user-confirmed)

| Decision | Choice |
|----------|--------|
| Orchestrator | Both: CLI orchestrate + MCP tools (layered) |
| Inter-agent messaging | Direct VM-to-VM (HTTP) + host vsock relay |
| Code merge strategy | Git-based merge tool (`workspace_merge`) |
| Task management | Vault-backed task board (markdown + YAML frontmatter) |
| Team nesting | Unlimited (with resource caps) |
| Agent discovery | A2A-style Agent Cards (JSON capability documents) |
| VM-to-VM protocol | HTTP for data payloads, vsock relay for control messages |
| Resource limits | Global cap + per-team budget with inheritance |

---

## 2. Architecture

```
                    MCP Client (Claude Code / OpenCode)
                              │
                    ┌─────────▼──────────┐
                    │   agentiso serve   │
                    │   (MCP server)     │
                    │                    │
                    │  Team Engine       │
                    │  ├─ TeamManager    │
                    │  ├─ TaskBoard      │
                    │  ├─ MessageRelay   │
                    │  └─ MergeEngine    │
                    └────────┬───────────┘
                             │ vsock (control)
              ┌──────────────┼──────────────┐
              ▼              ▼              ▼
         ┌─────────┐  ┌─────────┐   ┌─────────┐
         │  VM-A   │  │  VM-B   │   │  VM-C   │
         │ (lead)  │  │(worker) │   │(worker) │
         │         │──│         │───│         │
         │ HTTP    │  │ HTTP    │   │ HTTP    │
         │ :8080   │  │ :8080   │   │ :8080   │
         └─────────┘  └─────────┘   └─────────┘
              │              │              │
              └──────────────┼──────────────┘
                   br-agentiso (direct VM-to-VM)
                             │
                    ┌────────▼────────┐
                    │     Vault       │
                    │ (shared KB on   │
                    │  host FS)       │
                    │                 │
                    │ /teams/         │
                    │ /tasks/         │
                    │ /cards/         │
                    └─────────────────┘
```

### Layers

1. **MCP Layer**: `team_create`, `team_status`, `team_destroy`, `team_message`, `workspace_merge` MCP tools for interactive/AI-driven team management
2. **CLI Layer**: `agentiso orchestrate` extended with team semantics (TOML plan with depends_on, roles, agent cards)
3. **Team Engine** (host process): TeamManager, TaskBoard (vault-backed), MessageRelay (vsock hub), MergeEngine (git)
4. **Guest Agent**: Extended with vault proxy, HTTP API, message relay client, agent card server
5. **Vault**: Shared blackboard for tasks, agent cards, coordination artifacts

---

## 3. Components

### 3.1 Guest-Side Vault Access (Phase 1 — foundation)

New vsock protocol messages enabling agents inside VMs to read/write the host vault:

```
GuestRequest::VaultRead { path: String }
GuestRequest::VaultWrite { path: String, content: String }
GuestRequest::VaultSearch { query: String, max_results: u32, tag_filter: Option<String> }
GuestRequest::VaultList { path: Option<String>, recursive: bool }
```

Responses:
```
GuestResponse::VaultContent { path: String, content: String, frontmatter: Option<Map> }
GuestResponse::VaultSearchResults { results: Vec<SearchResult> }
GuestResponse::VaultEntries { entries: Vec<String> }
GuestResponse::VaultWriteOk
```

**Host handler**: Proxies to existing `VaultManager` methods. Same path traversal prevention, size limits, extension filtering.

**Guest agent**: New handlers in `handle_request()` match arm. Forward to host vsock connection.

**Security**: Vault access scoped to team-owned paths. Agent in team "alpha" can only access `vault/teams/alpha/**`. Global vault paths read-only for all agents.

### 3.2 Agent Cards (A2A-style discovery)

JSON documents describing agent capabilities, stored in vault at `teams/{team}/cards/{agent-name}.json`:

```json
{
  "name": "code-reviewer",
  "role": "specialist",
  "description": "Reviews code for bugs, security issues, and style",
  "skills": ["rust", "python", "security-audit"],
  "protocols": ["http", "vsock-relay"],
  "endpoints": {
    "http": "http://10.99.0.5:8080",
    "vsock": { "cid": 5, "port": 5000 }
  },
  "input_schema": {
    "type": "object",
    "properties": {
      "files": { "type": "array", "items": { "type": "string" } },
      "focus": { "type": "string", "enum": ["bugs", "security", "style", "all"] }
    }
  },
  "output_schema": {
    "type": "object",
    "properties": {
      "findings": { "type": "array" },
      "summary": { "type": "string" }
    }
  },
  "status": "ready",
  "team": "alpha",
  "parent_team": null,
  "workspace_id": "a1b2c3d4-..."
}
```

**Discovery flow**: Agent reads `vault/teams/{team}/cards/` to discover teammates. Agent Cards are written by the host on workspace creation and updated by agents themselves via vault write.

### 3.3 Vault-Backed Task Board

Tasks stored as markdown files in vault at `teams/{team}/tasks/{id}-{slug}.md`:

```markdown
---
id: "001"
status: pending        # pending | claimed | in_progress | blocked | completed | failed
owner: null            # agent name or null
priority: 1            # 1 (highest) to 5 (lowest)
depends_on: []         # list of task IDs
blocked_by: []         # auto-computed from depends_on
created_by: "lead"
created_at: "2026-02-19T21:00:00Z"
updated_at: "2026-02-19T21:00:00Z"
tags: ["auth", "backend"]
---

# Fix authentication middleware

## Description
The JWT validation in `src/auth/middleware.rs` rejects valid tokens after rotation.

## Acceptance Criteria
- [ ] Tokens from both old and new keys are accepted
- [ ] Unit test for key rotation scenario
- [ ] No regression in existing auth tests

## Notes
(agents append notes here as they work)
```

**Task operations** (via vault read/write):
- **Claim**: Agent reads task, checks `status: pending` and `blocked_by: []`, writes back with `status: claimed, owner: {self}`
- **Update progress**: Agent appends to Notes section, updates `status: in_progress`
- **Complete**: Agent sets `status: completed`, appends result summary
- **Block**: Agent sets `status: blocked` with reason in Notes

**Conflict resolution**: Optimistic concurrency — if two agents try to claim the same task, the second write sees `status: claimed` and backs off. The vault write is not atomic, so a TOCTOU race exists. Mitigation: host-side `TaskClaim` vsock message that does atomic claim under lock.

**Task board index**: Auto-generated `teams/{team}/tasks/INDEX.md` with summary table, updated on every task state change.

### 3.4 Inter-Agent Messaging

**Channel 1: vsock relay (control messages)**

New protocol messages:
```
GuestRequest::TeamMessage {
    to: String,           // agent name or "*" for broadcast
    content: String,
    message_type: String, // "text" | "task_update" | "request" | "response"
}

GuestResponse::TeamMessageDelivered { message_id: String }
GuestResponse::TeamMessageReceived {
    from: String,
    content: String,
    message_type: String,
    message_id: String,
}
```

Host `MessageRelay` maintains a per-agent inbox (bounded queue). Messages delivered on next vsock poll or pushed via long-poll.

**Channel 2: HTTP API (data payloads)**

Guest agent exposes HTTP server on port 8080 (configurable):

```
GET  /card              → Agent Card JSON
GET  /health            → { "status": "ready" }
POST /message           → receive message from peer
POST /task              → receive task delegation
GET  /files/{path}      → serve workspace files
POST /files/{path}      → receive files from peer
```

Requires `allow_inter_vm=true` on the team's network policy. The host configures nftables to allow intra-team traffic only.

### 3.5 Team Lifecycle

**States**: `creating` → `ready` → `working` → `completing` → `destroyed`

**MCP tools**:

```
team_create(name, roles: [{name, role, skills, image, resources}], plan: Option<String>)
  → Creates team entry in vault, forks VMs, writes Agent Cards, sets up network policy
  → Returns team_id, member list with IPs

team_status(team_id)
  → Returns team state, member states, task board summary, resource usage

team_message(team_id, to, content, message_type)
  → Sends message via host relay to target agent (or broadcast)

team_assign(team_id, task_id, agent_name)
  → Assigns vault task to specific agent

workspace_merge(source_workspaces: Vec<UUID>, target_workspace: UUID, strategy: String)
  → Extracts git patches from sources, applies to target
  → strategy: "sequential" | "branch-per-source" | "cherry-pick"
  → Returns merge result with any conflicts

team_destroy(team_id)
  → Stops all VMs, cleans up vault entries, destroys workspaces
```

**CLI extension**:

```toml
# teams.toml
[team]
name = "feature-sprint"
max_vms = 10

[[team.roles]]
name = "lead"
image = "alpine-dev"
skills = ["planning", "code-review"]
count = 1

[[team.roles]]
name = "backend"
image = "rust-toolchain"
skills = ["rust", "api", "database"]
count = 3

[[team.roles]]
name = "frontend"
image = "node-toolchain"
skills = ["typescript", "react"]
count = 2

[[team.tasks]]
id = "001"
name = "design-api"
assign_to = "lead"
depends_on = []

[[team.tasks]]
id = "002"
name = "implement-endpoints"
assign_to = "backend"
depends_on = ["001"]

[[team.tasks]]
id = "003"
name = "build-ui"
assign_to = "frontend"
depends_on = ["001"]

[[team.tasks]]
id = "004"
name = "integration-test"
assign_to = "lead"
depends_on = ["002", "003"]
```

### 3.6 Nested Teams

An agent can create a sub-team by sending a `CreateSubTeam` vsock request to the host:

```
GuestRequest::CreateSubTeam {
    name: String,
    roles: Vec<RoleDef>,
    budget: ResourceBudget,
}
```

**Budget inheritance**: Sub-team's budget is deducted from parent team's remaining budget. If parent has 10 VMs remaining and sub-team requests 4, parent's remaining drops to 6.

**Resource caps**:
- Global hard cap: `max_total_vms` in config (e.g., 50)
- Per-team budget: `max_vms` per team definition
- Nesting depth limit: configurable `max_nesting_depth` (default 3) to prevent runaway recursion
- Sub-team inherits parent's `allowed_ports`, `allow_internet`, `allow_inter_vm` policies (can restrict further, cannot escalate)

**Lifecycle**: Sub-team destroyed when parent team is destroyed. Sub-team can also self-destroy.

### 3.7 Git-Based Merge Tool

`workspace_merge` MCP tool:

```
workspace_merge(
    source_workspaces: Vec<UUID>,  // workspaces to merge from
    target_workspace: UUID,         // workspace to merge into
    strategy: String,               // "sequential" | "branch-per-source" | "cherry-pick"
    repo_path: Option<String>,      // default: /workspace
)
```

**Implementation**:
1. For each source workspace, exec `git diff HEAD` or `git format-patch` to extract changes
2. Transfer patches to target workspace via `file_transfer`
3. In target workspace, apply patches via `git am` or `git apply`
4. If conflicts, return conflict details (file list, conflict markers) for resolution

**Strategies**:
- `sequential`: Apply patches from each source in order. First source's changes applied first. Later sources may conflict.
- `branch-per-source`: Create a branch per source, apply patches, then merge branches. Better conflict visibility.
- `cherry-pick`: Extract individual commits, cherry-pick into target. Most granular control.

---

## 4. Implementation Phases

### Phase 1: Guest Vault Access (foundation)
- New protocol messages: VaultRead, VaultWrite, VaultSearch, VaultList (batch with Phase 2 types in single deploy — I1)
- Guest agent handlers proxying to host VaultManager via vsock
- Per-path RwLock in VaultManager for concurrent write safety (C2)
- VaultManager scope parameter for team path isolation (I3)
- Architect dual vsock connection model (C1 — second connection used in Phase 4 but protocol designed now)
- Tests: unit + e2e vault proxy

### Phase 2: Agent Cards + Team Lifecycle
- Agent Card JSON schema and vault storage
- TeamManager in host process
- Bundled `team` MCP tool with action=create/destroy/status (I5 — keeps tool count at 29)
- Team state machine (creating → ready → working → completing → destroyed)
- `team_id: Option<String>` on Workspace, `teams` HashMap in PersistedState schema v3 (I7)
- Global fork semaphore in WorkspaceManager (I6)
- Split tools.rs: extract team_tools.rs + git_tools.rs (I4)
- `agentiso orchestrate` extended with team TOML format
- Tests: unit + e2e team lifecycle

### Phase 3: Vault-Backed Task Board
- Task markdown format with YAML frontmatter
- TaskBoard engine with atomic claim (via vsock)
- Task dependency resolution (topological sort)
- Auto-generated INDEX.md
- Guest-side task operations via vault write
- Tests: unit + e2e task lifecycle

### Phase 4: Inter-Agent Messaging
- vsock relay: TeamMessage protocol messages + MessageRelay on host
- HTTP API on guest agent (port 8080)
- Network policy: intra-team traffic rules
- Message delivery: push via long-poll + pull via explicit receive
- Tests: unit + e2e messaging

### Phase 5: Git Merge + Nested Teams
- `workspace_merge` MCP tool with 3 strategies
- Nested team support: CreateSubTeam protocol, budget inheritance
- Resource caps: global + per-team + nesting depth
- Tests: unit + e2e merge and nesting

### Phase 6: CLI + Observability
- `agentiso orchestrate` team mode with depends_on DAG execution
- `agentiso team-status` CLI command
- Dashboard integration (ratatui): team view, task board, message log
- Prometheus metrics for team operations

---

## 5. New Files

```
agentiso/src/team/
├── mod.rs              # TeamManager, TeamState, team lifecycle
├── task_board.rs       # Vault-backed task board engine
├── message_relay.rs    # Inter-agent message routing
├── merge.rs            # Git-based workspace merge
├── agent_card.rs       # A2A-style Agent Card schema + operations
└── budget.rs           # Resource budget tracking + inheritance

protocol/src/lib.rs     # New message types (VaultRead/Write, TeamMessage, etc.)

guest-agent/src/
├── vault_proxy.rs      # Guest-side vault access via vsock
├── http_api.rs         # HTTP API server for peer communication
└── team_client.rs      # Team operations (card, messaging, tasks)
```

### Modified files

```
agentiso/src/mcp/tools.rs          # New MCP tools: team_create, team_status, etc.
agentiso/src/workspace/mod.rs      # Team-aware workspace creation
agentiso/src/workspace/orchestrate.rs  # DAG execution, team mode
agentiso/src/config.rs             # TeamConfig, ResourceCaps
guest-agent/src/main.rs            # New request handlers
```

---

## 6. New MCP Tools (Phase 2-5)

| Tool | Phase | Description |
|------|-------|-------------|
| `team` | 2 | Bundled: action=create/destroy/status. Create team with roles + spawn VMs, get team state + task summary, destroy team + clean vault |
| `team_message` | 4 | Send message to agent or broadcast to team. New rate limit category: burst=50, 300/min |
| `workspace_merge` | 5 | Git-based merge from N source workspaces to target. 3 strategies: sequential, branch-per-source, cherry-pick |

Tool count: 27 → 29 (not 32). Bundled `team` tool follows existing action-param pattern (like snapshot, exec_background).

**Existing tools extended**:
- `vault`: Team-scoped paths auto-prefixed
- `workspace_fork`: Team-aware, applies team network policy
- `network_policy`: Intra-team traffic rules

---

## 7. New Protocol Messages (Phase 1-5)

| Message | Phase | Direction | Purpose |
|---------|-------|-----------|---------|
| `VaultRead` | 1 | Guest→Host | Read vault note |
| `VaultWrite` | 1 | Guest→Host | Write vault note |
| `VaultSearch` | 1 | Guest→Host | Search vault by regex/tags |
| `VaultList` | 1 | Guest→Host | List vault entries |
| `TeamMessage` | 4 | Guest→Host→Guest | Inter-agent message relay |
| `TeamMessageReceived` | 4 | Host→Guest | Deliver message to recipient |
| `CreateSubTeam` | 5 | Guest→Host | Spawn nested sub-team |
| `TaskClaim` | 3 | Guest→Host | Atomic task claim under lock |

---

## 8. Security Considerations

- **Vault scoping**: Agents can only write to `teams/{their-team}/**`. Global vault is read-only.
- **Message isolation**: Host relay only routes messages within the same team (or parent↔child for nested teams).
- **HTTP API**: Only accessible from VMs on the same bridge with `allow_inter_vm=true`. Not exposed to host network.
- **Budget enforcement**: Hard caps prevent runaway VM creation. Sub-teams cannot exceed parent budget.
- **Agent Card validation**: Host validates Agent Cards before writing to vault. Schema enforcement prevents injection.
- **Merge safety**: `workspace_merge` operates on git repos only. No arbitrary file overwrites. Conflicts reported, not auto-resolved.

---

## 9. Open Questions

1. **Task claim atomicity**: The vault doesn't support transactions. The `TaskClaim` vsock message provides atomic claims, but what if an agent writes directly to the vault task file? Consider: vault write hook that rejects direct status changes, forcing all claims through `TaskClaim`.

2. **Message ordering**: vsock relay delivers messages in order per sender. HTTP is unordered. Do we need sequence numbers or vector clocks for consistency?

3. **Agent Card updates**: When an agent's capabilities change mid-session (e.g., it installs new tools), how does it update its card? Self-update via vault write + notify teammates?

4. **Merge conflict UX**: When `workspace_merge` encounters conflicts, who resolves them? The orchestrator? The original author? A dedicated "merger" agent?

5. **Vault performance under load**: With 50 VMs all reading/writing vault notes, is filesystem I/O a bottleneck? Consider: in-memory cache with periodic flush, or SQLite backend for task board.

---

## 10. Codebase Review Findings (5-agent audit, 2026-02-19)

Five agents independently reviewed each layer of the codebase against the teams design. Below are the findings that require plan amendments, organized by severity.

### Critical: Must address before implementation

**C1. Exec holds vsock Mutex for up to 125 seconds** (vm-reviewer)
- `VsockClient::exec()` acquires the `Arc<Mutex<VsockClient>>` and holds it for `timeout_secs + 5` (up to 125s).
- During a long exec, NO other vsock operation can reach that VM — no ping, no file ops, no message delivery.
- **Impact on teams**: Cannot deliver inter-agent messages to a VM while it's executing a command. This breaks real-time coordination.
- **Required fix**: Use a **second vsock connection** for the message relay channel, stored in a separate `Arc<Mutex<VsockClient>>`. The guest already supports multiple concurrent connections (accept loop + tokio::spawn, backlog=128). Host needs to create and store a dedicated message channel connection per VM.

**C2. Vault has NO file locking — concurrent writes cause data loss** (mcp-reviewer)
- `VaultManager` uses plain `tokio::fs::read_to_string` + `tokio::fs::write` with no locking.
- `write_note` (append/prepend), `set_frontmatter`, `add_tag`, `search_replace` all do read-modify-write. Last writer wins silently.
- **Impact on teams**: Multiple agents writing to the same team status/task note = data loss.
- **Required fix (Phase 1)**: Add per-path `RwLock` map to VaultManager: `locks: Arc<Mutex<HashMap<PathBuf, Arc<RwLock<()>>>>>`. Writers acquire write lock, readers acquire read lock. Alternatively, scope notes per-agent and document that shared notes need the `TaskClaim` vsock path for writes.

### Important: Should address in the relevant phase

**I1. Protocol change cascade is 4 steps** (vm-reviewer)
- Any new `GuestRequest`/`GuestResponse` variant requires: protocol crate change → guest binary rebuild → `setup-e2e.sh` to install into Alpine image → re-test.
- **Impact**: Phase 1 (vault proxy) adds 4+ new protocol types. Must batch all Phase 1 protocol changes into a single deploy cycle.
- **Mitigation**: Design all Phase 1-2 protocol types up front. Implement and deploy together.

**I2. No request multiplexing on vsock** (vm-reviewer)
- Single-request-per-connection, no correlation IDs. Cannot pipeline requests.
- **Impact**: If we need multiplexed vault ops + message delivery on one connection, we'd need protocol framing changes (add request IDs).
- **Mitigation**: Use separate connections for different channels (request/response on connection 1, push/relay on connection 2). Avoids framing changes.

**I3. VaultManager needs scope parameter** (mcp-reviewer)
- Currently holds a single root path with no per-caller scope. Any session can read/write any vault path.
- **Impact**: Agents in different teams can read/write each other's vault paths.
- **Required fix (Phase 1)**: Add `scope: Option<PathBuf>` to VaultManager. When set, `resolve_path` prepends scope and rejects escape. Create scoped instances per team. The vault MCP tool handler needs to plumb team context to select the right scoped instance.

**I4. tools.rs is 5,281 lines — needs splitting** (mcp-reviewer)
- Already at readability limits. Adding 2+ team tools makes it worse.
- **Required fix (Phase 2)**: Extract `team_tools.rs` and `git_tools.rs`. Keep core lifecycle/exec/file tools in `tools.rs`. Check rmcp `#[tool_router]` support for multi-file impls.

**I5. Bundle team lifecycle into single tool** (mcp-reviewer)
- Instead of 3 tools (`team_create`, `team_status`, `team_destroy`), use 1 bundled `team` tool with `action` param.
- `team_message` and `workspace_merge` stay standalone.
- **Result**: 27 → 29 tools (not 32). Stays in the sweet spot.

**I6. Need global fork semaphore** (ws-reviewer)
- Currently no global concurrency limit on fork operations. Two concurrent team creates of 10 VMs each = 20 simultaneous forks.
- **Required fix (Phase 2)**: Add `max_concurrent_forks` config + global `Semaphore` in WorkspaceManager gating all fork operations.

**I7. PersistedState needs schema v3** (ws-reviewer)
- Add `team_id: Option<String>` to `Workspace` struct.
- Add `teams: HashMap<String, TeamState>` to `PersistedState`.
- Bump `schema_version` to 3, use `#[serde(default)]` for backward compat.

### Nice-to-have: Address when convenient

**N1. `zfs promote` missing** (net-reviewer)
- If a team lead's workspace is destroyed while fork-workers still exist, the ZFS parent snapshot can't be deleted (dependent clones).
- **Workaround**: Destroy forks before parent (team destroy already does this).
- **Future**: Add `zfs promote` to make forks independent of parent.

**N2. CID goes stale on VM recreation** (vm-reviewer)
- If a VM is destroyed and recreated (snapshot restore), its CID changes. Any cached CID in a message routing table goes stale.
- **Mitigation**: Use workspace_id (stable UUID) for routing, resolve CID at message-send time from VmManager.

**N3. Per-peer nftables rules are O(N^2)** (net-reviewer)
- A team of N members generates N*(N-1) rules. For 10 members = 90 rules (fine). For 50 = 2,450 (still fine for nftables, but consider nftables sets as optimization).
- **Future**: Migrate to nftables sets for teams > 20 members.

**N4. New rate limit category for team_message** (mcp-reviewer)
- Messaging has different profile from creates or execs. Recommend: `team_message` category with burst=50, 300/min.

**N5. Warm pool stays as-is** (ws-reviewer)
- Teams use `fork()` which bypasses warm pool. Pool continues serving single `create()` operations. No changes needed.

### Answered questions from Section 9

Based on the review findings, several open questions are now resolved:

- **Q1 (Task claim atomicity)**: Use the `TaskClaim` vsock message for atomic claims. Vault file locking (C2) provides safety for other write operations. Direct vault writes to task files should be allowed for status updates (notes, progress) but NOT for claim/ownership changes.
- **Q5 (Vault performance)**: Filesystem I/O is not a bottleneck for 50 VMs (modern NVMe handles thousands of small file ops/sec). The real concern is write races (C2), not throughput. In-memory cache deferred to optimization phase.

### Summary: Plan amendments

| ID | Amendment | Phase affected |
|----|-----------|----------------|
| C1 | Dual vsock connection per VM (request + message relay) | Phase 4, but architect in Phase 1 |
| C2 | Per-path RwLock in VaultManager | Phase 1 |
| I1 | Batch all Phase 1-2 protocol types in single deploy | Phase 1-2 |
| I3 | VaultManager scope parameter | Phase 1 |
| I4 | Split tools.rs into team_tools.rs + git_tools.rs | Phase 2 |
| I5 | Bundle team lifecycle into single `team` tool (27→29) | Phase 2 |
| I6 | Global fork semaphore in WorkspaceManager | Phase 2 |
| I7 | PersistedState schema v3 with team fields | Phase 2 |
| N4 | New `team_message` rate limit category | Phase 4 |
