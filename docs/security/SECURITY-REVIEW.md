# agentiso Security Review — Consolidated Report

**Date**: 2026-02-24
**Team**: 6 specialist auditors (network, vm-boundary, auth-secrets, input-validation, privilege, frontend)
**Scope**: Full codebase — Rust host daemon, guest agent, protocol, frontend dashboard, build scripts, configuration

---

## Executive Summary

The agentiso security architecture is **fundamentally sound**. The VM boundary via QEMU microvm + vsock is well-implemented with no host escape vectors found. Input validation is strong across all 31 MCP tools with consistent use of `Command::arg()` and `shell_escape()`. Path traversal prevention, workspace name validation, and HMP tag sanitization are all correct.

However, the **dashboard and network layers have significant gaps** that must be addressed before any non-localhost deployment. The most urgent issues are: permissive CORS allowing cross-origin attacks, no authentication enforcement on the dashboard, guest HTTP API with zero auth enabling cross-VM message injection, and missing anti-spoofing rules on the bridge.

**Total findings across all audits**: 2 Critical, 13 High, 24 Medium, 16 Low, 18 Info

---

## Critical Findings (Fix Immediately)

### C-1: CORS Fully Permissive — Any Website Can Call Dashboard API
**Source**: AUTH-02, F-01 (cross-validated by 2 auditors)
**File**: `agentiso/src/dashboard/web/mod.rs:158`
**Impact**: Any website visited by a dashboard user can make authenticated cross-origin requests — create/destroy workspaces, execute commands, read files.

**Fix**: Replace `CorsLayer::permissive()` with restrictive policy:
```rust
CorsLayer::new()
    .allow_origin(AllowOrigin::exact(
        HeaderValue::from_str(&format!("http://{}:{}", bind_addr, port)).unwrap()
    ))
    .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
    .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
```

### C-2: Guest HTTP API Unauthenticated — Cross-VM Message Injection
**Source**: PRIV-02, VM-03 (cross-validated by 2 auditors)
**File**: `guest-agent/src/main.rs:1200-1219`
**Impact**: Any team VM can POST to another VM's port 8080, inject spoofed task assignments, trigger arbitrary exec via A2A daemon.

**Fix**: Bind to `127.0.0.1:8080` (single line change):
```rust
let addr = "127.0.0.1:8080"; // was "0.0.0.0:8080"
```

---

## High Findings (Fix Before Non-Localhost Deployment)

### H-1: Dashboard Unauthenticated on 0.0.0.0
**Source**: AUTH-03, PRIV-05
**Files**: `config.toml:62`, `dashboard/web/auth.rs:26`
**Fix**: Require non-empty `admin_token` when `bind_addr != 127.0.0.1`. Warn on startup.

### H-2: No Security Headers on Dashboard Responses
**Source**: F-02
**Files**: `dashboard/web/mod.rs`
**Fix**: Add middleware: X-Frame-Options: DENY, X-Content-Type-Options: nosniff, CSP, Referrer-Policy.

### H-3: WebSocket Endpoint Has No Authentication
**Source**: F-03
**File**: `dashboard/web/ws.rs:118`
**Fix**: Add token query parameter validation before WebSocket upgrade.

### H-4: Frontend Never Sends Auth Token (No Login UI)
**Source**: F-04
**Fix**: Add login page or cookie-based auth flow.

### H-5: Dashboard Admin Token Timing Side-Channel
**Source**: AUTH-01
**File**: `dashboard/web/auth.rs:34`
**Fix**: Use constant-time comparison (`subtle::ConstantTimeEq` or HMAC comparison).

### H-6: No Anti-Spoofing Rules on Bridge (IP Spoofing)
**Source**: NET-01
**File**: `network/nftables.rs:91-125`
**Fix**: Add per-TAP anti-spoofing: `iifname "tap-X" ip saddr != {allocated_ip} drop`.

### H-7: DNAT Port Forwarding Not Scoped to Bridge Interface
**Source**: NET-02
**File**: `network/nftables.rs:210-245`
**Fix**: Add `iifname "lo"` or `iifname "br-agentiso"` constraint to prerouting DNAT rules.

### H-8: cgroup v2 Limits Best-Effort (No Resource Limits If Unavailable)
**Source**: PRIV-01
**File**: `vm/cgroup.rs:1-8`
**Fix**: Add `cgroup_required = true` config option. Fail workspace creation if cgroups unavailable when required.

### H-9: Guest OOM Score -1000 Without Guaranteed cgroup Limits
**Source**: PRIV-11
**File**: `guest-agent/src/main.rs:1678`
**Fix**: Remove guest-side `oom_score_adj=-1000`. Set `-500` from host only after confirming cgroup memory limits.

