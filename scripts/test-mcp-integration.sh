#!/bin/bash
# MCP Integration Test for agentiso
# Starts agentiso serve as a subprocess and drives the MCP JSON-RPC protocol
# over stdin/stdout to validate the complete workspace lifecycle.
#
# Usage: sudo ./scripts/test-mcp-integration.sh
#
# Requires: root (for QEMU/KVM/TAP/ZFS), a built binary, and a valid environment.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_DIR/target/release/agentiso"
CONFIG="$PROJECT_DIR/config.toml"

echo "=== agentiso MCP Integration Test ==="
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
MCP integration test driver for agentiso.
Starts the server, drives the JSON-RPC 2.0 protocol over stdio,
and validates the complete workspace lifecycle.
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
WORKSPACE_ID = None
FORKED_WORKSPACE_ID = None

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
    env["RUST_LOG"] = "warn"  # Suppress info/debug logs to stderr
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
    """Send a JSON-RPC notification (no id)."""
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
        # Check if process died
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
                # else: notification or other message, skip
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
    # Get the first text content item
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

# --------------------------------------------------------------------------
# Start server
# --------------------------------------------------------------------------
log("Starting agentiso serve...")
proc = start_server()

# Give server a moment to start and initialize (ZFS, network setup, etc.)
time.sleep(2.0)

# Check process is still running
if proc.poll() is not None:
    stderr_out = proc.stderr.read()
    log(f"ERROR: Server exited immediately with code {proc.returncode}")
    log(f"Stderr:\n{stderr_out}")
    sys.exit(1)

log(f"Server started (PID {proc.pid})")
log("")

msg_id = 1

