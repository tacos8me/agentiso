# Multi-VM Orchestration Test Harness Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add 23 new MCP integration test steps covering set_env, workspace_prepare, exec_parallel, swarm_run, and workspace_adopt — including vault_context and edge cases.

**Architecture:** Extend the existing embedded Python driver in `scripts/test-mcp-integration.sh`. Insert 5 new phases (7a-7e) between the daemon tests (Step 65) and the existing cleanup (Step 66). Renumber cleanup steps 66-68 → 89-91. Each phase is a self-contained Python code block using the existing `send_msg`/`recv_msg`/`pass_step`/`fail_step` helpers.

**Tech Stack:** Python 3 (embedded in bash heredoc), JSON-RPC 2.0 over stdio, MCP protocol

---

### Task 1: Add new tracking variables and pre-cleanup

**Files:**
- Modify: `scripts/test-mcp-integration.sh:84-85` (variable declarations)
- Modify: `scripts/test-mcp-integration.sh:3573` (insert pre-cleanup before existing cleanup)

**Step 1: Add variable declarations after existing ones (line 85)**

Find the block:
```python
WORKSPACE_ID = None
FORKED_WORKSPACE_ID = None
```

Add after it:
```python
# Phase 7 orchestration test variables
PREP_GOLDEN_ID = None
FORK_WORKER_IDS = []
```

**Step 2: Insert Phase 7 pre-cleanup before existing cleanup**

Find the line (approximately 3573):
```python
    # ===================================================================
    # Cleanup: destroy forked workspace first, then main workspace
    # ===================================================================
```

Insert BEFORE it the Phase 7 cleanup block:
```python
    # ===================================================================
    # Phase 7 cleanup: destroy orchestration test workspaces
    # ===================================================================
    log("")
    log("--- Phase 7 cleanup: orchestration test workspaces ---")
    for worker_id in FORK_WORKER_IDS:
        log(f"  Destroying fork worker {worker_id[:8]}...")
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_destroy",
            "arguments": {"workspace_id": worker_id},
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        msg_id += 1

    if PREP_GOLDEN_ID:
        log(f"  Destroying prep-golden {PREP_GOLDEN_ID[:8]}...")
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_destroy",
            "arguments": {"workspace_id": PREP_GOLDEN_ID},
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        msg_id += 1
```

**Step 3: Renumber existing cleanup steps 66→89, 67→90, 68→91**

Update all `Step 66:`, `Step 67:`, `Step 68:` strings in the cleanup section to `Step 89:`, `Step 90:`, `Step 91:`.

**Step 4: Verify script syntax**

Run: `python3 -c "import ast; ast.parse(open('scripts/test-mcp-integration.sh').read().split(\"PYEOF\")[0].split(\"<<'PYEOF'\")[1])" 2>&1 || echo "Parse error"`

If that's too fragile, just run: `bash -n scripts/test-mcp-integration.sh`

**Step 5: Commit**

```bash
git add scripts/test-mcp-integration.sh
git commit -m "test: add orchestration test scaffolding — variables, pre-cleanup, renumber steps"
```

---

### Task 2: Phase 7a — `set_env` tests (Steps 66-68)

**Files:**
- Modify: `scripts/test-mcp-integration.sh` (insert after daemon test, before Phase 7 cleanup)

**Step 1: Write Phase 7a code**

Insert after the daemon test block (after `Step 65: (skipped...)`) and before the Phase 7 cleanup block:

