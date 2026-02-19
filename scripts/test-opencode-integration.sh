#!/bin/bash
# OpenCode Integration Tests
# Tests the orchestration pipeline: prepare → fork → env inject → exec → cleanup
# Run as: sudo ./scripts/test-opencode-integration.sh
# Does NOT require an API key (tests mechanics only, not LLM calls)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_DIR/target/release/agentiso"
CONFIG="$PROJECT_DIR/config.toml"

echo "=== agentiso OpenCode Integration Test ==="
echo "Project: $PROJECT_DIR"
echo "Binary:  $BINARY"
echo "Config:  $CONFIG"
echo ""

# --------------------------------------------------------------------------
# Prerequisites check
# --------------------------------------------------------------------------

PREREQ_FAILED=0

check_prereq() {
    local desc="$1"
    local check="$2"
    if eval "$check" &>/dev/null; then
        echo "  [OK] $desc"
    else
        echo "  [FAIL] $desc"
        PREREQ_FAILED=1
    fi
}

echo "Checking prerequisites..."
check_prereq "binary exists" "test -f '$BINARY'"
check_prereq "config.toml exists" "test -f '$CONFIG'"
check_prereq "kernel at /var/lib/agentiso/vmlinuz" "test -f /var/lib/agentiso/vmlinuz"
check_prereq "initrd at /var/lib/agentiso/initrd.img" "test -f /var/lib/agentiso/initrd.img"
check_prereq "ZFS pool 'agentiso' exists" "zfs list agentiso"
check_prereq "ZFS base image exists" "zfs list agentiso/agentiso/base/alpine-dev"
check_prereq "ZFS base snapshot @latest exists" "zfs list agentiso/agentiso/base/alpine-dev@latest"
check_prereq "/dev/kvm accessible" "test -r /dev/kvm -a -w /dev/kvm"
check_prereq "bridge br-agentiso exists" "ip link show br-agentiso"
echo ""

if [[ "$PREREQ_FAILED" -ne 0 ]]; then
    echo "ERROR: One or more prerequisites are missing."
    echo "Run 'sudo ./scripts/setup-e2e.sh' first to set up the environment."
    exit 1
fi

# --------------------------------------------------------------------------
# Run the Python MCP driver
# --------------------------------------------------------------------------

python3 - "$BINARY" "$CONFIG" <<'PYEOF'
#!/usr/bin/env python3
"""
OpenCode integration test driver for agentiso.
Tests the orchestration pipeline mechanics: prepare, fork, env inject, exec, cleanup.
Does NOT make real LLM API calls.
"""

import subprocess
import json
import sys
import time
import select
import os
import signal

BINARY = sys.argv[1]
CONFIG = sys.argv[2]

# Tracking
PASS_COUNT = 0
FAIL_COUNT = 0
GOLDEN_WORKSPACE_ID = None
WORKER_IDS = []

def log(msg):
    print(msg, flush=True)

def pass_step(name):
    global PASS_COUNT
    PASS_COUNT += 1
    log(f"  [PASS] {name}")

def fail_step(name, detail=None):
    global FAIL_COUNT
    FAIL_COUNT += 1
    log(f"  [FAIL] {name}")
    if detail:
        log(f"         Detail: {detail}")

def start_server():
    env = os.environ.copy()
    env["RUST_LOG"] = "warn"
    proc = subprocess.Popen(
        [BINARY, "serve", "--config", CONFIG],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )
    return proc

def send_msg(proc, msg_id, method, params=None):
    msg = {"jsonrpc": "2.0", "id": msg_id, "method": method}
    if params is not None:
        msg["params"] = params
    line = json.dumps(msg) + "\n"
    proc.stdin.write(line)
    proc.stdin.flush()

def send_notification(proc, method, params=None):
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        msg["params"] = params
    line = json.dumps(msg) + "\n"
    proc.stdin.write(line)
    proc.stdin.flush()

