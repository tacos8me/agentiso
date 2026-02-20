# Swarm Taming Design

**Date:** 2026-02-20
**Status:** Proposal
**Author:** workflow team

---

## Problem Statement

Parallel coding via MCP is currently too chatty. An agent orchestrating 5 parallel coding workers needs approximately **119 tool calls** through the current primitive-tool surface:

1. `workspace_prepare` (1 call)
2. `workspace_fork` with `count=5` (1 call, but returns 5 workspace IDs)
3. Per worker: `set_env` + `exec` + poll loop (`exec_background` start + N polls) + `git_status` = ~20 calls per worker
4. `workspace_merge` (1 call)
5. Per worker: `workspace_destroy` (5 calls)

The core issues:

- **No parallel blocking exec.** `exec` is blocking but single-workspace. To run commands across N workspaces concurrently, the agent must use `exec_background(action="start")` on each, then poll each with `exec_background(action="poll")` in a loop. Each poll is a separate MCP round-trip (~100ms). For 5 workers running 2-minute tasks, this is 5 start calls + ~300 poll calls (polling every 2s for 2 minutes across 5 workers).
- **No composite orchestration tool.** The `agentiso orchestrate` CLI command does exactly what agents need (fork, inject keys, exec, collect results, cleanup), but it reads TOML files from disk and is not exposed via MCP. Agents cannot call it.
- **No progress visibility during long runs.** Once workers are running, the only way to check progress is polling each one individually. There is no aggregate status view.

The existing `orchestrate.rs` engine (746 LOC at `agentiso/src/workspace/orchestrate.rs`) already solves the hard problems: semaphore-gated parallel forking, vault context resolution, worker lifecycle management, and result aggregation. It just needs an MCP-facing entry point.

## Proposed Tools

### Tool 1: `exec_parallel`

Execute the same or different commands across multiple workspaces concurrently, returning all results in a single response. This is the highest-impact standalone addition.

**Schema:**

```json
{
  "workspace_ids": ["worker-1", "worker-2", "worker-3"],
  "commands": ["cargo test", "cargo test", "cargo test"],
  "timeout_secs": 120,
  "workdir": "/workspace",
  "env": {"RUST_LOG": "debug"},
  "max_output_bytes": 262144
}
```

Parameter rules:
- `workspace_ids` (required): Array of workspace UUIDs or names. Max 20.
- `commands` (required): Either a single string (broadcast to all workspaces) or an array matching `workspace_ids` length (one command per workspace).
- `timeout_secs` (optional, default 120): Per-command timeout. All commands share the same timeout.
- `workdir` (optional): Working directory, applied to all commands.
- `env` (optional): Environment variables, applied to all commands.
- `max_output_bytes` (optional, default 262144): Per-command output limit.

**Response:**

```json
{
  "results": [
    {
      "workspace_id": "abc-123",
      "workspace_name": "worker-1",
      "exit_code": 0,
      "stdout": "...",
      "stderr": "",
      "timed_out": false
    },
    {
      "workspace_id": "def-456",
      "workspace_name": "worker-2",
      "exit_code": 1,
      "stdout": "",
      "stderr": "test failed",
      "timed_out": false
    }
  ],
  "summary": {
    "total": 3,
    "succeeded": 2,
    "failed": 1,
    "timed_out": 0,
    "elapsed_ms": 4230
  }
}
```

**Implementation:**

```rust
// In agentiso/src/mcp/tools.rs — new handler

async fn exec_parallel(&self, params: ExecParallelParams) -> Result<CallToolResult, McpError> {
    // 1. Validate: workspace_ids.len() <= 20, commands length matches
    // 2. Resolve all workspace IDs (fail fast if any unknown)
    // 3. Check ownership on all
    // 4. Rate limit: charge once per workspace (exec category)
    // 5. Spawn JoinSet with concurrent vsock exec calls
    // 6. Collect results, build summary
    // 7. Return structured JSON
}
```

The actual exec calls reuse `WorkspaceManager::exec()` (`agentiso/src/workspace/mod.rs:1669`), which already handles vsock client locking per-workspace. Multiple concurrent exec calls to different workspaces are safe because each workspace has its own `Arc<Mutex<VsockClient>>`.

