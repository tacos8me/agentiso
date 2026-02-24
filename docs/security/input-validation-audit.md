# Input Validation, Command Injection, Path Traversal & Deserialization Audit

**Date**: 2026-02-24
**Auditor**: sec-input-validation (security-review team)
**Scope**: All host-side and guest-side input handling across 11 source files

---

## Executive Summary

The agentiso codebase has a **strong overall validation posture**. All shell-outs use either `Command::new().arg()` (not shell-interpreted) or apply `shell_escape()` consistently. Path traversal prevention uses canonicalization. Type-safe representations are used for IPs (Ipv4Addr), UUIDs, and numeric parameters. However, three medium-severity issues were identified that should be addressed.

**Finding Counts**: 3 Medium, 2 Low, 10 Informational (positive findings)

---

## Medium Severity

### M1. UTF-8 Unsafe Truncation in git_diff

**File**: `agentiso/src/mcp/git_tools.rs:779`
**Category**: UTF-8 edge case

**Description**: The `git_diff` handler truncates output using Rust string byte-slicing (`&diff_output[..max_bytes]`) without checking if the slice lands on a UTF-8 character boundary. If `max_bytes` falls in the middle of a multi-byte UTF-8 sequence, this will panic at runtime.

**Code**:
```rust
// git_tools.rs:778-779
let diff_text = if truncated {
    format!("{}[TRUNCATED]", &diff_output[..max_bytes])
```

**Contrast**: The `truncate_output()` function in `tools.rs:4128` correctly handles this:
```rust
fn truncate_output(s: String, limit: usize) -> String {
    if s.len() <= limit { return s; }
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    format!("{}... [truncated]", &s[..end])
}
```

**Exploit Scenario**: A repository containing filenames or content with multi-byte UTF-8 characters (e.g., CJK, emoji) where `max_bytes` lands mid-character would cause the MCP server to panic, resulting in denial-of-service.

**Recommendation**: Use `truncate_output()` or apply the same `is_char_boundary()` check:

```rust
let diff_text = if truncated {
    let mut end = max_bytes;
    while end > 0 && !diff_output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[TRUNCATED]", &diff_output[..end])
} else {
    diff_output.to_string()
};
```

---

### M2. Regex Denial-of-Service (ReDoS) in Vault Search

**File**: `agentiso/src/mcp/vault.rs:417` and `vault.rs:757`
**Category**: Regex DoS

**Description**: The vault `search()` and `search_replace()` functions compile user-provided regex patterns using `Regex::new(query)` without any complexity limits. Crafted regex patterns with catastrophic backtracking (e.g., `(a+)+b`) can cause exponential CPU time, blocking the tokio runtime.

**Code**:
```rust
// vault.rs:416-417
let compiled_regex = if is_regex {
    Some(Regex::new(query).context("invalid regex pattern")?)
```

```rust
// vault.rs:757
let re = Regex::new(search).context("invalid regex pattern")?;
```

**Exploit Scenario**: An MCP client sends `vault(action="search", query="(a+)+$", is_regex=true)` with a target file containing `"aaaaaaaaaaaaaaaaaaaaaa"`. The regex engine would spend exponential time attempting to match, blocking the async runtime.

**Mitigating Factors**:
- The `regex` crate in Rust uses a finite automaton engine by default, which is immune to catastrophic backtracking. It only falls back to a backtracking engine if the pattern uses features that require it (e.g., backreferences, which `regex` does not support).
- The Rust `regex` crate has a built-in complexity limit (10MB DFA state).

**Revised Assessment**: Given the Rust `regex` crate's NFA/DFA-based engine, this is **lower risk than initially assessed**. The crate does not support backreferences, so patterns like `(a+)+b` are compiled to efficient automata. However, very large patterns or patterns with many alternations can still cause high compilation time.

**Recommendation**: Add a size limit on regex pattern length and set a compilation timeout:

```rust
if query.len() > 1024 {
    bail!("regex pattern too long (max 1024 chars)");
}
let compiled_regex = if is_regex {
    Some(Regex::new(query).context("invalid regex pattern")?)
} else {
    None
};
```

---

### M3. Role Name Not Validated Before Use in Workspace Names and Vault Paths

**File**: `agentiso/src/team/mod.rs:213`, `agentiso/src/team/mod.rs:276`
**Category**: Input validation gap

