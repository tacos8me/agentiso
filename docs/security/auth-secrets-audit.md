# Security Audit: Authentication, Token Management, Sessions, and Secret Handling

**Auditor**: sec-auth-secrets
**Date**: 2026-02-24
**Scope**: Authentication, token management, sessions, secret handling across the agentiso codebase

---

## Executive Summary

The authentication and session system is **well-designed for a single-user, localhost MCP server**. Session isolation, workspace ownership, quota enforcement, and force-adopt staleness checks are all implemented correctly. However, there are several findings — two High-severity issues (dashboard token comparison timing, CORS policy) and several Medium/Low issues — that should be addressed before any multi-user or network-exposed deployment.

**Finding Count**: 3 High, 5 Medium, 4 Low, 2 Info

---

## Findings

### AUTH-01: Dashboard Admin Token Uses Non-Constant-Time Comparison

**Severity**: High
**File**: `agentiso/src/dashboard/web/auth.rs:34`
**Category**: Timing side-channel

**Description**: The dashboard auth middleware compares the bearer token to the configured `admin_token` using standard string equality (`==`), which is vulnerable to timing side-channel attacks. An attacker can repeatedly send tokens and measure response time to character-by-character reconstruct the admin token.

**Code**:
```rust
// auth.rs:34
if bearer_token == token {
    return next.run(request).await;
}
```

**Exploit scenario**: An attacker with network access to the dashboard port sends thousands of requests varying one character at a time. By measuring response latencies, they can statistically determine each byte of the admin token. This is practical when the dashboard is bound to `0.0.0.0` (as in `config.toml`).

**Recommendation**: Use a constant-time comparison. The `subtle` crate provides `ConstantTimeEq`, or hash both sides and compare the hashes.

**Fix**:
```rust
// Option 1: Use subtle crate
use subtle::ConstantTimeEq;

if bearer_token.as_bytes().ct_eq(token.as_bytes()).into() {
    return next.run(request).await;
}

// Option 2: Hash-then-compare (no extra dependency)
use std::hash::{Hash, Hasher, DefaultHasher};
fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}
// Compare length first (constant-time for fixed-length hashes)
if bearer_token.len() == token.len() && hash_str(bearer_token) == hash_str(token) {
    return next.run(request).await;
}
```

---

### AUTH-02: Dashboard CORS Policy is Fully Permissive

**Severity**: High
**File**: `agentiso/src/dashboard/web/mod.rs:158`
**Category**: Cross-origin attack surface

**Description**: The dashboard uses `CorsLayer::permissive()`, which sets `Access-Control-Allow-Origin: *` and allows all methods, headers, and credentials. This allows any website visited by an administrator to make authenticated API requests to the dashboard on their behalf (if the browser sends cookies or the `Authorization` header).

**Code**:
```rust
// mod.rs:158
.layer(CorsLayer::permissive());
```

**Exploit scenario**: An attacker hosts a malicious webpage. When an admin visits it while logged into the agentiso dashboard, JavaScript on the attacker's page can call `fetch("http://dashboard:7070/api/workspaces/destroy/...", {method: "DELETE", headers: {"Authorization": "Bearer ..."}})`. Since CORS is permissive, the browser allows the request. If the admin token is stored in a cookie or injected by a browser extension, the attacker can destroy workspaces, read files, or execute commands.

**Recommendation**: Restrict the CORS policy. For a typical deployment:
```rust
use tower_http::cors::{CorsLayer, AllowOrigin};
CorsLayer::new()
    .allow_origin(AllowOrigin::exact("http://localhost:7070".parse().unwrap()))
    .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
    .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
```

If the dashboard needs to be accessible from other origins, use a configurable allowlist rather than a wildcard.

---

### AUTH-03: Dashboard Runs Without Authentication by Default When Bound to 0.0.0.0

**Severity**: High
**File**: `config.toml:62` and `agentiso/src/dashboard/web/auth.rs:26`

**Description**: The production `config.toml` binds the dashboard to `0.0.0.0:7070` with `admin_token = ""`. The auth middleware skips authentication when the token is empty. This means the dashboard is accessible **without any authentication** to anyone who can reach the host on port 7070, exposing full workspace management capabilities (create, destroy, exec, file read/write).

**Code**:
```rust
// auth.rs:26
if token.is_empty() {
    return next.run(request).await;
}
```

```toml
# config.toml:60-62
bind_addr = "0.0.0.0"
port = 7070
admin_token = ""
```

**Exploit scenario**: Any user on the same network can access `http://<host>:7070/api/workspaces` and manage all workspaces without credentials. They can execute arbitrary commands inside VMs, read files, destroy workspaces, etc.