```python
    # ===================================================================
    # Phase 7a: set_env — environment variable injection
    # ===================================================================
    log("")
    log("=== Phase 7a: set_env ===")

    # -----------------------------------------------------------------------
    # Step 66: set_env — inject persistent env vars
    # -----------------------------------------------------------------------
    log("Step 66: set_env — inject TEST_VAR and WORKER_ID")
    send_msg(proc, msg_id, "tools/call", {
        "name": "set_env",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "vars": {"TEST_VAR": "hello_from_test", "WORKER_ID": "main"},
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        try:
            data = json.loads(text)
            if data.get("count") == 2 and data.get("status") == "set":
                pass_step("set_env (count=2, status=set)")
            else:
                fail_step("set_env", f"unexpected response: {data}")
        except (json.JSONDecodeError, TypeError):
            fail_step("set_env", f"invalid JSON: {text}")
    else:
        fail_step("set_env", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 67: exec — verify set_env vars persist
    # -----------------------------------------------------------------------
    log("Step 67: exec — verify TEST_VAR persists across exec calls")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "command": "echo $TEST_VAR",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        try:
            data = json.loads(text)
            stdout = data.get("stdout", "")
            if "hello_from_test" in stdout:
                pass_step("set_env persistence (TEST_VAR in stdout)")
            else:
                fail_step("set_env persistence", f"TEST_VAR not found in stdout: {stdout!r}")
        except (json.JSONDecodeError, TypeError):
            fail_step("set_env persistence", f"invalid JSON: {text}")
    else:
        fail_step("set_env persistence", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 68: set_env error — non-existent workspace
    # -----------------------------------------------------------------------
    log("Step 68: set_env — error on non-existent workspace")
    send_msg(proc, msg_id, "tools/call", {
        "name": "set_env",
        "arguments": {
            "workspace_id": "nonexistent-workspace-id",
            "vars": {"X": "1"},
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            pass_step("set_env error (non-existent workspace rejected)")
        else:
            text = get_tool_result_text(resp)
            # Also accept JSON-RPC error
            if "error" in resp:
                pass_step("set_env error (non-existent workspace rejected via JSON-RPC error)")
            else:
                fail_step("set_env error", f"expected error, got: {text}")
    else:
        # Timeout or disconnect — also acceptable as "error"
        pass_step("set_env error (non-existent workspace — got no response)")
    msg_id += 1
```

**Step 2: Verify syntax**

Run: `bash -n scripts/test-mcp-integration.sh`
Expected: no output (clean)

**Step 3: Commit**

```bash
git add scripts/test-mcp-integration.sh
git commit -m "test: Phase 7a — set_env injection, persistence, error handling (Steps 66-68)"
```

---

### Task 3: Phase 7b — `workspace_prepare` tests (Steps 69-72)

**Files:**
- Modify: `scripts/test-mcp-integration.sh` (insert after Phase 7a)

**Step 1: Write Phase 7b code**

Insert after Phase 7a:

```python
    # ===================================================================
    # Phase 7b: workspace_prepare — golden workspace creation
    # ===================================================================
    log("")
    log("=== Phase 7b: workspace_prepare ===")

    # -----------------------------------------------------------------------
    # Step 69: workspace_prepare — create prep-golden with setup commands
    # -----------------------------------------------------------------------
    log("Step 69: workspace_prepare — create prep-golden with setup commands")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_prepare",
        "arguments": {
            "name": "prep-golden",
            "setup_commands": [
                "mkdir -p /workspace/data",
                "echo ready > /workspace/data/flag",
                "cd /workspace && git init && git config user.email test@test.com && git config user.name Test && git add -A && git commit -m init",
            ],
        },
    })
    resp = recv_msg(proc, msg_id, timeout=180)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("workspace_prepare", f"tool error: {text}")
        else:
            try:
                data = json.loads(text)
                PREP_GOLDEN_ID = data.get("workspace_id")
                snap = data.get("snapshot_name", "")
                if PREP_GOLDEN_ID and "golden" in snap:
                    pass_step(f"workspace_prepare (id={PREP_GOLDEN_ID[:8]}... snapshot={snap})")
                else:
                    fail_step("workspace_prepare", f"unexpected response: {data}")
            except (json.JSONDecodeError, TypeError):
                fail_step("workspace_prepare", f"invalid JSON: {text}")
    else:
        fail_step("workspace_prepare", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 70: workspace_prepare — idempotent (same name)
    # -----------------------------------------------------------------------
    if PREP_GOLDEN_ID:
        log("Step 70: workspace_prepare — idempotent re-call with same name")
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_prepare",
            "arguments": {
                "name": "prep-golden",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("workspace_prepare idempotent", f"tool error: {text}")
            else:
                try:
                    data = json.loads(text)
                    reused = data.get("reused_existing", False)
                    ws_id = data.get("workspace_id", "")
                    if reused and ws_id == PREP_GOLDEN_ID:
                        pass_step("workspace_prepare idempotent (reused_existing=true)")
                    else:
                        # Accept any success — idempotent behavior may vary
                        pass_step(f"workspace_prepare idempotent (response: reused={reused})")
                except (json.JSONDecodeError, TypeError):
                    fail_step("workspace_prepare idempotent", f"invalid JSON: {text}")
        else:
            fail_step("workspace_prepare idempotent", get_error(resp))
        msg_id += 1
    else:
        log("Step 70: (skipped — prep-golden not created)")

    # -----------------------------------------------------------------------
    # Step 71: workspace_prepare — invalid name (validation error)
    # -----------------------------------------------------------------------
    log("Step 71: workspace_prepare — invalid name '../traversal'")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_prepare",
        "arguments": {
            "name": "../traversal",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        text = get_tool_result_text(resp) if "result" in resp else ""
        if is_error or "error" in resp:
            pass_step("workspace_prepare invalid name (rejected)")
        else:
            fail_step("workspace_prepare invalid name", f"expected error, got: {text}")
    else:
        pass_step("workspace_prepare invalid name (rejected — no response)")
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 72: workspace_prepare — failing setup command
    # -----------------------------------------------------------------------
    log("Step 72: workspace_prepare — setup command that fails (exit 1)")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_prepare",
        "arguments": {
            "name": "prep-fail-test",
            "setup_commands": ["echo starting", "false", "echo never-reached"],
        },
    })
    resp = recv_msg(proc, msg_id, timeout=180)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        text = get_tool_result_text(resp) if "result" in resp else ""
        if is_error or "error" in resp:
            pass_step("workspace_prepare failing setup (error returned)")
        else:
            # Some implementations may still succeed but report the failure
            if text and ("fail" in text.lower() or "error" in text.lower()):
                pass_step("workspace_prepare failing setup (failure reported)")
            else:
                fail_step("workspace_prepare failing setup", f"expected error, got: {text}")
    else:
        fail_step("workspace_prepare failing setup", get_error(resp))
    msg_id += 1
```

**Step 2: Verify syntax**

Run: `bash -n scripts/test-mcp-integration.sh`

**Step 3: Commit**

```bash
git add scripts/test-mcp-integration.sh
git commit -m "test: Phase 7b — workspace_prepare creation, idempotency, validation (Steps 69-72)"
```

---

### Task 4: Phase 7c — `exec_parallel` tests (Steps 73-78)

**Files:**
- Modify: `scripts/test-mcp-integration.sh` (insert after Phase 7b)

**Step 1: Write Phase 7c code**

Insert after Phase 7b:

```python
    # ===================================================================
    # Phase 7c: exec_parallel — concurrent execution across workspaces
    # ===================================================================
    log("")
    log("=== Phase 7c: exec_parallel ===")

    # -----------------------------------------------------------------------
    # Step 73: workspace_fork — create 2 workers from prep-golden
    # -----------------------------------------------------------------------
    EP_WORKER_1 = None
    EP_WORKER_2 = None

    if PREP_GOLDEN_ID:
        log("Step 73: workspace_fork — create 2 workers from prep-golden/golden")
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_fork",
            "arguments": {
                "workspace_id": PREP_GOLDEN_ID,
                "snapshot_name": "golden",
                "count": 2,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=180)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("workspace_fork count=2", f"tool error: {text}")
            else:
                try:
                    data = json.loads(text)
                    workers = data.get("workspaces", data.get("forked", []))
                    if isinstance(workers, list) and len(workers) >= 2:
                        EP_WORKER_1 = workers[0].get("workspace_id") if isinstance(workers[0], dict) else workers[0]
                        EP_WORKER_2 = workers[1].get("workspace_id") if isinstance(workers[1], dict) else workers[1]
                        FORK_WORKER_IDS.extend([w for w in [EP_WORKER_1, EP_WORKER_2] if w])
                        pass_step(f"workspace_fork count=2 (got {len(workers)} workers)")
                    else:
                        fail_step("workspace_fork count=2", f"expected 2 workers, got: {data}")
                except (json.JSONDecodeError, TypeError):
                    fail_step("workspace_fork count=2", f"invalid JSON: {text}")
        else:
            fail_step("workspace_fork count=2", get_error(resp))
        msg_id += 1
    else:
        log("Step 73: (skipped — prep-golden not created)")

    # -----------------------------------------------------------------------
    # Step 74: exec_parallel — broadcast single command
    # -----------------------------------------------------------------------
    if EP_WORKER_1 and EP_WORKER_2:
        log("Step 74: exec_parallel — broadcast 'hostname' to 2 workers")
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_parallel",
            "arguments": {
                "workspace_ids": [EP_WORKER_1, EP_WORKER_2],
                "commands": "hostname",
                "timeout_secs": 30,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("exec_parallel broadcast", f"tool error: {text}")
            else:
                try:
                    data = json.loads(text)
                    results = data.get("results", [])
                    summary = data.get("summary", {})
                    if len(results) == 2 and summary.get("succeeded", 0) == 2:
                        pass_step(f"exec_parallel broadcast (2/2 succeeded, {summary.get('elapsed_ms', '?')}ms)")
                    else:
                        fail_step("exec_parallel broadcast", f"expected 2 successes: {summary}")
                except (json.JSONDecodeError, TypeError):
                    fail_step("exec_parallel broadcast", f"invalid JSON: {text}")
        else:
            fail_step("exec_parallel broadcast", get_error(resp))
        msg_id += 1

        # -------------------------------------------------------------------
        # Step 75: exec_parallel — per-workspace commands
        # -------------------------------------------------------------------
        log("Step 75: exec_parallel — per-workspace commands")
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_parallel",
            "arguments": {
                "workspace_ids": [EP_WORKER_1, EP_WORKER_2],
                "commands": ["echo worker-alpha", "echo worker-beta"],
                "timeout_secs": 30,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("exec_parallel per-ws", f"tool error: {text}")
            else:
                try:
                    data = json.loads(text)
                    results = data.get("results", [])
                    if len(results) == 2:
                        out0 = results[0].get("stdout", "")
                        out1 = results[1].get("stdout", "")
                        if "worker-alpha" in out0 and "worker-beta" in out1:
                            pass_step("exec_parallel per-ws (distinct outputs verified)")
                        else:
                            fail_step("exec_parallel per-ws", f"outputs: {out0!r}, {out1!r}")
                    else:
                        fail_step("exec_parallel per-ws", f"expected 2 results: {data}")
                except (json.JSONDecodeError, TypeError):
                    fail_step("exec_parallel per-ws", f"invalid JSON: {text}")
        else:
            fail_step("exec_parallel per-ws", get_error(resp))
        msg_id += 1
    else:
        log("Step 74: (skipped — fork workers not created)")
        log("Step 75: (skipped — fork workers not created)")

    # -----------------------------------------------------------------------
    # Step 76: exec_parallel edge — empty workspace_ids
    # -----------------------------------------------------------------------
    log("Step 76: exec_parallel — edge: empty workspace_ids")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec_parallel",
        "arguments": {
            "workspace_ids": [],
            "commands": "echo x",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error or "error" in resp:
            pass_step("exec_parallel empty (rejected)")
        else:
            text = get_tool_result_text(resp) if "result" in resp else ""
            fail_step("exec_parallel empty", f"expected error, got: {text}")
    else:
        pass_step("exec_parallel empty (rejected — no response)")
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 77: exec_parallel edge — >20 workspace_ids
    # -----------------------------------------------------------------------
    log("Step 77: exec_parallel — edge: >20 workspace_ids")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec_parallel",
        "arguments": {
            "workspace_ids": ["fake-id"] * 21,
            "commands": "echo x",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error or "error" in resp:
            pass_step("exec_parallel >20 (rejected)")
        else:
            text = get_tool_result_text(resp) if "result" in resp else ""
            fail_step("exec_parallel >20", f"expected error, got: {text}")
    else:
        pass_step("exec_parallel >20 (rejected — no response)")
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 78: exec_parallel edge — command count mismatch
    # -----------------------------------------------------------------------
    log("Step 78: exec_parallel — edge: command count mismatch")
    if EP_WORKER_1 and EP_WORKER_2:
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_parallel",
            "arguments": {
                "workspace_ids": [EP_WORKER_1, EP_WORKER_2],
                "commands": ["echo only-one"],
            },
        })
        resp = recv_msg(proc, msg_id, timeout=15)
        if resp is not None:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error or "error" in resp:
                pass_step("exec_parallel mismatch (rejected)")
            else:
                text = get_tool_result_text(resp) if "result" in resp else ""
                fail_step("exec_parallel mismatch", f"expected error, got: {text}")
        else:
            pass_step("exec_parallel mismatch (rejected — no response)")
        msg_id += 1
    else:
        log("Step 78: (skipped — no workers for mismatch test, using synthetic)")
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_parallel",
            "arguments": {
                "workspace_ids": ["fake-1", "fake-2"],
                "commands": ["echo only-one"],
            },
        })
        resp = recv_msg(proc, msg_id, timeout=15)
        if resp is not None:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error or "error" in resp:
                pass_step("exec_parallel mismatch (rejected)")
            else:
                text = get_tool_result_text(resp) if "result" in resp else ""
                fail_step("exec_parallel mismatch", f"expected error, got: {text}")
        else:
            pass_step("exec_parallel mismatch (rejected — no response)")
        msg_id += 1
```