**Description**: When creating a team, role names from the `RoleDef` struct are used without explicit validation in two contexts:
1. Workspace name construction: `format!("{}-{}-{}", name, role.name, suffix)` (line 213)
2. Vault card path: `format!("cards/{}.json", role.name)` (line 276)

While the workspace name will be rejected by `is_valid_workspace_name()` if the role name contains invalid characters (failing the team creation), the error message will be confusing. The vault path goes through `resolve_path()` which blocks `..` but allows other special characters.

**Mitigating Factors**:
- Workspace creation at line 213 will fail if role name has invalid chars (indirect validation).
- Vault's `resolve_path()` blocks path traversal via `..` rejection.
- Team names ARE validated (alphanumeric + hyphens, line 69-86).

**Recommendation**: Add explicit role name validation matching team name rules:

```rust
for role in &roles {
    Self::validate_name(&role.name)
        .context(format!("invalid role name '{}'", role.name))?;
}
```

---

## Low Severity

### L1. Duplicate shell_escape Implementations

**Files**: `agentiso/src/mcp/tools.rs:4209`, `agentiso/src/dashboard/web/workspaces.rs`, `agentiso/src/init.rs`
**Category**: Code duplication / maintenance risk

**Description**: The `shell_escape()` function is duplicated in three files with identical logic. If one copy is updated to fix a bug, the others may be missed.

**Recommendation**: Move `shell_escape()` to a shared utility module and re-export:

```rust
// agentiso/src/util.rs
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
```

---

### L2. TOML Deserialization Without Size Limits in Orchestration

**File**: `agentiso/src/workspace/orchestrate.rs`
**Category**: Deserialization safety

**Description**: Orchestration plan files are parsed via `toml::from_str()` from file contents without pre-checking file size. A maliciously large TOML file could cause excessive memory allocation during parsing.

**Mitigating Factors**:
- Orchestration plans are local files, not user-uploaded content from MCP.
- The `toml` crate does not support recursive structures that could cause stack overflow.

**Recommendation**: Add a file size check before parsing:

```rust
let content = std::fs::read_to_string(plan_path)?;
if content.len() > 10 * 1024 * 1024 {
    bail!("plan file too large (max 10 MiB)");
}
let plan: OrchestrationPlan = toml::from_str(&content)?;
```

---

## Informational (Positive Findings)

### I1. Shell Command Injection Prevention (PASS)

All shell-outs across the codebase are properly protected:

| File | Method | Protection |
|------|--------|------------|
| `tools.rs` | `Command::new("sh").arg("-c").arg(cmd)` | `shell_escape()` on all user inputs |
| `git_tools.rs` | `Command::new("sh").arg("-c").arg(cmd)` | `shell_escape()` on URL, branch, path, message |
| `init.rs` | `Command::new("zfs")`, `Command::new("ip")`, etc. | `.arg()` (not shell-interpreted) |
| `nftables.rs` | `Command::new("nft").arg("-f").arg("-")` with stdin pipe | Rules piped via stdin, not args |
| `dashboard/web/workspaces.rs` | `Command::new("sh").arg("-c").arg(cmd)` | `shell_escape()` on user inputs |

### I2. Path Traversal Prevention (PASS)

| File | Function | Method |
|------|----------|--------|
| `tools.rs:4159` | `validate_host_path_in_dir()` | Canonicalize + `starts_with()` containment check |
| `vault.rs` | `resolve_path()` | Component check rejects `..` before filesystem access |
| `guest-agent/src/main.rs` | File read/write handlers | Operate within `/workspace` mount point |

### I3. Workspace Name Validation (PASS)

**File**: `agentiso/src/workspace/mod.rs`

```rust
fn is_valid_workspace_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128
    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}
```

Used consistently at workspace creation and fork entry points. Prevents injection via workspace names into ZFS dataset paths, TAP device names, and filesystem paths.

### I4. Git URL Validation (PASS)

**File**: `agentiso/src/mcp/git_tools.rs`

