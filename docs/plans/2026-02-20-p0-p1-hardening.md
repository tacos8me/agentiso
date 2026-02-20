# P0+P1 Hardening Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix 6 issues found by security/reliability audit — 3 critical (P0) and 3 important (P1).

**Architecture:** Each task is independent and can be parallelized. P0 tasks fix security holes (TCP fallback, orphaned workspaces) and doc inaccuracies. P1 tasks extend the vsock fresh-connection pattern to file_read/file_write/sync and add input validation.

**Tech Stack:** Rust, tokio, serde, ZFS, vsock, nftables

---

### Task 1: Remove guest agent TCP fallback (P0 — Security)

The guest agent falls back to `0.0.0.0:5000` TCP if vsock kernel modules fail to load. This exposes unauthenticated exec access to any process on the network.

**Files:**
- Modify: `guest-agent/src/main.rs:1476-1477` (remove `Tcp` variant from `Listener` enum)
- Modify: `guest-agent/src/main.rs:1527-1562` (remove TCP fallback from `listen()`)
- Modify: `guest-agent/src/main.rs:1596-1631` (remove `Listener::Tcp` branch from relay accept loop)
- Modify: `guest-agent/src/main.rs:1653-1659` (remove `Listener::Tcp` branch from main accept loop)

**Step 1: Remove the `Tcp` variant from the `Listener` enum**

In `guest-agent/src/main.rs`, change lines 1476-1478 from:

```rust
enum Listener {
    Vsock(VsockListener),
    Tcp(TcpListener),
}
```

to:

```rust
enum Listener {
    Vsock(VsockListener),
}
```

**Step 2: Remove TCP fallback from `listen()` function**

Replace the `listen()` function (lines 1527-1562) with a version that returns an error instead of falling back:

```rust
/// Bind a vsock listener. Fails hard if vsock is not available —
/// TCP fallback was removed because it exposes unauthenticated exec
/// access to the entire network.
async fn listen(port: u32) -> Result<Listener> {
    // First attempt — modules may already be loaded (e.g. by init script)
    match VsockListener::bind(port) {
        Ok(listener) => {
            info!(port, "listening on vsock");
            return Ok(Listener::Vsock(listener));
        }
        Err(e) => {
            info!(error = %e, "vsock not available, loading kernel modules...");
        }
    }

    // Load vsock modules ourselves and retry with tight polling
    load_vsock_modules();
    for attempt in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        match VsockListener::bind(port) {
            Ok(listener) => {
                info!(port, attempt, "listening on vsock (after module load)");
                return Ok(Listener::Vsock(listener));
            }
            Err(_) if attempt < 19 => continue,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "vsock unavailable after loading kernel modules (port {port}): {e}. \
                     TCP fallback is disabled for security. Ensure vhost_vsock and \
                     vsock_loopback kernel modules are available."
                ));
            }
        }
    }
    unreachable!()
}
```

**Step 3: Remove `Listener::Tcp` branches from accept loops**

In the relay accept loop (around line 1601), remove the `Listener::Tcp` match arm. The `Listener::Vsock` arm stays. Since `Listener` now only has one variant, simplify the match to a `let` destructure:

```rust
// Relay listener — replace match with direct destructure
let Listener::Vsock(vsock) = listener;
loop {
    match vsock.accept().await {
        Ok((stream, peer_cid)) => {
            let peer = format!("relay:vsock:cid={peer_cid}");
            // ... existing handler ...
        }
        Err(e) => {
            warn!(error = %e, "relay accept failed");
        }
    }
}
```

Do the same for the main accept loop (around line 1642):

```rust
// Main listener — replace match with direct destructure
let Listener::Vsock(vsock) = listener;
loop {
    let (stream, peer_cid) = vsock.accept().await?;
    let peer = format!("vsock:cid={peer_cid}");
    let (reader, writer) = tokio::io::split(stream);
    // ... existing handler ...
}
```

**Step 4: Remove unused `TcpListener` import if present**

Search for `use tokio::net::TcpListener` and remove it if it's only used in the TCP fallback. If it's also used for the A2A HTTP server, keep it.