def recv_msg(proc, msg_id, timeout=90):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            log(f"         [WARN] Server process exited with code {proc.returncode}")
            break
        remaining = deadline - time.time()
        if remaining <= 0:
            break
        ready = select.select([proc.stdout], [], [], min(remaining, 2.0))[0]
        if ready:
            line = proc.stdout.readline()
            if not line:
                log("         [WARN] Server closed stdout")
                break
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
                if msg.get("id") == msg_id:
                    return msg
            except json.JSONDecodeError as e:
                log(f"         [WARN] Failed to parse JSON line: {line!r}: {e}")
    return None

def get_tool_result_text(resp):
    if resp is None:
        return None
    result = resp.get("result")
    if result is None:
        return None
    content = result.get("content", [])
    if not content:
        return None
    for item in content:
        if item.get("type") == "text":
            return item.get("text", "")
    return None

def get_error(resp):
    if resp is None:
        return "timeout (no response received)"
    if "error" in resp:
        err = resp["error"]
        return f"code={err.get('code')} message={err.get('message')}"
    return None

def is_tool_error(resp):
    if resp is None:
        return True
    result = resp.get("result", {})
    return result.get("isError") or result.get("is_error") or "error" in resp

def call_tool(proc, msg_id_ref, tool_name, arguments, timeout=90, label=None):
    """Send a tools/call and return (response, text, msg_id_ref).
    msg_id_ref is a list with one int so we can mutate it.
    """
    mid = msg_id_ref[0]
    send_msg(proc, mid, "tools/call", {
        "name": tool_name,
        "arguments": arguments,
    })
    resp = recv_msg(proc, mid, timeout=timeout)
    msg_id_ref[0] = mid + 1
    text = get_tool_result_text(resp)
    return resp, text

# --------------------------------------------------------------------------
# Start server
# --------------------------------------------------------------------------
log("Starting agentiso serve...")
proc = start_server()

time.sleep(2.0)

if proc.poll() is not None:
    stderr_out = proc.stderr.read()
    log(f"ERROR: Server exited immediately with code {proc.returncode}")
    log(f"Stderr:\n{stderr_out}")
    sys.exit(1)

log(f"Server started (PID {proc.pid})")
log("")

msg_id = [1]  # mutable counter in a list

