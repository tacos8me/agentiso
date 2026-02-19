# Teams Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add guest-side vault access via vsock proxy (Phase 1) and team lifecycle management (Phase 2) to agentiso, enabling AI agents in VMs to coordinate through a shared knowledge base.

**Architecture:** Extend the vsock protocol with VaultRead/Write/Search/List messages. Host proxies requests to existing VaultManager. Add per-path RwLock for concurrent write safety and scope parameter for team isolation. Phase 2 adds TeamManager, Agent Cards, bundled `team` MCP tool, and PersistedState schema v3.

**Tech Stack:** Rust, tokio, serde, agentiso-protocol crate, VaultManager, vsock, rmcp MCP SDK

**Design doc:** `docs/plans/2026-02-19-teams-design.md` (full architecture, review findings, phase breakdown)

---

## Phase 1: Guest Vault Access (foundation)

### Task 1: Add vault protocol types to shared crate

**Files:**
- Modify: `protocol/src/lib.rs:19-192` (GuestRequest/GuestResponse enums)

**Step 1: Add VaultRead/Write/Search/List request types**

Add these structs and enum variants to `protocol/src/lib.rs` after the existing request types:

```rust
// --- Vault proxy (Phase 1) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultReadRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultWriteRequest {
    pub path: String,
    pub content: String,
    /// "overwrite" | "append" | "prepend"
    #[serde(default = "default_write_mode")]
    pub mode: String,
}

fn default_write_mode() -> String {
    "overwrite".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSearchRequest {
    pub query: String,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    pub tag_filter: Option<String>,
}

fn default_max_results() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListRequest {
    pub path: Option<String>,
    #[serde(default)]
    pub recursive: bool,
}
```

Add to `GuestRequest` enum:
```rust
    VaultRead(VaultReadRequest),
    VaultWrite(VaultWriteRequest),
    VaultSearch(VaultSearchRequest),
    VaultList(VaultListRequest),
```

**Step 2: Add vault response types**