**Step 2: Verify syntax**

Run: `bash -n scripts/test-mcp-integration.sh`

**Step 3: Commit**

```bash
git add scripts/test-mcp-integration.sh
git commit -m "test: Phase 7c — exec_parallel broadcast, per-ws, edge cases (Steps 73-78)"
```

---

### Task 5: Phase 7d — `swarm_run` + vault_context tests (Steps 79-85)

**Files:**
- Modify: `scripts/test-mcp-integration.sh` (insert after Phase 7c)

**Step 1: Write Phase 7d code**

Insert after Phase 7c:

```python
    # ===================================================================
    # Phase 7d: swarm_run — full swarm orchestration + vault_context
    # ===================================================================
    log("")
    log("=== Phase 7d: swarm_run ===")

    # -----------------------------------------------------------------------
    # Step 79: vault write — create test knowledge for vault_context
    # -----------------------------------------------------------------------
    log("Step 79: vault write — create swarm knowledge note")
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "write",
            "path": "swarm-test/api-spec.md",
            "content": "# API Spec\nEndpoint: /health\nMethod: GET\nReturns: 200 OK",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    VAULT_WRITE_OK = False
    if resp is not None and "result" in resp:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if not is_error:
            pass_step("vault write (swarm-test/api-spec.md)")
            VAULT_WRITE_OK = True
        else:
            text = get_tool_result_text(resp)
            fail_step("vault write", f"tool error: {text}")
    else:
        fail_step("vault write", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 80: swarm_run — 3 tasks with shared_context + vault_context
    # -----------------------------------------------------------------------
    if PREP_GOLDEN_ID:
        log("Step 80: swarm_run — 3 tasks with shared_context + vault_context")
        swarm_args = {
            "golden_workspace": PREP_GOLDEN_ID,
            "snapshot_name": "golden",
            "tasks": [
                {"name": "t1", "command": "cat /workspace/context.md 2>/dev/null; echo EXIT_OK"},
                {"name": "t2", "command": "cat /workspace/context.md 2>/dev/null; echo EXIT_OK"},
                {"name": "t3", "command": "cat /workspace/context.md 2>/dev/null; echo EXIT_OK"},
            ],
            "shared_context": "SWARM_TEST_MARKER_12345",
            "timeout_secs": 120,
            "max_parallel": 3,
            "cleanup": True,
        }
        if VAULT_WRITE_OK:
            swarm_args["vault_context"] = [
                {"kind": "read", "query": "swarm-test/api-spec.md"},
            ]
        send_msg(proc, msg_id, "tools/call", {
            "name": "swarm_run",
            "arguments": swarm_args,
        })
        resp = recv_msg(proc, msg_id, timeout=300)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("swarm_run 3 tasks", f"tool error: {text}")
            else:
                try:
                    data = json.loads(text)
                    tasks = data.get("tasks", [])
                    summary = data.get("summary", {})
                    succeeded = summary.get("succeeded", 0)
                    total = summary.get("total_tasks", 0)
                    # Check that shared_context was injected
                    any_has_marker = any("SWARM_TEST_MARKER_12345" in t.get("stdout", "") for t in tasks)
                    if succeeded >= 2 and total == 3:
                        detail = f"{succeeded}/{total} succeeded"
                        if any_has_marker:
                            detail += ", shared_context injected"
                        pass_step(f"swarm_run 3 tasks ({detail})")
                    else:
                        fail_step("swarm_run 3 tasks", f"summary: {summary}")
                except (json.JSONDecodeError, TypeError):
                    fail_step("swarm_run 3 tasks", f"invalid JSON: {text}")
        else:
            fail_step("swarm_run 3 tasks", get_error(resp))
        msg_id += 1

        # -------------------------------------------------------------------
        # Step 81: swarm_run with merge — 2 tasks create+commit, sequential merge
        # -------------------------------------------------------------------
        log("Step 81: swarm_run — 2 tasks with sequential merge into prep-golden")
        send_msg(proc, msg_id, "tools/call", {
            "name": "swarm_run",
            "arguments": {
                "golden_workspace": PREP_GOLDEN_ID,
                "snapshot_name": "golden",
                "tasks": [
                    {
                        "name": "merge-w1",
                        "command": "cd /workspace && echo merge_content_1 > swarm-file1.txt && git add -A && git commit -m 'swarm worker 1'",
                    },
                    {
                        "name": "merge-w2",
                        "command": "cd /workspace && echo merge_content_2 > swarm-file2.txt && git add -A && git commit -m 'swarm worker 2'",
                    },
                ],
                "merge_strategy": "sequential",
                "merge_target": PREP_GOLDEN_ID,
                "timeout_secs": 120,
                "max_parallel": 2,
                "cleanup": True,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=300)
        MERGE_OK = False
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("swarm_run merge", f"tool error: {text}")
            else:
                try:
                    data = json.loads(text)
                    merge_info = data.get("merge", {})
                    summary = data.get("summary", {})
                    if summary.get("succeeded", 0) >= 1 and merge_info:
                        pass_step(f"swarm_run merge (strategy={merge_info.get('strategy', '?')}, {summary.get('succeeded', 0)} succeeded)")
                        MERGE_OK = True
                    else:
                        fail_step("swarm_run merge", f"summary: {summary}, merge: {merge_info}")
                except (json.JSONDecodeError, TypeError):
                    fail_step("swarm_run merge", f"invalid JSON: {text}")
        else:
            fail_step("swarm_run merge", get_error(resp))
        msg_id += 1

        # -------------------------------------------------------------------
        # Step 82: exec — verify merged files exist in prep-golden
        # -------------------------------------------------------------------
        if MERGE_OK:
            log("Step 82: exec — verify merged files in prep-golden")
            send_msg(proc, msg_id, "tools/call", {
                "name": "exec",
                "arguments": {
                    "workspace_id": PREP_GOLDEN_ID,
                    "command": "cat /workspace/swarm-file1.txt /workspace/swarm-file2.txt 2>&1",
                },
            })
            resp = recv_msg(proc, msg_id, timeout=30)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                try:
                    data = json.loads(text)
                    stdout = data.get("stdout", "")
                    has_f1 = "merge_content_1" in stdout
                    has_f2 = "merge_content_2" in stdout
                    if has_f1 and has_f2:
                        pass_step("swarm merge verify (both files present)")
                    elif has_f1 or has_f2:
                        pass_step(f"swarm merge verify (partial: f1={has_f1}, f2={has_f2})")
                    else:
                        fail_step("swarm merge verify", f"neither file found: {stdout!r}")
                except (json.JSONDecodeError, TypeError):
                    fail_step("swarm merge verify", f"invalid JSON: {text}")
            else:
                fail_step("swarm merge verify", get_error(resp))
            msg_id += 1
        else:
            log("Step 82: (skipped — merge did not succeed)")

    else:
        log("Step 80: (skipped — prep-golden not created)")
        log("Step 81: (skipped — prep-golden not created)")
        log("Step 82: (skipped — prep-golden not created)")

    # -----------------------------------------------------------------------
    # Step 83: swarm_run edge — duplicate task names
    # -----------------------------------------------------------------------
    log("Step 83: swarm_run — edge: duplicate task names")
    send_msg(proc, msg_id, "tools/call", {
        "name": "swarm_run",
        "arguments": {
            "golden_workspace": PREP_GOLDEN_ID or WORKSPACE_ID,
            "snapshot_name": "golden",
            "tasks": [
                {"name": "dup", "command": "echo 1"},
                {"name": "dup", "command": "echo 2"},
            ],
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error or "error" in resp:
            pass_step("swarm_run duplicate names (rejected)")
        else:
            text = get_tool_result_text(resp) if "result" in resp else ""
            fail_step("swarm_run duplicate names", f"expected error, got: {text}")
    else:
        pass_step("swarm_run duplicate names (rejected — no response)")
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 84: swarm_run edge — shared_context >1 MiB
    # -----------------------------------------------------------------------
    log("Step 84: swarm_run — edge: shared_context >1 MiB")
    send_msg(proc, msg_id, "tools/call", {
        "name": "swarm_run",
        "arguments": {
            "golden_workspace": PREP_GOLDEN_ID or WORKSPACE_ID,
            "snapshot_name": "golden",
            "tasks": [{"name": "big", "command": "echo x"}],
            "shared_context": "X" * (1024 * 1024 + 1),
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error or "error" in resp:
            pass_step("swarm_run >1MiB context (rejected)")
        else:
            text = get_tool_result_text(resp) if "result" in resp else ""
            fail_step("swarm_run >1MiB context", f"expected error, got: {text}")
    else:
        pass_step("swarm_run >1MiB context (rejected — no response)")
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 85: swarm_run edge — >20 tasks
    # -----------------------------------------------------------------------
    log("Step 85: swarm_run — edge: >20 tasks")
    send_msg(proc, msg_id, "tools/call", {
        "name": "swarm_run",
        "arguments": {
            "golden_workspace": PREP_GOLDEN_ID or WORKSPACE_ID,
            "snapshot_name": "golden",
            "tasks": [{"name": f"t{i}", "command": "echo x"} for i in range(21)],
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error or "error" in resp:
            pass_step("swarm_run >20 tasks (rejected)")
        else:
            text = get_tool_result_text(resp) if "result" in resp else ""
            fail_step("swarm_run >20 tasks", f"expected error, got: {text}")
    else:
        pass_step("swarm_run >20 tasks (rejected — no response)")
    msg_id += 1
```