```rust
pub(crate) fn validate_git_url(url: &str) -> Result<(), McpError> {
    // Allows: https://, http://, git://, ssh://, SCP-style (user@host:path)
    // Rejects: ; | & $ ` ' " \ \n \r
}
```

Blocks all shell metacharacters. SCP-style URLs are allowed but sanitized.

### I5. HMP/QMP Tag Sanitization (PASS)

**File**: `agentiso/src/vm/qemu.rs`

```rust
fn validate_hmp_tag(tag: &str) -> Result<()> {
    // Alphanumeric + hyphen + underscore + dot only, max 128 chars
}
```

Prevents injection into QEMU human monitor protocol command strings.

### I6. Guest Agent Environment Variable Security (PASS)

**File**: `guest-agent/src/main.rs`

- Env var names: alphanumeric + underscore, no leading digit
- Blocklist: PATH, IFS, ENV, BASH_ENV, LD_* prefix
- Values: no validation needed (handled by kernel)

### I7. Snapshot and Base Image Name Validation (PASS)

**File**: `agentiso/src/mcp/tools.rs`

- `validate_snapshot_name()`: alphanumeric + hyphen + underscore + dot
- `validate_base_image()`: alphanumeric + hyphen + underscore + dot, no `..`, max 128 chars

Both prevent ZFS dataset name injection.

### I8. Network Type Safety (PASS)

**File**: `agentiso/src/network/nftables.rs`

- Guest IPs are `Ipv4Addr` type (parsed, not raw strings)
- Workspace IDs in TAP names are first 8 chars of UUID (hex only)
- Bridge IP is a constant `10.99.0.1`
- Team member IPs in nftables rules are Ipv4Addr-derived strings (safe)

### I9. Credential Redaction (PASS)

**File**: `agentiso/src/mcp/git_tools.rs`

```rust
fn redact_credentials(s: &str) -> String {
    // Strips GitHub/GitLab personal access tokens from output
}
```

Applied to all git command output before returning via MCP.

### I10. Size Limits and Rate Limiting (PASS)

| Resource | Limit | File |
|----------|-------|------|
| Guest file read | 32 MiB | `guest-agent/src/main.rs` |
| Exec output | 2 MiB | `guest-agent/src/main.rs` |
| Vault write | 10 MiB | `vault.rs` |
| shared_context | 1 MiB | `tools.rs:3054` |
| Message content | 256 KiB | `team/message_relay.rs` |
| Vsock message | MAX_MESSAGE_SIZE | `protocol/src/lib.rs` |
| Vsock connections | 64 concurrent | `guest-agent/src/main.rs` |
| Workspace create | 5/min | rate limiter |
| Exec calls | 60/min | rate limiter |
| Team messages | 300/min | rate limiter |

---

## Validation Function Index

| Function | File | Characters Allowed |
|----------|------|--------------------|
| `shell_escape()` | tools.rs, init.rs, web/workspaces.rs | Single-quote wrapping with `'\''` escape |
| `is_valid_workspace_name()` | workspace/mod.rs | `[a-zA-Z0-9_.-]`, 1-128 chars |
| `validate_name()` (team) | team/mod.rs | `[a-zA-Z0-9-]`, no leading/trailing `-`, 1-64 chars |
| `validate_git_url()` | git_tools.rs | Scheme whitelist + shell metachar rejection |
| `validate_base_image()` | tools.rs | `[a-zA-Z0-9_.-]`, no `..`, max 128 chars |
| `validate_snapshot_name()` | tools.rs | `[a-zA-Z0-9_.-]` |
| `validate_hmp_tag()` | vm/qemu.rs | `[a-zA-Z0-9_.-]`, max 128 chars |
| `validate_host_path_in_dir()` | tools.rs | Canonicalize + containment check |
| `resolve_path()` (vault) | vault.rs | Component-level `..` rejection |
| `is_valid_hostname()` (guest) | guest-agent main.rs | `[a-zA-Z0-9-]`, max 63 chars |
| `is_valid_env_name()` (guest) | guest-agent main.rs | `[a-zA-Z0-9_]`, no leading digit |
| `is_dangerous_env_name()` (guest) | guest-agent main.rs | Blocklist: PATH, IFS, ENV, BASH_ENV, LD_* |
| `parse_bare_ipv4()` (guest) | guest-agent main.rs | Standard IPv4 format via Rust parser |
| `truncate_output()` | tools.rs | UTF-8 safe via `is_char_boundary()` |

---

## Recommendations Summary

| ID | Severity | Description | Effort |
|----|----------|-------------|--------|
| M1 | Medium | Fix UTF-8 truncation in git_diff | 5 min |
| M2 | Medium | Add regex pattern length limit in vault search | 5 min |
| M3 | Medium | Validate role names in team creation | 5 min |
| L1 | Low | Deduplicate shell_escape to shared module | 15 min |
| L2 | Low | Add file size check before TOML plan parsing | 5 min |
