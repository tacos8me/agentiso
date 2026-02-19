#!/usr/bin/env python3
"""Direct vsock diagnostic: talks to guest agent, runs network commands."""
import socket
import struct
import json
import sys
import time

VSOCK_PORT = 5000

def send_recv(sock, request):
    """Send a request and receive response on an existing connection."""
    payload = json.dumps(request).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)

    # Read response: 4-byte BE length + JSON
    length_bytes = b""
    while len(length_bytes) < 4:
        chunk = sock.recv(4 - len(length_bytes))
        if not chunk:
            raise Exception("connection closed before length")
        length_bytes += chunk

    length = struct.unpack(">I", length_bytes)[0]

    data = b""
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise Exception("connection closed before payload")
        data += chunk

    return json.loads(data)

def exec_on_new_conn(cid, cmd_str):
    """Execute a shell command via guest agent on a fresh connection."""
    parts = cmd_str.split(None, 1)  # split into command + rest
    if len(parts) == 1:
        command, args_str = parts[0], ""
    else:
        command, args_str = parts

    # For commands like "ping -c 1 -W 2 10.99.0.1", split args properly
    import shlex
    full = shlex.split(cmd_str)
    request = {
        "type": "Exec",
        "command": full[0],
        "args": full[1:],
        "env": {},
        "timeout_secs": 15
    }

    sock = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
    sock.settimeout(15)
    sock.connect((cid, VSOCK_PORT))
    try:
        resp = send_recv(sock, request)
    finally:
        sock.close()
    return resp

def main():
    cid = int(sys.argv[1]) if len(sys.argv) > 1 else 102

    print(f"=== Guest Diagnostics (vsock CID {cid}) ===\n")

    commands = [
        "ip addr show",
        "ip route show",
        "cat /etc/resolv.conf",
        "ping -c 1 -W 2 10.99.0.1",
        "ping -c 1 -W 2 1.1.1.1",
        "ip link show lo",
        "arp -a",
    ]

    for cmd in commands:
        print(f"--- {cmd} ---")
        try:
            resp = exec_on_new_conn(cid, cmd)
            if resp.get("type") == "ExecResult":
                print(f"exit_code: {resp['exit_code']}")
                if resp.get("stdout"):
                    print(resp["stdout"].rstrip())
                if resp.get("stderr"):
                    print(f"STDERR: {resp['stderr'].rstrip()}")
            elif resp.get("type") == "Error":
                print(f"ERROR: {resp.get('message', resp)}")
            else:
                print(json.dumps(resp, indent=2))
        except Exception as e:
            print(f"FAILED: {e}")
        print()
        time.sleep(0.5)  # Give guest agent time to accept next connection

if __name__ == "__main__":
    main()