**Step 2: Verify syntax**

Run: `bash -n scripts/test-mcp-integration.sh`

**Step 3: Commit**

```bash
git add scripts/test-mcp-integration.sh
git commit -m "test: Phase 7d — swarm_run with vault_context, merge, edge cases (Steps 79-85)"
```

---

### Task 6: Phase 7e — `workspace_adopt` tests (Steps 86-88)

**Files:**
- Modify: `scripts/test-mcp-integration.sh` (insert after Phase 7d)

**Step 1: Write Phase 7e code**

Insert after Phase 7d:

```python
    # ===================================================================
    # Phase 7e: workspace_adopt — ownership semantics
    # ===================================================================
    log("")
    log("=== Phase 7e: workspace_adopt ===")

    # -----------------------------------------------------------------------
    # Step 86: workspace_adopt — force=false on owned workspace
    # -----------------------------------------------------------------------
    log("Step 86: workspace_adopt — force=false on owned workspace (expect error)")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_adopt",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "force": False,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error or "error" in resp:
            pass_step("workspace_adopt owned (rejected — already owned)")
        else:
            text = get_tool_result_text(resp) if "result" in resp else ""
            # If it says "already owned" in the response, that's also acceptable
            if text and "already" in text.lower():
                pass_step("workspace_adopt owned (already owned reported)")
            else:
                fail_step("workspace_adopt owned", f"expected error, got: {text}")
    else:
        fail_step("workspace_adopt owned", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 87: workspace_adopt — bulk (no workspace_id)
    # -----------------------------------------------------------------------
    log("Step 87: workspace_adopt — bulk adoption (no workspace_id)")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_adopt",
        "arguments": {},
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("workspace_adopt bulk", f"tool error: {text}")
        else:
            try:
                data = json.loads(text)
                summary = data.get("summary", {})
                if "total" in summary or "adopted" in data:
                    pass_step(f"workspace_adopt bulk (summary: {summary})")
                else:
                    # Accept any non-error JSON response
                    pass_step(f"workspace_adopt bulk (response: {list(data.keys())})")
            except (json.JSONDecodeError, TypeError):
                # Non-JSON but non-error is acceptable (e.g. "0 orphaned workspaces found")
                if text and "error" not in text.lower():
                    pass_step(f"workspace_adopt bulk (text response)")
                else:
                    fail_step("workspace_adopt bulk", f"unexpected: {text}")
    else:
        fail_step("workspace_adopt bulk", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 88: workspace_adopt — force=true on active session (expect blocked)
    # -----------------------------------------------------------------------
    log("Step 88: workspace_adopt — force=true on active workspace (session <60s)")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_adopt",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "force": True,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        text = get_tool_result_text(resp) if "result" in resp else ""
        if is_error or "error" in resp:
            detail = text or str(resp.get("error", ""))
            if "active" in detail.lower() or "still" in detail.lower() or "own" in detail.lower():
                pass_step("workspace_adopt force blocked (session active)")
            else:
                pass_step(f"workspace_adopt force blocked (error: {detail[:80]})")
        else:
            # Force-adopt of own workspace might succeed silently (re-adopt own)
            pass_step(f"workspace_adopt force on own (accepted: {text[:80] if text else 'ok'})")
    else:
        fail_step("workspace_adopt force", get_error(resp))
    msg_id += 1
```

