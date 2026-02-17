# Adversarial Readiness Evaluation — 2026-02-17

## Executive Summary

**Verdict: Ready for personal Claude Code use. Not ready for multi-tenant/production.**

The core is solid — VM isolation via QEMU microvm, per-session auth tokens, secure-by-default networking (deny-all nftables), and a clean 2-call workflow (`workspace_create` then `workspace_exec`). Two blockers were identified and fixed during this sprint: non-atomic state file writes and missing single-instance enforcement.

Nine of twenty MCP tools remain completely untested. The most critical gap is `snapshot_restore`, which is the foundation of the checkpoint/rollback workflow but has never been exercised end-to-end.

This evaluation was conducted by a 4-agent adversarial team covering coverage audit, security analysis, UX evaluation, and ops readiness.

---

## Coverage Findings

**11 of 20 MCP tools tested via integration test (55%)**

The `scripts/test-mcp-integration.sh` suite covers the core lifecycle:

| Status | Tools |
|--------|-------|
| Tested | `workspace_create`, `workspace_destroy`, `workspace_info`, `workspace_ip`, `workspace_exec`, `workspace_list`, `file_write`, `file_read`, `snapshot_create`, `file_list`, `file_edit` |
| Untested | `workspace_stop`, `workspace_start`, `file_upload`, `file_download`, `snapshot_restore`, `snapshot_delete`, `workspace_fork`, `port_forward`, `port_forward_remove`, `network_policy` |

**Critical gap:** `snapshot_restore` has never been tested end-to-end. This is the entire checkpoint/rollback workflow — an agent creates a snapshot, does work, then rolls back. If this path is broken, the snapshot feature is unusable.

**Unit test limitation:** The 194 Rust unit tests primarily cover parameter deserialization and internal logic. They do not test actual tool execution against real VMs, ZFS, or networking.

---

## Security Findings

| # | Finding | Severity | Root Cause | Fixed? |
|---|---------|----------|------------|--------|
| 1 | QMP command injection via snapshot names | High | `qemu.rs` `savevm`/`loadvm`/`delvm` passes unsanitized tag into `human-monitor-command` | Yes — snapshot name validation added |
| 2 | ZFS command injection via snapshot names | High | Same snapshot name passed to `zfs` CLI without shell escaping or validation | Yes — same validation |
| 3 | TOCTOU quota race in `workspace_create` | High | `check_quota()` and `register_workspace()` are not atomic; concurrent creates can exceed quota | No — medium-term fix |
| 4 | No MCP stdio authentication | Medium | No token or secret required on stdio connection | No — by design for single-user stdio; needs auth for TCP transport |
| 5 | Guest agent runs as root with no path restrictions | Medium | `guest-agent/src/main.rs` accepts any path for file ops, no allowlist | No — VM boundary mitigates; defense-in-depth improvement for later |
| 6 | Port forward allows privileged host ports | Low | `host_port < 1024` not rejected, allowing bind to system ports | Yes — validation added |
| 7 | Non-atomic state file write | **Blocker** | `tokio::fs::write` truncates file in place; crash during write corrupts state | Yes — atomic write-to-temp + rename |
| 8 | No single-instance enforcement | **Blocker** | Two concurrent `agentiso serve` processes corrupt state file, IP allocations, and nftables rules | Yes — `flock` on `agentiso.lock` |
| 9 | `config.toml` ships `allow_internet=true` | High | Default config contradicts the secure-by-default deny-all nftables policy in code | Yes — default changed to `false` |

---

## UX Findings

**Overall rating: 3.9 / 5** — Good for AI agent consumption. The 2-call workflow is clean and discoverable. Issues are mostly edge cases.