Add these structs and enum variants to `GuestResponse`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultContentResponse {
    pub path: String,
    pub content: String,
    pub frontmatter: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSearchResultEntry {
    pub path: String,
    pub line_number: usize,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSearchResponse {
    pub results: Vec<VaultSearchResultEntry>,
    pub total_matches: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListResponse {
    pub entries: Vec<String>,
}
```

Add to `GuestResponse` enum:
```rust
    VaultContent(VaultContentResponse),
    VaultWriteOk,
    VaultSearchResults(VaultSearchResponse),
    VaultEntries(VaultListResponse),
```

**Step 3: Add unit tests for serialization roundtrips**

Add tests following the existing pattern (e.g., `test_request_exec_roundtrip`):

```rust
#[test]
fn test_vault_read_roundtrip() {
    let req = GuestRequest::VaultRead(VaultReadRequest {
        path: "teams/alpha/notes/status.md".to_string(),
    });
    let json = serde_json::to_string(&req).unwrap();
    let parsed: GuestRequest = serde_json::from_str(&json).unwrap();
    // verify fields match
}

#[test]
fn test_vault_write_roundtrip() { ... }

#[test]
fn test_vault_search_roundtrip() { ... }

#[test]
fn test_vault_list_roundtrip() { ... }

#[test]
fn test_vault_content_response_roundtrip() { ... }
```

**Step 4: Run tests**

Run: `cargo test -p agentiso-protocol`
Expected: All existing tests pass + 5 new tests pass

**Step 5: Commit**

```bash
git add protocol/src/lib.rs
git commit -m "feat(protocol): add VaultRead/Write/Search/List protocol types for guest vault proxy"
```

---

### Task 2: Add per-path RwLock to VaultManager (C2 fix)

**Files:**
- Modify: `agentiso/src/mcp/vault.rs:68-86` (VaultManager struct + constructor)
- Modify: `agentiso/src/mcp/vault.rs:160-230` (read_note, write_note)
- Modify: `agentiso/src/mcp/vault.rs:347-430` (search)
- Modify: `agentiso/src/mcp/vault.rs:674+` (search_replace, set_frontmatter, etc.)

**Step 1: Add lock map to VaultManager**

```rust
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct VaultManager {
    config: VaultConfig,
    /// Per-path RwLock for concurrent access safety.
    /// Readers can proceed in parallel; writers get exclusive access per path.
    locks: Mutex<HashMap<PathBuf, Arc<RwLock<()>>>>,
}
```

Update constructor:
```rust
Some(Arc::new(Self {
    config: config.clone(),
    locks: Mutex::new(HashMap::new()),
}))
```

Add helper method:
```rust
/// Get or create an RwLock for a specific file path.
async fn path_lock(&self, path: &PathBuf) -> Arc<RwLock<()>> {
    let mut map = self.locks.lock().await;
    map.entry(path.clone())
        .or_insert_with(|| Arc::new(RwLock::new(())))
        .clone()
}
```

**Step 2: Wrap read_note with read lock**

In `read_note`, after resolving the path:
```rust
let lock = self.path_lock(&resolved).await;
let _guard = lock.read().await;
// ... existing read logic
```

**Step 3: Wrap write_note with write lock**

In `write_note`, after resolving the path:
```rust
let lock = self.path_lock(&resolved).await;
let _guard = lock.write().await;
// ... existing write logic
```

Apply same pattern to: `set_frontmatter`, `delete_frontmatter`, `add_tag`, `remove_tag`, `search_replace`, `delete_note`, `move_note`.

**Step 4: Write test for concurrent write safety**

```rust
#[tokio::test]
async fn test_concurrent_writes_no_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    // create vault config pointing to temp dir
    // write initial file
    // spawn 10 tasks that each append a unique line
    // join all tasks
    // read file, verify all 10 lines present
}
```

**Step 5: Run tests**

Run: `cargo test --manifest-path agentiso/Cargo.toml -- vault`
Expected: All existing vault tests pass + new concurrency test passes

**Step 6: Commit**

```bash
git add agentiso/src/mcp/vault.rs
git commit -m "fix(vault): add per-path RwLock to prevent concurrent write data loss (C2)"
```

---

### Task 3: Add scope parameter to VaultManager (I3 fix)

**Files:**
- Modify: `agentiso/src/mcp/vault.rs:68-130` (VaultManager struct, constructor, resolve_path)

**Step 1: Add scope field**

```rust
pub struct VaultManager {
    config: VaultConfig,
    locks: Mutex<HashMap<PathBuf, Arc<RwLock<()>>>>,
    /// Optional scope prefix. When set, all paths are resolved relative to this
    /// subdirectory of the vault root. Agents in team "alpha" get scope "teams/alpha".
    scope: Option<PathBuf>,
}
```

Add scoped constructor:
```rust
/// Create a scoped VaultManager. All paths resolve under vault_root/scope/.
pub fn with_scope(config: &VaultConfig, scope: &str) -> Option<Arc<Self>> {
    if !config.enabled {
        return None;
    }
    let scoped_path = config.path.join(scope);
    if !scoped_path.exists() {
        // Create the scope directory
        std::fs::create_dir_all(&scoped_path).ok()?;
    }
    Some(Arc::new(Self {
        config: config.clone(),
        locks: Mutex::new(HashMap::new()),
        scope: Some(PathBuf::from(scope)),
    }))
}
```

Update existing `new()` to set `scope: None`.

**Step 2: Modify resolve_path to apply scope**

```rust
pub fn resolve_path(&self, path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    for component in path.components() {
        if let std::path::Component::ParentDir = component {
            bail!("path traversal (.. component) is not allowed");
        }
    }

    // If scoped, resolve under scope directory
    let base = if let Some(ref scope) = self.scope {
        self.config.path.join(scope)
    } else {
        self.config.path.clone()
    };

    let joined = base.join(path);
    // ... rest of existing canonicalization logic, but check against `base` not `self.config.path`
}
```

**Step 3: Write tests**

```rust
#[tokio::test]
async fn test_scoped_vault_read_write() {
    // create vault with scope "teams/alpha"
    // write note to "status.md" -> resolves to vault_root/teams/alpha/status.md
    // read it back
    // verify path traversal escape is blocked
}

#[test]
fn test_scoped_resolve_path_blocks_escape() {
    // scope = "teams/alpha"
    // path = "../../other-team/secret.md" -> rejected
}
```

**Step 4: Run tests**

Run: `cargo test --manifest-path agentiso/Cargo.toml -- vault`
Expected: All tests pass

**Step 5: Commit**

```bash
git add agentiso/src/mcp/vault.rs
git commit -m "feat(vault): add scope parameter for team path isolation (I3)"
```

---

### Task 4: Add vault proxy methods to host-side VsockClient

**Files:**
- Modify: `agentiso/src/vm/vsock.rs` (add vault_read, vault_write, vault_search, vault_list methods)
- Modify: `agentiso/src/guest/mod.rs` (add vault proxy wrapper functions)

**Step 1: Add vault methods to VsockClient**

Follow the existing pattern (e.g., `exec`, `read_file`). In `vsock.rs`:

```rust
/// Read a vault note via guest proxy.
pub async fn vault_read(&mut self, path: &str) -> Result<VaultContentResponse> {
    let req = GuestRequest::VaultRead(VaultReadRequest {
        path: path.to_string(),
    });
    let resp = self.request(&req).await?;
    match resp {
        GuestResponse::VaultContent(content) => Ok(content),
        GuestResponse::Error(e) => bail!("vault read failed: {}", e.message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn vault_write(&mut self, path: &str, content: &str, mode: &str) -> Result<()> {
    let req = GuestRequest::VaultWrite(VaultWriteRequest {
        path: path.to_string(),
        content: content.to_string(),
        mode: mode.to_string(),
    });
    let resp = self.request(&req).await?;
    match resp {
        GuestResponse::VaultWriteOk => Ok(()),
        GuestResponse::Error(e) => bail!("vault write failed: {}", e.message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn vault_search(&mut self, query: &str, max_results: u32, tag_filter: Option<&str>) -> Result<VaultSearchResponse> { ... }

pub async fn vault_list(&mut self, path: Option<&str>, recursive: bool) -> Result<VaultListResponse> { ... }
```

**Step 2: Add unit tests for request construction**

Follow the existing `test_exec_sends_correct_request` pattern.

**Step 3: Run tests**

Run: `cargo test --manifest-path agentiso/Cargo.toml -- vsock`
Expected: All tests pass

**Step 4: Commit**

```bash
git add agentiso/src/vm/vsock.rs agentiso/src/guest/mod.rs
git commit -m "feat(vsock): add vault_read/write/search/list client methods"
```

---

### Task 5: Add vault proxy handler to guest agent

**Files:**
- Modify: `guest-agent/src/main.rs` (add vault request handlers in handle_request match)

**Step 1: Add vault request handling**

The guest agent receives vault requests from the MCP server (host) and needs to proxy them. But wait — the guest agent is INSIDE the VM. The vault is on the HOST. The flow is:

1. MCP client calls vault tool → host MCP server handles it directly via VaultManager (existing flow)
2. For teams: agent inside VM wants vault access → sends VaultRead to host via vsock → host proxies to VaultManager → returns result

So the guest agent needs to FORWARD vault requests to the host. But the guest agent is the SERVER side of the vsock connection — it receives requests from the host, not the other way around.

**Architecture clarification**: The vault proxy goes in the OPPOSITE direction from what's typical:
- Normal: Host sends request → Guest handles it
- Vault proxy: Guest wants vault access → Guest sends request to host

This means we need a way for the guest to initiate requests TO the host. Currently the protocol is unidirectional (host→guest only).

**Solution**: Add a host-side vsock LISTENER that guests can connect to. OR: use the existing connection but add a "reverse request" mechanism where the guest sends a vault request as a response-like message.

**Simpler solution**: The vault proxy doesn't go through the guest agent at all. Instead, tools running inside the VM (like OpenCode) call the MCP server's vault tool directly — because the MCP server IS the host process. The guest-side vault access is for agents that need programmatic vault access from WITHIN the VM via a CLI tool or library.

**Revised approach for Phase 1**: Add vault proxy methods to the HOST-SIDE workspace manager. When an MCP client calls `vault` with a team scope, the host handles it directly. The guest-side vault CLI tool (for agents inside VMs) will be Phase 1b — a small binary that connects to a host vsock listener.

For now, implement the host-side vault proxy that the MCP tool handler uses:

In `guest-agent/src/main.rs`, add handlers for the new protocol variants so the guest agent doesn't crash on unknown messages:

```rust
GuestRequest::VaultRead(_) |
GuestRequest::VaultWrite(_) |
GuestRequest::VaultSearch(_) |
GuestRequest::VaultList(_) => {
    // Vault operations are handled host-side. If a guest somehow
    // receives these, return an error.
    GuestResponse::Error(ErrorResponse {
        message: "vault operations are handled by the host, not the guest agent".to_string(),
    })
}
```

**Step 2: Run tests**

Run: `cargo test -p agentiso-guest`
Expected: All tests pass (guest agent compiles with new protocol types)

**Step 3: Commit**

```bash
git add guest-agent/src/main.rs
git commit -m "feat(guest): handle vault protocol types (host-side proxy, guest returns error)"
```

---

### Task 6: Add host-side vault proxy in workspace manager

**Files:**
- Modify: `agentiso/src/workspace/mod.rs` (add vault_read, vault_write, vault_search, vault_list methods)

**Step 1: Add vault proxy methods to WorkspaceManager**

These methods allow MCP tool handlers to proxy vault requests for specific workspaces/teams:

```rust
/// Read a vault note, optionally scoped to a team.
pub async fn vault_read(&self, path: &str, team_scope: Option<&str>) -> Result<VaultNote> {
    let vm = self.vault_manager_for_scope(team_scope)?;
    vm.read_note(path).await
}

/// Write a vault note, optionally scoped to a team.
pub async fn vault_write(&self, path: &str, content: &str, mode: WriteMode, team_scope: Option<&str>) -> Result<()> {
    let vm = self.vault_manager_for_scope(team_scope)?;
    vm.write_note(path, content, mode).await
}

/// Get a VaultManager for a given scope. Returns the global vault if scope is None.
fn vault_manager_for_scope(&self, scope: Option<&str>) -> Result<Arc<VaultManager>> {
    match scope {
        None => self.vault.clone().context("vault not configured"),
        Some(s) => {
            // Create a scoped VaultManager for the team
            VaultManager::with_scope(&self.config.vault, s)
                .context("failed to create scoped vault")
        }
    }
}
```

**Step 2: Run tests**

Run: `cargo test --manifest-path agentiso/Cargo.toml`
Expected: All 550+ tests pass

**Step 3: Commit**

```bash
git add agentiso/src/workspace/mod.rs
git commit -m "feat(workspace): add vault proxy methods with team scope support"
```

---

### Task 7: Build and deploy guest agent with new protocol types

**Step 1: Build guest agent binary**

Run: `cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest`
Expected: Build succeeds

**Step 2: Build host binary**

Run: `cargo build --release`
Expected: Build succeeds

**Step 3: Deploy to VM image**

Run: `sudo ./scripts/setup-e2e.sh`
Expected: Guest binary installed into Alpine image, all checks pass

**Step 4: Commit (if setup-e2e.sh was modified)**

No code changes expected — this is a deployment step.

---

### Task 8: Add vault proxy e2e test to MCP integration test

**Files:**
- Modify: `scripts/test-mcp-integration.sh` (add vault proxy test steps)

**Step 1: Add vault read/write test step after existing vault tests**

Add a test step that uses the `vault` MCP tool to write a note to a team-scoped path, then read it back:

```python
# Step N: vault — write a team-scoped note
send_msg(proc, msg_id, "tools/call", {
    "name": "vault",
    "arguments": {
        "action": "write",
        "path": "teams/test-team/notes/hello.md",
        "content": "# Hello from integration test\n\nThis is a team-scoped note.",
    },
})
# verify success

# Step N+1: vault — read the team-scoped note back
send_msg(proc, msg_id, "tools/call", {
    "name": "vault",
    "arguments": {
        "action": "read",
        "path": "teams/test-team/notes/hello.md",
    },
})
# verify content matches
```

**Step 2: Run integration test**

Run: `sudo ./scripts/test-mcp-integration.sh`
Expected: All existing steps pass + new vault steps pass

**Step 3: Commit**

```bash
git add scripts/test-mcp-integration.sh
git commit -m "test: add vault team-scoped read/write to MCP integration test"
```

---

## Phase 2: Agent Cards + Team Lifecycle

### Task 9: Split tools.rs into modules (I4)

**Files:**
- Modify: `agentiso/src/mcp/tools.rs` (extract git tools)
- Create: `agentiso/src/mcp/git_tools.rs`
- Modify: `agentiso/src/mcp/mod.rs` (register new module)

Extract the 5 git tool handlers (`git_clone`, `git_status`, `git_commit`, `git_push`, `git_diff`) and their param structs into `git_tools.rs`. Keep them in the same `impl AgentisoServer` block if rmcp requires it (use a partial impl or `include!` macro).

**Step 1: Create git_tools.rs with extracted handlers**
**Step 2: Verify compilation**: `cargo build --manifest-path agentiso/Cargo.toml`
**Step 3: Run tests**: `cargo test --manifest-path agentiso/Cargo.toml`
**Step 4: Commit**

---

### Task 10: Add team_id to Workspace + PersistedState schema v3 (I7)

**Files:**
- Modify: `agentiso/src/workspace/mod.rs:73-91` (Workspace struct)
- Modify: `agentiso/src/workspace/mod.rs:118-130` (PersistedState struct)

**Step 1: Add team_id field to Workspace**

```rust
pub struct Workspace {
    // ... existing fields ...
    #[serde(default)]
    pub team_id: Option<String>,
}
```

**Step 2: Add TeamState and teams field to PersistedState**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamState {
    pub name: String,
    pub state: TeamLifecycleState,
    pub member_workspace_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub parent_team: Option<String>,
    pub max_vms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TeamLifecycleState {
    Creating,
    Ready,
    Working,
    Completing,
    Destroyed,
}
```

Add to PersistedState:
```rust
#[serde(default)]
pub teams: HashMap<String, TeamState>,
```

Bump schema_version to 3 in `save_state()`.

**Step 3: Write schema migration test**
**Step 4: Run tests**: `cargo test --manifest-path agentiso/Cargo.toml`
**Step 5: Commit**

---

### Task 11: Add global fork semaphore (I6)

**Files:**
- Modify: `agentiso/src/workspace/mod.rs` (add semaphore to WorkspaceManager)
- Modify: `agentiso/src/config.rs` (add max_concurrent_forks config)

**Step 1: Add config field**: `max_concurrent_forks: u32` (default 10)
**Step 2: Add `fork_semaphore: Arc<Semaphore>` to WorkspaceManager**
**Step 3: Acquire semaphore in fork() before doing work**
**Step 4: Write test**: concurrent forks are gated by semaphore
**Step 5: Run tests**
**Step 6: Commit**

---

### Task 12: Create team module scaffold

**Files:**
- Create: `agentiso/src/team/mod.rs`
- Create: `agentiso/src/team/agent_card.rs`
- Modify: `agentiso/src/lib.rs` or `agentiso/src/main.rs` (register module)

**Step 1: Create TeamManager struct**

```rust
pub struct TeamManager {
    workspace_manager: Arc<WorkspaceManager>,
    vault: Option<Arc<VaultManager>>,
}

impl TeamManager {
    pub async fn create_team(&self, name: &str, roles: Vec<RoleDef>, max_vms: u32) -> Result<TeamState> { ... }
    pub async fn destroy_team(&self, name: &str) -> Result<()> { ... }
    pub async fn team_status(&self, name: &str) -> Result<TeamStatusResponse> { ... }
}
```

**Step 2: Create AgentCard struct in agent_card.rs**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub role: String,
    pub description: String,
    pub skills: Vec<String>,
    pub endpoints: AgentEndpoints,
    pub status: String,
    pub team: String,
    pub workspace_id: String,
}
```

**Step 3: Write unit tests for AgentCard serialization**
**Step 4: Run tests**
**Step 5: Commit**

---

### Task 13: Implement team_create in TeamManager

**Files:**
- Modify: `agentiso/src/team/mod.rs`

**Step 1: Implement create_team()**

Logic:
1. Validate name (alphanumeric + hyphens, max 64 chars)
2. Check global VM cap (config.max_total_vms)
3. Create team vault directory: `teams/{name}/cards/`, `teams/{name}/tasks/`, `teams/{name}/notes/`
4. For each role: fork workspace from golden snapshot, set team_id on workspace
5. Write Agent Card for each member to vault
6. Apply intra-team nftables rules (per-peer allow)
7. Register TeamState in PersistedState
8. Return TeamState with member list

**Step 2: Write test for team creation**
**Step 3: Run tests**
**Step 4: Commit**

---

### Task 14: Implement team_destroy in TeamManager

**Files:**
- Modify: `agentiso/src/team/mod.rs`

Logic:
1. Look up team by name
2. Destroy all member workspaces in parallel (JoinSet)
3. Remove intra-team nftables rules
4. Clean vault entries (teams/{name}/ directory)
5. Remove from PersistedState
6. Handle sub-teams: recursively destroy children first

**Step 1: Implement destroy_team()**
**Step 2: Write test**
**Step 3: Run tests**
**Step 4: Commit**

---

### Task 15: Implement team_status in TeamManager

**Files:**
- Modify: `agentiso/src/team/mod.rs`

Returns: team state, member list with workspace info (name, IP, state), task board summary, resource usage.

**Step 1: Implement team_status()**
**Step 2: Write test**
**Step 3: Run tests**
**Step 4: Commit**

---

### Task 16: Add bundled `team` MCP tool (I5)

**Files:**
- Create: `agentiso/src/mcp/team_tools.rs`
- Modify: `agentiso/src/mcp/mod.rs`
- Modify: `agentiso/src/mcp/tools.rs` (register in tool_router, update server instructions, update test counts)

**Step 1: Create team_tools.rs with bundled tool handler**

```rust
#[derive(Debug, Deserialize, JsonSchema)]
struct TeamParams {
    action: String,  // "create", "destroy", "status"
    name: String,
    // ... action-specific optional fields
}
```

**Step 2: Update tool count test**: 27 → 28 (team tool added)
**Step 3: Update server instructions**: document team tool
**Step 4: Run tests**
**Step 5: Commit**

---

### Task 17: Add intra-team nftables rules

**Files:**
- Modify: `agentiso/src/network/nftables.rs`

**Step 1: Add apply_team_rules(team_id, member_ips) method**

Generate per-peer allow rules with `team-{id}-` comment prefix:
```
iifname "br-agentiso" ip saddr {a} ip daddr {b} oifname "br-agentiso" accept comment "team-{id}-{a}-to-{b}"
```

**Step 2: Add remove_team_rules(team_id) method** (reuses existing comment-prefix deletion)
**Step 3: Write tests for rule generation**
**Step 4: Run tests**
**Step 5: Commit**

---

### Task 18: E2E test for team lifecycle

**Files:**
- Modify: `scripts/test-mcp-integration.sh` (add team create/status/destroy steps)

**Step 1: Add team lifecycle test steps**

```python
# Step N: team(action="create") — create a 2-member team
# Step N+1: team(action="status") — verify team is ready
# Step N+2: exec in team member — verify workspace is accessible
# Step N+3: team(action="destroy") — destroy team
# Step N+4: workspace_list — verify team workspaces are gone
```

**Step 2: Update expected_tools set**: add "team"
**Step 3: Run integration test**
**Step 4: Commit**

---

### Task 19: Update docs and CLAUDE.md

**Files:**
- Modify: `CLAUDE.md` (update tool counts, test counts, current status)
- Modify: `AGENTS.md` (update for new team module)
- Modify: `docs/tools.md` (add team tool documentation)
- Modify: `README.md` (update feature list)

**Step 1: Update all docs**
**Step 2: Commit**

---

## Phases 3-6 (task-level outline)

### Phase 3: Vault-Backed Task Board
- Task 20: TaskBoard struct + task markdown format with YAML frontmatter parsing
- Task 21: TaskClaim vsock protocol message (atomic claim under lock)
- Task 22: Task dependency resolution (topological sort)
- Task 23: Auto-generated INDEX.md on task state change
- Task 24: E2E test for task board lifecycle

### Phase 4: Inter-Agent Messaging
- Task 25: Dual vsock connection per VM (C1 fix — message relay connection)
- Task 26: TeamMessage + TeamMessageReceived protocol types
- Task 27: MessageRelay host-side router (per-agent inbox, bounded queue)
- Task 28: team_message MCP tool + rate limit category (N4)
- Task 29: Guest HTTP API server (axum, port 8080) — /card, /health, /message, /files
- Task 30: Intra-team nftables rules for HTTP API access
- Task 31: E2E test for messaging (vsock relay + HTTP)

### Phase 5: Git Merge + Nested Teams
- Task 32: workspace_merge MCP tool (sequential strategy first)
- Task 33: Branch-per-source and cherry-pick merge strategies
- Task 34: CreateSubTeam protocol type + nested team creation
- Task 35: Budget inheritance + resource caps (global + per-team + nesting depth)
- Task 36: E2E test for merge + nested teams

### Phase 6: CLI + Observability
- Task 37: `agentiso orchestrate` team mode with depends_on DAG execution
- Task 38: `agentiso team-status` CLI command
- Task 39: Dashboard team view (ratatui)
- Task 40: Prometheus metrics for team operations
- Task 41: Final docs update + comprehensive e2e test suite