**Step 5: Run tests**

```bash
cargo test -p agentiso-guest
```

Expected: All 33 guest-agent tests pass. No test covers the TCP fallback path (it requires missing vsock modules).

**Step 6: Build guest agent and verify compilation**

```bash
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest
```

Expected: Clean build, no errors or warnings about dead code.

**Step 7: Commit**

```bash
git add guest-agent/src/main.rs
git commit -m "security: remove guest agent TCP fallback on 0.0.0.0:5000

TCP fallback exposed unauthenticated exec access to any network peer.
Now fails hard with a diagnostic error if vsock modules aren't available."
```

---

### Task 2: Fix force-adopt quota check ordering (P0 — Correctness)

`adopt_workspace` with `force=true` removes the workspace from the previous owner (line 377-386) BEFORE checking quotas (line 397-425). If the quota check fails, the workspace is orphaned — it belongs to no session.

**Files:**
- Modify: `agentiso/src/mcp/auth.rs:376-425`
- Test: `agentiso/src/mcp/auth.rs` (existing tests module, add new test)

**Step 1: Write the failing test**

Add this test to the `mod tests` block at the bottom of `agentiso/src/mcp/auth.rs` (after `test_adopt_workspace_unknown_session`):

```rust
#[tokio::test]
async fn test_force_adopt_quota_exceeded_preserves_previous_owner() {
    // Session B has a tight quota (max 1 workspace, already full).
    let quota = SessionQuota {
        max_workspaces: 1,
        max_memory_mb: 1024,
        max_disk_gb: 10,
    };
    let auth = AuthManager::new(quota);
    let sid_a = auth.register_session("session-a".into()).await;
    let sid_b = auth.register_session("session-b".into()).await;

    let ws_owned_by_a = Uuid::new_v4();
    auth.register_workspace(&sid_a, ws_owned_by_a, 512, 5).await.unwrap();

    // Fill session B to its quota.
    let ws_owned_by_b = Uuid::new_v4();
    auth.register_workspace(&sid_b, ws_owned_by_b, 512, 5).await.unwrap();

    // Force-adopt should fail because session B is at quota.
    let result = auth
        .adopt_workspace(&sid_b, &ws_owned_by_a, 512, 5, true)
        .await;
    assert!(matches!(result, Err(AuthError::QuotaExceeded { .. })));

    // CRITICAL: Session A must still own the workspace (not orphaned).
    auth.check_ownership(&sid_a, ws_owned_by_a).await.unwrap();

    // Session A usage should be unchanged.
    let usage_a = auth.get_usage(&sid_a).await.unwrap();
    assert_eq!(usage_a.workspace_count, 1);
    assert_eq!(usage_a.memory_mb, 512);
    assert_eq!(usage_a.disk_gb, 5);
}
```

**Step 2: Run the test to verify it fails**

```bash
cargo test -p agentiso test_force_adopt_quota_exceeded_preserves_previous_owner -- --nocapture
```

Expected: FAIL — `check_ownership` returns `NotOwner` because the workspace was removed from session A before the quota check.

**Step 3: Fix the ordering in `adopt_workspace`**

In `agentiso/src/mcp/auth.rs`, restructure lines 376-432 to check quotas BEFORE removing from previous owner. Replace the section from "If force-adopting, remove from previous owner first" through the registration:

```rust
        // ---- Quota check BEFORE modifying previous owner ----
        // Re-read the adopting session.
        let session = sessions.get(session_id).unwrap();

        // If this session already owns it, no-op.
        if session.workspaces.contains(workspace_id) {
            return Ok(());
        }

        // Check workspace count quota.
        if session.usage.workspace_count >= session.quota.max_workspaces {
            return Err(AuthError::QuotaExceeded {
                resource: "workspaces".into(),
                limit: session.quota.max_workspaces as u64,
                current: session.usage.workspace_count as u64,
                requested: 1,
            });
        }

        // Check memory quota.
        if session.usage.memory_mb + memory_mb > session.quota.max_memory_mb {
            return Err(AuthError::QuotaExceeded {
                resource: "memory_mb".into(),
                limit: session.quota.max_memory_mb,
                current: session.usage.memory_mb,
                requested: memory_mb,
            });
        }

        // Check disk quota.
        if session.usage.disk_gb + disk_gb > session.quota.max_disk_gb {
            return Err(AuthError::QuotaExceeded {
                resource: "disk_gb".into(),
                limit: session.quota.max_disk_gb,
                current: session.usage.disk_gb,
                requested: disk_gb,
            });
        }

        // ---- Quotas passed — now safe to remove from previous owner ----
        if let Some(ref prev_sid) = previous_owner {
            if let Some(prev_session) = sessions.get_mut(prev_sid) {
                prev_session.workspaces.remove(workspace_id);
                prev_session.usage.workspace_count =
                    prev_session.usage.workspace_count.saturating_sub(1);
                prev_session.usage.memory_mb =
                    prev_session.usage.memory_mb.saturating_sub(memory_mb);
                prev_session.usage.disk_gb =
                    prev_session.usage.disk_gb.saturating_sub(disk_gb);
            }
        }

        // Register to new owner.
        let session = sessions.get_mut(session_id).unwrap();
        session.workspaces.insert(*workspace_id);
        session.usage.workspace_count += 1;
        session.usage.memory_mb += memory_mb;
        session.usage.disk_gb += disk_gb;
        Ok(())
```

**Step 4: Run the test to verify it passes**

```bash
cargo test -p agentiso test_force_adopt_quota_exceeded_preserves_previous_owner -- --nocapture
```

Expected: PASS

**Step 5: Run all auth tests to verify no regressions**

```bash
cargo test -p agentiso -- auth::tests --nocapture
```

Expected: All auth tests pass (the existing `test_adopt_workspace_force_from_other_session` still passes because quotas are generous in the default quota).

**Step 6: Commit**

```bash
git add agentiso/src/mcp/auth.rs
git commit -m "fix: check quotas before removing previous owner in force-adopt

Prevents workspace orphaning when the adopting session is at quota.
Previously, force-adopt removed from old owner first, then checked
quotas — if quota check failed, workspace belonged to nobody."
```

---

### Task 3: Fix doc inaccuracies (P0 — Docs)

Three doc errors found by audit:
1. CLAUDE.md says "19 steps" for e2e test — actual is ~14 pass outcomes across 9 sections
2. workflows.md Quick Reference missing `vault_context`, `shared_context` on `swarm_run`
3. workflows.md Quick Reference missing `force` on `workspace_adopt`

**Files:**
- Modify: `CLAUDE.md:82` (e2e step count)
- Modify: `docs/workflows.md:1751` (workspace_adopt `force` param)
- Modify: `docs/workflows.md:1758` (swarm_run missing params)

**Step 1: Fix CLAUDE.md e2e step count**

In `CLAUDE.md`, find line 82:

```
# E2E test (needs root for QEMU/KVM/TAP/ZFS) — 19 steps
```

Change to:

```
# E2E test (needs root for QEMU/KVM/TAP/ZFS) — 9 sections
```

**Step 2: Fix workflows.md workspace_adopt**

In `docs/workflows.md`, find line 1751:

```
| `workspace_adopt` | -- | `workspace_id` |
```

Change to:

```
| `workspace_adopt` | -- | `workspace_id`, `force` |
```

**Step 3: Fix workflows.md swarm_run**

In `docs/workflows.md`, find line 1758:

```
| `swarm_run` | `golden_workspace`, `snapshot_name`, `tasks` | `env_vars`, `merge_strategy`, `merge_target`, `max_parallel`, `timeout_secs`, `cleanup`, `allow_internet` |
```

Change to:

```
| `swarm_run` | `golden_workspace`, `snapshot_name`, `tasks` | `env_vars`, `merge_strategy`, `merge_target`, `max_parallel`, `timeout_secs`, `cleanup`, `allow_internet`, `vault_context`, `shared_context` |
```

**Step 4: Run a quick check that no other stale counts exist**