| Finding | Impact | Fixed? |
|---------|--------|--------|
| `mode` field (file_write) treated as decimal but documented as octal (e.g., `755` interpreted as decimal `755`, not octal `0o755`) | Agent sets wrong permissions silently | Yes — parse as octal string |
| State-transition failures return `internal_error` instead of `invalid_request` | Agent cannot distinguish user error from server bug | Yes — correct error codes |
| Missing `file_list`, `file_edit`, `exec_background` tools | Agents forced into multi-step workarounds | Yes — added in this sprint |
| `workspace_ip` is redundant with `workspace_info` (which includes IP) | Minor API bloat | No — kept for backwards compatibility |
| `workspace_exec` output can overflow agent context window on large command output | Agent context poisoned by huge stdout/stderr | Yes — `max_output_bytes` parameter with truncation indicator |
| No tool-call logging | Cannot debug agent behavior or audit actions | Yes — structured logging added |

---

## Ops Findings

### Blockers (both fixed)

1. **Non-atomic state writes** — A crash during `tokio::fs::write` leaves a truncated or empty state file, losing all workspace tracking. Fixed with atomic write-to-temp + `rename(2)`.

2. **No instance lock** — Running two `agentiso serve` processes simultaneously corrupts the state file, double-allocates IPs, and creates conflicting nftables rules. Fixed with `flock` on `/var/lib/agentiso/agentiso.lock`.

### High (partially fixed)

| Issue | Status |
|-------|--------|
| Systemd unit missing `TimeoutStopSec`, `RUST_LOG`, `ProtectHome` | Fixed — hardened unit file |
| Sequential VM shutdown: 180s worst case for 20 VMs (9s timeout each) | Partially fixed — `TimeoutStopSec` increased; shutdown still sequential |
| Zero MCP tool-call logging | Fixed — structured request/response logging |

### Medium (not fixed)

| Issue | Notes |
|-------|-------|
| No startup orphan reconciliation | After a crash, stale QEMU processes, ZFS datasets, and TAP devices are not cleaned up on restart |
| Session state lost after restart | Workspaces tracked in memory; restarting the server loses all session tokens |
| Global `ip_forward=1` affects all interfaces | `sysctl net.ipv4.ip_forward=1` is system-wide, not scoped to `br-agentiso` |
| No ZFS quotas per workspace | A single workspace can fill the entire pool |

---

## What Was Fixed in This Sprint

- Snapshot name validation (alphanumeric, hyphens, underscores, max 64 chars) — prevents QMP and ZFS command injection
- Privileged port validation — `host_port >= 1024` enforced in `port_forward`
- Atomic state file writes — write to `.tmp` then `rename(2)`
- Single-instance `flock` enforcement on startup
- `config.toml` default `allow_internet=false`
- File mode octal parsing — `mode` field parsed as octal string
- Correct MCP error codes for state-transition failures
- Added `file_list`, `file_edit`, `exec_background` tools
- `max_output_bytes` truncation on `workspace_exec`
- Structured tool-call logging
- Hardened systemd unit (`TimeoutStopSec=300`, `RUST_LOG=info`, `ProtectHome=true`)

---

## Remaining Work for Production

**Must-have for multi-tenant deployment:**

- **Startup orphan reconciliation** — On restart, scan for stale QEMU processes, ZFS datasets, and TAP devices; reconcile with state file or clean up
- **Parallel VM shutdown** — Use `tokio::JoinSet` to shut down VMs concurrently instead of sequentially
- **Test coverage for 9 untested tools** — Especially `snapshot_restore` and `workspace_fork`, which are core agent workflows
- **ZFS per-dataset quotas** — `zfs set quota=10G` on each workspace dataset
- **Session re-claim after restart** — Persist session tokens and allow agents to reconnect
- **MCP authentication for TCP transport** — Token-based auth when not using stdio

**Nice-to-have:**

- Network bandwidth limits (`tc`/QoS on TAP devices)
- Prometheus metrics endpoint (workspace count, exec latency, resource usage)
- `WorkspaceManager` mock for unit testing MCP tools without real infrastructure
- Scoped `ip_forward` (per-interface forwarding or nftables-only forwarding)
