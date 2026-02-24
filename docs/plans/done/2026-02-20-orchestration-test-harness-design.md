# Multi-VM Orchestration Test Harness Design

**Date**: 2026-02-20
**Status**: Approved

## Goal

Extend `scripts/test-mcp-integration.sh` with 5 new phases covering the untested multi-VM orchestration tools: `set_env`, `workspace_prepare`, `exec_parallel`, `swarm_run`, and `workspace_adopt`. Adds 23 new test steps (95 → 118 total) including vault_context integration and edge case validation.

## Architecture

Insert phases 7a-7e before the existing cleanup phase (Phase 7 → Phase 8). Reuse the existing Python driver helpers (`send_msg`, `recv_msg`, `pass_step`, `get_tool_result_text`). The existing primary workspace serves as the base for some tests; `workspace_prepare` creates a golden workspace for fork-based tests.

## Phase 7a: `set_env` (3 steps)

Uses the existing primary workspace (WORKSPACE_ID from Phase 1a).

| Step | Tool | Input | Assertion |
|------|------|-------|-----------|
| 1 | set_env | workspace=primary, vars={TEST_VAR: "hello", WORKER_ID: "main"} | count=2, status="set" |
| 2 | exec | workspace=primary, command="echo $TEST_VAR" | stdout contains "hello" |
| 3 | set_env | workspace=primary (after stop) | error: "VM must be Running" |

Step 3 requires stopping the workspace first, then restarting after the test. Alternative: use a separate stopped workspace if available, or skip if primary must stay running for later phases. Decision: stop primary, test, restart primary — validates stop/start cycle too.

**Revised approach**: Don't stop the primary — it's needed for later phases. Instead, create a throwaway workspace, stop it, test set_env against it, destroy it. This isolates the error test.

## Phase 7b: `workspace_prepare` (4 steps)

| Step | Tool | Input | Assertion |
|------|------|-------|-----------|
| 4 | workspace_prepare | name="prep-golden", setup_commands=["mkdir -p /workspace/data", "echo ready > /workspace/data/flag"] | snapshot_name="golden", workspace_id present |
| 5 | workspace_prepare | name="prep-golden" (same name) | reused_existing=true (idempotent) |
| 6 | workspace_prepare | name="../traversal" | error: name validation (invalid chars) |
| 7 | workspace_prepare | name="prep-fail", setup_commands=["false"] | error: setup command failed |

Step 7 creates a workspace that fails during setup. The handler should clean up the failed workspace. Verify by checking workspace_list doesn't include "prep-fail".

## Phase 7c: `exec_parallel` (6 steps)

Requires 2+ running workspaces. Fork from prep-golden's golden snapshot.

| Step | Tool | Input | Assertion |
|------|------|-------|-----------|
| 8 | workspace_fork | source=prep-golden, snapshot=golden, count=2 | 2 workspace IDs returned |
| 9 | exec_parallel | workspace_ids=[fork1, fork2], commands="hostname" (broadcast) | results.length=2, both exit_code=0, distinct hostnames |
| 10 | exec_parallel | workspace_ids=[fork1, fork2], commands=["echo worker-1", "echo worker-2"] (per-ws) | results[0].stdout="worker-1", results[1].stdout="worker-2" |
| 11 | exec_parallel | workspace_ids=[] | error: "must not be empty" |
| 12 | exec_parallel | workspace_ids=[fake-id] * 21 | error: "max length" or similar |
| 13 | exec_parallel | workspace_ids=[fork1, fork2], commands=["echo one"] (1 cmd, 2 ws) | error: "length must match" |

## Phase 7d: `swarm_run` + vault_context (7 steps)

Uses prep-golden/golden as the fork source. Tests the full orchestration pipeline including vault integration.