### H-10: Nested Team Budget Race — Concurrent Sub-Team Creation Bypasses Limits
**Source**: PRIV-09
**File**: `team/mod.rs:118-139`
**Fix**: Hold write lock during sub-team creation, count VMs recursively through hierarchy.

### H-11: No RLIMIT_NPROC Cap on QEMU Processes
**Source**: PRIV-03
**Fix**: Add `LimitNPROC=` in systemd unit. Cross-check `pool.max_size + max_workspaces <= max_total_vms` in config validation.

---

## Medium Findings (Fix in Next Sprint)

| ID | Finding | Source | File | Fix Effort |
|----|---------|--------|------|------------|
| M-1 | Input chain `policy accept` — VMs can probe host services | NET-03 | nftables.rs:93 | 30 min |
| M-2 | Inter-VM isolation bypass via conntrack | NET-04 | nftables.rs:170 | 30 min |
| M-3 | No ARP protections on bridge | NET-06 | bridge.rs | 1 hr |
| M-4 | Team rules use `String` IPs (fragile injection vector) | NET-05 | nftables.rs:689 | 15 min |
| M-5 | MCP bridge token grants all 31 tools (no scoping) | NET-07, AUTH-08 | bridge.rs:64 | 1 hr |
| M-6 | Bridge token prefix logged on invalid attempts | AUTH-06 | bridge.rs:193 | 5 min |
| M-7 | Git credential redaction incomplete (missing patterns + clone) | AUTH-07 | git_tools.rs:180 | 15 min |
| M-8 | Session IDs caller-supplied for MCP stdio | AUTH-05 | auth.rs:127 | 15 min |
| M-9 | Per-request exec env bypasses blocklist | VM-05 | guest main.rs:97 | 10 min |
| M-10 | Exec output read into unbounded Vec (guest OOM) | VM-08 | guest main.rs:150 | 15 min |
| M-11 | Guest HTTP API on 0.0.0.0 (if not fixed as C-2) | VM-03 | guest main.rs:1207 | 1 min |
| M-12 | 16 MiB allocation per vsock message (coordinated DoS) | VM-02 | protocol/lib.rs:11 | 5 min |
| M-13 | No CSRF protection on state-changing endpoints | F-06 | dashboard web | Resolved by C-1 |
| M-14 | Terminal escape sequence injection via xterm.js | F-07 | TerminalPane.tsx | 5 min |
| M-15 | SSE exec streaming lacks connection limits | F-08 | workspaces.rs:1109 | 15 min |
| M-16 | UTF-8 unsafe truncation in git_diff | INP-M1 | git_tools.rs:779 | 5 min |
| M-17 | Regex pattern length unlimited in vault search | INP-M2 | vault.rs:417 | 5 min |
| M-18 | Role names not validated before workspace/vault use | INP-M3 | team/mod.rs:213 | 5 min |
| M-19 | ZFS snapshots bypass volsize quotas (disk exhaustion) | PRIV-04 | workspace/mod.rs | 30 min |
| M-20 | Root password "agentiso" in base image | PRIV-06 | setup-e2e.sh:229 | 5 min |
| M-21 | Per-session rate limits (not global) | PRIV-07 | auth.rs | 30 min |
| M-22 | Host FD exhaustion from vsock burst | PRIV-10 | vm/vsock.rs | 15 min |
| M-23 | Bridge token 122-bit entropy (UUID v4 — adequate but could be 256-bit) | AUTH-04 | auth.rs:496 | 5 min |
| M-24 | XSS defense-in-depth (React is safe, needs CSP backup) | F-05 | Resolved by H-2 |

---

## Low Findings (Hardening)

| ID | Finding | Source | Fix Effort |
|----|---------|--------|------------|
| L-1 | Port forward doesn't check reserved ports (3100, 7070) | NET-08 | 10 min |
| L-2 | iptables belt-and-suspenders rules overly broad | NET-09 | 15 min |
| L-3 | nftables identifiers not explicitly validated | NET-10 | 10 min |
| L-4 | DNS exfiltration when internet disabled | NET-11 | 15 min |
| L-5 | No auth on relay vsock channel | VM-04 | Accept |
| L-6 | CID reuse window after VM destroy | VM-07 | Accept |
| L-7 | MCP bridge token on disk in guest config.jsonc | VM-11 | 5 min |
| L-8 | Guest HTTP inbox no rate limit | VM-13 | Resolved by C-2 |
| L-9 | Daemon task env vars not validated against blocklist | VM-14 | 5 min |
| L-10 | encode_message doesn't check MAX_MESSAGE_SIZE | VM-17 | 5 min |
| L-11 | Config file could contain plaintext secrets | AUTH-11 | 10 min |
| L-12 | Force-adopt 60s threshold hardcoded | AUTH-12 | 5 min |
| L-13 | Rate limiting per-session not global | AUTH-13 | Accept |
| L-14 | Duplicate shell_escape in 3 files | INP-L1 | 15 min |
| L-15 | TOML plan parsing without size pre-check | INP-L2 | 5 min |
| L-16 | Workspace name echoed in error messages (info leak) | F-10 | 5 min |

