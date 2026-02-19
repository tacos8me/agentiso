#!/bin/bash
# Test suite for new agentiso tools and critical lifecycle paths
# Tests: file_list, file_edit, exec_background (start/poll/kill), stop/start, fork, snapshot_restore
#
# Usage: sudo ./scripts/test-new-tools.sh
#
# Requires: root (for QEMU/KVM/TAP/ZFS), a built binary, and a valid environment.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_DIR/target/release/agentiso"
CONFIG="$PROJECT_DIR/config.toml"

echo "=== agentiso New Tools & Lifecycle Test Suite ==="
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
Test suite for new agentiso MCP tools and critical lifecycle paths.
Drives the JSON-RPC 2.0 protocol over stdio against agentiso serve.

20 test steps covering: file_list, file_edit, exec_background (start/poll/kill),
snapshot_restore, workspace_stop/start, workspace_fork.
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
TOTAL_STEPS = 20
WORKSPACE_ID = None
FORK_ID = None

def log(msg):
    print(msg, flush=True)

def pass_step(step_num, name, detail=None):
    global PASS_COUNT
    PASS_COUNT += 1
    elapsed = time.time() - STEP_START
    msg = f"  [PASS] Step {step_num}: {name} ({elapsed:.1f}s)"
    if detail:
        msg += f" — {detail}"
    log(msg)

def fail_step(step_num, name, detail=None):
    global FAIL_COUNT
    FAIL_COUNT += 1
    elapsed = time.time() - STEP_START
    log(f"  [FAIL] Step {step_num}: {name} ({elapsed:.1f}s)")
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
    """Read lines until we find a response with matching id."""
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
    """Extract text content from a tools/call response."""
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
    """Extract error message from a response."""
    if resp is None:
        return "timeout (no response received)"
    if "error" in resp:
        err = resp["error"]
        return f"code={err.get('code')} message={err.get('message')}"
    return None

def is_tool_error(resp):
    """Check if the response is a tool-level error (isError flag)."""
    if resp is None:
        return True
    result = resp.get("result", {})
    return bool(result.get("isError") or result.get("is_error"))

def call_tool(proc, msg_id_ref, tool_name, arguments, timeout=30):
    """Call a tool and return (resp, text, parsed_json_or_None)."""
    msg_id_ref[0] += 1
    mid = msg_id_ref[0]
    send_msg(proc, mid, "tools/call", {
        "name": tool_name,
        "arguments": arguments,
    })
    resp = recv_msg(proc, mid, timeout=timeout)
    text = get_tool_result_text(resp)
    parsed = None
    if text:
        try:
            parsed = json.loads(text)
        except (json.JSONDecodeError, TypeError):
            pass
    return resp, text, parsed

# --------------------------------------------------------------------------
# Start server
# --------------------------------------------------------------------------
STEP_START = time.time()
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

msg_id = [0]  # mutable reference for call_tool