**Recommendation**:
1. When `bind_addr` is not `127.0.0.1` or `::1`, **require** a non-empty `admin_token` in config validation.
2. Generate a random admin token on first init if binding to a non-loopback address.
3. Add a startup warning log when `admin_token` is empty and `bind_addr` is not localhost.

---

### AUTH-04: Bridge Token Has Only 122 Bits of Entropy (UUID v4)

**Severity**: Medium
**File**: `agentiso/src/mcp/auth.rs:496`
**Category**: Token entropy

**Description**: Bridge tokens are generated using `Uuid::new_v4()`, which provides 122 bits of randomness. While this is adequate for most purposes, the token format (`mcp-` prefix + hex UUID without dashes = 36 hex chars) leaks the generation method. The `uuid` crate uses `getrandom`/OS CSPRNG under the hood, so the randomness source is cryptographically secure.

**Code**:
```rust
let token = format!("mcp-{}", Uuid::new_v4().to_string().replace('-', ""));
```

**Assessment**: 122 bits from a CSPRNG is sufficient to prevent brute-force attacks. The `mcp-` prefix is an information leak but does not weaken the token. The 32 hex characters after the prefix provide the actual entropy.

**Recommendation (hardening, not critical)**: Consider using a full 256-bit token from `getrandom` for defense-in-depth:
```rust
use getrandom::getrandom;
let mut bytes = [0u8; 32];
getrandom(&mut bytes).unwrap();
let token = format!("mcp-{}", hex::encode(bytes));
```

---

### AUTH-05: Session IDs Are Not Server-Generated for MCP Clients

**Severity**: Medium
**File**: `agentiso/src/mcp/auth.rs:127-133`
**Category**: Session fixation

**Description**: `register_session()` accepts an arbitrary caller-supplied session ID and uses it directly. If the session ID comes from the MCP client, the client can choose a predictable or reused session ID. Due to the `entry.or_insert_with` pattern, re-registering an existing session ID is a no-op (good), but a malicious client could attempt to guess another client's session ID to access their workspaces.

**Code**:
```rust
pub async fn register_session(&self, session_id: String) -> String {
    let mut sessions = self.sessions.write().await;
    sessions
        .entry(session_id.clone())
        .or_insert_with(|| Session::new(session_id.clone(), self.default_quota.clone()));
    session_id
}
```

**Mitigating factors**:
- MCP uses stdio transport, so the session ID is typically generated by the MCP framework, not user-controlled.
- The bridge generates session IDs server-side (`format!("bridge-{}", Uuid::new_v4())`).
- Each session ID must match to perform operations, providing isolation.

**Recommendation**: For the bridge and dashboard paths, always generate session IDs server-side. Document that stdio MCP session IDs should be framework-generated UUIDs. Consider validating that session IDs match a UUID format.

---

### AUTH-06: Bridge Token Partial Logging on Invalid Token

**Severity**: Medium
**File**: `agentiso/src/mcp/bridge.rs:193`
**Category**: Token information leakage

**Description**: When an invalid bridge token is presented, the first 8 characters of the token are logged. While this aids debugging, it leaks a portion of potentially valid tokens (e.g., from a rotation race) into log files.

**Code**:
```rust
warn!(token_prefix = &token[..token.len().min(8)], "invalid bridge token");
```

**Recommendation**: Log only a hash or the first 4 characters, or log nothing about the token value. For debugging, logging that an invalid token was received (without the value) is sufficient.

---

### AUTH-07: Git Credential Redaction Is Incomplete

**Severity**: Medium
**File**: `agentiso/src/mcp/git_tools.rs:180-199`
**Category**: Credential leakage

**Description**: `redact_credentials()` covers GitHub PATs (`ghp_*`, `github_pat_*`) and GitLab PATs (`glpat-*`), but misses several common credential patterns:
- Bitbucket app passwords
- Generic `https://user:password@host` URLs in git remote output
- Azure DevOps PATs
- Generic `token` or `password` values in error messages
- SSH key passphrases in error output

Additionally, `redact_credentials()` is only called in `handle_git_push()`. It is **not** called in `handle_git_clone()`, which also runs git commands that could leak credentials in error output (e.g., `git clone https://ghp_xxxxx@github.com/...` failures).

**Code**:
```rust
// Only used in git_push:
let output = redact_credentials(&format!("{}\n{}", push_result.stdout, push_result.stderr));
```

**Recommendation**:
1. Add `https?://[^@]+:[^@]+@` URL credential pattern to the regex.
2. Call `redact_credentials()` on all git command outputs, not just `git_push`.
3. Add patterns for `x-access-token`, Bitbucket, and Azure DevOps tokens.

**Fix** (additional regex):
```rust
// Add to TOKEN_RE:
r"https?://[^:@\s]+:[^@\s]+@[^\s]+"  // URL-embedded credentials
```

---