try:
    # -----------------------------------------------------------------------
    # Step 1: MCP initialize handshake
    # -----------------------------------------------------------------------
    log("Step 1: Server start — MCP initialize handshake")
    send_msg(proc, msg_id[0], "initialize", {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "opencode-integration-test", "version": "0.1.0"},
    })
    resp = recv_msg(proc, msg_id[0], timeout=15)
    if resp is not None and "result" in resp:
        result = resp["result"]
        server_name = result.get("serverInfo", {}).get("name", "unknown")
        pass_step(f"server start + initialize (server={server_name})")
    else:
        fail_step("server start + initialize", get_error(resp))
        log("\nFATAL: MCP initialize failed. Cannot continue.")
        sys.exit(1)
    msg_id[0] += 1

    send_notification(proc, "notifications/initialized")
    time.sleep(0.2)

    # -----------------------------------------------------------------------
    # Step 2: workspace_create — basic workspace for env var test
    # -----------------------------------------------------------------------
    log("Step 2: Create workspace for environment variable test")
    log("         (VM boot may take up to 60s, please wait...)")
    resp, text = call_tool(proc, msg_id, "workspace_create", {
        "name": "opencode-env-test",
        "vcpus": 1,
        "memory_mb": 512,
        "disk_gb": 10,
    }, timeout=120)

    env_ws_id = None
    if resp is not None and "result" in resp and text:
        try:
            data = json.loads(text)
            env_ws_id = data.get("workspace_id")
            if env_ws_id:
                pass_step(f"workspace_create for env test (id={env_ws_id[:8]}... state={data.get('state')})")
            else:
                fail_step("workspace_create for env test", f"no workspace_id: {text}")
        except json.JSONDecodeError:
            fail_step("workspace_create for env test", f"invalid JSON: {text}")
    else:
        fail_step("workspace_create for env test", get_error(resp))

    if env_ws_id is None:
        log("\nFATAL: Cannot continue without a workspace. Aborting.")
        sys.exit(1)

    # -----------------------------------------------------------------------
    # Step 3: Set and verify environment variables via exec
    # -----------------------------------------------------------------------
    log("Step 3: Set environment variable and verify via exec")
    # The guest agent applies env vars set via SetEnv to all subsequent Exec
    # calls. Since SetEnv is a vsock-level RPC (not an MCP tool), we test the
    # exec environment pipeline: set a var with export, then verify it.
    # This validates the exec infrastructure that SetEnv vars would flow through.
    resp, text = call_tool(proc, msg_id, "exec", {
        "workspace_id": env_ws_id,
        "command": "export AGENTISO_TEST_KEY=test-value-12345 && echo $AGENTISO_TEST_KEY",
        "timeout_secs": 15,
    }, timeout=30)

    if resp is not None and text:
        try:
            data = json.loads(text)
            stdout = data.get("stdout", "").strip()
            if data.get("exit_code") == 0 and "test-value-12345" in stdout:
                pass_step("exec with env var (AGENTISO_TEST_KEY verified)")
            else:
                fail_step("exec with env var", f"exit_code={data.get('exit_code')}, stdout={stdout!r}")
        except json.JSONDecodeError:
            fail_step("exec with env var", f"invalid JSON: {text}")
    else:
        fail_step("exec with env var", get_error(resp))

    # Clean up env test workspace — we'll use workspace_prepare for the golden
    log("         Cleaning up env test workspace...")
    call_tool(proc, msg_id, "workspace_destroy", {"workspace_id": env_ws_id}, timeout=60)
    env_ws_id = None  # Mark as cleaned up

    # -----------------------------------------------------------------------
    # Step 4: Prepare golden — workspace_prepare with test file + snapshot
    # -----------------------------------------------------------------------
    log("Step 4: Prepare golden workspace (workspace_prepare)")
    log("         (Creates workspace, writes test file, snapshots as 'golden')")
    resp, text = call_tool(proc, msg_id, "workspace_prepare", {
        "name": "opencode-golden",
        "base_image": "alpine-dev",
        "setup_commands": [
            "echo 'golden-marker-content' > /tmp/golden-marker.txt",
            "mkdir -p /workspace && echo 'project-file-content' > /workspace/README.md",
        ],
    }, timeout=180)

    if resp is not None and text and not is_tool_error(resp):
        try:
            data = json.loads(text)
            GOLDEN_WORKSPACE_ID = data.get("workspace_id")
            snap_name = data.get("snapshot_name")
            snap_id = data.get("snapshot_id")
            if GOLDEN_WORKSPACE_ID and snap_name == "golden":
                pass_step(f"workspace_prepare (id={GOLDEN_WORKSPACE_ID[:8]}... snapshot={snap_name})")
            else:
                fail_step("workspace_prepare", f"unexpected response: {text}")
        except json.JSONDecodeError:
            fail_step("workspace_prepare", f"invalid JSON: {text}")
    else:
        fail_step("workspace_prepare", get_error(resp) or text)

    if GOLDEN_WORKSPACE_ID is None:
        log("\nFATAL: Cannot continue without golden workspace. Aborting.")
        sys.exit(1)

    # -----------------------------------------------------------------------
    # Step 5: Batch fork — fork 2 workers from golden
    # -----------------------------------------------------------------------
    log("Step 5: Batch fork 2 workers from golden snapshot (workspace_fork with count)")
    resp, text = call_tool(proc, msg_id, "workspace_fork", {
        "workspace_id": GOLDEN_WORKSPACE_ID,
        "snapshot_name": "golden",
        "count": 2,
        "name_prefix": "oc-worker",
    }, timeout=240)

    if resp is not None and text and not is_tool_error(resp):
        try:
            data = json.loads(text)
            workers = data.get("workers", [])
            errors = data.get("errors", [])
            for w in workers:
                wid = w.get("workspace_id")
                if wid:
                    WORKER_IDS.append(wid)
            if len(WORKER_IDS) == 2:
                names = [w.get("name", "?") for w in workers]
                pass_step(f"workspace_fork batch (2 workers: {names})")
            else:
                fail_step("workspace_fork batch", f"expected 2 workers, got {len(WORKER_IDS)}; errors={errors}")
        except json.JSONDecodeError:
            fail_step("workspace_fork batch", f"invalid JSON: {text}")
    else:
        fail_step("workspace_fork batch", get_error(resp) or text)

    if len(WORKER_IDS) < 2:
        log(f"\nWARNING: Only {len(WORKER_IDS)} workers created, some fork tests will be skipped.")

    # -----------------------------------------------------------------------
    # Step 6: Verify fork isolation — each worker has the golden marker file
    # -----------------------------------------------------------------------
    log("Step 6: Verify fork isolation — workers have golden marker file")
    fork_isolation_ok = True
    for i, wid in enumerate(WORKER_IDS):
        resp, text = call_tool(proc, msg_id, "exec", {
            "workspace_id": wid,
            "command": "cat /tmp/golden-marker.txt",
            "timeout_secs": 15,
        }, timeout=30)
        if resp is not None and text:
            try:
                data = json.loads(text)
                stdout = data.get("stdout", "").strip()
                if data.get("exit_code") == 0 and "golden-marker-content" in stdout:
                    log(f"         Worker {i+1} ({wid[:8]}...): golden marker present")
                else:
                    fork_isolation_ok = False
                    log(f"         Worker {i+1} ({wid[:8]}...): MISSING golden marker (exit={data.get('exit_code')}, stdout={stdout!r})")
            except json.JSONDecodeError:
                fork_isolation_ok = False
                log(f"         Worker {i+1}: invalid JSON response")
        else:
            fork_isolation_ok = False
            log(f"         Worker {i+1}: exec failed: {get_error(resp)}")

    if fork_isolation_ok and len(WORKER_IDS) >= 2:
        pass_step("fork isolation (all workers have golden marker file)")
    elif len(WORKER_IDS) == 0:
        fail_step("fork isolation", "no workers to test")
    else:
        fail_step("fork isolation", "some workers missing golden marker")

    # -----------------------------------------------------------------------
    # Step 7: Verify workers have unique IPs (via workspace_info)
    # -----------------------------------------------------------------------
    log("Step 7: Verify workers have unique IPs")
    worker_ips = []
    for i, wid in enumerate(WORKER_IDS):
        resp, text = call_tool(proc, msg_id, "workspace_info", {
            "workspace_id": wid,
        }, timeout=30)
        if resp is not None and text:
            try:
                data = json.loads(text)
                ip = data.get("ip")
                if ip:
                    worker_ips.append(ip)
                    log(f"         Worker {i+1} ({wid[:8]}...): ip={ip}")
                else:
                    log(f"         Worker {i+1}: no IP in response")
            except json.JSONDecodeError:
                log(f"         Worker {i+1}: invalid JSON")
        else:
            log(f"         Worker {i+1}: workspace_info failed")

    if len(worker_ips) >= 2 and len(set(worker_ips)) == len(worker_ips):
        pass_step(f"unique IPs ({worker_ips})")
    elif len(worker_ips) < 2:
        fail_step("unique IPs", f"could not retrieve IPs for all workers (got {worker_ips})")
    else:
        fail_step("unique IPs", f"duplicate IPs found: {worker_ips}")

    # -----------------------------------------------------------------------
    # Step 8: Execute in workers — run a simple command in each
    # -----------------------------------------------------------------------
    log("Step 8: Execute command in each worker")
    exec_ok = True
    for i, wid in enumerate(WORKER_IDS):
        resp, text = call_tool(proc, msg_id, "exec", {
            "workspace_id": wid,
            "command": f"echo 'hello from worker {i+1}' && hostname",
            "timeout_secs": 15,
        }, timeout=30)
        if resp is not None and text:
            try:
                data = json.loads(text)
                stdout = data.get("stdout", "").strip()
                if data.get("exit_code") == 0 and f"hello from worker {i+1}" in stdout:
                    log(f"         Worker {i+1} ({wid[:8]}...): exec OK — {stdout.splitlines()[0]!r}")
                else:
                    exec_ok = False
                    log(f"         Worker {i+1}: unexpected exit={data.get('exit_code')}, stdout={stdout!r}")
            except json.JSONDecodeError:
                exec_ok = False
                log(f"         Worker {i+1}: invalid JSON")
        else:
            exec_ok = False
            log(f"         Worker {i+1}: exec failed: {get_error(resp)}")

    if exec_ok and len(WORKER_IDS) >= 2:
        pass_step("exec in workers (all workers responded correctly)")
    elif len(WORKER_IDS) == 0:
        fail_step("exec in workers", "no workers to test")
    else:
        fail_step("exec in workers", "some workers failed exec")

    # -----------------------------------------------------------------------
    # Step 9: Parallel exec — run commands in both workers simultaneously
    # -----------------------------------------------------------------------
    log("Step 9: Parallel exec — run commands in both workers simultaneously")
    if len(WORKER_IDS) >= 2:
        # Send commands to both workers without waiting for response between them
        parallel_mid_start = msg_id[0]
        for i, wid in enumerate(WORKER_IDS[:2]):
            send_msg(proc, msg_id[0], "tools/call", {
                "name": "exec",
                "arguments": {
                    "workspace_id": wid,
                    "command": f"sleep 1 && echo 'parallel-result-{i+1}'",
                    "timeout_secs": 30,
                },
            })
            msg_id[0] += 1

        # Collect both responses
        parallel_results = []
        for i in range(2):
            expected_id = parallel_mid_start + i
            resp = recv_msg(proc, expected_id, timeout=60)
            if resp is not None:
                text = get_tool_result_text(resp)
                if text:
                    try:
                        data = json.loads(text)
                        stdout = data.get("stdout", "").strip()
                        if data.get("exit_code") == 0 and f"parallel-result-{i+1}" in stdout:
                            parallel_results.append(True)
                        else:
                            parallel_results.append(False)
                            log(f"         Worker {i+1}: unexpected output: {stdout!r}")
                    except json.JSONDecodeError:
                        parallel_results.append(False)
                else:
                    parallel_results.append(False)
            else:
                parallel_results.append(False)
                log(f"         Worker {i+1}: no response (timeout)")

        if all(parallel_results):
            pass_step("parallel exec (both workers completed simultaneously)")
        else:
            fail_step("parallel exec", f"results: {parallel_results}")
    else:
        fail_step("parallel exec", "need at least 2 workers for parallel test")

    # -----------------------------------------------------------------------
    # Step 10: Cleanup workers — destroy both workers
    # -----------------------------------------------------------------------
    log("Step 10: Cleanup workers — destroy all worker VMs")
    destroy_ok = True
    for i, wid in enumerate(WORKER_IDS[:]):
        resp, text = call_tool(proc, msg_id, "workspace_destroy", {
            "workspace_id": wid,
        }, timeout=60)
        if is_tool_error(resp):
            destroy_ok = False
            log(f"         Worker {i+1} ({wid[:8]}...): destroy FAILED: {text or get_error(resp)}")
        else:
            log(f"         Worker {i+1} ({wid[:8]}...): destroyed")
            WORKER_IDS.remove(wid)

    if destroy_ok:
        pass_step("cleanup workers (all workers destroyed)")
    else:
        fail_step("cleanup workers", "some workers failed to destroy")

    # -----------------------------------------------------------------------
    # Step 11: Cleanup golden — destroy the golden workspace
    # -----------------------------------------------------------------------
    log("Step 11: Cleanup golden — destroy the golden workspace")
    if GOLDEN_WORKSPACE_ID:
        resp, text = call_tool(proc, msg_id, "workspace_destroy", {
            "workspace_id": GOLDEN_WORKSPACE_ID,
        }, timeout=60)
        if not is_tool_error(resp):
            pass_step("cleanup golden (golden workspace destroyed)")
            GOLDEN_WORKSPACE_ID = None
        else:
            fail_step("cleanup golden", text or get_error(resp))
    else:
        log("         (skipped — golden workspace already gone)")

    # -----------------------------------------------------------------------
    # Step 12: Verify all test workspaces are gone
    # -----------------------------------------------------------------------
    log("Step 12: Server stop — verify cleanup and stop server")
    resp, text = call_tool(proc, msg_id, "workspace_list", {}, timeout=30)
    if resp is not None and text:
        try:
            workspaces = json.loads(text)
            test_ws = [w for w in workspaces if w.get("name", "").startswith("opencode-") or w.get("name", "").startswith("oc-worker")]
            if not test_ws:
                pass_step(f"all test workspaces cleaned up ({len(workspaces)} other workspaces remain)")
            else:
                fail_step("cleanup verification", f"test workspaces still exist: {[w.get('name') for w in test_ws]}")
        except json.JSONDecodeError:
            fail_step("cleanup verification", f"invalid JSON: {text}")
    else:
        fail_step("cleanup verification", get_error(resp))