**Rate limiting:** Each workspace execution counts as one `exec` category call. A 5-workspace parallel exec consumes 5 tokens from the exec bucket (60/min default). This prevents using `exec_parallel` to bypass rate limits.

**Error handling:** Partial success. Each result includes its own exit code and output. The response always contains one result per input workspace, even if some fail (failed ones get `exit_code: -1` and the error in `stderr`). The tool call itself only returns an MCP error if *all* workspace IDs are invalid or the rate limit is exceeded.

**Why not just multiple `exec` calls?** The MCP protocol does not support concurrent tool calls from a single agent turn. The agent must issue one tool call, wait for the response, then issue the next. `exec_parallel` makes a single round-trip do the work of N sequential ones.

### Tool 2: `swarm_run`

End-to-end parallel coding orchestration in a single MCP call. Wraps the existing `orchestrate.rs` engine with structured JSON input/output instead of TOML files.

**Schema:**

```json
{
  "golden_workspace": "my-project",
  "snapshot_name": "golden",
  "tasks": [
    {
      "name": "implement-auth",
      "command": "opencode run 'Implement JWT authentication in src/auth.rs'",
      "workdir": "/workspace"
    },
    {
      "name": "add-tests",
      "command": "opencode run 'Add unit tests for the auth module'",
      "workdir": "/workspace"
    }
  ],
  "env_vars": {
    "ANTHROPIC_API_KEY": "sk-..."
  },
  "merge_strategy": "branch-per-source",
  "merge_target": "my-project",
  "max_parallel": 4,
  "timeout_secs": 600,
  "cleanup": true
}
```

Parameter rules:
- `golden_workspace` (required): Name or UUID of the workspace to fork from.
- `snapshot_name` (required): Snapshot to fork from (must exist on golden workspace).
- `tasks` (required): Array of task definitions. Max 20. Each has `name` (unique), `command` (shell command to run), and optional `workdir`.
- `env_vars` (optional): Environment variables to inject into all workers via `set_env` before execution. This is how API keys reach the workers.
- `merge_strategy` (optional): If set, merge results from all successful workers back into `merge_target` after execution. One of `"sequential"`, `"branch-per-source"`, `"cherry-pick"`. If omitted, no merge is performed and workers are left running for manual inspection.
- `merge_target` (optional): Workspace to merge into. Required if `merge_strategy` is set. Can be the golden workspace or any other workspace.
- `max_parallel` (optional, default 4): Maximum concurrent workers during both fork and execution phases.
- `timeout_secs` (optional, default 600): Per-task execution timeout.
- `cleanup` (optional, default true): Whether to destroy worker VMs after execution. Set to `false` to keep workers alive for debugging.

**Response:**

```json
{
  "swarm_id": "swarm-20260220-143022",
  "tasks": [
    {
      "name": "implement-auth",
      "workspace_id": "abc-123",
      "success": true,
      "exit_code": 0,
      "stdout": "...",
      "stderr": "",
      "elapsed_ms": 45200
    },
    {
      "name": "add-tests",
      "workspace_id": "def-456",
      "success": true,
      "exit_code": 0,
      "stdout": "...",
      "stderr": "",
      "elapsed_ms": 38100
    }
  ],
  "merge": {
    "strategy": "branch-per-source",
    "target_workspace": "my-project",
    "results": [
      {"source": "implement-auth", "success": true, "commits_applied": 3},
      {"source": "add-tests", "success": true, "commits_applied": 2}
    ]
  },
  "summary": {
    "total_tasks": 2,
    "succeeded": 2,
    "failed": 0,
    "total_elapsed_ms": 52300,
    "workers_destroyed": 2
  }
}
```

**Implementation:**

The implementation reuses the existing orchestrate engine (`agentiso/src/workspace/orchestrate.rs`) but replaces the OpenCode-specific execution path with generic shell command execution:

```rust
// New file: agentiso/src/mcp/swarm_tools.rs

pub(crate) async fn handle_swarm_run(
    &self,
    params: SwarmRunParams,
) -> Result<CallToolResult, McpError> {
    // 1. Validate params (task count <= 20, unique names, snapshot exists)
    // 2. Rate limit: charge "create" category once per task (fork cost)
    // 3. Fork workers via WorkspaceManager::batch_fork()
    //    (reuses orchestrate.rs fork phase with JoinSet + Semaphore)
    // 4. Inject env_vars into each worker via set_env
    // 5. Execute commands in parallel (JoinSet, gated by max_parallel semaphore)
    // 6. Collect results
    // 7. If merge_strategy set: call handle_workspace_merge on successful workers
    // 8. If cleanup: destroy all workers
    // 9. Return structured JSON
}
```