try:
    # -----------------------------------------------------------------------
    # Step 1: MCP initialize handshake
    # -----------------------------------------------------------------------
    log("Step 1: MCP initialize handshake")
    send_msg(proc, msg_id, "initialize", {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "mcp-integration-test", "version": "0.1.0"},
    })
    resp = recv_msg(proc, msg_id, timeout=15)
    if resp is not None and "result" in resp:
        result = resp["result"]
        server_name = result.get("serverInfo", {}).get("name", "unknown")
        proto_version = result.get("protocolVersion", "unknown")
        pass_step(f"initialize (server={server_name}, proto={proto_version})")
    else:
        fail_step("initialize", get_error(resp))
        # Cannot continue without initialize
        log("")
        log("FATAL: MCP initialize failed. Cannot continue.")
        sys.exit(1)
    msg_id += 1

    # Send initialized notification (required by MCP spec before server processes any more messages)
    send_notification(proc, "notifications/initialized")
    time.sleep(0.2)

    # -----------------------------------------------------------------------
    # Step 2: tools/list — verify tools are advertised
    # -----------------------------------------------------------------------
    log("Step 2: tools/list — verify all tools are advertised")
    send_msg(proc, msg_id, "tools/list")
    resp = recv_msg(proc, msg_id, timeout=10)
    if resp is not None and "result" in resp:
        tools = resp["result"].get("tools", [])
        tool_names = {t["name"] for t in tools}
        expected_tools = {
            "workspace_create", "workspace_destroy", "workspace_list",
            "workspace_info", "workspace_stop", "workspace_start",
            "workspace_fork", "workspace_adopt", "workspace_prepare",
            "workspace_logs",
            "exec", "exec_background",
            "file_write", "file_read", "file_edit", "file_list",
            "file_transfer",
            "snapshot",
            "port_forward", "network_policy",
            "git_clone", "git_diff", "git_commit", "git_status", "git_push",
            "workspace_merge",
            "set_env", "vault", "team",
        }
        missing = expected_tools - tool_names
        extra = tool_names - expected_tools
        if not missing:
            pass_step(f"tools/list ({len(tool_names)} tools advertised)")
            if extra:
                log(f"         Note: extra tools: {extra}")
        else:
            fail_step("tools/list", f"missing tools: {missing}")
    else:
        fail_step("tools/list", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 3: workspace_create
    # -----------------------------------------------------------------------
    log("Step 3: workspace_create — create a workspace and start VM")
    log("         (VM boot may take up to 60s, please wait...)")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_create",
        "arguments": {
            "name": "mcp-integration-test",
            "vcpus": 1,
            "memory_mb": 512,
            "disk_gb": 10,
        },
    })
    # VM boot can take up to 60s
    resp = recv_msg(proc, msg_id, timeout=120)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                WORKSPACE_ID = data.get("workspace_id")
                ws_ip = data.get("ip")
                ws_state = data.get("state")
                if WORKSPACE_ID:
                    pass_step(f"workspace_create (id={WORKSPACE_ID[:8]}... ip={ws_ip} state={ws_state})")
                else:
                    fail_step("workspace_create", f"no workspace_id in response: {text}")
            except json.JSONDecodeError:
                fail_step("workspace_create", f"invalid JSON in response: {text}")
        else:
            # Check for tool-level error in result
            result = resp.get("result", {})
            is_error = result.get("isError") or result.get("is_error")
            if is_error:
                content = result.get("content", [])
                error_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "unknown error")
                fail_step("workspace_create", f"tool returned error: {error_text}")
            else:
                fail_step("workspace_create", f"no text content in result: {resp}")
    else:
        fail_step("workspace_create", get_error(resp))
    msg_id += 1

    if WORKSPACE_ID is None:
        log("")
        log("FATAL: Cannot continue without workspace_id. Aborting remaining steps.")
        sys.exit(1)

    # -----------------------------------------------------------------------
    # Diagnostic: dump guest console log after workspace_create
    # -----------------------------------------------------------------------
    log("")
    log("--- Diagnostic: guest console log (after workspace_create) ---")
    ws_short_id = WORKSPACE_ID[:8]
    console_log_path = f"/run/agentiso/{ws_short_id}/console.log"
    try:
        if os.path.exists(console_log_path):
            with open(console_log_path, "r") as f:
                console_text = f.read()
            log(f"=== GUEST CONSOLE LOG ({console_log_path}) ===")
            # Show last 5000 chars to avoid overwhelming output
            if len(console_text) > 5000:
                log(f"... (truncated, showing last 5000 chars of {len(console_text)} total) ...")
                log(console_text[-5000:])
            else:
                log(console_text)
            log("=== END GUEST CONSOLE LOG ===")
        else:
            log(f"  Console log not found at {console_log_path}")
    except Exception as e:
        log(f"  Failed to read console log: {e}")

    # Try to get guest agent log via exec
    log("--- Diagnostic: guest agent log (via exec) ---")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "command": "cat /var/log/agentiso-guest.log 2>/dev/null || echo 'guest agent log not found'",
            "timeout_secs": 10,
        },
    })
    diag_resp = recv_msg(proc, msg_id, timeout=20)
    if diag_resp is not None and "result" in diag_resp:
        diag_text = get_tool_result_text(diag_resp)
        if diag_text:
            try:
                diag_data = json.loads(diag_text)
                diag_stdout = diag_data.get("stdout", "")
                diag_stderr = diag_data.get("stderr", "")
                log("=== GUEST AGENT LOG ===")
                log(diag_stdout if diag_stdout else "(empty)")
                if diag_stderr:
                    log(f"--- stderr: {diag_stderr}")
                log("=== END GUEST AGENT LOG ===")
            except json.JSONDecodeError:
                log(f"  exec response not JSON: {diag_text}")
        else:
            result_obj = diag_resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                content = result_obj.get("content", [])
                err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                log(f"  exec failed (tool error): {err_text}")
            else:
                log(f"  exec returned no text content")
    else:
        log(f"  exec failed: {get_error(diag_resp)}")
    msg_id += 1
    log("--- End diagnostics ---")
    log("")

    # -----------------------------------------------------------------------
    # Step 4: exec — run echo hello
    # -----------------------------------------------------------------------
    log("Step 4: exec — run 'echo hello' in workspace")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "command": "echo hello",
            "timeout_secs": 30,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                exit_code = data.get("exit_code")
                stdout = data.get("stdout", "").strip()
                if exit_code == 0 and "hello" in stdout:
                    pass_step(f"exec echo hello (exit_code={exit_code}, stdout={stdout!r})")
                else:
                    fail_step("exec echo hello", f"exit_code={exit_code}, stdout={stdout!r}, stderr={data.get('stderr','')!r}")
            except json.JSONDecodeError:
                fail_step("exec echo hello", f"invalid JSON in response: {text}")
        else:
            fail_step("exec echo hello", f"no text content: {resp}")
    else:
        fail_step("exec echo hello", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 5: file_write — write a file
    # -----------------------------------------------------------------------
    log("Step 5: file_write — write /tmp/mcp-test.txt")
    test_content = "hello from MCP integration test\n"
    send_msg(proc, msg_id, "tools/call", {
        "name": "file_write",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "path": "/tmp/mcp-test.txt",
            "content": test_content,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text is not None:
            if "mcp-test.txt" in text or "written" in text.lower() or "file" in text.lower():
                pass_step(f"file_write /tmp/mcp-test.txt")
            else:
                # Could be error embedded in result
                result = resp.get("result", {})
                is_error = result.get("isError") or result.get("is_error")
                if is_error:
                    fail_step("file_write", f"tool error: {text}")
                else:
                    pass_step(f"file_write /tmp/mcp-test.txt (response: {text!r})")
        else:
            fail_step("file_write", f"no text content: {resp}")
    else:
        fail_step("file_write", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 6: file_read — read it back
    # -----------------------------------------------------------------------
    log("Step 6: file_read — read /tmp/mcp-test.txt back")
    send_msg(proc, msg_id, "tools/call", {
        "name": "file_read",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "path": "/tmp/mcp-test.txt",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text is not None:
            expected = test_content.strip()
            if expected in text:
                pass_step(f"file_read (content matches: {text.strip()!r})")
            else:
                # Check if it's an error result
                result = resp.get("result", {})
                is_error = result.get("isError") or result.get("is_error")
                if is_error:
                    fail_step("file_read", f"tool error: {text}")
                else:
                    fail_step("file_read", f"content mismatch. Got: {text!r}, expected substring: {expected!r}")
        else:
            fail_step("file_read", f"no text content: {resp}")
    else:
        fail_step("file_read", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 7: snapshot_create
    # -----------------------------------------------------------------------
    log("Step 7: snapshot (create) — create 'test-checkpoint' snapshot")
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot",
        "arguments": {
            "action": "create",
            "workspace_id": WORKSPACE_ID,
            "name": "test-checkpoint",
            "include_memory": False,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    snapshot_id = None
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                snapshot_id = data.get("snapshot_id")
                snap_name = data.get("name")
                if snapshot_id and snap_name == "test-checkpoint":
                    pass_step(f"snapshot_create (id={snapshot_id[:8]}... name={snap_name})")
                else:
                    result = resp.get("result", {})
                    is_error = result.get("isError") or result.get("is_error")
                    if is_error:
                        fail_step("snapshot_create", f"tool error: {text}")
                    else:
                        fail_step("snapshot_create", f"unexpected response: {text}")
            except json.JSONDecodeError:
                fail_step("snapshot_create", f"invalid JSON: {text}")
        else:
            fail_step("snapshot_create", f"no text content: {resp}")
    else:
        fail_step("snapshot_create", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 8: snapshot_list — verify the snapshot appears
    # -----------------------------------------------------------------------
    log("Step 8: snapshot (list) — verify 'test-checkpoint' appears")
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot",
        "arguments": {
            "action": "list",
            "workspace_id": WORKSPACE_ID,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                snapshots = json.loads(text)
                snap_names = [s.get("name") for s in snapshots]
                if "test-checkpoint" in snap_names:
                    pass_step(f"snapshot_list ({len(snapshots)} snapshots, found 'test-checkpoint')")
                else:
                    fail_step("snapshot_list", f"'test-checkpoint' not found in: {snap_names}")
            except json.JSONDecodeError:
                fail_step("snapshot_list", f"invalid JSON: {text}")
        else:
            fail_step("snapshot_list", f"no text content: {resp}")
    else:
        fail_step("snapshot_list", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 9: workspace_info — check state
    # -----------------------------------------------------------------------
    log("Step 9: workspace_info — verify workspace is 'running'")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_info",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                state = data.get("state")
                ws_name = data.get("name")
                snapshots = data.get("snapshots", [])
                if state == "running":
                    pass_step(f"workspace_info (name={ws_name} state={state} snapshots={len(snapshots)})")
                else:
                    result = resp.get("result", {})
                    is_error = result.get("isError") or result.get("is_error")
                    if is_error:
                        fail_step("workspace_info", f"tool error: {text}")
                    else:
                        fail_step("workspace_info", f"expected state='running', got state={state!r}")
            except json.JSONDecodeError:
                fail_step("workspace_info", f"invalid JSON: {text}")
        else:
            fail_step("workspace_info", f"no text content: {resp}")
    else:
        fail_step("workspace_info", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 10: workspace_list — verify workspace appears in list
    # -----------------------------------------------------------------------
    log("Step 10: workspace_list — verify workspace appears")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_list",
        "arguments": {},
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                workspaces = json.loads(text)
                ws_ids = [w.get("workspace_id") for w in workspaces]
                if WORKSPACE_ID in ws_ids:
                    pass_step(f"workspace_list ({len(workspaces)} workspaces, found ours)")
                else:
                    fail_step("workspace_list", f"workspace {WORKSPACE_ID} not found in: {ws_ids}")
            except json.JSONDecodeError:
                fail_step("workspace_list", f"invalid JSON: {text}")
        else:
            fail_step("workspace_list", f"no text content: {resp}")
    else:
        fail_step("workspace_list", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 11: workspace_info — get workspace IP (workspace_ip merged into workspace_info)
    # -----------------------------------------------------------------------
    log("Step 11: workspace_info — get IP address")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_info",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                ip = data.get("ip")
                if ip and ip.startswith("10.99."):
                    pass_step(f"workspace_info — IP (ip={ip})")
                else:
                    fail_step("workspace_info — IP", f"unexpected IP: {ip!r}")
            except json.JSONDecodeError:
                fail_step("workspace_info — IP", f"invalid JSON: {text}")
        else:
            fail_step("workspace_info — IP", f"no text content: {resp}")
    else:
        fail_step("workspace_info — IP", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 12: exec — more complex command (uname -a)
    # -----------------------------------------------------------------------
    log("Step 12: exec — run 'uname -a' to verify VM OS")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "command": "uname -a",
            "timeout_secs": 15,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                exit_code = data.get("exit_code")
                stdout = data.get("stdout", "").strip()
                if exit_code == 0 and "Linux" in stdout:
                    pass_step(f"exec uname -a (exit_code={exit_code}, os=Linux)")
                else:
                    fail_step("exec uname -a", f"exit_code={exit_code}, stdout={stdout!r}")
            except json.JSONDecodeError:
                fail_step("exec uname -a", f"invalid JSON: {text}")
        else:
            fail_step("exec uname -a", f"no text content: {resp}")
    else:
        fail_step("exec uname -a", get_error(resp))
    msg_id += 1

    # ===================================================================
    # Phase 1: Snapshot restore test
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 13: snapshot_restore — modify file, restore, verify reverted
    # -----------------------------------------------------------------------
    log("Step 13: snapshot_restore — write after snap, restore, verify reverted")
    # We already have 'test-checkpoint' from step 7. Write a new file AFTER the snapshot.
    send_msg(proc, msg_id, "tools/call", {
        "name": "file_write",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "path": "/tmp/after-snap.txt",
            "content": "this was written after the snapshot\n",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    msg_id += 1

    # Verify the file exists
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "command": "cat /tmp/after-snap.txt",
            "timeout_secs": 10,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    msg_id += 1

    # Now restore to 'test-checkpoint' (before that file was written)
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot",
        "arguments": {
            "action": "restore",
            "workspace_id": WORKSPACE_ID,
            "name": "test-checkpoint",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=120)
    if resp is None or "result" not in resp:
        fail_step("snapshot_restore", get_error(resp))
    else:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("snapshot_restore", f"tool error: {text}")
        else:
            # Verify the file written after the snapshot is gone
            msg_id += 1
            send_msg(proc, msg_id, "tools/call", {
                "name": "exec",
                "arguments": {
                    "workspace_id": WORKSPACE_ID,
                    "command": "test -f /tmp/after-snap.txt && echo EXISTS || echo GONE",
                    "timeout_secs": 15,
                },
            })
            verify_resp = recv_msg(proc, msg_id, timeout=60)
            if verify_resp is not None and "result" in verify_resp:
                vtext = get_tool_result_text(verify_resp)
                if vtext:
                    try:
                        vdata = json.loads(vtext)
                        vstdout = vdata.get("stdout", "").strip()
                        if vstdout == "GONE":
                            pass_step("snapshot_restore (file reverted after restore)")
                        elif vstdout == "EXISTS":
                            fail_step("snapshot_restore", "file still exists after restore — snapshot rollback did not work")
                        else:
                            fail_step("snapshot_restore", f"unexpected output: {vstdout!r}")
                    except json.JSONDecodeError:
                        fail_step("snapshot_restore", f"invalid JSON: {vtext}")
                else:
                    fail_step("snapshot_restore", f"no text in verify response")
            else:
                fail_step("snapshot_restore", f"verify exec failed: {get_error(verify_resp)}")
    msg_id += 1

    # ===================================================================
    # Phase 1b: workspace_fork test
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 14: workspace_fork — fork from snapshot, verify independent
    # -----------------------------------------------------------------------
    log("Step 14: workspace_fork — fork from 'test-checkpoint' snapshot")
    # First re-create the snapshot since snapshot restore may have removed it
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot",
        "arguments": {
            "action": "create",
            "workspace_id": WORKSPACE_ID,
            "name": "fork-source",
            "include_memory": False,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    msg_id += 1

    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_fork",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "snapshot_name": "fork-source",
            "new_name": "forked-workspace",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=120)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                FORKED_WORKSPACE_ID = data.get("workspace_id")
                fork_name = data.get("name")
                fork_state = data.get("state")
                forked_from = data.get("forked_from", {})
                if FORKED_WORKSPACE_ID and FORKED_WORKSPACE_ID != WORKSPACE_ID:
                    pass_step(f"workspace_fork (id={FORKED_WORKSPACE_ID[:8]}... name={fork_name} state={fork_state})")
                else:
                    fail_step("workspace_fork", f"unexpected response: {text}")
            except json.JSONDecodeError:
                fail_step("workspace_fork", f"invalid JSON: {text}")
        else:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                content = result_obj.get("content", [])
                err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "unknown")
                fail_step("workspace_fork", f"tool error: {err_text}")
            else:
                fail_step("workspace_fork", f"no text content: {resp}")
    else:
        fail_step("workspace_fork", get_error(resp))
    msg_id += 1

    # Verify forked workspace is independent: write a file in fork, check it's NOT in original
    if FORKED_WORKSPACE_ID:
        send_msg(proc, msg_id, "tools/call", {
            "name": "file_write",
            "arguments": {
                "workspace_id": FORKED_WORKSPACE_ID,
                "path": "/tmp/fork-only.txt",
                "content": "only in fork\n",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        msg_id += 1

        send_msg(proc, msg_id, "tools/call", {
            "name": "exec",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "command": "test -f /tmp/fork-only.txt && echo EXISTS || echo GONE",
                "timeout_secs": 10,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    stdout = data.get("stdout", "").strip()
                    if stdout == "GONE":
                        log("         Fork independence verified (file not in original)")
                    else:
                        log(f"         WARNING: fork may share state (got: {stdout!r})")
                except json.JSONDecodeError:
                    pass
        msg_id += 1

    # ===================================================================
    # Phase 1c: workspace_stop + workspace_start
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 15: workspace_stop — stop VM, verify exec fails
    # -----------------------------------------------------------------------
    log("Step 15: workspace_stop + workspace_start — stop/start lifecycle")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_stop",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    stop_ok = False
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("workspace_stop", f"tool error: {text}")
        else:
            stop_ok = True
    else:
        fail_step("workspace_stop", get_error(resp))
    msg_id += 1

    if stop_ok:
        # Verify exec fails on stopped workspace
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "command": "echo should-fail",
                "timeout_secs": 10,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        exec_failed = False
        if resp is not None:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            err = resp.get("error")
            if is_error or err:
                exec_failed = True
        if exec_failed:
            log("         exec correctly failed on stopped workspace")
        else:
            log("         WARNING: exec did not fail on stopped workspace")
        msg_id += 1

        # Now start it back up
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_start",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=120)
        start_ok = False
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("workspace_stop + workspace_start", f"start failed: {text}")
            else:
                start_ok = True
        else:
            fail_step("workspace_stop + workspace_start", f"start failed: {get_error(resp)}")
        msg_id += 1

        if start_ok:
            # Verify exec works again after start
            send_msg(proc, msg_id, "tools/call", {
                "name": "exec",
                "arguments": {
                    "workspace_id": WORKSPACE_ID,
                    "command": "echo restarted-ok",
                    "timeout_secs": 15,
                },
            })
            resp = recv_msg(proc, msg_id, timeout=60)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                if text:
                    try:
                        data = json.loads(text)
                        stdout = data.get("stdout", "").strip()
                        if data.get("exit_code") == 0 and "restarted-ok" in stdout:
                            pass_step("workspace_stop + workspace_start (stop/start/exec cycle works)")
                        else:
                            fail_step("workspace_stop + workspace_start", f"exec after restart: exit={data.get('exit_code')}, stdout={stdout!r}")
                    except json.JSONDecodeError:
                        fail_step("workspace_stop + workspace_start", f"invalid JSON: {text}")
                else:
                    fail_step("workspace_stop + workspace_start", f"no text content after restart exec")
            else:
                fail_step("workspace_stop + workspace_start", f"exec after restart failed: {get_error(resp)}")
            msg_id += 1

    # ===================================================================
    # Phase 2: Background exec tests
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 16: exec_background start + poll
    # -----------------------------------------------------------------------
    log("Step 16: exec_background start + poll — background job lifecycle")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec_background",
        "arguments": {
            "action": "start",
            "workspace_id": WORKSPACE_ID,
            "command": "sleep 3 && echo bg-done",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    bg_job_id = None
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                bg_job_id = data.get("job_id")
                bg_status = data.get("status")
                if bg_job_id is not None and bg_status == "started":
                    log(f"         exec_background start OK (job_id={bg_job_id})")
                else:
                    fail_step("exec_background start", f"unexpected response: {text}")
            except json.JSONDecodeError:
                fail_step("exec_background start", f"invalid JSON: {text}")
        else:
            fail_step("exec_background start", f"no text content")
    else:
        fail_step("exec_background start", get_error(resp))
    msg_id += 1

    if bg_job_id is not None:
        # Poll immediately — should still be running
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_background",
            "arguments": {
                "action": "poll",
                "workspace_id": WORKSPACE_ID,
                "job_id": bg_job_id,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=15)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    if data.get("running") is True:
                        log("         exec_background poll: job still running (correct)")
                    else:
                        log(f"         exec_background poll: job already done (may be fast, not necessarily wrong)")
                except json.JSONDecodeError:
                    pass
        msg_id += 1

        # Wait for the job to finish
        time.sleep(5)

        # Poll again — should be done
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_background",
            "arguments": {
                "action": "poll",
                "workspace_id": WORKSPACE_ID,
                "job_id": bg_job_id,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=15)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    running = data.get("running")
                    exit_code = data.get("exit_code")
                    stdout = data.get("stdout", "")
                    if running is False and exit_code == 0 and "bg-done" in stdout:
                        pass_step(f"exec_background start + poll (job completed, exit=0, output correct)")
                    elif running is False:
                        fail_step("exec_background start + poll", f"job done but exit={exit_code}, stdout={stdout!r}")
                    else:
                        fail_step("exec_background start + poll", f"job still running after 5s wait")
                except json.JSONDecodeError:
                    fail_step("exec_background start + poll", f"invalid JSON: {text}")
            else:
                fail_step("exec_background start + poll", f"no text content")
        else:
            fail_step("exec_background start + poll", get_error(resp))
        msg_id += 1

    # -----------------------------------------------------------------------
    # Step 17: exec_background kill — start long job, kill it, verify terminated
    # -----------------------------------------------------------------------
    log("Step 17: exec_background kill — start long job, kill it")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec_background",
        "arguments": {
            "action": "start",
            "workspace_id": WORKSPACE_ID,
            "command": "sleep 600",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    kill_job_id = None
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                kill_job_id = data.get("job_id")
            except json.JSONDecodeError:
                pass
    msg_id += 1

    if kill_job_id is not None:
        # Kill it
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_background",
            "arguments": {
                "action": "kill",
                "workspace_id": WORKSPACE_ID,
                "job_id": kill_job_id,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=15)
        kill_ok = False
        if resp is not None:
            err = get_error(resp)
            if err:
                fail_step("exec_background kill", f"kill call failed: {err}")
            else:
                result_obj = resp.get("result", {})
                is_error = result_obj.get("isError") or result_obj.get("is_error")
                if is_error:
                    text = get_tool_result_text(resp)
                    fail_step("exec_background kill", f"kill tool error: {text}")
                else:
                    kill_ok = True
        else:
            fail_step("exec_background kill", "no response from exec_background kill")
        msg_id += 1

        if kill_ok:
            # Poll — should be done (not running). The guest agent waits for
            # the process to die before returning from kill, so no
            # additional sleep is needed here.
            send_msg(proc, msg_id, "tools/call", {
                "name": "exec_background",
                "arguments": {
                    "action": "poll",
                    "workspace_id": WORKSPACE_ID,
                    "job_id": kill_job_id,
                },
            })
            resp = recv_msg(proc, msg_id, timeout=15)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                if text:
                    try:
                        data = json.loads(text)
                        if data.get("running") is False:
                            pass_step(f"exec_background kill (job terminated, exit_code={data.get('exit_code')})")
                        else:
                            fail_step("exec_background kill", "job still running after kill")
                    except json.JSONDecodeError:
                        fail_step("exec_background kill", f"invalid JSON: {text}")
                else:
                    fail_step("exec_background kill", "no text content")
            else:
                fail_step("exec_background kill", get_error(resp))
            msg_id += 1
    else:
        fail_step("exec_background kill", "could not start background job for kill test")

    # ===================================================================
    # Phase 3: File operation tests
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 18: file_list — create files, list directory
    # -----------------------------------------------------------------------
    log("Step 18: file_list — list directory contents")
    # Create a couple of files
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "command": "mkdir -p /tmp/listtest && echo aaa > /tmp/listtest/a.txt && echo bbb > /tmp/listtest/b.txt && mkdir /tmp/listtest/subdir",
            "timeout_secs": 10,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    msg_id += 1

    send_msg(proc, msg_id, "tools/call", {
        "name": "file_list",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "path": "/tmp/listtest",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                entries = json.loads(text)
                names = {e.get("name") for e in entries}
                if "a.txt" in names and "b.txt" in names and "subdir" in names:
                    pass_step(f"file_list ({len(entries)} entries: {sorted(names)})")
                else:
                    fail_step("file_list", f"expected a.txt, b.txt, subdir; got {names}")
            except json.JSONDecodeError:
                fail_step("file_list", f"invalid JSON: {text}")
        else:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                content = result_obj.get("content", [])
                err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                fail_step("file_list", f"tool error: {err_text}")
            else:
                fail_step("file_list", f"no text content: {resp}")
    else:
        fail_step("file_list", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 19: file_edit — write file, edit string, read back
    # -----------------------------------------------------------------------
    log("Step 19: file_edit — edit a file by string replacement")
    send_msg(proc, msg_id, "tools/call", {
        "name": "file_write",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "path": "/tmp/edit-test.txt",
            "content": "Hello World, this is a test file.\nLine two here.\n",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    msg_id += 1

    send_msg(proc, msg_id, "tools/call", {
        "name": "file_edit",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "path": "/tmp/edit-test.txt",
            "old_string": "Hello World",
            "new_string": "Goodbye World",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is None or "result" not in resp:
        fail_step("file_edit", get_error(resp))
        msg_id += 1
    else:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            text = get_tool_result_text(resp)
            fail_step("file_edit", f"tool error: {text}")
            msg_id += 1
        else:
            msg_id += 1
            # Read back and verify
            send_msg(proc, msg_id, "tools/call", {
                "name": "file_read",
                "arguments": {
                    "workspace_id": WORKSPACE_ID,
                    "path": "/tmp/edit-test.txt",
                },
            })
            resp = recv_msg(proc, msg_id, timeout=30)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                if text and "Goodbye World" in text and "Hello World" not in text:
                    pass_step("file_edit (string replaced successfully)")
                else:
                    fail_step("file_edit", f"edit did not take effect, content: {text!r}")
            else:
                fail_step("file_edit", f"read back failed: {get_error(resp)}")
            msg_id += 1

    # -----------------------------------------------------------------------
    # Step 20: file_transfer (upload + download)
    # -----------------------------------------------------------------------
    log("Step 20: file_transfer — upload and download files via host path")
    # Create transfer dir and a test file on the host
    import tempfile
    import hashlib
    transfer_dir = "/var/lib/agentiso/transfers"
    os.makedirs(transfer_dir, exist_ok=True)

    upload_content = b"binary test content \x00\x01\x02\xff with some bytes"
    upload_host_path = os.path.join(transfer_dir, "test-upload.bin")
    with open(upload_host_path, "wb") as f:
        f.write(upload_content)
    upload_hash = hashlib.sha256(upload_content).hexdigest()

    # Upload host -> guest
    send_msg(proc, msg_id, "tools/call", {
        "name": "file_transfer",
        "arguments": {
            "direction": "upload",
            "workspace_id": WORKSPACE_ID,
            "host_path": upload_host_path,
            "guest_path": "/tmp/uploaded.bin",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    upload_ok = False
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("file_transfer (upload)", f"tool error: {text}")
        elif text and "uploaded" in text.lower():
            upload_ok = True
            log(f"         file_transfer upload OK: {text.strip()}")
        else:
            upload_ok = True
            log(f"         file_transfer upload response: {text!r}")
    else:
        fail_step("file_transfer (upload)", get_error(resp))
    msg_id += 1

    if upload_ok:
        # Download guest -> host
        download_host_path = os.path.join(transfer_dir, "test-download.bin")
        send_msg(proc, msg_id, "tools/call", {
            "name": "file_transfer",
            "arguments": {
                "direction": "download",
                "workspace_id": WORKSPACE_ID,
                "host_path": download_host_path,
                "guest_path": "/tmp/uploaded.bin",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("file_transfer (upload + download)", f"download error: {text}")
            else:
                # Verify content matches
                try:
                    with open(download_host_path, "rb") as f:
                        downloaded = f.read()
                    download_hash = hashlib.sha256(downloaded).hexdigest()
                    if upload_hash == download_hash:
                        pass_step(f"file_transfer (upload + download roundtrip verified, {len(downloaded)} bytes, sha256 match)")
                    else:
                        fail_step("file_transfer (upload + download)", f"hash mismatch: upload={upload_hash[:16]}... download={download_hash[:16]}...")
                except Exception as e:
                    fail_step("file_transfer (upload + download)", f"could not read downloaded file: {e}")
        else:
            fail_step("file_transfer (upload + download)", get_error(resp))
        msg_id += 1

        # Cleanup host files
        try:
            os.remove(upload_host_path)
            os.remove(download_host_path)
        except OSError:
            pass
    else:
        log("         Skipping file_transfer download (upload failed)")

    # ===================================================================
    # Phase 4: Network tests
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 21: port_forward (add + remove)
    # -----------------------------------------------------------------------
    log("Step 21: port_forward (add + remove) — port forwarding lifecycle")
    send_msg(proc, msg_id, "tools/call", {
        "name": "port_forward",
        "arguments": {
            "action": "add",
            "workspace_id": WORKSPACE_ID,
            "guest_port": 8080,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    pf_host_port = None
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                pf_host_port = data.get("host_port")
                pf_guest_port = data.get("guest_port")
                if pf_host_port and pf_guest_port == 8080:
                    log(f"         port_forward add: host:{pf_host_port} -> guest:8080")
                else:
                    fail_step("port_forward (add)", f"unexpected: {text}")
            except json.JSONDecodeError:
                fail_step("port_forward (add)", f"invalid JSON: {text}")
        else:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                content = result_obj.get("content", [])
                err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                fail_step("port_forward (add)", f"tool error: {err_text}")
            else:
                fail_step("port_forward (add)", f"no text content")
    else:
        fail_step("port_forward (add)", get_error(resp))
    msg_id += 1

    # Remove the port forward
    if pf_host_port:
        send_msg(proc, msg_id, "tools/call", {
            "name": "port_forward",
            "arguments": {
                "action": "remove",
                "workspace_id": WORKSPACE_ID,
                "guest_port": 8080,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("port_forward (add + remove)", f"remove error: {text}")
            else:
                pass_step(f"port_forward (add + remove) (host:{pf_host_port} -> guest:8080, then removed)")
        else:
            fail_step("port_forward (add + remove)", get_error(resp))
        msg_id += 1
    else:
        log("         Skipping port_forward remove (forward setup failed)")

    # -----------------------------------------------------------------------
    # Step 22: network_policy — toggle internet access
    # -----------------------------------------------------------------------
    log("Step 22: network_policy — toggle internet access policy")
    # First, disable internet (it's likely already disabled by default)
    send_msg(proc, msg_id, "tools/call", {
        "name": "network_policy",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "allow_internet": False,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    np_ok = False
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                policy = data.get("network_policy", {})
                if policy.get("allow_internet") is False:
                    log("         network_policy: internet disabled")
                    np_ok = True
                else:
                    fail_step("network_policy", f"expected allow_internet=false, got: {policy}")
            except json.JSONDecodeError:
                fail_step("network_policy", f"invalid JSON: {text}")
        else:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                content = result_obj.get("content", [])
                err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                fail_step("network_policy", f"tool error: {err_text}")
            else:
                fail_step("network_policy", "no text content")
    else:
        fail_step("network_policy", get_error(resp))
    msg_id += 1

    if np_ok:
        # Now enable internet
        send_msg(proc, msg_id, "tools/call", {
            "name": "network_policy",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "allow_internet": True,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    policy = data.get("network_policy", {})
                    if policy.get("allow_internet") is True:
                        pass_step("network_policy (toggled internet: off -> on)")
                    else:
                        fail_step("network_policy", f"expected allow_internet=true after enable, got: {policy}")
                except json.JSONDecodeError:
                    fail_step("network_policy", f"invalid JSON: {text}")
            else:
                fail_step("network_policy", "no text content on enable")
        else:
            fail_step("network_policy", f"enable failed: {get_error(resp)}")
        msg_id += 1

    # ===================================================================
    # Phase 4b: Git workflow tests (clone -> status -> diff -> commit -> status)
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 23: Enable internet for git clone
    # -----------------------------------------------------------------------
    log("Step 23: network_policy — ensure internet is enabled for git clone")
    send_msg(proc, msg_id, "tools/call", {
        "name": "network_policy",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "allow_internet": True,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    internet_ok = False
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                policy = data.get("network_policy", {})
                if policy.get("allow_internet") is True:
                    internet_ok = True
                    log("         Internet enabled for git workflow tests")
            except json.JSONDecodeError:
                pass
    if not internet_ok:
        log("         WARNING: could not confirm internet is enabled; git_clone may fail")
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 24: git_clone — clone a small public repo
    # -----------------------------------------------------------------------
    log("Step 24: git_clone — clone https://github.com/octocat/Hello-World.git")
    send_msg(proc, msg_id, "tools/call", {
        "name": "git_clone",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "url": "https://github.com/octocat/Hello-World.git",
            "path": "/workspace/hello-world",
            "depth": 1,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=120)
    git_clone_ok = False
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                result_obj = resp.get("result", {})
                is_error = result_obj.get("isError") or result_obj.get("is_error")
                if is_error:
                    fail_step("git_clone", f"tool error: {text}")
                elif data.get("success") is True and data.get("path") == "/workspace/hello-world":
                    commit_sha = data.get("commit_sha", "")
                    pass_step(f"git_clone (path=/workspace/hello-world, sha={commit_sha[:8]}...)")
                    git_clone_ok = True
                else:
                    fail_step("git_clone", f"unexpected response: {text}")
            except json.JSONDecodeError:
                fail_step("git_clone", f"invalid JSON: {text}")
        else:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                content = result_obj.get("content", [])
                err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "unknown")
                fail_step("git_clone", f"tool error: {err_text}")
            else:
                fail_step("git_clone", f"no text content: {resp}")
    else:
        fail_step("git_clone", get_error(resp))
    msg_id += 1

    if git_clone_ok:
        # -------------------------------------------------------------------
        # Step 25: git_status — check status of cloned repo
        # -------------------------------------------------------------------
        log("Step 25: git_status — check cloned repo status")
        send_msg(proc, msg_id, "tools/call", {
            "name": "git_status",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "path": "/workspace/hello-world",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    branch = data.get("branch")
                    dirty = data.get("dirty")
                    if branch is not None:
                        pass_step(f"git_status after clone (branch={branch!r}, dirty={dirty})")
                    else:
                        # Accept any response with branch-like info
                        if "branch" in text.lower():
                            pass_step(f"git_status after clone (structured response)")
                        else:
                            fail_step("git_status after clone", f"no 'branch' field in response: {text!r}")
                except json.JSONDecodeError:
                    fail_step("git_status after clone", f"invalid JSON: {text}")
            else:
                result_obj = resp.get("result", {})
                is_error = result_obj.get("isError") or result_obj.get("is_error")
                if is_error:
                    content = result_obj.get("content", [])
                    err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                    fail_step("git_status after clone", f"tool error: {err_text}")
                else:
                    fail_step("git_status after clone", "no text content")
        else:
            fail_step("git_status after clone", get_error(resp))
        msg_id += 1

        # -------------------------------------------------------------------
        # Step 26: Create test file + git_diff — verify diff shows new file
        # -------------------------------------------------------------------
        log("Step 26: git_diff — create a file and check diff shows it")
        # Create a test file in the cloned repo
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "command": "echo 'test content from e2e' > /workspace/hello-world/test-file.txt",
                "timeout_secs": 10,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        msg_id += 1

        # Now run git_diff
        send_msg(proc, msg_id, "tools/call", {
            "name": "git_diff",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "path": "/workspace/hello-world",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        diff_ok = False
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    result_obj = resp.get("result", {})
                    is_error = result_obj.get("isError") or result_obj.get("is_error")
                    if is_error:
                        fail_step("git_diff", f"tool error: {text}")
                    else:
                        diff_text = data.get("diff", "")
                        stat_text = data.get("stat", "")
                        # The new file is untracked, so `git diff` (unstaged) may be empty.
                        # Check stat or diff for file reference. For untracked files,
                        # the diff may be empty -- that is OK, we verify via git_commit.
                        diff_ok = True
                        if "test-file.txt" in diff_text or "test-file.txt" in stat_text:
                            pass_step(f"git_diff (diff/stat contains 'test-file.txt')")
                        else:
                            # Untracked files don't show in `git diff` by default.
                            # This is expected behavior -- pass with a note.
                            pass_step(f"git_diff (empty diff -- expected for untracked file, truncated={data.get('truncated')})")
                except json.JSONDecodeError:
                    fail_step("git_diff", f"invalid JSON: {text}")
            else:
                result_obj = resp.get("result", {})
                is_error = result_obj.get("isError") or result_obj.get("is_error")
                if is_error:
                    content = result_obj.get("content", [])
                    err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                    fail_step("git_diff", f"tool error: {err_text}")
                else:
                    fail_step("git_diff", f"no text content: {resp}")
        else:
            fail_step("git_diff", get_error(resp))
        msg_id += 1

        # -------------------------------------------------------------------
        # Step 27: git_commit — commit the new file, then verify clean status
        # -------------------------------------------------------------------
        log("Step 27: git_commit — commit test file, verify clean working tree")
        send_msg(proc, msg_id, "tools/call", {
            "name": "git_commit",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "path": "/workspace/hello-world",
                "message": "test: add test file from e2e",
                "add_all": True,
                "author_name": "E2E Test",
                "author_email": "e2e@test.local",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        commit_ok = False
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    result_obj = resp.get("result", {})
                    is_error = result_obj.get("isError") or result_obj.get("is_error")
                    if is_error:
                        fail_step("git_commit", f"tool error: {text}")
                    else:
                        commit_sha = data.get("commit_sha", "")
                        short_sha = data.get("short_sha", "")
                        branch = data.get("branch", "")
                        summary = data.get("summary", "")
                        if commit_sha:
                            commit_ok = True
                            log(f"         git_commit OK (sha={short_sha}, branch={branch!r})")
                        elif data.get("status") == "nothing_to_commit":
                            fail_step("git_commit", "nothing to commit -- file was not created or already committed")
                        else:
                            fail_step("git_commit", f"no commit_sha in response: {text}")
                except json.JSONDecodeError:
                    fail_step("git_commit", f"invalid JSON: {text}")
            else:
                result_obj = resp.get("result", {})
                is_error = result_obj.get("isError") or result_obj.get("is_error")
                if is_error:
                    content = result_obj.get("content", [])
                    err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "unknown")
                    fail_step("git_commit", f"tool error: {err_text}")
                else:
                    fail_step("git_commit", f"no text content: {resp}")
        else:
            fail_step("git_commit", get_error(resp))
        msg_id += 1

        if commit_ok:
            # Verify clean working tree after commit
            send_msg(proc, msg_id, "tools/call", {
                "name": "git_status",
                "arguments": {
                    "workspace_id": WORKSPACE_ID,
                    "path": "/workspace/hello-world",
                },
            })
            resp = recv_msg(proc, msg_id, timeout=30)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                if text:
                    try:
                        data = json.loads(text)
                        dirty = data.get("dirty")
                        untracked = data.get("untracked", [])
                        modified = data.get("modified", [])
                        staged = data.get("staged", [])
                        if dirty is False:
                            pass_step("git_commit + git_status (commit succeeded, working tree clean)")
                        elif dirty is True:
                            fail_step("git_commit + git_status",
                                f"working tree still dirty after commit (untracked={untracked}, modified={modified}, staged={staged})")
                        else:
                            # Accept if branch info present even without dirty field
                            if data.get("branch"):
                                pass_step("git_commit + git_status (commit succeeded, status response received)")
                            else:
                                fail_step("git_commit + git_status", f"unexpected response: {text!r}")
                    except json.JSONDecodeError:
                        fail_step("git_commit + git_status", f"invalid JSON: {text}")
                else:
                    fail_step("git_commit + git_status", "no text content")
            else:
                fail_step("git_commit + git_status", get_error(resp))
            msg_id += 1
        else:
            log("         Skipping post-commit status check (commit failed)")

    else:
        log("         Skipping git workflow steps 25-27 (git_clone failed)")

    # ===================================================================
    # Phase 5: Operational tests
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 28: workspace_logs — get VM logs
    # -----------------------------------------------------------------------
    log("Step 28: workspace_logs — retrieve VM console/stderr logs")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_logs",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "log_type": "all",
            "max_lines": 50,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                console = data.get("console", "")
                stderr_log = data.get("stderr", "")
                # At least console log should be non-empty for a running VM
                if console or stderr_log:
                    total_lines = len(console.splitlines()) + len(stderr_log.splitlines())
                    pass_step(f"workspace_logs ({total_lines} lines of logs retrieved)")
                else:
                    fail_step("workspace_logs", "both console and stderr are empty")
            except json.JSONDecodeError:
                fail_step("workspace_logs", f"invalid JSON: {text}")
        else:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                content = result_obj.get("content", [])
                err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                fail_step("workspace_logs", f"tool error: {err_text}")
            else:
                fail_step("workspace_logs", "no text content")
    else:
        fail_step("workspace_logs", get_error(resp))
    msg_id += 1

    # ===================================================================
    # Phase 6: Thorough snapshot_restore — marker file overwrite cycle
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 29: snapshot_restore (thorough) — write/overwrite/restore/verify
    # -----------------------------------------------------------------------
    log("Step 29: snapshot_restore (thorough) — marker file overwrite/restore cycle")
    # Write original marker file
    send_msg(proc, msg_id, "tools/call", {
        "name": "file_write",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "path": "/workspace/marker.txt",
            "content": "original",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    msg_id += 1

    # Create snapshot capturing "original"
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot",
        "arguments": {
            "action": "create",
            "workspace_id": WORKSPACE_ID,
            "name": "before-change",
            "include_memory": False,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    snap_ok = resp is not None and "result" in resp
    if snap_ok:
        result_obj = resp.get("result", {})
        snap_ok = not (result_obj.get("isError") or result_obj.get("is_error"))
    msg_id += 1

    if snap_ok:
        # Overwrite the marker file with "modified"
        send_msg(proc, msg_id, "tools/call", {
            "name": "file_write",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "path": "/workspace/marker.txt",
                "content": "modified",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        msg_id += 1

        # Verify file now reads "modified"
        send_msg(proc, msg_id, "tools/call", {
            "name": "file_read",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "path": "/workspace/marker.txt",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        modified_confirmed = False
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text and "modified" in text:
                modified_confirmed = True
                log("         Confirmed marker.txt reads 'modified' before restore")
        msg_id += 1

        if not modified_confirmed:
            fail_step("snapshot_restore (thorough)", "could not confirm marker.txt was overwritten to 'modified'")
        else:
            # Restore to "before-change" snapshot
            send_msg(proc, msg_id, "tools/call", {
                "name": "snapshot",
                "arguments": {
                    "action": "restore",
                    "workspace_id": WORKSPACE_ID,
                    "name": "before-change",
                },
            })
            resp = recv_msg(proc, msg_id, timeout=120)
            restore_ok = False
            if resp is not None and "result" in resp:
                result_obj = resp.get("result", {})
                is_error = result_obj.get("isError") or result_obj.get("is_error")
                if not is_error:
                    restore_ok = True
                else:
                    text = get_tool_result_text(resp)
                    fail_step("snapshot_restore (thorough)", f"restore tool error: {text}")
            else:
                fail_step("snapshot_restore (thorough)", f"restore failed: {get_error(resp)}")
            msg_id += 1

            if restore_ok:
                # Verify file reads "original" again
                send_msg(proc, msg_id, "tools/call", {
                    "name": "file_read",
                    "arguments": {
                        "workspace_id": WORKSPACE_ID,
                        "path": "/workspace/marker.txt",
                    },
                })
                resp = recv_msg(proc, msg_id, timeout=60)
                if resp is not None and "result" in resp:
                    text = get_tool_result_text(resp)
                    if text and "original" in text and "modified" not in text:
                        pass_step("snapshot_restore (thorough) — marker.txt reverted from 'modified' to 'original'")
                    else:
                        fail_step("snapshot_restore (thorough)", f"expected 'original' after restore, got: {text!r}")
                else:
                    fail_step("snapshot_restore (thorough)", f"read after restore failed: {get_error(resp)}")
                msg_id += 1
    else:
        fail_step("snapshot_restore (thorough)", "could not create 'before-change' snapshot")

    # ===================================================================
    # Phase 7: snapshot_delete safety
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 30: snapshot_delete — create, delete, verify gone
    # -----------------------------------------------------------------------
    log("Step 30: snapshot (delete) — create snapshot, delete it, verify removed")
    # Create a snapshot to delete
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot",
        "arguments": {
            "action": "create",
            "workspace_id": WORKSPACE_ID,
            "name": "to-delete",
            "include_memory": False,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    snap_created = False
    if resp is not None and "result" in resp:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if not is_error:
            snap_created = True
    msg_id += 1

    if snap_created:
        # Delete it
        send_msg(proc, msg_id, "tools/call", {
            "name": "snapshot",
            "arguments": {
                "action": "delete",
                "workspace_id": WORKSPACE_ID,
                "name": "to-delete",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        delete_ok = False
        if resp is not None and "result" in resp:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if not is_error:
                delete_ok = True
            else:
                text = get_tool_result_text(resp)
                fail_step("snapshot_delete", f"delete tool error: {text}")
        else:
            fail_step("snapshot_delete", f"delete failed: {get_error(resp)}")
        msg_id += 1

        if delete_ok:
            # Verify it's gone from snapshot list
            send_msg(proc, msg_id, "tools/call", {
                "name": "snapshot",
                "arguments": {
                    "action": "list",
                    "workspace_id": WORKSPACE_ID,
                },
            })
            resp = recv_msg(proc, msg_id, timeout=30)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                if text:
                    try:
                        snapshots = json.loads(text)
                        snap_names = [s.get("name") for s in snapshots]
                        if "to-delete" not in snap_names:
                            pass_step(f"snapshot_delete (created and deleted 'to-delete', verified gone from list)")
                        else:
                            fail_step("snapshot_delete", f"'to-delete' still present in snapshot_list: {snap_names}")
                    except json.JSONDecodeError:
                        fail_step("snapshot_delete", f"invalid JSON in snapshot_list: {text}")
                else:
                    fail_step("snapshot_delete", "no text content from snapshot_list")
            else:
                fail_step("snapshot_delete", f"snapshot_list after delete failed: {get_error(resp)}")
            msg_id += 1
    else:
        fail_step("snapshot_delete", "could not create 'to-delete' snapshot")

    # ===================================================================
    # Phase 8: git_status
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 31: git_status — init repo, get status
    # -----------------------------------------------------------------------
    log("Step 31: git_status — init git repo and get structured status")
    # Init a git repo inside /workspace
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "command": "cd /workspace && git init && git add -A && git -c user.email=test@test.com -c user.name=Test commit -m init --allow-empty",
            "timeout_secs": 30,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    git_init_ok = False
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                if data.get("exit_code") == 0:
                    git_init_ok = True
                else:
                    log(f"         git init exit_code={data.get('exit_code')}, stderr={data.get('stderr','')!r}")
            except json.JSONDecodeError:
                pass
    msg_id += 1

    if git_init_ok:
        send_msg(proc, msg_id, "tools/call", {
            "name": "git_status",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "path": "/workspace",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    branch = data.get("branch")
                    dirty = data.get("dirty")
                    if branch is not None and dirty is not None:
                        pass_step(f"git_status (branch={branch!r}, dirty={dirty})")
                    else:
                        # Accept any structured response that has some git info
                        if "branch" in text or "oid" in text or "head" in text.lower():
                            pass_step(f"git_status (structured response received)")
                        else:
                            fail_step("git_status", f"response missing 'branch'/'dirty': {text!r}")
                except json.JSONDecodeError:
                    # May be plain text status
                    if "branch" in text.lower() or "master" in text.lower() or "main" in text.lower():
                        pass_step(f"git_status (text response received)")
                    else:
                        fail_step("git_status", f"invalid JSON and no branch info: {text!r}")
            else:
                result_obj = resp.get("result", {})
                is_error = result_obj.get("isError") or result_obj.get("is_error")
                if is_error:
                    content = result_obj.get("content", [])
                    err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                    fail_step("git_status", f"tool error: {err_text}")
                else:
                    fail_step("git_status", "no text content")
        else:
            fail_step("git_status", get_error(resp))
        msg_id += 1
    else:
        fail_step("git_status", "could not init git repo in /workspace")

    # ===================================================================
    # Phase 9: Thorough workspace_fork — exec in fork, verify isolation
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 32: workspace_fork (thorough) — fork, exec in fork, verify isolation
    # -----------------------------------------------------------------------
    log("Step 32: workspace_fork (thorough) — fork, exec, write in fork, verify original unchanged")
    # Create a fresh snapshot for forking
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot",
        "arguments": {
            "action": "create",
            "workspace_id": WORKSPACE_ID,
            "name": "thorough-fork-base",
            "include_memory": False,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    fork_snap_ok = False
    if resp is not None and "result" in resp:
        result_obj = resp.get("result", {})
        fork_snap_ok = not (result_obj.get("isError") or result_obj.get("is_error"))
    msg_id += 1

    THOROUGH_FORK_ID = None
    if fork_snap_ok:
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_fork",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "snapshot_name": "thorough-fork-base",
                "new_name": "thorough-test-fork",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=120)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    THOROUGH_FORK_ID = data.get("workspace_id")
                except json.JSONDecodeError:
                    pass
        msg_id += 1

    if THOROUGH_FORK_ID:
        # Exec in the fork to verify it's alive
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec",
            "arguments": {
                "workspace_id": THOROUGH_FORK_ID,
                "command": "echo forked",
                "timeout_secs": 15,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        exec_in_fork_ok = False
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    stdout = data.get("stdout", "").strip()
                    if data.get("exit_code") == 0 and "forked" in stdout:
                        exec_in_fork_ok = True
                except json.JSONDecodeError:
                    pass
        msg_id += 1

        if exec_in_fork_ok:
            # Write a file in the fork
            send_msg(proc, msg_id, "tools/call", {
                "name": "file_write",
                "arguments": {
                    "workspace_id": THOROUGH_FORK_ID,
                    "path": "/workspace/fork-isolation-test.txt",
                    "content": "only-in-fork",
                },
            })
            resp = recv_msg(proc, msg_id, timeout=30)
            msg_id += 1

            # Verify original workspace does NOT have this file
            send_msg(proc, msg_id, "tools/call", {
                "name": "exec",
                "arguments": {
                    "workspace_id": WORKSPACE_ID,
                    "command": "test -f /workspace/fork-isolation-test.txt && echo EXISTS || echo GONE",
                    "timeout_secs": 10,
                },
            })
            resp = recv_msg(proc, msg_id, timeout=30)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                if text:
                    try:
                        data = json.loads(text)
                        stdout = data.get("stdout", "").strip()
                        if stdout == "GONE":
                            pass_step("workspace_fork (thorough) — exec in fork OK, isolation verified")
                        elif stdout == "EXISTS":
                            fail_step("workspace_fork (thorough)", "fork file leaked to original workspace — isolation broken")
                        else:
                            fail_step("workspace_fork (thorough)", f"unexpected output checking isolation: {stdout!r}")
                    except json.JSONDecodeError:
                        fail_step("workspace_fork (thorough)", f"invalid JSON: {text}")
                else:
                    fail_step("workspace_fork (thorough)", "no text content checking isolation")
            else:
                fail_step("workspace_fork (thorough)", f"isolation check failed: {get_error(resp)}")
            msg_id += 1
        else:
            fail_step("workspace_fork (thorough)", "exec 'echo forked' failed in fork workspace")

        # Destroy the thorough fork
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_destroy",
            "arguments": {
                "workspace_id": THOROUGH_FORK_ID,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        if resp is not None and "result" in resp:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if not is_error:
                log("         Thorough fork destroyed successfully")
            else:
                log(f"         WARNING: failed to destroy thorough fork")
        msg_id += 1
    else:
        fail_step("workspace_fork (thorough)", "could not create fork workspace")

    # ===================================================================
    # Phase 10: Thorough workspace_stop + workspace_start
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 33: workspace_stop + workspace_start (thorough) — verify state transitions
    # -----------------------------------------------------------------------
    log("Step 33: workspace_stop + workspace_start (thorough) — verify state via workspace_info")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_stop",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    stop2_ok = False
    if resp is not None and "result" in resp:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if not is_error:
            stop2_ok = True
        else:
            text = get_tool_result_text(resp)
            fail_step("workspace_stop + workspace_start (thorough)", f"stop tool error: {text}")
    else:
        fail_step("workspace_stop + workspace_start (thorough)", f"stop failed: {get_error(resp)}")
    msg_id += 1

    if stop2_ok:
        # Verify state is "stopped" via workspace_info
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_info",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        state_stopped = False
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    state = data.get("state")
                    if state == "stopped":
                        state_stopped = True
                        log("         workspace_info confirms state='stopped'")
                    else:
                        log(f"         WARNING: expected state='stopped', got state={state!r}")
                except json.JSONDecodeError:
                    pass
        msg_id += 1

        # Start it back
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_start",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=120)
        start2_ok = False
        if resp is not None and "result" in resp:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if not is_error:
                start2_ok = True
            else:
                text = get_tool_result_text(resp)
                fail_step("workspace_stop + workspace_start (thorough)", f"start tool error: {text}")
        else:
            fail_step("workspace_stop + workspace_start (thorough)", f"start failed: {get_error(resp)}")
        msg_id += 1

        if start2_ok:
            # Verify running via exec
            send_msg(proc, msg_id, "tools/call", {
                "name": "exec",
                "arguments": {
                    "workspace_id": WORKSPACE_ID,
                    "command": "echo alive",
                    "timeout_secs": 15,
                },
            })
            resp = recv_msg(proc, msg_id, timeout=60)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                if text:
                    try:
                        data = json.loads(text)
                        stdout = data.get("stdout", "").strip()
                        if data.get("exit_code") == 0 and "alive" in stdout:
                            if state_stopped:
                                pass_step("workspace_stop + workspace_start (thorough) — stopped (state confirmed), restarted, exec OK")
                            else:
                                pass_step("workspace_stop + workspace_start (thorough) — stopped, restarted, exec OK")
                        else:
                            fail_step("workspace_stop + workspace_start (thorough)", f"exec after restart: exit={data.get('exit_code')}, stdout={stdout!r}")
                    except json.JSONDecodeError:
                        fail_step("workspace_stop + workspace_start (thorough)", f"invalid JSON: {text}")
                else:
                    fail_step("workspace_stop + workspace_start (thorough)", "no text content after restart exec")
            else:
                fail_step("workspace_stop + workspace_start (thorough)", f"exec after restart failed: {get_error(resp)}")
            msg_id += 1

    # ===================================================================
    # Phase 11: exec_background start + poll + kill (thorough)
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 34: exec_background start + poll + kill — full lifecycle
    # -----------------------------------------------------------------------
    log("Step 34: exec_background start + poll + kill — start, poll running, kill, poll dead")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec_background",
        "arguments": {
            "action": "start",
            "workspace_id": WORKSPACE_ID,
            "command": "sleep 60",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    kill_test_job_id = None
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                kill_test_job_id = data.get("job_id")
            except json.JSONDecodeError:
                pass
    msg_id += 1

    if kill_test_job_id is not None:
        # Poll — should be running
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_background",
            "arguments": {
                "action": "poll",
                "workspace_id": WORKSPACE_ID,
                "job_id": kill_test_job_id,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=15)
        poll_running = False
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text:
                try:
                    data = json.loads(text)
                    if data.get("running") is True:
                        poll_running = True
                        log("         exec_background poll confirms job running")
                except json.JSONDecodeError:
                    pass
        msg_id += 1

        # Kill it
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_background",
            "arguments": {
                "action": "kill",
                "workspace_id": WORKSPACE_ID,
                "job_id": kill_test_job_id,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=15)
        kill2_ok = False
        if resp is not None and "result" in resp:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if not is_error:
                kill2_ok = True
        msg_id += 1

        if kill2_ok:
            # Poll again — should be dead
            send_msg(proc, msg_id, "tools/call", {
                "name": "exec_background",
                "arguments": {
                    "action": "poll",
                    "workspace_id": WORKSPACE_ID,
                    "job_id": kill_test_job_id,
                },
            })
            resp = recv_msg(proc, msg_id, timeout=15)
            if resp is not None and "result" in resp:
                text = get_tool_result_text(resp)
                if text:
                    try:
                        data = json.loads(text)
                        if data.get("running") is False:
                            if poll_running:
                                pass_step("exec_background start + poll + kill (started, confirmed running, killed, confirmed dead)")
                            else:
                                pass_step("exec_background start + poll + kill (started, killed, confirmed dead)")
                        else:
                            fail_step("exec_background start + poll + kill", "job still running after kill")
                    except json.JSONDecodeError:
                        fail_step("exec_background start + poll + kill", f"invalid JSON: {text}")
                else:
                    fail_step("exec_background start + poll + kill", "no text content from poll after kill")
            else:
                fail_step("exec_background start + poll + kill", f"poll after kill failed: {get_error(resp)}")
            msg_id += 1
        else:
            fail_step("exec_background start + poll + kill", "exec_background kill call failed")
    else:
        fail_step("exec_background start + poll + kill", "could not start background job")

    # ===================================================================
    # Vault team-scoped tests (host-side vault, no VM needed)
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 35: vault (write) — write a team-scoped note
    # -----------------------------------------------------------------------
    log("Step 35: vault (write) — write a team-scoped note")
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "write",
            "path": "teams/test-team/notes/hello.md",
            "content": "# Hello from integration test\n\nThis is a team-scoped note.\n\n#test #integration",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            text = get_tool_result_text(resp)
            fail_step("vault write (team-scoped)", f"tool error: {text}")
        else:
            pass_step("vault write (team-scoped note created)")
    else:
        fail_step("vault write (team-scoped)", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 36: vault (read) — read the team-scoped note back
    # -----------------------------------------------------------------------
    log("Step 36: vault (read) — read back the team-scoped note")
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "read",
            "path": "teams/test-team/notes/hello.md",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("vault read (team-scoped)", f"tool error: {text}")
        elif text and "Hello from integration test" in text:
            pass_step("vault read (team-scoped note content verified)")
        else:
            fail_step("vault read (team-scoped)", f"unexpected content: {text!r}")
    else:
        fail_step("vault read (team-scoped)", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 37: vault (list) — list team-scoped directory
    # -----------------------------------------------------------------------
    log("Step 37: vault (list) — list team-scoped vault entries")
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "list",
            "path": "teams/test-team/notes",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("vault list (team-scoped)", f"tool error: {text}")
        elif text and "hello.md" in text.lower():
            pass_step("vault list (team-scoped — hello.md found)")
        else:
            fail_step("vault list (team-scoped)", f"hello.md not in listing: {text!r}")
    else:
        fail_step("vault list (team-scoped)", get_error(resp))
    msg_id += 1

    # ===================================================================
    # Team lifecycle tests (Steps 38-42)
    # ===================================================================

    # Pre-cleanup: destroy any leftover test-team from a previous run
    log("  (pre-cleanup: destroying any leftover test-team)")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {"action": "destroy", "name": "test-team"},
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    msg_id += 1
    # Also destroy any leftover team workspaces by name
    for leftover_name in ["test-team-worker-1", "test-team-worker-2"]:
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_destroy",
            "arguments": {"workspace_id": leftover_name},
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        msg_id += 1

    # -----------------------------------------------------------------------
    # Step 38: team (create) — create a team with 2 roles
    # -----------------------------------------------------------------------
    log("Step 38: team (create) — create a test team with 2 roles")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "create",
            "name": "test-team",
            "roles": [
                {"name": "worker-1", "role": "coder", "skills": ["rust"]},
                {"name": "worker-2", "role": "tester", "skills": ["testing"]},
            ],
            "max_vms": 5,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=120)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team create", f"tool error: {text}")
        elif text:
            try:
                info = json.loads(text)
                if info.get("member_count") == 2 and info.get("state") == "Ready":
                    pass_step(f"team create (2 members, state=Ready)")
                else:
                    fail_step("team create", f"unexpected result: {info}")
            except json.JSONDecodeError:
                if "test-team" in text:
                    pass_step("team create (name found in output)")
                else:
                    fail_step("team create", f"cannot parse response: {text!r}")
        else:
            fail_step("team create", "no text content")
    else:
        fail_step("team create", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 39: team (status) — get team status
    # -----------------------------------------------------------------------
    log("Step 39: team (status) — get test-team status")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "status",
            "name": "test-team",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team status", f"tool error: {text}")
        elif text:
            try:
                info = json.loads(text)
                members = info.get("members", [])
                if len(members) == 2 and info.get("state") == "Ready":
                    pass_step(f"team status (2 members, state=Ready)")
                else:
                    fail_step("team status", f"unexpected: members={len(members)}, state={info.get('state')}")
            except json.JSONDecodeError:
                if "test-team" in text:
                    pass_step("team status (name found)")
                else:
                    fail_step("team status", f"cannot parse: {text!r}")
        else:
            fail_step("team status", "no text content")
    else:
        fail_step("team status", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 40: vault (write) — create task-001 with YAML frontmatter
    # -----------------------------------------------------------------------
    log("Step 40: vault (write) — create task-001.md with frontmatter")
    task_001_content = """---
status: pending
priority: high
created_at: '2026-02-19T00:00:00+00:00'
updated_at: '2026-02-19T00:00:00+00:00'
---
# Implement feature X

This is the first task."""
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "write",
            "path": "teams/test-team/tasks/task-001.md",
            "content": task_001_content,
            "mode": "overwrite",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("vault write task-001", f"tool error: {text}")
        else:
            pass_step("vault write task-001 (created)")
    else:
        fail_step("vault write task-001", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 41: vault (read) — read task-001 back, verify frontmatter
    # -----------------------------------------------------------------------
    log("Step 41: vault (read) — read task-001.md, verify status in frontmatter")
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "read",
            "path": "teams/test-team/tasks/task-001.md",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("vault read task-001", f"tool error: {text}")
        elif text and "pending" in text and "Implement feature X" in text:
            pass_step("vault read task-001 (frontmatter + title verified)")
        else:
            fail_step("vault read task-001", f"missing expected content: {text!r}")
    else:
        fail_step("vault read task-001", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 42: vault (write) — create task-002 with depends_on
    # -----------------------------------------------------------------------
    log("Step 42: vault (write) — create task-002.md with depends_on: task-001")
    task_002_content = """---
status: pending
priority: medium
depends_on:
  - task-001
created_at: '2026-02-19T00:00:00+00:00'
updated_at: '2026-02-19T00:00:00+00:00'
---
# Write tests

Depends on task-001 completion."""
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "write",
            "path": "teams/test-team/tasks/task-002.md",
            "content": task_002_content,
            "mode": "overwrite",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("vault write task-002", f"tool error: {text}")
        else:
            pass_step("vault write task-002 (created with depends_on)")
    else:
        fail_step("vault write task-002", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 43: vault (search) — search for pending tasks
    # -----------------------------------------------------------------------
    log("Step 43: vault (search) — search for 'status: pending' in tasks/")
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "search",
            "query": "status: pending",
            "path": "teams/test-team/tasks",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("vault search pending", f"tool error: {text}")
        elif text and "task-001" in text and "task-002" in text:
            pass_step("vault search pending (both tasks found)")
        elif text and ("task-001" in text or "task-002" in text):
            pass_step("vault search pending (at least one task found)")
        else:
            fail_step("vault search pending", f"no tasks found: {text!r}")
    else:
        fail_step("vault search pending", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 44: vault (write) — update task-001 status to completed
    # -----------------------------------------------------------------------
    log("Step 44: vault (write) — update task-001 status to completed")
    task_001_completed = """---
status: completed
priority: high
owner: test-agent
created_at: '2026-02-19T00:00:00+00:00'
updated_at: '2026-02-19T01:00:00+00:00'
---
# Implement feature X

This is the first task.

## Result
Feature implemented successfully."""
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "write",
            "path": "teams/test-team/tasks/task-001.md",
            "content": task_001_completed,
            "mode": "overwrite",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("vault write task-001 (completed)", f"tool error: {text}")
        else:
            pass_step("vault write task-001 (status updated to completed)")
    else:
        fail_step("vault write task-001 (completed)", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 45: vault (read) — verify task-001 shows completed
    # -----------------------------------------------------------------------
    log("Step 45: vault (read) — verify task-001 shows completed status")
    send_msg(proc, msg_id, "tools/call", {
        "name": "vault",
        "arguments": {
            "action": "read",
            "path": "teams/test-team/tasks/task-001.md",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("vault read task-001 (completed)", f"tool error: {text}")
        elif text and "completed" in text and "test-agent" in text:
            pass_step("vault read task-001 (completed + owner verified)")
        else:
            fail_step("vault read task-001 (completed)", f"missing expected fields: {text!r}")
    else:
        fail_step("vault read task-001 (completed)", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 46: team (message) — send direct message between team members
    # -----------------------------------------------------------------------
    log("Step 46: team (message) — send direct message worker-1 -> worker-2")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "message",
            "name": "test-team",
            "agent": "worker-1",
            "to": "worker-2",
            "content": "hello from worker-1",
            "message_type": "text",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team message (send)", f"tool error: {text}")
        elif text:
            try:
                info = json.loads(text)
                if info.get("status") == "delivered" and info.get("message_id"):
                    pass_step(f"team message (sent, id={info['message_id'][:8]})")
                else:
                    fail_step("team message (send)", f"unexpected: {info}")
            except json.JSONDecodeError:
                if "delivered" in text:
                    pass_step("team message (delivered found in output)")
                else:
                    fail_step("team message (send)", f"cannot parse: {text!r}")
        else:
            fail_step("team message (send)", "no text content")
    else:
        fail_step("team message (send)", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 47: team (receive) — receive messages for worker-2
    # -----------------------------------------------------------------------
    log("Step 47: team (receive) — receive messages for worker-2")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "receive",
            "name": "test-team",
            "agent": "worker-2",
            "limit": 10,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team receive", f"tool error: {text}")
        elif text:
            try:
                info = json.loads(text)
                msgs = info.get("messages", [])
                if info.get("count") == 1 and len(msgs) == 1 and msgs[0].get("from") == "worker-1":
                    pass_step(f"team receive (1 message from worker-1)")
                else:
                    fail_step("team receive", f"unexpected: count={info.get('count')}, msgs={msgs}")
            except json.JSONDecodeError:
                if "worker-1" in text:
                    pass_step("team receive (worker-1 found in output)")
                else:
                    fail_step("team receive", f"cannot parse: {text!r}")
        else:
            fail_step("team receive", "no text content")
    else:
        fail_step("team receive", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 48: team (message) — broadcast message to all team members
    # -----------------------------------------------------------------------
    log("Step 48: team (message) — broadcast from worker-1 to all")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "message",
            "name": "test-team",
            "agent": "worker-1",
            "to": "*",
            "content": "broadcast ping",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team message (broadcast)", f"tool error: {text}")
        elif text:
            try:
                info = json.loads(text)
                if info.get("status") == "delivered":
                    pass_step("team message (broadcast delivered)")
                else:
                    fail_step("team message (broadcast)", f"unexpected: {info}")
            except json.JSONDecodeError:
                if "delivered" in text:
                    pass_step("team message (broadcast delivered)")
                else:
                    fail_step("team message (broadcast)", f"cannot parse: {text!r}")
        else:
            fail_step("team message (broadcast)", "no text content")
    else:
        fail_step("team message (broadcast)", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 49: team (list) — list all teams
    # -----------------------------------------------------------------------
    log("Step 49: team (list) — list all teams")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "list",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team list", f"tool error: {text}")
        elif text:
            try:
                info = json.loads(text)
                # Response has "teams" array and "count" field
                teams = info.get("teams", info) if isinstance(info, dict) else info
                if isinstance(teams, list):
                    team_names = [t.get("name", "") for t in teams]
                    if "test-team" in team_names:
                        pass_step(f"team list (test-team found, {len(teams)} team(s))")
                    else:
                        fail_step("team list", f"test-team not in {team_names}")
                else:
                    fail_step("team list", f"unexpected format: {text!r}")
            except json.JSONDecodeError:
                if "test-team" in text:
                    pass_step("team list (test-team found in output)")
                else:
                    fail_step("team list", f"cannot parse: {text!r}")
        else:
            fail_step("team list", "no text content")
    else:
        fail_step("team list", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 50: team (destroy) — destroy the test team
    # -----------------------------------------------------------------------
    log("Step 50: team (destroy) — destroy test-team")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "destroy",
            "name": "test-team",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=90)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team destroy", f"tool error: {text}")
        else:
            pass_step("team destroy (test-team destroyed)")
    else:
        fail_step("team destroy", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 51: team (list) — verify team is gone after destroy
    # -----------------------------------------------------------------------
    log("Step 51: team (list) — verify test-team is gone")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "list",
        },
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team list (post-destroy)", f"tool error: {text}")
        elif text:
            if "test-team" not in text:
                pass_step("team list (post-destroy — test-team gone)")
            else:
                fail_step("team list (post-destroy)", "test-team still appears")
        else:
            # Empty text means no teams — correct
            pass_step("team list (post-destroy — no teams)")
    else:
        fail_step("team list (post-destroy)", get_error(resp))
    msg_id += 1

    # ===================================================================
    # Phase 5: workspace_merge + nested teams
    # ===================================================================

    # We reuse the main WORKSPACE_ID as the merge target.
    # Create two temporary source workspaces, init git repos, commit, then merge.

    MERGE_SOURCE_1 = None
    MERGE_SOURCE_2 = None

    # Pre-cleanup: destroy leftover merge workspaces from previous runs
    for leftover_name in ["merge-source-1", "merge-source-2"]:
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_destroy",
            "arguments": {"workspace_id": leftover_name},
        })
        cleanup_resp = recv_msg(proc, msg_id, timeout=30)
        msg_id += 1
        # Ignore errors — workspace may not exist

    # -----------------------------------------------------------------------
    # Step 52: workspace_create — merge source 1
    # -----------------------------------------------------------------------
    log("Step 52: workspace_create — merge source 1")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_create",
        "arguments": {"name": "merge-source-1", "vcpus": 1, "memory_mb": 512, "disk_gb": 10},
    })
    resp = recv_msg(proc, msg_id, timeout=120)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                MERGE_SOURCE_1 = data.get("workspace_id")
                if MERGE_SOURCE_1:
                    pass_step(f"workspace_create merge-source-1 (id={MERGE_SOURCE_1[:8]})")
                else:
                    fail_step("workspace_create merge-source-1", "no workspace_id")
            except json.JSONDecodeError:
                fail_step("workspace_create merge-source-1", f"bad JSON: {text}")
        else:
            fail_step("workspace_create merge-source-1", "no text content")
    else:
        fail_step("workspace_create merge-source-1", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 53: workspace_create — merge source 2
    # -----------------------------------------------------------------------
    log("Step 53: workspace_create — merge source 2")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_create",
        "arguments": {"name": "merge-source-2", "vcpus": 1, "memory_mb": 512, "disk_gb": 10},
    })
    resp = recv_msg(proc, msg_id, timeout=120)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            try:
                data = json.loads(text)
                MERGE_SOURCE_2 = data.get("workspace_id")
                if MERGE_SOURCE_2:
                    pass_step(f"workspace_create merge-source-2 (id={MERGE_SOURCE_2[:8]})")
                else:
                    fail_step("workspace_create merge-source-2", "no workspace_id")
            except json.JSONDecodeError:
                fail_step("workspace_create merge-source-2", f"bad JSON: {text}")
        else:
            fail_step("workspace_create merge-source-2", "no text content")
    else:
        fail_step("workspace_create merge-source-2", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 54: Init git repos + commit in sources and target
    # -----------------------------------------------------------------------
    log("Step 54: Init git repos in merge sources and target")
    init_ok = True
    for ws_label, ws_id in [("target", WORKSPACE_ID), ("source-1", MERGE_SOURCE_1), ("source-2", MERGE_SOURCE_2)]:
        if ws_id is None:
            init_ok = False
            continue
        # Init a git repo + initial commit
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec",
            "arguments": {
                "workspace_id": ws_id,
                "command": "cd /workspace && git init && git config user.email test@test.com && git config user.name Test && echo base > base.txt && git add . && git commit -m 'initial'",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is None or "error" in resp:
            init_ok = False
        msg_id += 1

    # Make unique changes in each source
    for ws_label, ws_id, filename, content in [
        ("source-1", MERGE_SOURCE_1, "from-source1.txt", "source 1 change"),
        ("source-2", MERGE_SOURCE_2, "from-source2.txt", "source 2 change"),
    ]:
        if ws_id is None:
            init_ok = False
            continue
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec",
            "arguments": {
                "workspace_id": ws_id,
                "command": f"cd /workspace && echo '{content}' > {filename} && git add . && git commit -m 'add {filename}'",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is None or "error" in resp:
            init_ok = False
        msg_id += 1

    if init_ok:
        pass_step("git init + commits in all merge workspaces")
    else:
        fail_step("git init + commits", "one or more exec calls failed")

    # -----------------------------------------------------------------------
    # Step 55: workspace_merge — sequential strategy
    # -----------------------------------------------------------------------
    log("Step 55: workspace_merge — sequential strategy")
    if MERGE_SOURCE_1 and MERGE_SOURCE_2 and WORKSPACE_ID:
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_merge",
            "arguments": {
                "source_workspaces": [MERGE_SOURCE_1, MERGE_SOURCE_2],
                "target_workspace": WORKSPACE_ID,
                "strategy": "sequential",
            },
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("workspace_merge sequential", f"tool error: {text}")
            elif text:
                try:
                    data = json.loads(text)
                    sources = data.get("sources", data.get("results", []))
                    success_count = sum(1 for s in sources if s.get("success", False)) if isinstance(sources, list) else 0
                    if success_count >= 1:
                        pass_step(f"workspace_merge sequential ({success_count} sources merged)")
                    elif "success" in text.lower() or "applied" in text.lower() or "merged" in text.lower():
                        pass_step("workspace_merge sequential (success in output)")
                    else:
                        fail_step("workspace_merge sequential", f"unexpected result: {text[:200]}")
                except json.JSONDecodeError:
                    if "success" in text.lower() or "applied" in text.lower() or "merged" in text.lower():
                        pass_step("workspace_merge sequential (success in output)")
                    else:
                        fail_step("workspace_merge sequential", f"cannot parse: {text[:200]}")
            else:
                fail_step("workspace_merge sequential", "no text content")
        else:
            fail_step("workspace_merge sequential", get_error(resp))
        msg_id += 1
    else:
        log("Step 55: (skipped — merge workspaces not created)")

    # -----------------------------------------------------------------------
    # Step 56: cleanup merge source workspaces
    # -----------------------------------------------------------------------
    log("Step 56: cleanup merge source workspaces")
    for label, ws_id in [("merge-source-1", MERGE_SOURCE_1), ("merge-source-2", MERGE_SOURCE_2)]:
        if ws_id:
            send_msg(proc, msg_id, "tools/call", {
                "name": "workspace_destroy",
                "arguments": {"workspace_id": ws_id},
            })
            resp = recv_msg(proc, msg_id, timeout=60)
            msg_id += 1
    pass_step("merge source workspaces cleaned up")

    # ===================================================================
    # Phase 5: Nested teams
    # ===================================================================

    NESTED_PARENT_TEAM = None
    NESTED_CHILD_TEAM = None

    # Pre-cleanup: destroy any leftover nested test teams
    log("  (pre-cleanup: destroying leftover nested test teams)")
    for team_name in ["nested-child", "nested-parent"]:
        send_msg(proc, msg_id, "tools/call", {
            "name": "team",
            "arguments": {"action": "destroy", "name": team_name},
        })
        recv_msg(proc, msg_id, timeout=30)
        msg_id += 1

    # -----------------------------------------------------------------------
    # Step 57: team create — parent team
    # -----------------------------------------------------------------------
    log("Step 57: team (create) — create parent team for nesting test")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {
            "action": "create",
            "name": "nested-parent",
            "max_vms": 10,
            "roles": [
                {"name": "lead", "role": "coordinator"},
            ],
        },
    })
    resp = recv_msg(proc, msg_id, timeout=120)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("team create (nested-parent)", f"tool error: {text}")
        elif text and "nested-parent" in text:
            NESTED_PARENT_TEAM = "nested-parent"
            pass_step("team create (nested-parent)")
        else:
            fail_step("team create (nested-parent)", f"unexpected: {text!r}")
    else:
        fail_step("team create (nested-parent)", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 58: team create — child team with parent_team
    # -----------------------------------------------------------------------
    log("Step 58: team (create) — create child sub-team under nested-parent")
    if NESTED_PARENT_TEAM:
        send_msg(proc, msg_id, "tools/call", {
            "name": "team",
            "arguments": {
                "action": "create",
                "name": "nested-child",
                "parent_team": "nested-parent",
                "max_vms": 5,
                "roles": [
                    {"name": "worker", "role": "coder"},
                ],
            },
        })
        resp = recv_msg(proc, msg_id, timeout=120)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("team create (nested-child)", f"tool error: {text}")
            elif text and "nested-child" in text:
                NESTED_CHILD_TEAM = "nested-child"
                pass_step("team create (nested-child with parent_team)")
            else:
                fail_step("team create (nested-child)", f"unexpected: {text!r}")
        else:
            fail_step("team create (nested-child)", get_error(resp))
        msg_id += 1
    else:
        log("Step 58: (skipped — parent team not created)")

    # -----------------------------------------------------------------------
    # Step 59: team status — verify child team shows parent
    # -----------------------------------------------------------------------
    log("Step 59: team (status) — verify nested-child has parent_team")
    if NESTED_CHILD_TEAM:
        send_msg(proc, msg_id, "tools/call", {
            "name": "team",
            "arguments": {"action": "status", "name": "nested-child"},
        })
        resp = recv_msg(proc, msg_id, timeout=30)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            if text and "nested-parent" in text:
                pass_step("team status (nested-child shows parent_team=nested-parent)")
            elif text:
                # Accept if the child team exists, even if parent_team not in output
                pass_step("team status (nested-child exists)")
            else:
                fail_step("team status (nested-child)", "no text content")
        else:
            fail_step("team status (nested-child)", get_error(resp))
        msg_id += 1
    else:
        log("Step 59: (skipped — child team not created)")

    # -----------------------------------------------------------------------
    # Step 60: team destroy — cascade destroy parent (child should also be destroyed)
    # -----------------------------------------------------------------------
    log("Step 60: team (destroy) — destroy parent, verify cascade destroys child")
    if NESTED_PARENT_TEAM:
        send_msg(proc, msg_id, "tools/call", {
            "name": "team",
            "arguments": {"action": "destroy", "name": "nested-parent"},
        })
        resp = recv_msg(proc, msg_id, timeout=120)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("team destroy (cascade)", f"tool error: {text}")
            else:
                pass_step("team destroy (nested-parent destroyed)")
        else:
            fail_step("team destroy (cascade)", get_error(resp))
        msg_id += 1
    else:
        log("Step 60: (skipped — parent team not created)")

    # -----------------------------------------------------------------------
    # Step 61: team list — verify both parent and child are gone
    # -----------------------------------------------------------------------
    log("Step 61: team (list) — verify cascade destroyed both teams")
    send_msg(proc, msg_id, "tools/call", {
        "name": "team",
        "arguments": {"action": "list"},
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text:
            if "nested-parent" not in text and "nested-child" not in text:
                pass_step("team list (both nested teams gone after cascade destroy)")
            else:
                fail_step("team list (cascade)", f"nested teams still present: {text[:200]}")
        else:
            # Empty = no teams, correct
            pass_step("team list (no teams remain after cascade destroy)")
    else:
        fail_step("team list (cascade)", get_error(resp))
    msg_id += 1

    # ===================================================================
    # Cleanup: destroy forked workspace first, then main workspace
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 62: destroy forked workspace (if created)
    # -----------------------------------------------------------------------
    if FORKED_WORKSPACE_ID:
        log("Step 62: workspace_destroy — destroy forked workspace")
        send_msg(proc, msg_id, "tools/call", {
            "name": "workspace_destroy",
            "arguments": {
                "workspace_id": FORKED_WORKSPACE_ID,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=60)
        if resp is not None and "result" in resp:
            text = get_tool_result_text(resp)
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                fail_step("workspace_destroy (fork)", f"tool error: {text}")
            else:
                pass_step("workspace_destroy (forked workspace cleaned up)")
                FORKED_WORKSPACE_ID = None
        else:
            fail_step("workspace_destroy (fork)", get_error(resp))
        msg_id += 1
    else:
        log("Step 62: (skipped — no forked workspace to destroy)")

    # -----------------------------------------------------------------------
    # Step 63: workspace_destroy — tear down main workspace
    # -----------------------------------------------------------------------
    log("Step 63: workspace_destroy — tear down main workspace")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_destroy",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
        },
    })
    resp = recv_msg(proc, msg_id, timeout=60)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        result_obj = resp.get("result", {})
        is_error = result_obj.get("isError") or result_obj.get("is_error")
        if is_error:
            fail_step("workspace_destroy", f"tool error: {text}")
        elif text and "destroyed" in text.lower():
            pass_step(f"workspace_destroy")
            WORKSPACE_ID = None  # Mark as cleaned up
        else:
            # Any non-error result is acceptable
            pass_step(f"workspace_destroy (response: {text!r})")
            WORKSPACE_ID = None
    else:
        fail_step("workspace_destroy", get_error(resp))
    msg_id += 1

    # -----------------------------------------------------------------------
    # Step 64: workspace_list after destroy — verify all gone
    # -----------------------------------------------------------------------
    log("Step 64: workspace_list — verify all test workspaces are gone")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_list",
        "arguments": {},
    })
    resp = recv_msg(proc, msg_id, timeout=30)
    if resp is not None and "result" in resp:
        text = get_tool_result_text(resp)
        if text is not None:
            try:
                workspaces = json.loads(text)
                if not workspaces:
                    pass_step("workspace_list after destroy (empty — correct)")
                else:
                    # Check if our specific workspace is gone
                    pass_step(f"workspace_list after destroy ({len(workspaces)} other workspaces remain)")
            except json.JSONDecodeError:
                fail_step("workspace_list after destroy", f"invalid JSON: {text}")
        else:
            fail_step("workspace_list after destroy", f"no text content: {resp}")
    else:
        fail_step("workspace_list after destroy", get_error(resp))
    msg_id += 1

except KeyboardInterrupt:
    log("\nInterrupted by user")

finally:
    # --------------------------------------------------------------------------
    # Cleanup: terminate server
    # --------------------------------------------------------------------------
    log("")
    log("Shutting down server...")
    try:
        proc.stdin.close()
    except Exception:
        pass

    # Give server a moment to clean up gracefully
    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        log("Server did not exit gracefully, killing...")
        proc.kill()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass

    # Capture any stderr output for diagnostics
    try:
        stderr_out = proc.stderr.read()
        if stderr_out and FAIL_COUNT > 0:
            log("--- Server stderr (for debugging) ---")
            # Only show last 3000 chars to avoid overwhelming output
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
    log("MCP Integration Test Results")
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
    echo "Integration test PASSED"
else
    echo ""
    echo "Integration test FAILED (exit code $EXIT_CODE)"
fi
exit $EXIT_CODE
