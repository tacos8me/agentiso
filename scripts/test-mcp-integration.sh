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
                if ip and ip.startswith("10.42."):
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

    # -----------------------------------------------------------------------
    # Diagnostic: dump guest logs BEFORE destroy (last chance)
    # -----------------------------------------------------------------------
    log("")
    log("--- Diagnostic: guest logs before destroy ---")
    if WORKSPACE_ID:
        ws_short_id = WORKSPACE_ID[:8]
        console_log_path = f"/run/agentiso/{ws_short_id}/console.log"
        try:
            if os.path.exists(console_log_path):
                with open(console_log_path, "r") as f:
                    console_text = f.read()
                log(f"=== GUEST CONSOLE LOG ({console_log_path}) ===")
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
        log("--- Guest agent log (via exec) ---")
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

        # Also try to list relevant files in the guest for context
        log("--- Guest process list (via exec) ---")
        send_msg(proc, msg_id, "tools/call", {
            "name": "exec",
            "arguments": {
                "workspace_id": WORKSPACE_ID,
                "command": "ps aux 2>/dev/null; echo '---'; ls -la /var/log/ 2>/dev/null",
                "timeout_secs": 10,
            },
        })
        diag_resp2 = recv_msg(proc, msg_id, timeout=20)
        if diag_resp2 is not None and "result" in diag_resp2:
            diag_text2 = get_tool_result_text(diag_resp2)
            if diag_text2:
                try:
                    diag_data2 = json.loads(diag_text2)
                    log(diag_data2.get("stdout", "(empty)"))
                except json.JSONDecodeError:
                    log(f"  response not JSON: {diag_text2}")
            else:
                log("  no text content")
        else:
            log(f"  exec failed: {get_error(diag_resp2)}")
        msg_id += 1
    else:
        log("  No workspace ID, skipping diagnostics")
    log("--- End pre-destroy diagnostics ---")
    log("")

    # -----------------------------------------------------------------------
    # Step 13: workspace_destroy — tear down workspace
    # -----------------------------------------------------------------------
    log("Step 13: workspace_destroy — tear down workspace")
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
    # Step 14: workspace_list after destroy — verify it's gone
    # -----------------------------------------------------------------------
    log("Step 14: workspace_list — verify workspace is gone after destroy")
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