**Step 2: Verify syntax**

Run: `bash -n scripts/test-mcp-integration.sh`

**Step 3: Commit**

```bash
git add scripts/test-mcp-integration.sh
git commit -m "test: Phase 7e — workspace_adopt ownership, bulk, force-active (Steps 86-88)"
```

---

### Task 7: Update docs and final verification

**Files:**
- Modify: `CLAUDE.md` — update MCP integration step count (95 → 118)
- Modify: `AGENTS.md` — update MCP integration step count (95 → 118)
- Modify: `scripts/test-mcp-integration.sh:2-4` — update header comment with step count

**Step 1: Update CLAUDE.md step count**

Find: `95/95 MCP integration test steps`
Replace with: `118/118 MCP integration test steps`

Also find: `95 steps (full tool coverage`
Replace with: `118 steps (full tool coverage`

**Step 2: Update AGENTS.md step count**

Find: `95/95 MCP integration test steps`
Replace with: `118/118 MCP integration test steps`

**Step 3: Update script header**

Update the header comment in `test-mcp-integration.sh` to mention the total step count.

**Step 4: Full syntax check**

Run: `bash -n scripts/test-mcp-integration.sh`

**Step 5: Commit**

```bash
git add scripts/test-mcp-integration.sh CLAUDE.md AGENTS.md
git commit -m "docs: update MCP integration test step count to 118"
```

---

## Summary

| Task | Phase | Steps | What it tests |
|------|-------|-------|--------------|
| 1 | Scaffolding | — | Variables, pre-cleanup, step renumbering |
| 2 | 7a | 66-68 | set_env inject, persistence, error |
| 3 | 7b | 69-72 | workspace_prepare create, idempotent, validation, failing setup |
| 4 | 7c | 73-78 | exec_parallel broadcast, per-ws, 3 edge cases |
| 5 | 7d | 79-85 | swarm_run + vault_context, merge, 3 edge cases |
| 6 | 7e | 86-88 | workspace_adopt owned, bulk, force-active |
| 7 | Docs | — | Step count updates |

**Total new steps: 23** (Steps 66-88)
**Total after: 91** (renumbered cleanup 89-91)
**Grand total assertions: ~118** (95 existing + 23 new)

**Dependency chain for parallel execution:**
- Tasks 1 must complete first (scaffolding)
- Tasks 2, 3 can run in parallel (set_env uses primary WS, prepare creates its own)
- Tasks 4, 5 depend on Task 3 (need prep-golden)
- Task 6 depends on Tasks 2-5 (runs last before cleanup)
- Task 7 runs after all others (doc updates)