```bash
grep -rn "19 steps" docs/ CLAUDE.md README.md
grep -rn "19 step" docs/ CLAUDE.md README.md
```

Expected: No remaining "19 steps" references.

**Step 5: Commit**

```bash
git add CLAUDE.md docs/workflows.md
git commit -m "docs: fix e2e step count, add missing params to workflows Quick Reference

- CLAUDE.md: 19 steps → 9 sections (e2e test)
- workflows.md: add force to workspace_adopt, vault_context + shared_context to swarm_run"
```

---

### Task 4: Move file_read, file_write, sync to fresh vsock connections (P1 — Reliability)

`file_read` and `file_write` hold the shared vsock mutex for up to 30s. Pre-snapshot `sync` holds it for up to 15s. These block all other vsock operations on the same workspace, just like `exec` did before the fix in commit `a117842`.

Apply the same pattern: use `self.fresh_vsock_client()` instead of `self.vm.read().await.vsock_client_arc()`.

**Files:**
- Modify: `agentiso/src/workspace/mod.rs:1754-1771` (file_write)
- Modify: `agentiso/src/workspace/mod.rs:1776-1796` (file_read)
- Modify: `agentiso/src/workspace/mod.rs:1955-1968` (pre-snapshot sync)

**Step 1: Change `file_write` to use fresh connection**

In `agentiso/src/workspace/mod.rs`, replace lines 1765-1766:

```rust
        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;
```

with:

```rust
        let mut vsock = self.fresh_vsock_client(&workspace_id).await?;
```

**Step 2: Change `file_read` to use fresh connection**

In `agentiso/src/workspace/mod.rs`, replace lines 1785-1786:

```rust
        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;
```

with:

```rust
        let mut vsock = self.fresh_vsock_client(&workspace_id).await?;
```

**Step 3: Change pre-snapshot `sync` to use fresh connection**

In `agentiso/src/workspace/mod.rs`, replace lines 1955-1957:

```rust
    {
        let vsock_arc = self.vm.read().await.vsock_client_arc(&workspace_id)?;
        let mut vsock = vsock_arc.lock().await;
```

with:

```rust
    {
        let mut vsock = self.fresh_vsock_client(&workspace_id).await?;
```

(Keep the closing brace and the rest of the sync block unchanged.)

**Step 4: Run all tests**

```bash
cargo test -p agentiso
```

Expected: All 717 tests pass. The unit tests don't actually connect to vsock (they test logic, not I/O), so this change is safe.

**Step 5: Commit**

```bash
git add agentiso/src/workspace/mod.rs
git commit -m "fix: use fresh vsock for file_read, file_write, and pre-snapshot sync

Same pattern as exec fix (a117842). These held the shared mutex for
up to 30s (file ops) or 15s (sync), blocking all other operations on
the workspace."
```

---

### Task 5: Add size limit to shared_context (P1 — Security)

`shared_context` in `swarm_run` accepts an unbounded string that gets written to every worker VM. A malicious or buggy caller could pass gigabytes.

**Files:**
- Modify: `agentiso/src/mcp/tools.rs` (swarm_run handler, near line 3040)

**Step 1: Write the failing test**

In the `tools.rs` test module (or add inline if no test module exists), add a test that verifies oversized `shared_context` is rejected. Since tools.rs tests may be integration-level, add validation inline. Skip the test and go straight to implementation since the validation is a one-liner.

**Step 2: Add the size check**

In `agentiso/src/mcp/tools.rs`, find the `swarm_run` handler where `shared_context` is first extracted from params. Before it's used (before the injection loop around line 3040), add:

```rust
        // Limit shared_context to 1 MiB (same limit as vault writes).
        if let Some(ref ctx) = shared_ctx {
            const MAX_SHARED_CONTEXT: usize = 1024 * 1024; // 1 MiB
            if ctx.len() > MAX_SHARED_CONTEXT {
                return Ok(tool_error(format!(
                    "shared_context too large: {} bytes (max {} bytes)",
                    ctx.len(),
                    MAX_SHARED_CONTEXT
                )));
            }
        }
```

