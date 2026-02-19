# Adversarial Readiness Evaluation — 2026-02-19

## Executive Summary

**Verdict: Ready for personal use. Approaching production readiness.**

Two sprints since the last eval (02-17) delivered tool bundling (45→34 tools), git tools, auto-adopt on restart, warm VM pool, `agentiso init` CLI, and expanded test coverage. This eval was conducted by a 4-agent adversarial team covering coverage, security, UX, and ops.

The most impactful finding was the snapshot bundling aftermath — phantom tool names left across server instructions, error messages, e2e tests, and docs. All were fixed in a follow-up sprint during this eval session.

---

## Scorecard

| Area | Previous (02-17) | Current (02-19) | Delta |
|------|-------------------|------------------|-------|
| **Coverage** | 55% e2e (11/20) | 82% e2e (28/34) | +27pp |
| **Security** | ~3.0/5 | 3.2/5 | +0.2 |
| **UX** | 3.9/5 | 4.2/5 | +0.3 |
| **Ops** | ~2.5/5 | 3.8/5 | +1.3 |
| **Unit tests** | 194 | 537 | +343 |

---

## Coverage Findings

**28 of 34 MCP tools tested via e2e integration tests (82.4%)**

All 9 previously untested tools now have e2e coverage: `workspace_stop`, `workspace_start`, `file_upload`, `file_download`, `snapshot_restore`, `snapshot_delete`, `workspace_fork`, `port_forward`, `port_forward_remove`, `network_policy`.

| Status | Tools |
|--------|-------|
| E2E tested | `workspace_create`, `workspace_destroy`, `workspace_list`, `workspace_info`, `workspace_stop`, `workspace_start`, `exec`, `file_write`, `file_read`, `file_upload`, `file_download`, `snapshot`, `workspace_fork`, `port_forward`, `port_forward_remove`, `workspace_ip`, `network_policy`, `file_list`, `file_edit`, `exec_background`, `exec_poll`, `exec_kill`, `workspace_logs`, `workspace_adopt_all`, `workspace_prepare`, `workspace_batch_fork`, `workspace_git_status` |
| Unit only | `set_env`, `workspace_adopt`, `git_clone`, `vault`, `git_commit`, `git_push`, `git_diff` |

**Remaining gaps:** The git workflow chain (`git_clone` → `git_commit` → `git_diff` → `git_push`) has zero e2e coverage. `vault` has 20+ unit tests but no filesystem e2e test. `set_env` is tested only at the param deserialization level.

### Unit tests by module

| Module | Tests |
|--------|-------|
| MCP Tools | 142 |
| Storage (ZFS) | 45 |
| Workspace Core | 33 |
| Protocol | 29 |
| VM (QEMU) | 22 |
| VM (vsock) | 22 |
| Guest Agent | 21 |
| Workspace Orchestrate | 19 |
| Config | 17 |
| Network (nftables) | 17 |
| VM (OpenCode) | 16 |
| VM (microvm) | 14 |
| Network (DHCP) | 14 |
| Workspace Snapshot | 13 |
| Dashboard | 11 |
| VM (mod) | 8 |
| VM (cgroup) | 8 |
| MCP Metrics | 7 |
| Network (bridge) | 6 |
| Init | 3 |
| Storage (mod) | 3 |
| Workspace Pool | 1 |
| **Total** | **471** |

(537 total with cross-crate tests.)

**Coverage verdict: 4/5** — massive improvement from 55% to 82%. Would reach 5/5 with git workflow e2e tests and vault smoke test.

---

## Security Findings

### Original 9 findings re-scored

| # | Finding | Previous | Current |
|---|---------|----------|---------|
| 1 | QMP command injection via snapshot names | Fixed | **Fixed** |
| 2 | ZFS command injection via snapshot names | Fixed | **Fixed** |
| 3 | TOCTOU quota race in workspace_create | Not fixed | **Not fixed** |
| 4 | No MCP stdio authentication | By design | **By design** |
| 5 | Guest agent runs as root, no path restrictions | VM mitigates | **VM mitigates** |
| 6 | Port forward allows privileged host ports | Fixed | **Fixed** |
| 7 | Non-atomic state file write | Fixed | **Fixed** |
| 8 | No single-instance enforcement | Fixed | **Fixed** |
| 9 | allow_internet=true default | Fixed | **Fixed** (re-fixed: compiled-in default changed to false) |

### New findings from Sprint 1+2

| # | Finding | Severity | Status |
|---|---------|----------|--------|
| N1 | git_push may expose credentials in stderr | Medium | **Fixed** — regex redaction added |
| N2 | git_commit message injection | Info | Safe — shell_escape() used |
| N3 | snapshot action injection | Info | Safe — Rust match dispatch |
| N4 | Vault path traversal | Info | Safe — resolve_path() rejects `..` |
| N5 | Auto-adopt PID reuse risk | Medium | **Fixed** — /proc/cmdline verification added |
| N6 | Init writes nftables to predictable /tmp path | Low | Open |
| N7 | Init interpolates SUDO_USER into shell command | Medium | Open |
| N8 | allow_internet compiled-in default was true | Medium | **Fixed** |
| N9 | workspace_adopt_all session hijacking (multi-client) | Low | Open — mitigated by stdio single-client |
| N10 | git_clone SCP-style URL permissiveness | Low | Open — contained within guest VM |
| N11 | Warm pool VMs may have stale network on reconfigure failure | Low | Open |
| N12 | No rate limiting on MCP tool calls | Low | Open |

