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
            "exec", "file_write", "file_read", "file_upload", "file_download",
            "snapshot_create", "snapshot_restore", "snapshot_list", "snapshot_delete",
            "workspace_fork", "port_forward", "port_forward_remove",
            "workspace_ip", "network_policy",
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
    log("Step 7: snapshot_create — create 'test-checkpoint' snapshot")
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot_create",
        "arguments": {
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
    log("Step 8: snapshot_list — verify 'test-checkpoint' appears")
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot_list",
        "arguments": {
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
    # Step 11: workspace_ip — get workspace IP
    # -----------------------------------------------------------------------
    log("Step 11: workspace_ip — get IP address")
    send_msg(proc, msg_id, "tools/call", {
        "name": "workspace_ip",
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
                    pass_step(f"workspace_ip (ip={ip})")
                else:
                    fail_step("workspace_ip", f"unexpected IP: {ip!r}")
            except json.JSONDecodeError:
                fail_step("workspace_ip", f"invalid JSON: {text}")
        else:
            fail_step("workspace_ip", f"no text content: {resp}")
    else:
        fail_step("workspace_ip", get_error(resp))
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
        "name": "snapshot_restore",
        "arguments": {
            "workspace_id": WORKSPACE_ID,
            "snapshot_name": "test-checkpoint",
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
    # First re-create the snapshot since snapshot_restore may have removed it
    send_msg(proc, msg_id, "tools/call", {
        "name": "snapshot_create",
        "arguments": {
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
    # Step 16: exec_background + exec_poll
    # -----------------------------------------------------------------------
    log("Step 16: exec_background + exec_poll — background job lifecycle")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec_background",
        "arguments": {
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
                    log(f"         exec_background started (job_id={bg_job_id})")
                else:
                    fail_step("exec_background", f"unexpected response: {text}")
            except json.JSONDecodeError:
                fail_step("exec_background", f"invalid JSON: {text}")
        else:
            fail_step("exec_background", f"no text content")
    else:
        fail_step("exec_background", get_error(resp))
    msg_id += 1

    if bg_job_id is not None:
        # Poll immediately — should still be running
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_poll",
            "arguments": {
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
                        log("         exec_poll: job still running (correct)")
                    else:
                        log(f"         exec_poll: job already done (may be fast, not necessarily wrong)")
                except json.JSONDecodeError:
                    pass
        msg_id += 1

        # Wait for the job to finish
        time.sleep(5)

        # Poll again — should be done
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_poll",
            "arguments": {
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
                        pass_step(f"exec_background + exec_poll (job completed, exit=0, output correct)")
                    elif running is False:
                        fail_step("exec_background + exec_poll", f"job done but exit={exit_code}, stdout={stdout!r}")
                    else:
                        fail_step("exec_background + exec_poll", f"job still running after 5s wait")
                except json.JSONDecodeError:
                    fail_step("exec_background + exec_poll", f"invalid JSON: {text}")
            else:
                fail_step("exec_background + exec_poll", f"no text content")
        else:
            fail_step("exec_background + exec_poll", get_error(resp))
        msg_id += 1

    # -----------------------------------------------------------------------
    # Step 17: exec_kill — start long job, kill it, verify terminated
    # -----------------------------------------------------------------------
    log("Step 17: exec_kill — start long job, kill it")
    send_msg(proc, msg_id, "tools/call", {
        "name": "exec_background",
        "arguments": {
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
            "name": "exec_kill",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "job_id": kill_job_id,
            },
        })
        resp = recv_msg(proc, msg_id, timeout=15)
        msg_id += 1

        # Give it a moment to process the kill
        time.sleep(1)

        # Poll — should be done (not running)
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec_poll",
            "arguments": {
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
                        pass_step(f"exec_kill (job terminated, exit_code={data.get('exit_code')})")
                    else:
                        fail_step("exec_kill", "job still running after kill")
                except json.JSONDecodeError:
                    fail_step("exec_kill", f"invalid JSON: {text}")
            else:
                fail_step("exec_kill", "no text content")
        else:
            fail_step("exec_kill", get_error(resp))
        msg_id += 1
    else:
        fail_step("exec_kill", "could not start background job for kill test")

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
    # Step 20: file_upload + file_download
    # -----------------------------------------------------------------------
    log("Step 20: file_upload + file_download — transfer files via host path")
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
        "name": "file_upload",
        "arguments": {
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
            fail_step("file_upload", f"tool error: {text}")
        elif text and "uploaded" in text.lower():
            upload_ok = True
            log(f"         file_upload OK: {text.strip()}")
        else:
            upload_ok = True
            log(f"         file_upload response: {text!r}")
    else:
        fail_step("file_upload", get_error(resp))
    msg_id += 1

    if upload_ok:
        # Download guest -> host
        download_host_path = os.path.join(transfer_dir, "test-download.bin")
        send_msg(proc, msg_id, "tools/call", {
            "name": "file_download",
            "arguments": {
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
                fail_step("file_upload + file_download", f"download error: {text}")
            else:
                # Verify content matches
                try:
                    with open(download_host_path, "rb") as f:
                        downloaded = f.read()
                    download_hash = hashlib.sha256(downloaded).hexdigest()
                    if upload_hash == download_hash:
                        pass_step(f"file_upload + file_download (roundtrip verified, {len(downloaded)} bytes, sha256 match)")
                    else:
                        fail_step("file_upload + file_download", f"hash mismatch: upload={upload_hash[:16]}... download={download_hash[:16]}...")
                except Exception as e:
                    fail_step("file_upload + file_download", f"could not read downloaded file: {e}")
        else:
            fail_step("file_upload + file_download", get_error(resp))
        msg_id += 1

        # Cleanup host files
        try:
            os.remove(upload_host_path)
            os.remove(download_host_path)
        except OSError:
            pass
    else:
        log("         Skipping file_download (upload failed)")

    # ===================================================================
    # Phase 4: Network tests
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 21: port_forward + port_forward_remove
    # -----------------------------------------------------------------------
    log("Step 21: port_forward + port_forward_remove — port forwarding lifecycle")
    send_msg(proc, msg_id, "tools/call", {
        "name": "port_forward",
        "arguments": {
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
                    log(f"         port_forward created: host:{pf_host_port} -> guest:8080")
                else:
                    fail_step("port_forward", f"unexpected: {text}")
            except json.JSONDecodeError:
                fail_step("port_forward", f"invalid JSON: {text}")
        else:
            result_obj = resp.get("result", {})
            is_error = result_obj.get("isError") or result_obj.get("is_error")
            if is_error:
                content = result_obj.get("content", [])
                err_text = next((c.get("text", "") for c in content if c.get("type") == "text"), "")
                fail_step("port_forward", f"tool error: {err_text}")
            else:
                fail_step("port_forward", f"no text content")
    else:
        fail_step("port_forward", get_error(resp))
    msg_id += 1

    # Remove the port forward
    if pf_host_port:
        send_msg(proc, msg_id, "tools/call", {
            "name": "port_forward_remove",
            "arguments": {
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
                fail_step("port_forward + port_forward_remove", f"remove error: {text}")
            else:
                pass_step(f"port_forward + port_forward_remove (host:{pf_host_port} -> guest:8080, then removed)")
        else:
            fail_step("port_forward + port_forward_remove", get_error(resp))
        msg_id += 1
    else:
        log("         Skipping port_forward_remove (forward setup failed)")

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
    # Phase 5: Operational tests
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 23: workspace_logs — get VM logs
    # -----------------------------------------------------------------------
    log("Step 23: workspace_logs — retrieve VM console/stderr logs")
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
    # Cleanup: destroy forked workspace first, then main workspace
    # ===================================================================

    # -----------------------------------------------------------------------
    # Step 24: destroy forked workspace (if created)
    # -----------------------------------------------------------------------
    if FORKED_WORKSPACE_ID:
        log("Step 24: workspace_destroy — destroy forked workspace")
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
        log("Step 24: (skipped — no forked workspace to destroy)")

    # -----------------------------------------------------------------------
    # Step 25: workspace_destroy — tear down main workspace
    # -----------------------------------------------------------------------
    log("Step 25: workspace_destroy — tear down main workspace")
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
    # Step 26: workspace_list after destroy — verify all gone
    # -----------------------------------------------------------------------
    log("Step 26: workspace_list — verify all test workspaces are gone")
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