Key differences from `orchestrate.rs`:
- Input is JSON (MCP params), not TOML file
- Executes arbitrary shell commands, not specifically `opencode run`
- Adds optional merge step (leveraging existing `workspace_merge` implementation at `agentiso/src/mcp/git_tools.rs:830`)
- Returns structured MCP response, not writes to filesystem

**Why not just expose the CLI `orchestrate` command as-is?** Three reasons:
1. The CLI reads a TOML file from disk; MCP tools need structured JSON parameters.
2. The CLI is opinionated about running `opencode run`; the MCP tool should accept arbitrary commands.
3. The CLI writes results to a filesystem directory; the MCP tool should return results in the response.

The refactoring strategy: extract the core fork-inject-exec-collect loop from `orchestrate.rs::execute()` into a shared `SwarmEngine` that both the CLI and MCP tool can call. The CLI wraps it with TOML parsing + filesystem output; the MCP tool wraps it with JSON params + MCP response.

**Rate limiting:** Fork operations are charged to the `create` category (5/min). A 5-task swarm consumes 5 create tokens upfront. Exec operations during the swarm are *not* individually rate-limited (the swarm is already gated by `max_parallel` and the fork rate limit caps total worker creation).

### Tool 3: `swarm_status` (stretch goal)

For long-running swarms (e.g., 10 workers each running 5-minute AI coding tasks), provide a way to check progress without blocking.

**Schema:**

```json
{
  "swarm_id": "swarm-20260220-143022"
}
```

**Response:**

```json
{
  "swarm_id": "swarm-20260220-143022",
  "state": "running",
  "workers": [
    {
      "name": "implement-auth",
      "workspace_id": "abc-123",
      "state": "executing",
      "elapsed_ms": 23400
    },
    {
      "name": "add-tests",
      "workspace_id": "def-456",
      "state": "completed",
      "exit_code": 0,
      "elapsed_ms": 18200
    }
  ],
  "progress": {
    "total": 2,
    "completed": 1,
    "running": 1,
    "failed": 0
  }
}
```

**Implementation:** This requires `swarm_run` to be refactored into a two-phase model:
1. `swarm_run` with a `background: true` parameter returns immediately with a `swarm_id`
2. `swarm_status` polls progress
3. A new `swarm_collect` action retrieves final results

This is more complex and only needed if swarm execution routinely exceeds the MCP response timeout. Deferring to Phase 3.

## Implementation Plan

### Phase 1: `exec_parallel` (~150 LOC, 1-2 sessions)

Files to modify:
- `agentiso/src/mcp/tools.rs` — Add `ExecParallelParams` struct (~20 LOC), handler method (~80 LOC), register in tool router
- `agentiso/src/mcp/tools.rs` — Add tool description and schema registration (~10 LOC)

Files to add:
- None. All code fits in the existing tools module.

Test additions:
- Unit tests for param validation (~40 LOC)
- Integration test step in `scripts/test-mcp-integration.sh` (~20 lines bash)

Dependencies: None. Uses existing `WorkspaceManager::exec()`.

Why first: Smallest scope, highest standalone value. Every swarm workflow benefits from parallel exec even without the full `swarm_run` composite. Also validates the pattern of multi-workspace tool calls before building the larger tool.

### Phase 2: `swarm_run` (~400 LOC, 2-3 sessions)

Files to modify:
- `agentiso/src/workspace/orchestrate.rs` — Extract core loop into `SwarmEngine` struct (~100 LOC refactor)
- `agentiso/src/mcp/tools.rs` — Register `swarm_run` tool

Files to add:
- `agentiso/src/mcp/swarm_tools.rs` — `SwarmRunParams`, handler, merge integration (~250 LOC)

Test additions:
- Unit tests for param validation, plan building (~50 LOC)
- Integration test in `scripts/test-mcp-integration.sh` (full fork-exec-merge cycle, ~40 lines bash)

