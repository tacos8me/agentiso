#!/bin/bash
# State Persistence Integration Test for agentiso
# Tests that workspace state survives server restarts by driving the MCP
# JSON-RPC protocol over stdin/stdout, stopping and restarting the server
# between operations.
#
# Usage: sudo ./scripts/test-state-persistence.sh
#
# Requires: root (for QEMU/KVM/TAP/ZFS), a built binary, and a valid environment.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_DIR/target/release/agentiso"
BASE_CONFIG="$PROJECT_DIR/config.toml"

echo "=== agentiso State Persistence Integration Test ==="
echo "Project: $PROJECT_DIR"
echo "Binary:  $BINARY"
echo "Config:  $BASE_CONFIG"
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
check_prereq "config.toml exists" "test -f '$BASE_CONFIG'"
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

python3 - "$BINARY" "$BASE_CONFIG" <<'PYEOF'
#!/usr/bin/env python3
"""
State persistence integration test driver for agentiso.
Starts/stops the MCP server multiple times, verifying workspace state
is preserved across restarts.
"""

import subprocess
import json
import sys
import time
import select
import os
import signal
import tempfile
import shutil

BINARY = sys.argv[1]
BASE_CONFIG = sys.argv[2]

# Tracking
PASS_COUNT = 0
FAIL_COUNT = 0

# Workspace IDs we create (for cleanup)
CREATED_WORKSPACE_IDS = []

def log(msg):
    print(msg, flush=True)

def pass_test(name):
    global PASS_COUNT
    PASS_COUNT += 1
    log(f"  [PASS] {name}")

def fail_test(name, detail=None):
    global FAIL_COUNT
    FAIL_COUNT += 1
    log(f"  [FAIL] {name}")
    if detail:
        log(f"         Detail: {detail}")

# --------------------------------------------------------------------------
# Temp directory for test state file and config
# --------------------------------------------------------------------------
TEMP_DIR = tempfile.mkdtemp(prefix="agentiso-persist-test-")
STATE_FILE = os.path.join(TEMP_DIR, "state.json")
TEST_CONFIG = os.path.join(TEMP_DIR, "config.toml")

def create_test_config():
    """Create a config.toml pointing state_file to our temp directory."""
    with open(BASE_CONFIG, "r") as f:
        config_text = f.read()

    # Replace the state_file line
    lines = config_text.splitlines()
    new_lines = []
    for line in lines:
        if line.strip().startswith("state_file"):
            new_lines.append(f'state_file = "{STATE_FILE}"')
        else:
            new_lines.append(line)

    with open(TEST_CONFIG, "w") as f:
        f.write("\n".join(new_lines) + "\n")

create_test_config()
log(f"Temp dir:    {TEMP_DIR}")
log(f"State file:  {STATE_FILE}")
log(f"Test config: {TEST_CONFIG}")
log("")

# --------------------------------------------------------------------------
# Server lifecycle helpers
# --------------------------------------------------------------------------