### AUTH-08: Bridge Token Scope Enforcement Is Indirect

**Severity**: Medium
**File**: `agentiso/src/mcp/bridge.rs:102-135`
**Category**: Access control

**Description**: Bridge tokens map to a `(session_id, workspace_id)` pair, but the bridge auth middleware only validates that the token exists. It does **not** inject the workspace_id constraint into the MCP session. Each bridge session gets a fresh `AgentisoServer` with its own session ID, and the workspace is supposed to be "adopted" on first use. However, the bridge session has access to **all MCP tools**, not just the workspace it was scoped to.

A token issued for workspace-A could potentially be used to create new workspaces, access the vault, or interact with team tools — functionality not limited to workspace-A.

**Code**:
```rust
// The token is validated, but the workspace_id from the token is not used:
if auth.validate_bridge_token(token).await.is_some() {
    next.run(req).await  // No workspace_id scoping applied
}
```

**Mitigating factors**:
- Bridge tokens are short-lived (revoked on swarm_run cleanup).
- VMs have limited network access to reach the bridge.
- The session starts empty and must adopt a workspace before operating on it.

**Recommendation**: After validating the bridge token, inject the associated workspace_id into the request context, and restrict the bridge session to only operate on that specific workspace. This provides true per-workspace scoping.

---

### AUTH-09: API Keys Injected via SetEnv Are Only Held in Guest Memory

**Severity**: Low
**File**: `agentiso/src/workspace/orchestrate.rs:677-684` and `agentiso/src/workspace/mod.rs:1851-1859`
**Category**: Secret handling

**Description**: API keys (`ANTHROPIC_API_KEY`) are injected via the `SetEnv` vsock protocol into guest VMs. The keys are transmitted over vsock (host-guest only, not network-routable) and stored in the guest agent's memory. They are **not** written to the host filesystem or included in state persistence.

**Assessment**: This is a **good** design. The keys:
- Travel over vsock (not TCP/IP, not exposed to other VMs).
- Are not logged by the host (no `tracing::info!("key=...")` patterns found).
- Are not persisted in `state.json`.
- Are destroyed when the VM is destroyed.

The `set_env` tool logs only the variable count, not the values:
```rust
info!(workspace_id = %ws_id, tool = "set_env", var_count = params.vars.len(), "tool call");
```

**Residual risk**: The guest agent holds keys in memory. A compromised guest could exfiltrate them. This is inherent to the design and mitigated by VM isolation.

**Recommendation**: No changes needed. The current design is sound.

---

### AUTH-10: State File Permissions Are Set Correctly

**Severity**: Low (Positive finding)
**File**: `agentiso/src/workspace/mod.rs:1002-1005`
**Category**: File permissions

**Description**: The state file is written with mode `0o600` (owner read/write only). This prevents other users on the system from reading session ownership data.

**Code**:
```rust
let perms = std::fs::Permissions::from_mode(0o600);
tokio::fs::set_permissions(state_path, perms).await.ok();
```

**Assessment**: Good practice. The state file contains session IDs and workspace mappings but no secrets (no API keys, no bridge tokens).

**Recommendation**: None needed.

---

### AUTH-11: Config File Contains No Secrets by Default, But Could

**Severity**: Low
**File**: `config.toml` and `config.example.toml`
**Category**: Secret storage

**Description**: The config file is world-readable by default (inherits directory permissions). If a user sets `admin_token` to a strong secret value, it will be stored in plaintext in the config file. The application does not warn about config file permissions.

**Recommendation**:
1. On startup, check that `config.toml` is not world-readable (mode should be `0o600` or `0o640`) if it contains a non-empty `admin_token`.
2. Support environment variable references in config for secrets: `admin_token = "$AGENTISO_ADMIN_TOKEN"`.

---

### AUTH-12: Force-Adopt Staleness Check Uses 60-Second Fixed Threshold

**Severity**: Low
**File**: `agentiso/src/mcp/auth.rs:413`
**Category**: Session security

**Description**: The force-adopt mechanism uses a hardcoded 60-second staleness threshold. A session that has been inactive for exactly 61 seconds is considered stale and can be force-adopted. This is a reasonable default but could lead to workspace theft during brief network interruptions or MCP client restarts.

**Code**:
```rust
let stale_threshold = chrono::Duration::seconds(60);
if idle < stale_threshold {
    return Err(AuthError::AlreadyOwned { ... });
}
```

**Mitigating factors**:
- The attacker must be an authenticated MCP session.
- The `touch_session()` call on every tool invocation keeps active sessions fresh.
- The threshold protects against truly dead sessions (crashes).

**Recommendation**: Make the threshold configurable in `config.toml`. Consider increasing the default to 120 or 300 seconds for production environments where MCP clients may have brief disconnects.

---

### AUTH-13: Rate Limiting Is Per-Process, Not Per-Session