Dependencies: Phase 1 (validates multi-workspace patterns). Also depends on `workspace_merge` (already implemented at `agentiso/src/mcp/git_tools.rs:830`).

### Phase 3: Wire DAG execution to TeamPlan types (~300 LOC, 2 sessions)

The `TeamPlan` and `TeamTaskDef` types with DAG validation already exist at `agentiso/src/workspace/orchestrate.rs:76-113`. Kahn's algorithm topological sort is implemented at line 171. What's missing is the execution engine that respects the DAG ordering.

Files to modify:
- `agentiso/src/workspace/orchestrate.rs` — Add `execute_team_plan()` that runs tasks in topological order, releasing downstream tasks as dependencies complete
- `agentiso/src/mcp/swarm_tools.rs` — Add `swarm_run` support for `depends_on` in task definitions

This would allow:

```json
{
  "golden_workspace": "my-project",
  "snapshot_name": "golden",
  "tasks": [
    {"name": "setup-db", "command": "...", "depends_on": []},
    {"name": "impl-api", "command": "...", "depends_on": ["setup-db"]},
    {"name": "impl-ui", "command": "...", "depends_on": ["setup-db"]},
    {"name": "integration-test", "command": "...", "depends_on": ["impl-api", "impl-ui"]}
  ]
}
```

The DAG executor would use the existing `validate_team_dag()` to compute execution order, then use a `JoinSet` with a notification channel: when a task completes, check which downstream tasks have all dependencies satisfied and spawn them.

### Phase 3b: `swarm_status` (stretch, ~200 LOC)

Only if Phase 2 reveals that blocking `swarm_run` calls hit MCP timeout limits. Requires an in-memory swarm state registry and background execution model.

## Estimated Impact on Tool Call Count

Current workflow for 5 parallel workers (119 calls):

```
workspace_prepare                      1
workspace_fork(count=5)                1
set_env x5                             5
exec_background(start) x5             5
exec_background(poll) x5 * ~20 polls  100
git_status x5                          5
workspace_merge                        1
workspace_destroy x5                   5
                                      ---
                                      ~123 calls
```

With `exec_parallel` only (Phase 1):

```
workspace_prepare                      1
workspace_fork(count=5)                1
exec_parallel (set_env equivalent)     1  (env param)
exec_parallel (run commands)           1
exec_parallel (git status)             1
workspace_merge                        1
workspace_destroy x5                   5
                                      ---
                                      ~11 calls  (91% reduction)
```

With `swarm_run` (Phase 2):

```
workspace_prepare                      1
swarm_run (fork+env+exec+merge+cleanup) 1
                                      ---
                                      2 calls    (98% reduction)
```

## Not Proposed (and why)

### Merging `team` and `workspace_fork`

These serve different purposes. `team` creates named roles with intra-team networking and message passing -- it models organizational structure. `workspace_fork` creates anonymous clones for parallel execution. A swarm of coding workers does not need named roles or messaging; it needs fork, exec, collect, merge. The tools should remain separate but composable (a team can fork workers; a swarm does not need a team).

### Push-based messaging / Server-Sent Events

Replacing the poll-based `exec_background` + `team(action="receive")` model with push notifications would reduce call count further. However:
- MCP's stdio transport does not natively support server-initiated messages to clients
- SSE transport exists but not all MCP clients support it
- The polling cost is already eliminated by `exec_parallel` (blocking, no polling needed)

This is worth revisiting when MCP protocol evolves, but not worth the complexity now.

### Multi-provider support in `swarm_run`

Supporting different LLM providers per worker (e.g., some workers use Claude, others use GPT-4) would require per-task `env_vars` overrides. The schema supports this trivially (add `env` to each task definition), but the use case is rare enough to defer. The current `env_vars` top-level field handles the common case (same provider for all workers).

### Streaming output from workers

Real-time streaming of worker stdout/stderr would be ideal for observability but requires either:
- SSE/WebSocket transport (not universally supported by MCP clients)
- A log-tailing MCP tool that reads from a shared filesystem

The existing `workspace_logs` tool already provides post-hoc log access. Combined with `swarm_status` (Phase 3b), this covers the observability need without streaming infrastructure.