class McpServer:
    """Manages an agentiso MCP server process."""

    def __init__(self):
        self.proc = None
        self.msg_id = 0

    def start(self, timeout=15):
        """Start the server and perform MCP handshake. Returns True on success."""
        env = os.environ.copy()
        env["RUST_LOG"] = "warn"
        self.proc = subprocess.Popen(
            [BINARY, "serve", "--config", TEST_CONFIG],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=env,
        )

        # Give server a moment to initialize
        time.sleep(2.0)

        if self.proc.poll() is not None:
            stderr_out = self.proc.stderr.read()
            log(f"  ERROR: Server exited immediately with code {self.proc.returncode}")
            log(f"  Stderr: {stderr_out[:2000]}")
            return False

        # MCP initialize handshake
        self.msg_id = 1
        self._send("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "state-persistence-test", "version": "0.1.0"},
        })
        resp = self._recv(timeout=timeout)
        if resp is None or "result" not in resp:
            log(f"  ERROR: MCP initialize failed: {_get_error(resp)}")
            self.stop()
            return False

        # Send initialized notification
        self._send_notification("notifications/initialized")
        time.sleep(0.2)
        return True

    def stop(self, timeout=15):
        """Gracefully stop the server."""
        if self.proc is None:
            return
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        self.proc = None

    def call_tool(self, name, arguments, timeout=120):
        """Call an MCP tool and return the parsed response."""
        self._send("tools/call", {"name": name, "arguments": arguments})
        return self._recv(timeout=timeout)

    def _send(self, method, params=None):
        self.msg_id += 1
        msg = {"jsonrpc": "2.0", "id": self.msg_id, "method": method}
        if params is not None:
            msg["params"] = params
        line = json.dumps(msg) + "\n"
        self.proc.stdin.write(line)
        self.proc.stdin.flush()

    def _send_notification(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        line = json.dumps(msg) + "\n"
        self.proc.stdin.write(line)
        self.proc.stdin.flush()

    def _recv(self, timeout=90):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc.poll() is not None:
                break
            remaining = deadline - time.time()
            if remaining <= 0:
                break
            ready = select.select([self.proc.stdout], [], [], min(remaining, 2.0))[0]
            if ready:
                line = self.proc.stdout.readline()
                if not line:
                    break
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                    if msg.get("id") == self.msg_id:
                        return msg
                except json.JSONDecodeError:
                    pass
        return None


def _get_error(resp):
    if resp is None:
        return "timeout (no response)"
    if "error" in resp:
        err = resp["error"]
        return f"code={err.get('code')} message={err.get('message')}"
    return None


def get_tool_text(resp):
    """Extract text content from a tools/call response."""
    if resp is None:
        return None
    result = resp.get("result")
    if result is None:
        return None
    for item in result.get("content", []):
        if item.get("type") == "text":
            return item.get("text", "")
    return None


def is_tool_error(resp):
    """Check if the response is a tool-level error."""
    if resp is None:
        return True
    result = resp.get("result", {})
    return result.get("isError") or result.get("is_error") or "error" in resp


def create_workspace(server, name="persist-test", vcpus=1, memory_mb=512, disk_gb=10):
    """Create a workspace and return (workspace_id, ip) or (None, None) on failure."""
    resp = server.call_tool("workspace_create", {
        "name": name,
        "vcpus": vcpus,
        "memory_mb": memory_mb,
        "disk_gb": disk_gb,
    }, timeout=120)
    text = get_tool_text(resp)
    if text:
        try:
            data = json.loads(text)
            ws_id = data.get("workspace_id")
            ws_ip = data.get("ip")
            if ws_id:
                CREATED_WORKSPACE_IDS.append(ws_id)
                return ws_id, ws_ip
        except json.JSONDecodeError:
            pass
    return None, None


def destroy_workspace(server, ws_id):
    """Destroy a workspace. Returns True on success."""
    resp = server.call_tool("workspace_destroy", {"workspace_id": ws_id}, timeout=60)
    if not is_tool_error(resp):
        if ws_id in CREATED_WORKSPACE_IDS:
            CREATED_WORKSPACE_IDS.remove(ws_id)
        return True
    return False


def list_workspaces(server):
    """List workspaces. Returns list of dicts or empty list on failure."""
    resp = server.call_tool("workspace_list", {}, timeout=30)
    text = get_tool_text(resp)
    if text:
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            pass
    return []


def get_workspace_info(server, ws_id):
    """Get workspace info. Returns dict or None on failure."""
    resp = server.call_tool("workspace_info", {"workspace_id": ws_id}, timeout=30)
    text = get_tool_text(resp)
    if text:
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            pass
    return None


def adopt_all_workspaces(server):
    """Adopt all orphaned workspaces into the current session.

    After a server restart, workspaces are not owned by the new MCP session.
    Tools like workspace_info and workspace_destroy require ownership, so
    this must be called before using them on restored workspaces.
    """
    resp = server.call_tool("workspace_adopt_all", {}, timeout=30)
    return not is_tool_error(resp)


# --------------------------------------------------------------------------
# Cleanup
# --------------------------------------------------------------------------

def cleanup():
    """Destroy all created workspaces and clean up temp files."""
    log("")
    log("--- Cleanup ---")

    # Try to destroy all tracked workspaces
    if CREATED_WORKSPACE_IDS:
        log(f"  Destroying {len(CREATED_WORKSPACE_IDS)} leftover workspace(s)...")
        server = McpServer()
        if server.start(timeout=15):
            adopt_all_workspaces(server)
            for ws_id in list(CREATED_WORKSPACE_IDS):
                log(f"  Destroying {ws_id[:8]}...")
                destroy_workspace(server, ws_id)
            server.stop()
        else:
            log("  WARNING: Could not start server for cleanup")

    # Remove temp dir
    if os.path.exists(TEMP_DIR):
        shutil.rmtree(TEMP_DIR, ignore_errors=True)
        log(f"  Removed temp dir: {TEMP_DIR}")

    log("--- End cleanup ---")


# ==========================================================================
# Test execution
# ==========================================================================

try:
    # ======================================================================
    # T1: State file created on save
    # ======================================================================
    log("T1: State file created on save")

    # Ensure state file does not exist yet
    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T1: State file created on save", "server failed to start")
    else:
        ws_id_t1, _ = create_workspace(server, name="t1-state-file")
        if ws_id_t1 is None:
            fail_test("T1: State file created on save", "workspace_create failed")
        else:
            # Stop server (triggers save_state on shutdown)
            server.stop()
            time.sleep(1)

            if os.path.exists(STATE_FILE):
                try:
                    with open(STATE_FILE, "r") as f:
                        data = json.loads(f.read())
                    if isinstance(data, dict) and "workspaces" in data:
                        pass_test("T1: State file created on save")
                    else:
                        fail_test("T1: State file created on save", f"state file is valid JSON but missing 'workspaces' key")
                except json.JSONDecodeError as e:
                    fail_test("T1: State file created on save", f"state file is not valid JSON: {e}")
            else:
                fail_test("T1: State file created on save", f"state file not found at {STATE_FILE}")

            # Clean up workspace for next tests
            server2 = McpServer()
            if server2.start():
                destroy_workspace(server2, ws_id_t1)
                server2.stop()
                time.sleep(1)

    # ======================================================================
    # T2: Workspace survives restart
    # ======================================================================
    log("T2: Workspace survives restart")

    # Clean state
    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T2: Workspace survives restart", "server failed to start")
    else:
        ws_id_t2, _ = create_workspace(server, name="t2-survive-restart")
        if ws_id_t2 is None:
            fail_test("T2: Workspace survives restart", "workspace_create failed")
            server.stop()
        else:
            server.stop()
            time.sleep(1)

            # Restart server
            server = McpServer()
            if not server.start():
                fail_test("T2: Workspace survives restart", "server failed to restart")
            else:
                adopt_all_workspaces(server)
                ws_list = list_workspaces(server)
                ws_ids = [w.get("workspace_id") for w in ws_list]
                if ws_id_t2 in ws_ids:
                    pass_test("T2: Workspace survives restart")
                else:
                    fail_test("T2: Workspace survives restart",
                              f"workspace {ws_id_t2[:8]} not found in list after restart: {ws_ids}")

                # Clean up
                destroy_workspace(server, ws_id_t2)
                server.stop()
                time.sleep(1)

    # ======================================================================
    # T3: Snapshot tree preserved
    # ======================================================================
    log("T3: Snapshot tree preserved")

    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T3: Snapshot tree preserved", "server failed to start")
    else:
        ws_id_t3, _ = create_workspace(server, name="t3-snapshot-tree")
        if ws_id_t3 is None:
            fail_test("T3: Snapshot tree preserved", "workspace_create failed")
            server.stop()
        else:
            # Create a snapshot
            resp = server.call_tool("snapshot", {
                "action": "create",
                "workspace_id": ws_id_t3,
                "name": "snap1",
                "include_memory": False,
            }, timeout=60)
            snap_text = get_tool_text(resp)
            snap_ok = False
            if snap_text:
                try:
                    snap_data = json.loads(snap_text)
                    if snap_data.get("snapshot_id") and snap_data.get("name") == "snap1":
                        snap_ok = True
                except json.JSONDecodeError:
                    pass

            if not snap_ok:
                fail_test("T3: Snapshot tree preserved", "snapshot_create failed")
                destroy_workspace(server, ws_id_t3)
                server.stop()
            else:
                server.stop()
                time.sleep(1)

                # Restart and check
                server = McpServer()
                if not server.start():
                    fail_test("T3: Snapshot tree preserved", "server failed to restart")
                else:
                    adopt_all_workspaces(server)
                    info = get_workspace_info(server, ws_id_t3)
                    if info is None:
                        fail_test("T3: Snapshot tree preserved", "workspace_info returned None after restart")
                    else:
                        snapshots = info.get("snapshots", [])
                        snap_names = [s.get("name") for s in snapshots]
                        if "snap1" in snap_names:
                            pass_test("T3: Snapshot tree preserved")
                        else:
                            fail_test("T3: Snapshot tree preserved",
                                      f"snap1 not found in snapshots after restart: {snap_names}")

                    destroy_workspace(server, ws_id_t3)
                    server.stop()
                    time.sleep(1)

    # ======================================================================
    # T4: Multiple workspaces preserved
    # ======================================================================
    log("T4: Multiple workspaces preserved")

    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T4: Multiple workspaces preserved", "server failed to start")
    else:
        ws_ids_t4 = []
        all_created = True
        for i in range(3):
            ws_id, _ = create_workspace(server, name=f"t4-multi-{i}")
            if ws_id:
                ws_ids_t4.append(ws_id)
            else:
                all_created = False
                fail_test("T4: Multiple workspaces preserved", f"workspace_create failed for workspace {i}")
                break

        if all_created and len(ws_ids_t4) == 3:
            server.stop()
            time.sleep(1)

            # Restart and verify all 3 present
            server = McpServer()
            if not server.start():
                fail_test("T4: Multiple workspaces preserved", "server failed to restart")
            else:
                adopt_all_workspaces(server)
                ws_list = list_workspaces(server)
                listed_ids = {w.get("workspace_id") for w in ws_list}
                missing = [wid for wid in ws_ids_t4 if wid not in listed_ids]
                if not missing:
                    pass_test(f"T4: Multiple workspaces preserved (all 3 found)")
                else:
                    fail_test("T4: Multiple workspaces preserved",
                              f"missing {len(missing)} workspace(s) after restart")

                # Clean up all 3
                for ws_id in ws_ids_t4:
                    destroy_workspace(server, ws_id)
                server.stop()
                time.sleep(1)
        else:
            # Clean up whatever we created
            for ws_id in ws_ids_t4:
                destroy_workspace(server, ws_id)
            server.stop()
            time.sleep(1)

    # ======================================================================
    # T5: Destroyed workspace removed from state
    # ======================================================================
    log("T5: Destroyed workspace removed from state")

    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T5: Destroyed workspace removed from state", "server failed to start")
    else:
        ws_id_t5, _ = create_workspace(server, name="t5-destroy-check")
        if ws_id_t5 is None:
            fail_test("T5: Destroyed workspace removed from state", "workspace_create failed")
            server.stop()
        else:
            # Destroy the workspace while the server is still running
            if not destroy_workspace(server, ws_id_t5):
                fail_test("T5: Destroyed workspace removed from state", "workspace_destroy failed")
                server.stop()
            else:
                server.stop()
                time.sleep(1)

                # Restart and verify it is NOT in the list
                server = McpServer()
                if not server.start():
                    fail_test("T5: Destroyed workspace removed from state", "server failed to restart")
                else:
                    ws_list = list_workspaces(server)
                    listed_ids = [w.get("workspace_id") for w in ws_list]
                    if ws_id_t5 not in listed_ids:
                        pass_test("T5: Destroyed workspace removed from state")
                    else:
                        fail_test("T5: Destroyed workspace removed from state",
                                  f"destroyed workspace {ws_id_t5[:8]} still in list")

                    server.stop()
                    time.sleep(1)

    # ======================================================================
    # T6: State file atomic write (no corruption)
    # ======================================================================
    log("T6: State file atomic write (no corruption)")

    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T6: State file atomic write (no corruption)", "server failed to start")
    else:
        ws_id_t6, _ = create_workspace(server, name="t6-atomic-write")
        if ws_id_t6 is None:
            fail_test("T6: State file atomic write (no corruption)", "workspace_create failed")
            server.stop()
        else:
            server.stop()
            time.sleep(1)

            # Check that no .tmp file is left behind (atomic rename should clean it up)
            tmp_path = STATE_FILE + ".tmp"
            if os.path.exists(tmp_path):
                fail_test("T6: State file atomic write (no corruption)",
                          f"temp file {tmp_path} still exists (atomic rename may have failed)")
            elif os.path.exists(STATE_FILE):
                # Verify the state file is valid JSON (not truncated/corrupt)
                try:
                    with open(STATE_FILE, "r") as f:
                        data = json.loads(f.read())
                    if isinstance(data, dict) and "workspaces" in data:
                        pass_test("T6: State file atomic write (no corruption)")
                    else:
                        fail_test("T6: State file atomic write (no corruption)",
                                  "state file valid JSON but unexpected structure")
                except json.JSONDecodeError as e:
                    fail_test("T6: State file atomic write (no corruption)",
                              f"state file corrupted: {e}")
            else:
                fail_test("T6: State file atomic write (no corruption)",
                          "state file does not exist after server stop")

            # Clean up workspace
            server2 = McpServer()
            if server2.start():
                destroy_workspace(server2, ws_id_t6)
                server2.stop()
                time.sleep(1)

    # ======================================================================
    # T7: Schema version check
    # ======================================================================
    log("T7: Schema version check")

    if os.path.exists(STATE_FILE):
        # We should have a state file from previous tests. If not, create one.
        pass
    else:
        # Create a workspace to generate a state file
        server = McpServer()
        if server.start():
            ws_id_tmp, _ = create_workspace(server, name="t7-schema-check")
            server.stop()
            time.sleep(1)
            if ws_id_tmp:
                # Clean up later
                pass

    if os.path.exists(STATE_FILE):
        try:
            with open(STATE_FILE, "r") as f:
                data = json.loads(f.read())
            if "schema_version" in data:
                sv = data["schema_version"]
                if isinstance(sv, int) and sv >= 1:
                    pass_test(f"T7: Schema version check (schema_version={sv})")
                else:
                    fail_test("T7: Schema version check",
                              f"schema_version is not a valid integer: {sv}")
            else:
                fail_test("T7: Schema version check",
                          "state file does not contain 'schema_version' field")
        except json.JSONDecodeError as e:
            fail_test("T7: Schema version check", f"state file not valid JSON: {e}")
    else:
        fail_test("T7: Schema version check", "no state file available to check")

    # Clean up any T7 workspace if we created one
    if os.path.exists(STATE_FILE):
        try:
            with open(STATE_FILE, "r") as f:
                data = json.loads(f.read())
            ws_map = data.get("workspaces", {})
            if ws_map:
                server = McpServer()
                if server.start():
                    adopt_all_workspaces(server)
                    for wid in list(ws_map.keys()):
                        # UUIDs with hyphens
                        destroy_workspace(server, wid)
                    server.stop()
                    time.sleep(1)
        except Exception:
            pass

    # ======================================================================
    # T8: Empty state on first boot
    # ======================================================================
    log("T8: Empty state on first boot")

    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T8: Empty state on first boot", "server failed to start")
    else:
        ws_list = list_workspaces(server)
        if len(ws_list) == 0:
            pass_test("T8: Empty state on first boot (0 workspaces)")
        else:
            fail_test("T8: Empty state on first boot",
                      f"expected 0 workspaces, got {len(ws_list)}")
        server.stop()
        time.sleep(1)

    # ======================================================================
    # T9: Workspace name preserved
    # ======================================================================
    log("T9: Workspace name preserved")

    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T9: Workspace name preserved", "server failed to start")
    else:
        ws_id_t9, _ = create_workspace(server, name="my-test-ws")
        if ws_id_t9 is None:
            fail_test("T9: Workspace name preserved", "workspace_create failed")
            server.stop()
        else:
            server.stop()
            time.sleep(1)

            server = McpServer()
            if not server.start():
                fail_test("T9: Workspace name preserved", "server failed to restart")
            else:
                adopt_all_workspaces(server)
                info = get_workspace_info(server, ws_id_t9)
                if info is None:
                    fail_test("T9: Workspace name preserved", "workspace_info returned None after restart")
                else:
                    name = info.get("name")
                    if name == "my-test-ws":
                        pass_test("T9: Workspace name preserved")
                    else:
                        fail_test("T9: Workspace name preserved",
                                  f"expected name='my-test-ws', got name={name!r}")

                destroy_workspace(server, ws_id_t9)
                server.stop()
                time.sleep(1)

    # ======================================================================
    # T10: IP allocation preserved
    # ======================================================================
    log("T10: IP allocation preserved")

    if os.path.exists(STATE_FILE):
        os.remove(STATE_FILE)

    server = McpServer()
    if not server.start():
        fail_test("T10: IP allocation preserved", "server failed to start")
    else:
        ws_id_t10, ip_before = create_workspace(server, name="t10-ip-preserve")
        if ws_id_t10 is None or ip_before is None:
            fail_test("T10: IP allocation preserved", "workspace_create failed or returned no IP")
            server.stop()
        else:
            server.stop()
            time.sleep(1)

            server = McpServer()
            if not server.start():
                fail_test("T10: IP allocation preserved", "server failed to restart")
            else:
                adopt_all_workspaces(server)
                info = get_workspace_info(server, ws_id_t10)
                if info is None:
                    fail_test("T10: IP allocation preserved", "workspace_info returned None after restart")
                else:
                    ip_after = info.get("ip")
                    if ip_after == ip_before:
                        pass_test(f"T10: IP allocation preserved (ip={ip_before})")
                    else:
                        fail_test("T10: IP allocation preserved",
                                  f"IP changed: before={ip_before}, after={ip_after}")

                destroy_workspace(server, ws_id_t10)
                server.stop()
                time.sleep(1)

except KeyboardInterrupt:
    log("\nInterrupted by user")

except Exception as e:
    log(f"\nUnexpected error: {e}")
    import traceback
    traceback.print_exc()

finally:
    cleanup()

    # --------------------------------------------------------------------------
    # Results summary
    # --------------------------------------------------------------------------
    log("")
    log("=" * 60)
    log("State Persistence Test Results")
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
    echo "State persistence test PASSED"
else
    echo ""
    echo "State persistence test FAILED (exit code $EXIT_CODE)"
fi
exit $EXIT_CODE