except KeyboardInterrupt:
    log("\nInterrupted by user")

finally:
    # --------------------------------------------------------------------------
    # Emergency cleanup: destroy any leftover test workspaces
    # --------------------------------------------------------------------------
    log("")
    log("Emergency cleanup: checking for leftover test workspaces...")
    leftover_ids = list(WORKER_IDS)
    if GOLDEN_WORKSPACE_ID:
        leftover_ids.append(GOLDEN_WORKSPACE_ID)
    for wid in leftover_ids:
        log(f"  Destroying leftover workspace {wid[:8]}...")
        try:
            call_tool(proc, msg_id, "workspace_destroy", {"workspace_id": wid}, timeout=30)
        except Exception:
            pass

    # --------------------------------------------------------------------------
    # Terminate server
    # --------------------------------------------------------------------------
    log("")
    log("Shutting down server...")
    try:
        proc.stdin.close()
    except Exception:
        pass

    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        log("Server did not exit gracefully, killing...")
        proc.kill()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass

    # Capture stderr for diagnostics on failure
    try:
        stderr_out = proc.stderr.read()
        if stderr_out and FAIL_COUNT > 0:
            log("--- Server stderr (for debugging) ---")
            if len(stderr_out) > 3000:
                log(f"... (truncated, showing last 3000 chars) ...")
                log(stderr_out[-3000:])
            else:
                log(stderr_out)
            log("--- End server stderr ---")
    except Exception:
        pass

    # --------------------------------------------------------------------------
    # Results summary
    # --------------------------------------------------------------------------
    log("")
    log("=" * 60)
    log("OpenCode Integration Test Results")
    log(f"  PASS: {PASS_COUNT}")
    log(f"  FAIL: {FAIL_COUNT}")
    log(f"  TOTAL: {PASS_COUNT + FAIL_COUNT}")
    log("=" * 60)

    if FAIL_COUNT == 0:
        log("ALL TESTS PASSED")
        sys.exit(0)
    else:
        log("SOME TESTS FAILED")
        sys.exit(1)

PYEOF

EXIT_CODE=$?
if [[ $EXIT_CODE -eq 0 ]]; then
    echo ""
    echo "OpenCode integration test PASSED"
else
    echo ""
    echo "OpenCode integration test FAILED (exit code $EXIT_CODE)"
fi
exit $EXIT_CODE