---

## Positive Findings (Things Done Well)

1. **VM boundary is solid** — No host escape vectors. vsock-only communication eliminates network-based attacks.
2. **Input validation comprehensive** — All 31 MCP tools have proper validation. `shell_escape()` + `Command::arg()` used consistently.
3. **Path traversal blocked** — Canonicalization + containment checks in vault, file transfer, host paths.
4. **API keys never on disk** — SetEnv via vsock, never logged, never persisted to state file.
5. **HMP tag sanitization correct** — Prevents QEMU monitor injection.
6. **kill_on_drop on QEMU** — Prevents orphaned processes on daemon crash.
7. **vsock semaphore limits** — 64 concurrent connections per listener, correct lifetime management.
8. **Forward chain default-drop** — Correct nftables posture for VM isolation.
9. **Per-interface IP forwarding** — Scoped to br-agentiso, not global.
10. **React JSX escaping** — No dangerouslySetInnerHTML, Tiptap html:false. XSS well-mitigated.
11. **Workspace name validation** — Strict 1-128 char alphanumeric + hyphen/underscore/dot.
12. **Force-adopt staleness guards** — Proper ordering (quota check before owner removal).
13. **RAII name locks** — Prevent concurrent duplicate workspace creation.
14. **State file 0600 permissions** — No secrets in state file.
15. **Bridge tokens not persisted** — Correct security behavior on restart.

---

## Recommended Fix Priority

### Wave 1: Critical (do now, <1 hour total)
1. **C-1**: Replace `CorsLayer::permissive()` with restrictive CORS
2. **C-2**: Bind guest HTTP API to `127.0.0.1`

### Wave 2: High — Dashboard Auth (before any network exposure)
3. **H-1**: Require admin_token when bind_addr != localhost
4. **H-2**: Add security headers middleware
5. **H-3**: WebSocket auth via query parameter
6. **H-5**: Constant-time token comparison

### Wave 3: High — Network Hardening
7. **H-6**: Anti-spoofing rules per TAP device
8. **H-7**: Scope DNAT to bridge/localhost
9. **M-1**: Input chain policy drop
10. **M-2**: Inter-VM isolation fix (explicit DROP for isolated VMs)

### Wave 4: High — Resource Protection
11. **H-8**: cgroup_required config option
12. **H-9**: Remove guest oom_score_adj=-1000, manage from host
13. **H-10**: Recursive VM counting for nested teams
14. **H-11**: LimitNPROC in systemd + config validation

### Wave 5: Medium Quick Wins (<5 min each)
15. **M-6**: Reduce bridge token log to 4 chars
16. **M-9**: Apply env blocklist to per-request exec env
17. **M-12**: Reduce MAX_MESSAGE_SIZE to 4 MiB
18. **M-14**: Disable xterm.js allowProposedApi
19. **M-16**: Fix UTF-8 truncation in git_diff
20. **M-17**: Add regex pattern length limit (1024 chars)
21. **M-18**: Validate role names in team creation
22. **M-20**: Lock root account in base image

### Wave 6: Medium Larger Items
23. **M-3**: ARP protections on bridge
24. **M-5**: Bridge session tool whitelist
25. **M-7**: Expand git credential redaction
26. **M-10**: Bounded reads for exec output
27. **M-19**: ZFS snapshot limits + pool free-space monitoring
28. **M-21**: Global rate limiter for create operations
29. **M-22**: Host-side vsock connection semaphore + LimitNOFILE=65536

---

## Cross-Cutting Themes

1. **Dashboard was built for localhost, now exposed on 0.0.0.0** — Most dashboard findings stem from this mismatch. The auth layer exists but isn't enforced or wired in the frontend.

2. **Best-effort resource limits** — cgroups, rate limits, and VM counts all have soft enforcement. When any layer fails, the fallback is "no limit at all" rather than "fail closed."

3. **Per-session vs global** — Rate limiting, token buckets, and session IDs are all per-session. Multiple concurrent sessions multiply attack capacity.

4. **Guest HTTP API is the weakest link inside VMs** — The A2A daemon trusts messages from the HTTP inbox without authentication, making cross-VM message injection the highest-impact guest-side attack.

5. **nftables input chain too permissive** — The forward chain is correctly default-drop, but the input chain is default-accept, allowing VMs to reach any host service.