**Security verdict: 3.2/5** — modest improvement. Input validation is strong (4/5), credential handling improved, allow_internet fixed. Remaining gaps: TOCTOU quota race, init.rs command injection, no rate limiting.

---

## UX Findings

**Overall UX score: 4.2/5** (up from 3.9)

### Server instructions: 3.5/5

~800 words across 4 sections (TOOL GROUPS, QUICK START, WORKFLOW TIPS, COMMON PITFALLS). Well-structured quick-start workflow. Phantom tool names were fixed in this eval. Missing mention of `workspace_prepare`/`workspace_batch_fork` for parallel workflows.

### Tool descriptions: 4.0/5

Every tool and parameter has clear doc comments. Bundled tools (snapshot, vault) document valid actions. `workspace_id` consistently accepts UUID or name. Defaults documented on all optional params. Issue: vault's 22-field god-object param struct shows all fields for every action.

### Error handling: 4.5/5

Best dimension — every error includes workspace ID and actionable suggestion. Error codes properly categorized (invalid_request vs internal_error). git_push has context-dependent suggestions. Binary file detection in file_read. Phantom tool references in error suggestions were fixed in this eval.

### Tool surface: 4.5/5

34 tools is right-sized (was 45). Bundling snapshot 5→1 and vault 11→1 was the right call. Minor issues: `workspace_ip` still redundant with `workspace_info`, `snapshot(action="diff")` is effectively a no-op, git tool naming inconsistency (`workspace_git_status` vs `git_*`).

### Workflow friction still remaining

- Vault god-object params (22 optional fields regardless of action)
- Poll loop required for background commands (no blocking exec_poll)
- `snapshot(action="diff")` returns only metadata, not file-level diff (ZFS zvol limitation)

---

## Ops Findings

### Original medium findings re-scored

| Finding | Previous | Current |
|---------|----------|---------|
| No startup orphan reconciliation | Not fixed | **Fixed** — 5-phase reconciliation in load_state |
| Session state lost after restart | Not fixed | **Fixed** — schema v2 with PersistedSession |
| Global ip_forward=1 affects all interfaces | Not fixed | **Not fixed** |
| No ZFS quotas per workspace | Not fixed | **Not fixed** |

### Auto-adopt assessment: Excellent

5-phase reconciliation on startup:
1. PID-based workspace adoption (now with /proc/cmdline QEMU verification)
2. Orphan QEMU kill (PID files + /proc scan fallback)
3. Stale TAP cleanup for stopped workspaces
4. Orphaned TAP cleanup (bridge scan vs known workspaces)
5. Orphaned ZFS detection (log warnings, conservative — no auto-destroy)

### Warm pool assessment: Good

- Clean degradation to cold create on empty pool
- Proper rollback at every failure stage (storage → network → CID → QEMU)
- Memory budget gate prevents over-provisioning
- Gap: pool VMs not tracked in state.json — crash leaves orphaned ZFS datasets

### Graceful shutdown: Good

- Parallel VM shutdown via JoinSet (was sequential)
- 4-stage escalation: guest agent → wait → ACPI powerdown → force kill
- Pre-shutdown state save for resilience
- SystemD alignment: 180s timeout with 30s graceful + force kill

### Init CLI: Solid

- 8-step idempotent setup with 11-item verification
- Sequential with proper partial-failure recovery (re-run picks up where it left off)
- Gap: guest agent rebuild on every run (harmless but unnecessary)

**Ops verdict: 3.8/5** — biggest improvement area (+1.3). Auto-adopt transforms reliability story. Remaining: ZFS quotas, ip_forward scoping, pool VM state persistence.

---

## What Was Fixed During This Eval

- Phantom snapshot tool names in server instructions (snapshot_create → snapshot(action="create"))
- Phantom snapshot tool names in error messages (snapshot_list → snapshot(action="list"))
- Phantom snapshot tool names in e2e test scripts (13 call sites across 3 scripts)
- Snapshot param name mismatch in docs (snapshot_name → name)
- False claims about unimplemented used_bytes/referenced_bytes in docs
- Credential redaction for git_push stderr (ghp_, glpat-, github_pat_ patterns)
- allow_internet compiled-in default changed to false (config.rs + init.rs)
- PID reuse verification in auto-adopt via /proc/{pid}/cmdline

---

## Remaining Work for Production

**Must-have:**

- ZFS per-dataset quotas (`zfs set refquota=`)
- TOCTOU quota race fix (atomic check-and-register)
- Git workflow e2e tests (clone → commit → diff → push)
- Scoped ip_forward (per-interface, not global)
- Rate limiting on MCP tool calls
- Init.rs: escape SUDO_USER, use secure temp files for nftables

**Nice-to-have:**

- Vault e2e smoke test
- Systemd watchdog integration
- Pool VM state persistence (avoid ZFS leak on crash)
- `file_append` or write mode parameter
- Blocking `exec_poll` with timeout (eliminate poll loops)
- Normalize git tool naming (`workspace_git_status` → `git_status`)
- Remove `snapshot(action="diff")` until useful
- Remove `workspace_ip` (redundant with `workspace_info`)