Find where `shared_ctx` is first extracted from params (search for `let shared_ctx` or `params.shared_context`). Add the check immediately after that extraction, before the fork/inject loop.

**Step 3: Run tests**

```bash
cargo test -p agentiso
```

Expected: All 717 tests pass (no existing test sends oversized shared_context).

**Step 4: Commit**

```bash
git add agentiso/src/mcp/tools.rs
git commit -m "security: limit shared_context to 1 MiB in swarm_run

Prevents unbounded memory allocation from a malicious or buggy caller
passing gigabytes of context that gets written to every worker VM."
```

---

### Task 6: Add workspace name validation (P1 — Input Validation)

Workspace names have zero character validation. While the human name isn't used in ZFS paths (UUID prefix is used), invalid names could cause logging issues, JSON serialization problems, or confuse display output.

**Files:**
- Modify: `agentiso/src/workspace/mod.rs` (in `create()` function, around line 972)
- Test: `agentiso/src/workspace/mod.rs` (existing tests module)

**Step 1: Write the failing test**

Add to the workspace tests module in `agentiso/src/workspace/mod.rs`:

```rust
#[test]
fn test_workspace_name_validation() {
    // Valid names
    assert!(is_valid_workspace_name("my-workspace"));
    assert!(is_valid_workspace_name("ws_test_123"));
    assert!(is_valid_workspace_name("a"));
    assert!(is_valid_workspace_name("my.workspace.v2"));

    // Invalid names
    assert!(!is_valid_workspace_name(""));                    // empty
    assert!(!is_valid_workspace_name(&"a".repeat(129)));      // too long (max 128)
    assert!(!is_valid_workspace_name("has spaces"));          // spaces
    assert!(!is_valid_workspace_name("has/slash"));           // path separator
    assert!(!is_valid_workspace_name("has\nnewline"));        // control chars
    assert!(!is_valid_workspace_name("has\0null"));           // null byte
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test -p agentiso test_workspace_name_validation
```

Expected: FAIL — function `is_valid_workspace_name` does not exist.

**Step 3: Implement the validation function**

Add near the top of `agentiso/src/workspace/mod.rs` (in a utility section):

```rust
/// Validate a workspace name. Names must be 1-128 chars, containing only
/// alphanumeric characters, hyphens, underscores, and dots. No spaces,
/// path separators, control characters, or null bytes.
fn is_valid_workspace_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}
```

**Step 4: Wire into `create()`**

In the `create()` function, after `let name = params.name.unwrap_or_else(...)` (around line 972), add:

```rust
        if !is_valid_workspace_name(&name) {
            bail!(
                "invalid workspace name '{}': must be 1-128 chars, alphanumeric/hyphen/underscore/dot only",
                name
            );
        }
```

Also wire into `workspace_prepare`'s name handling in `tools.rs` — find where `prepare_acquire_workspace` receives the name and add the same check, or rely on the `create()` guard since all paths go through it.

**Step 5: Run test to verify it passes**

```bash
cargo test -p agentiso test_workspace_name_validation
```

Expected: PASS

**Step 6: Run all tests to check for regressions**

```bash
cargo test -p agentiso
```

Expected: All tests pass. Existing tests use valid names like `"test-workspace"` or auto-generated `"ws-{uuid}"` which match the pattern.

**Step 7: Commit**

```bash
git add agentiso/src/workspace/mod.rs
git commit -m "security: validate workspace names (1-128 chars, alphanumeric/hyphen/underscore/dot)

Prevents control characters, path separators, null bytes, and
unreasonably long names from reaching logs, JSON output, or display."
```

---

## Task Dependency Graph

All 6 tasks are independent and can be parallelized:

```
Task 1 (guest TCP fallback)  ─┐
Task 2 (force-adopt ordering) ─┤
Task 3 (doc fixes)            ─┼── all independent, merge at end
Task 4 (fresh vsock for files) ─┤
Task 5 (shared_context limit)  ─┤
Task 6 (workspace name valid.) ─┘
```

After all tasks complete, run the full test suite:

```bash
cargo test
```

Expected: All 806+ tests pass (new tests in Tasks 2 and 6 add to the count).