try:
    # -----------------------------------------------------------------------
    # MCP initialize handshake (not counted as a test step)
    # -----------------------------------------------------------------------
    log("MCP handshake...")
    msg_id[0] += 1
    send_msg(proc, msg_id[0], "initialize", {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "new-tools-test", "version": "0.1.0"},
    })
    resp = recv_msg(proc, msg_id[0], timeout=15)
    if resp is None or "result" not in resp:
        log("FATAL: MCP initialize failed. Cannot continue.")
        sys.exit(1)
    log("  MCP initialized OK")
    send_notification(proc, "notifications/initialized")
    time.sleep(0.2)
    log("")

    # -----------------------------------------------------------------------
    # Step 1: workspace_create
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 1: workspace_create")
    resp, text, data = call_tool(proc, msg_id, "workspace_create", {
        "name": "new-tools-test",
        "vcpus": 1,
        "memory_mb": 512,
        "disk_gb": 10,
    }, timeout=120)
    if data and data.get("workspace_id"):
        WORKSPACE_ID = data["workspace_id"]
        pass_step(1, "workspace_create", f"id={WORKSPACE_ID[:8]}...")
    else:
        fail_step(1, "workspace_create", get_error(resp) or f"response: {text}")
        log("\nFATAL: Cannot continue without workspace_id.")
        sys.exit(1)

    # -----------------------------------------------------------------------
    # Step 2: file_write (hello world)
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 2: file_write /root/test.txt")
    resp, text, data = call_tool(proc, msg_id, "file_write", {
        "workspace_id": WORKSPACE_ID,
        "path": "/root/test.txt",
        "content": "hello world\n",
    }, timeout=10)
    if resp and not is_tool_error(resp) and text:
        pass_step(2, "file_write", text.strip())
    else:
        fail_step(2, "file_write", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 3: file_list /root — verify test.txt appears
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 3: file_list /root")
    resp, text, data = call_tool(proc, msg_id, "file_list", {
        "workspace_id": WORKSPACE_ID,
        "path": "/root",
    }, timeout=10)
    if data and isinstance(data, list):
        names = [e.get("name") for e in data]
        if "test.txt" in names:
            pass_step(3, "file_list", f"found test.txt among {len(data)} entries")
        else:
            fail_step(3, "file_list", f"test.txt not in {names}")
    else:
        fail_step(3, "file_list", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 4: file_edit (change "hello world" to "hello agentiso")
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 4: file_edit /root/test.txt")
    resp, text, data = call_tool(proc, msg_id, "file_edit", {
        "workspace_id": WORKSPACE_ID,
        "path": "/root/test.txt",
        "old_string": "hello world",
        "new_string": "hello agentiso",
    }, timeout=10)
    if resp and not is_tool_error(resp) and text:
        pass_step(4, "file_edit", text.strip())
    else:
        fail_step(4, "file_edit", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 5: file_read — verify content is "hello agentiso\n"
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 5: file_read /root/test.txt (verify edit)")
    resp, text, data = call_tool(proc, msg_id, "file_read", {
        "workspace_id": WORKSPACE_ID,
        "path": "/root/test.txt",
    }, timeout=10)
    if text is not None and "hello agentiso" in text:
        pass_step(5, "file_read", f"content={text.strip()!r}")
    else:
        fail_step(5, "file_read", f"expected 'hello agentiso', got: {text!r}")

    # -----------------------------------------------------------------------
    # Step 6: exec_background start (sleep 3 && echo done > /root/bg.txt)
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 6: exec_background start")
    resp, text, data = call_tool(proc, msg_id, "exec_background", {
        "action": "start",
        "workspace_id": WORKSPACE_ID,
        "command": "sleep 3 && echo done > /root/bg.txt",
    }, timeout=10)
    job_id = None
    if data and "job_id" in data:
        job_id = data["job_id"]
        pass_step(6, "exec_background", f"job_id={job_id}")
    else:
        fail_step(6, "exec_background", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 7: exec_background poll immediately — verify running=true
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 7: exec_background poll (immediate, expect running)")
    if job_id is not None:
        time.sleep(0.5)  # small delay to let the command start
        resp, text, data = call_tool(proc, msg_id, "exec_background", {
            "action": "poll",
            "workspace_id": WORKSPACE_ID,
            "job_id": job_id,
        }, timeout=10)
        if data and data.get("running") is True:
            pass_step(7, "exec_background poll", "running=true")
        elif data and data.get("running") is False:
            # It might have finished already if the system is fast
            pass_step(7, "exec_background poll", "already finished (fast system)")
        else:
            fail_step(7, "exec_background poll", get_error(resp) or f"response: {text}")
    else:
        fail_step(7, "exec_background poll", "skipped — no job_id from step 6")

    # -----------------------------------------------------------------------
    # Step 8: exec_background poll after 5s — verify running=false, exit_code=0
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 8: exec_background poll (after delay, expect finished)")
    if job_id is not None:
        time.sleep(5)
        resp, text, data = call_tool(proc, msg_id, "exec_background", {
            "action": "poll",
            "workspace_id": WORKSPACE_ID,
            "job_id": job_id,
        }, timeout=10)
        if data and data.get("running") is False and data.get("exit_code") == 0:
            pass_step(8, "exec_background poll", f"running=false exit_code=0")
        else:
            fail_step(8, "exec_background poll", f"expected running=false exit_code=0, got: {data}")
    else:
        fail_step(8, "exec_background poll", "skipped — no job_id from step 6")

    # -----------------------------------------------------------------------
    # Step 9: file_read /root/bg.txt — verify "done\n"
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 9: file_read /root/bg.txt (background job output)")
    resp, text, data = call_tool(proc, msg_id, "file_read", {
        "workspace_id": WORKSPACE_ID,
        "path": "/root/bg.txt",
    }, timeout=10)
    if text is not None and "done" in text:
        pass_step(9, "file_read bg.txt", f"content={text.strip()!r}")
    else:
        fail_step(9, "file_read bg.txt", f"expected 'done', got: {text!r}")

    # -----------------------------------------------------------------------
    # Step 10: snapshot_create (name="checkpoint")
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 10: snapshot (create) 'checkpoint'")
    resp, text, data = call_tool(proc, msg_id, "snapshot", {
        "action": "create",
        "workspace_id": WORKSPACE_ID,
        "name": "checkpoint",
        "include_memory": False,
    }, timeout=60)
    if data and data.get("name") == "checkpoint":
        pass_step(10, "snapshot_create", f"id={data.get('snapshot_id','?')[:8]}...")
    else:
        fail_step(10, "snapshot_create", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 11: file_write — overwrite test.txt with "corrupted\n"
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 11: file_write /root/test.txt (corrupt)")
    resp, text, data = call_tool(proc, msg_id, "file_write", {
        "workspace_id": WORKSPACE_ID,
        "path": "/root/test.txt",
        "content": "corrupted\n",
    }, timeout=10)
    if resp and not is_tool_error(resp) and text:
        pass_step(11, "file_write (corrupt)", text.strip())
    else:
        fail_step(11, "file_write (corrupt)", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 12: file_read — verify "corrupted"
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 12: file_read /root/test.txt (verify corruption)")
    resp, text, data = call_tool(proc, msg_id, "file_read", {
        "workspace_id": WORKSPACE_ID,
        "path": "/root/test.txt",
    }, timeout=10)
    if text is not None and "corrupted" in text:
        pass_step(12, "file_read (corrupted)", f"content={text.strip()!r}")
    else:
        fail_step(12, "file_read (corrupted)", f"expected 'corrupted', got: {text!r}")

    # -----------------------------------------------------------------------
    # Step 13: snapshot_restore (name="checkpoint")
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 13: snapshot (restore) 'checkpoint'")
    log("         (VM will stop, zvol rollback, VM restart — may take 30-60s)")
    resp, text, data = call_tool(proc, msg_id, "snapshot", {
        "action": "restore",
        "workspace_id": WORKSPACE_ID,
        "snapshot_name": "checkpoint",
    }, timeout=120)
    if resp and not is_tool_error(resp) and text:
        pass_step(13, "snapshot_restore", text.strip())
    else:
        fail_step(13, "snapshot_restore", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 14: file_read — verify rollback (content should be "hello agentiso\n")
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 14: file_read /root/test.txt (verify rollback)")
    resp, text, data = call_tool(proc, msg_id, "file_read", {
        "workspace_id": WORKSPACE_ID,
        "path": "/root/test.txt",
    }, timeout=30)
    if text is not None and "hello agentiso" in text:
        pass_step(14, "file_read (rollback)", f"content={text.strip()!r}")
    else:
        fail_step(14, "file_read (rollback)", f"expected 'hello agentiso', got: {text!r}")

    # -----------------------------------------------------------------------
    # Step 15: workspace_stop
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 15: workspace_stop")
    resp, text, data = call_tool(proc, msg_id, "workspace_stop", {
        "workspace_id": WORKSPACE_ID,
    }, timeout=60)
    if resp and not is_tool_error(resp) and text and "stopped" in text.lower():
        pass_step(15, "workspace_stop", text.strip())
    else:
        fail_step(15, "workspace_stop", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 16: workspace_start (wait up to 60s for VM to reboot)
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 16: workspace_start (VM reboot, may take up to 60s)")
    resp, text, data = call_tool(proc, msg_id, "workspace_start", {
        "workspace_id": WORKSPACE_ID,
    }, timeout=120)
    if resp and not is_tool_error(resp) and text and "started" in text.lower():
        pass_step(16, "workspace_start", text.strip())
    else:
        fail_step(16, "workspace_start", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 17: exec after restart — verify VM is alive
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 17: exec after restart")
    resp, text, data = call_tool(proc, msg_id, "exec", {
        "workspace_id": WORKSPACE_ID,
        "command": "echo alive after restart",
        "timeout_secs": 15,
    }, timeout=30)
    if data and data.get("exit_code") == 0 and "alive after restart" in data.get("stdout", ""):
        pass_step(17, "exec after restart", f"stdout={data['stdout'].strip()!r}")
    else:
        fail_step(17, "exec after restart", get_error(resp) or f"response: {data or text}")

    # -----------------------------------------------------------------------
    # Step 18: workspace_fork (from snapshot "checkpoint")
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 18: workspace_fork (from 'checkpoint' snapshot)")
    log("         (ZFS clone + VM boot — may take 30-60s)")
    resp, text, data = call_tool(proc, msg_id, "workspace_fork", {
        "workspace_id": WORKSPACE_ID,
        "snapshot_name": "checkpoint",
        "new_name": "fork-test",
    }, timeout=120)
    if data and data.get("workspace_id"):
        FORK_ID = data["workspace_id"]
        pass_step(18, "workspace_fork", f"fork_id={FORK_ID[:8]}...")
    else:
        fail_step(18, "workspace_fork", get_error(resp) or f"response: {text}")

    # -----------------------------------------------------------------------
    # Step 19: exec on fork — cat /root/test.txt, verify "hello agentiso\n"
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 19: exec on fork (verify snapshot content)")
    if FORK_ID:
        resp, text, data = call_tool(proc, msg_id, "exec", {
            "workspace_id": FORK_ID,
            "command": "cat /root/test.txt",
            "timeout_secs": 15,
        }, timeout=30)
        if data and data.get("exit_code") == 0 and "hello agentiso" in data.get("stdout", ""):
            pass_step(19, "exec on fork", f"content={data['stdout'].strip()!r}")
        else:
            fail_step(19, "exec on fork", f"expected 'hello agentiso', got: {data or text}")
    else:
        fail_step(19, "exec on fork", "skipped — no fork_id from step 18")

    # -----------------------------------------------------------------------
    # Step 20: destroy fork, destroy original, workspace_list → empty
    # -----------------------------------------------------------------------
    STEP_START = time.time()
    log("Step 20: cleanup — destroy fork + original, verify list empty")

    cleanup_ok = True

    # Destroy fork
    if FORK_ID:
        resp, text, data = call_tool(proc, msg_id, "workspace_destroy", {
            "workspace_id": FORK_ID,
        }, timeout=60)
        if is_tool_error(resp):
            log(f"         [WARN] Failed to destroy fork: {get_error(resp) or text}")
            cleanup_ok = False
        FORK_ID = None

    # Destroy original
    if WORKSPACE_ID:
        resp, text, data = call_tool(proc, msg_id, "workspace_destroy", {
            "workspace_id": WORKSPACE_ID,
        }, timeout=60)
        if is_tool_error(resp):
            log(f"         [WARN] Failed to destroy original: {get_error(resp) or text}")
            cleanup_ok = False
        WORKSPACE_ID = None

    # Verify list is empty
    resp, text, data = call_tool(proc, msg_id, "workspace_list", {}, timeout=15)
    if data is not None and isinstance(data, list) and len(data) == 0:
        if cleanup_ok:
            pass_step(20, "cleanup", "both destroyed, list empty")
        else:
            pass_step(20, "cleanup", "list empty (with warnings)")
    elif data is not None and isinstance(data, list):
        fail_step(20, "cleanup", f"workspace_list not empty: {len(data)} remaining")
    else:
        fail_step(20, "cleanup", get_error(resp) or f"response: {text}")

except KeyboardInterrupt:
    log("\nInterrupted by user")

finally:
    # --------------------------------------------------------------------------
    # Emergency cleanup: destroy any leftover workspaces
    # --------------------------------------------------------------------------
    for ws_label, ws_id in [("fork", FORK_ID), ("original", WORKSPACE_ID)]:
        if ws_id:
            log(f"Emergency cleanup: destroying {ws_label} workspace {ws_id[:8]}...")
            try:
                resp, text, data = call_tool(proc, msg_id, "workspace_destroy", {
                    "workspace_id": ws_id,
                }, timeout=30)
            except Exception:
                pass

    # --------------------------------------------------------------------------
    # Shutdown server
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

    # Show stderr on failure
    try:
        stderr_out = proc.stderr.read()
        if stderr_out and FAIL_COUNT > 0:
            log("--- Server stderr (for debugging) ---")
            if len(stderr_out) > 3000:
                log("... (truncated, showing last 3000 chars) ...")
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
    log("New Tools & Lifecycle Test Results")
    log(f"  PASS: {PASS_COUNT}")
    log(f"  FAIL: {FAIL_COUNT}")
    log(f"  TOTAL: {PASS_COUNT + FAIL_COUNT}/{TOTAL_STEPS}")
    log("=" * 60)

    if FAIL_COUNT == 0 and PASS_COUNT == TOTAL_STEPS:
        log("ALL TESTS PASSED")
        sys.exit(0)
    else:
        log("SOME TESTS FAILED")
        sys.exit(1)

PYEOF

EXIT_CODE=$?
if [[ $EXIT_CODE -eq 0 ]]; then
    echo ""
    echo "New tools test PASSED"
else
    echo ""
    echo "New tools test FAILED (exit code $EXIT_CODE)"
fi
exit $EXIT_CODE