**Severity**: Low
**File**: `agentiso/src/mcp/tools.rs:501` (rate_limiter is shared across sessions)
**Category**: Rate limit bypass

**Description**: The `RateLimiter` is instantiated per `AgentisoServer` instance, meaning each MCP session gets its own rate limiter. An attacker cannot bypass rate limits by reconnecting (they get a new limiter, which starts full). However, the rate limiter does not share state across sessions, so N simultaneous sessions get N times the rate allowance.

**Mitigating factors**:
- MCP stdio transport typically has one session at a time.
- Bridge sessions are short-lived and limited to swarm operations.
- Resource quotas (workspace count, memory, disk) provide a hard cap regardless of rate.

**Recommendation**: For multi-session deployments, consider a global rate limiter shared across all sessions. For the current single-user design, this is acceptable.

---

### AUTH-14: Bridge Tokens Not Persisted Across Restart

**Severity**: Info
**File**: `agentiso/src/mcp/auth.rs:113`
**Category**: Design observation

**Description**: Bridge tokens are stored in an in-memory `HashMap` (`bridge_tokens: Arc<RwLock<HashMap<...>>>`). They are not persisted to the state file. On server restart, all bridge tokens are lost, which means any running OpenCode instances in VMs will lose MCP access.

**Assessment**: This is **correct behavior** for security — tokens should not persist. The swarm workers that used them are also destroyed on cleanup. If a restart occurs mid-swarm, the workers will need to be re-provisioned anyway.

**Recommendation**: None needed. Document this as expected behavior.

---

### AUTH-15: Session Activity Timestamp Not Cryptographically Protected

**Severity**: Info
**File**: `agentiso/src/mcp/auth.rs:138-140`
**Category**: Design observation

**Description**: `touch_session()` updates `last_activity` using `Utc::now()`. A compromised MCP session could theoretically keep itself "alive" by making tool calls, preventing force-adopt from working. However, this is by design — an actively-used session should not be stealable.

**Assessment**: The design is correct. The `last_activity` timestamp accurately reflects whether a session is actively making tool calls. An attacker with session access already has full workspace control, making force-adopt protection irrelevant in that threat model.

**Recommendation**: None needed.

---

## Summary Table

| ID | Severity | Title | File |
|----|----------|-------|------|
| AUTH-01 | High | Dashboard admin token timing side-channel | `dashboard/web/auth.rs:34` |
| AUTH-02 | High | Dashboard CORS fully permissive | `dashboard/web/mod.rs:158` |
| AUTH-03 | High | Dashboard unauthenticated on 0.0.0.0 | `config.toml:62`, `dashboard/web/auth.rs:26` |
| AUTH-04 | Medium | Bridge token 122-bit entropy (UUID v4) | `mcp/auth.rs:496` |
| AUTH-05 | Medium | Session IDs caller-supplied | `mcp/auth.rs:127` |
| AUTH-06 | Medium | Bridge token prefix logged on failure | `mcp/bridge.rs:193` |
| AUTH-07 | Medium | Git credential redaction incomplete | `mcp/git_tools.rs:180` |
| AUTH-08 | Medium | Bridge token scope not enforced | `mcp/bridge.rs:102-135` |
| AUTH-09 | Low | API keys correctly isolated in guest memory | `workspace/orchestrate.rs:677` |
| AUTH-10 | Low | State file permissions 0600 (good) | `workspace/mod.rs:1002` |
| AUTH-11 | Low | Config file could contain plaintext secrets | `config.toml` |
| AUTH-12 | Low | Force-adopt 60s threshold hardcoded | `mcp/auth.rs:413` |
| AUTH-13 | Low | Rate limiting per-session, not global | `mcp/tools.rs:501` |
| AUTH-14 | Info | Bridge tokens not persisted (by design) | `mcp/auth.rs:113` |
| AUTH-15 | Info | Session activity not cryptographically protected | `mcp/auth.rs:138` |

---

## Positive Security Observations

1. **Session ownership model**: Well-implemented with quota enforcement, orphan detection, and force-adopt with staleness protection.
2. **API key injection**: Keys travel over vsock only, are never logged, never persisted to disk, and destroyed with the VM.
3. **Bridge token lifecycle**: Generated, scoped, and revoked correctly. Not persisted.
4. **State file**: Contains no secrets. Permissions restricted to 0600.
5. **Force-adopt quota ordering**: Quotas checked before removing previous owner, preventing orphaning on quota failure.
6. **Session re-registration is a no-op**: Prevents session state reset attacks.
7. **Credential redaction**: Present on git_push output (though needs expansion).
8. **Workspace name validation**: 1-128 chars, strict character allowlist.
9. **RAII name locks**: Prevent concurrent duplicate workspace creation.