| Step | Tool | Input | Assertion |
|------|------|-------|-----------|
| 14 | vault | action=write, path="swarm-test/api-spec.md", content="# API Spec\nEndpoint: /health\nMethod: GET" | success |
| 15 | swarm_run | golden_workspace=prep-golden, snapshot=golden, tasks=[{name:"t1", command:"cat /workspace/context.md"}, {name:"t2", command:"cat /workspace/context.md"}, {name:"t3", command:"cat /workspace/context.md"}], shared_context="swarm test context", vault_context=[{kind:"read", query:"swarm-test/api-spec.md"}], cleanup=true | all 3 succeed, stdout contains "swarm test context" AND "API Spec", workers_destroyed=3 |
| 16 | swarm_run | golden_workspace=prep-golden, snapshot=golden, tasks=[{name:"m1", command:"cd /workspace && echo merge1 > file1.txt && git add . && git commit -m 'add file1'"}, {name:"m2", command:"cd /workspace && echo merge2 > file2.txt && git add . && git commit -m 'add file2'"}], merge_strategy="sequential", merge_target=prep-golden, cleanup=true | merge result present, succeeded=2 |
| 17 | exec | workspace=prep-golden, command="cat /workspace/file1.txt /workspace/file2.txt" | stdout contains "merge1" and "merge2" |
| 18 | swarm_run | tasks with duplicate name "dup" | error: "duplicate task name" |
| 19 | swarm_run | shared_context: 2 MiB string | error: "too large" |
| 20 | swarm_run | tasks: 21 entries | error: max length |

**Note on step 16**: The prep-golden workspace needs a git repo initialized at /workspace for merge to work. The workspace_prepare setup_commands should include `cd /workspace && git init && git add . && git commit -m 'init'`. Update step 4's setup_commands accordingly.

**Revised step 4 setup_commands**:
```json
["mkdir -p /workspace/data",
 "echo ready > /workspace/data/flag",
 "cd /workspace && git init && git config user.email test@test.com && git config user.name Test && git add . && git commit -m init"]
```

## Phase 7e: `workspace_adopt` (3 steps)

Tests ownership semantics. Limited by single-session test driver — can't truly test cross-session adoption. Focus on error paths and bulk response shape.

| Step | Tool | Input | Assertion |
|------|------|-------|-----------|
| 21 | workspace_adopt | workspace_id=primary, force=false | error: "already owned" |
| 22 | workspace_adopt | (no workspace_id — bulk) | response has "adopted" array, "summary" object |
| 23 | workspace_adopt | workspace_id=primary, force=true | error: "session still active" (activity <60s) |

## Cleanup Additions

Add to the existing finally block:

```python
# Phase 7 cleanup
for ws_id in FORK_WORKER_IDS:
    workspace_destroy(ws_id)
if PREP_GOLDEN_ID:
    workspace_destroy(PREP_GOLDEN_ID)
if PREP_FAIL_ID:
    workspace_destroy(PREP_FAIL_ID)
```

## Variables

New Python variables to track:

```python
PREP_GOLDEN_ID = None      # workspace_prepare result
PREP_FAIL_ID = None        # workspace_prepare failure test
FORK_WORKER_IDS = []       # exec_parallel fork workers
SET_ENV_STOPPED_ID = None  # stopped workspace for set_env error test
```

## Implementation Notes

1. All new phases are **conditional** — if workspace_prepare fails in step 4, skip steps 5-20 (fork/exec/swarm all depend on prep-golden).
2. Edge case steps (6, 7, 11-13, 18-20) are independent — they can run even if earlier happy-path steps fail, since they test validation before any VM interaction.
3. Timeout for swarm_run steps: 300s (3 VMs boot + exec + merge + cleanup).
4. Timeout for workspace_prepare: 180s (VM boot + setup commands).
5. The vault write in step 14 uses the existing vault path from config.toml.
6. Step count in script header and CLAUDE.md must be updated: 95 → 118.

## Swarm Team Assignment

5 agents, each implementing one phase plus its cleanup:

| Agent | Phase | Steps | Dependencies |
|-------|-------|-------|-------------|
| `set-env-agent` | 7a | 1-3 | None (uses existing primary workspace) |
| `prepare-agent` | 7b | 4-7 | None (creates own workspace) |
| `parallel-agent` | 7c | 8-13 | prep-golden from Phase 7b |
| `swarm-agent` | 7d | 14-20 | prep-golden from Phase 7b |
| `adopt-agent` | 7e | 21-23 | None (tests error paths on existing workspaces) |

Dependency chain: `set-env-agent` + `prepare-agent` (parallel) → `parallel-agent` + `swarm-agent` (parallel, after prepare) → `adopt-agent` (after all, cleans up)

**Integration approach**: Each agent writes their phase as a Python function. A coordinator merges them into the test script in order, updating step numbers, cleanup block, and variable declarations.
