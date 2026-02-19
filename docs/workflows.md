# Agent Workflow Guide

How to use agentiso effectively as an AI agent. This guide covers the most common patterns, with concrete tool call sequences you can follow.

## The 8 Tools That Cover 90% of Use Cases

You have 45 tools available. In practice, these 8 handle almost everything:

| Tool | What it does |
|------|-------------|
| `workspace_create` | Spin up a fresh isolated VM |
| `exec` | Run a shell command and get stdout/stderr/exit_code |
| `file_write` | Write a file into the VM (full content replace) |
| `file_read` | Read a file from the VM |
| `file_list` | List directory contents |
| `snapshot_create` | Save a named checkpoint |
| `snapshot_restore` | Roll back to a checkpoint |
| `workspace_destroy` | Tear down the VM and free all resources |

Learn these first. Everything else is situational.

## When NOT to Use agentiso

Do not create a workspace for:

- **Read-only analysis** of code already on the host. Use your native file reading tools instead.
- **Quick string manipulation** or computation that doesn't need a shell.
- **One-shot questions** that don't require execution. An exec round-trip costs ~100ms; don't pay it for nothing.
- **Tasks that only read files the agent already has access to.** agentiso adds value through isolation, persistence, and snapshotting -- not as a file viewer.

Use agentiso when you need: isolation from the host, a persistent Linux environment, the ability to roll back, or parallel exploration.

---

## Pattern 1: Disposable Sandbox

**Use case:** Run untrusted or risky code safely. Install packages, run build scripts, execute user-provided code -- all without touching the host.

### Sequence

**Step 1: Create a workspace**

```json
{
  "tool": "workspace_create",
  "arguments": {
    "name": "sandbox-untrusted-script"
  }
}
```

Response includes `workspace_id` (a UUID). Save it -- you need it for every subsequent call.

**Step 2: Write the code into the VM**

```json
{
  "tool": "file_write",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/tmp/script.py",
    "content": "import os\nprint(os.listdir('/'))\n"
  }
}
```

**Step 3: Execute it**

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "command": "python3 /tmp/script.py",
    "timeout_secs": 10
  }
}
```

**Step 4: Destroy when done**

```json
{
  "tool": "workspace_destroy",
  "arguments": {
    "workspace_id": "a1b2c3d4-..."
  }
}
```

### Key points

- Default network policy blocks internet access. The sandbox cannot phone home.
- If you want to allow `pip install` or `apk add`, either pass `allow_internet: true` at creation time:

```json
{
  "tool": "workspace_create",
  "arguments": {
    "name": "sandbox-with-net",
    "allow_internet": true
  }
}
```

  Or enable it later via `network_policy`:

```json
{
  "tool": "network_policy",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "allow_internet": true
  }
}
```

- Always destroy sandboxes when done. They consume memory and disk until destroyed.
- With the warm pool enabled (default), workspace creation is sub-second.

---

## Pattern 2: Checkpoint/Rollback Dev Loop

**Use case:** Iterate on code changes with instant rollback. Try something, and if it breaks, restore to the last known-good state in under a second.

### Sequence

**Step 1: Create workspace and set up the project**

```json
{
  "tool": "workspace_create",
  "arguments": {
    "name": "dev-loop",
    "memory_mb": 1024
  }
}
```

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "command": "apk add nodejs npm && mkdir -p /app && cd /app && npm init -y && npm install express",
    "timeout_secs": 120
  }
}
```

**Step 2: Snapshot the clean state**

```json
{
  "tool": "snapshot_create",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "name": "after-npm-install"
  }
}
```

**Step 3: Make changes and test**

```json
{
  "tool": "file_write",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/app/index.js",
    "content": "const express = require('express');\nconst app = express();\napp.get('/', (req, res) => res.send('v1'));\napp.listen(3000);\n"
  }
}
```

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "command": "cd /app && node -e \"require('./index.js')\" 2>&1 || true",
    "timeout_secs": 5
  }
}
```

**Step 4: If it breaks, roll back instantly**

```json
{
  "tool": "snapshot_restore",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "snapshot_name": "after-npm-install"
  }
}
```

The workspace disk is now exactly as it was after Step 2. The VM restarts with a clean slate.

**Step 5: Try a different approach**

Write new code, test again. Snapshot again when it works:

```json
{
  "tool": "snapshot_create",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "name": "v2-working"
  }
}
```

### Snapshot naming

Use descriptive names that tell you what state the snapshot captures:

| Good | Bad |
|------|-----|
| `after-npm-install` | `snap1` |
| `before-db-migration` | `checkpoint` |
| `v2-working` | `test` |
| `clean-build` | `s3` |

Snapshot names allow: letters, digits, hyphens, underscores, dots. No spaces.

---

## Pattern 3: Parallel Hypothesis Testing via Fork

**Use case:** You have N possible approaches to a problem. Test them all simultaneously in independent copies of the same environment.

### Sequence

**Step 1: Set up the base environment and snapshot it**

```json
{
  "tool": "workspace_create",
  "arguments": { "name": "hypothesis-base" }
}
```

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "command": "apk add python3 py3-pip && pip install pytest",
    "timeout_secs": 60
  }
}
```

```json
{
  "tool": "file_write",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/app/problem.py",
    "content": "def solve(data):\n    pass  # TODO\n"
  }
}
```

```json
{
  "tool": "snapshot_create",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "name": "base-ready"
  }
}
```

**Step 2: Fork independent workspaces from that snapshot**

```json
{
  "tool": "workspace_fork",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "snapshot_name": "base-ready",
    "new_name": "approach-sorting"
  }
}
```

```json
{
  "tool": "workspace_fork",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "snapshot_name": "base-ready",
    "new_name": "approach-hashmap"
  }
}
```

```json
{
  "tool": "workspace_fork",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "snapshot_name": "base-ready",
    "new_name": "approach-dp"
  }
}
```

Each fork is a full independent VM with its own copy of the disk. ZFS clones make this near-instant and space-efficient (only divergent blocks are stored).

**Step 3: Implement and test each approach independently**

Write different implementations into each fork, run tests, compare results. The forks cannot interfere with each other.

**Step 4: Keep the winner, destroy the rest**

```json
{
  "tool": "workspace_destroy",
  "arguments": { "workspace_id": "<losing-fork-1>" }
}
```

```json
{
  "tool": "workspace_destroy",
  "arguments": { "workspace_id": "<losing-fork-2>" }
}
```

### Key points

- Forks start as running VMs. Each consumes memory. Destroy forks you are done with.
- Fork from a snapshot, not from live state. Always `snapshot_create` first, then `workspace_fork` from that snapshot.
- The source workspace and all forks are independent after forking. Changes in one do not affect the others.
- Each fork tracks its lineage. The `workspace_fork` response includes `forked_from` with the source workspace and snapshot name. Use `workspace_info` to see lineage later.
- You cannot delete a snapshot that has dependent forks. Destroy the forked workspaces first.

---

## Pattern 4: Persistent Build Environment

**Use case:** Heavy setup once (compile toolchain, install dependencies, clone repos), then use the workspace across multiple agent sessions. The workspace survives agent restarts.

### Sequence

**Step 1: Create and set up (first session)**

```json
{
  "tool": "workspace_create",
  "arguments": {
    "name": "rust-dev-env",
    "memory_mb": 2048,
    "disk_gb": 20
  }
}
```

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "command": "apk add build-base curl && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y",
    "timeout_secs": 300
  }
}
```

```json
{
  "tool": "snapshot_create",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "name": "toolchain-installed"
  }
}
```

Save the `workspace_id` string. You will need it to reconnect.

**Step 2: Reconnect in a later session**

In a new agent session, find existing workspaces:

```json
{
  "tool": "workspace_list",
  "arguments": {}
}
```

This returns all workspaces. Find the one named `rust-dev-env` and use its `workspace_id` for subsequent calls.

If the workspace was stopped:

```json
{
  "tool": "workspace_start",
  "arguments": {
    "workspace_id": "a1b2c3d4-..."
  }
}
```

**Step 3: Continue working**

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "command": "source $HOME/.cargo/env && cargo build --release",
    "workdir": "/app",
    "timeout_secs": 120
  }
}
```

### Key points

- Workspaces persist across agent session restarts. The agentiso server maintains state.
- Save the workspace_id somewhere you can retrieve it (e.g., mention it in your response to the user).
- Snapshot after expensive setup steps so you can restore if something goes wrong later.
- If you do not need the workspace anymore, destroy it to free resources.

---

## Pattern 5: File Editing Workflow

Two approaches depending on the size of the change.

### Surgical edit (change a specific string)

Use `file_edit` when you know exactly what to replace. It finds the first occurrence of `old_string` and replaces it with `new_string`.

**Step 1: Read the file to understand its contents**

```json
{
  "tool": "file_read",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/app/config.py"
  }
}
```

**Step 2: Apply the edit**

```json
{
  "tool": "file_edit",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/app/config.py",
    "old_string": "DEBUG = True",
    "new_string": "DEBUG = False"
  }
}
```

### Full file replace

Use `file_write` when creating a new file or rewriting most of a file. It replaces the entire file content.

```json
{
  "tool": "file_write",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/app/config.py",
    "content": "DEBUG = False\nDATABASE_URL = 'sqlite:///db.sqlite3'\nSECRET_KEY = 'dev-only'\n",
    "mode": "0644"
  }
}
```

### Exploring file structure first

When you don't know the layout, use `file_list` to navigate:

```json
{
  "tool": "file_list",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/app"
  }
}
```

Returns entries with `name`, `kind` (file/directory), `size`, `permissions`, and `modified` timestamp. Then drill into specific files with `file_read`.

### Decision guide

| Situation | Use |
|-----------|-----|
| Change one line or a specific string | `file_edit` |
| Create a new file | `file_write` |
| Rewrite most of a file | `file_write` |
| Don't know the file layout | `file_list` then `file_read` |
| Read a large file partially | `file_read` with `offset` and `limit` |

---

## Background Execution

For long-running commands (builds, test suites, servers), use `exec_background` so you can do other work while it runs.

### Starting a background job

```json
{
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "command": "cd /app && cargo build --release 2>&1",
    "workdir": "/app"
  }
}
```

Response:

```json
{
  "job_id": 1,
  "status": "started"
}
```

### Polling for completion

```json
{
  "tool": "exec_poll",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "job_id": 1
  }
}
```

Response while still running:

```json
{
  "job_id": 1,
  "running": true,
  "exit_code": null,
  "stdout": "   Compiling agentiso v0.1.0\n",
  "stderr": ""
}
```

Response when finished:

```json
{
  "job_id": 1,
  "running": false,
  "exit_code": 0,
  "stdout": "   Compiling agentiso v0.1.0\n    Finished release target(s)\n",
  "stderr": ""
}
```

### When to use background execution

| Scenario | Use |
|----------|-----|
| Command finishes in < 30s | `exec` (synchronous, simpler) |
| Long build (> 30s) | `exec_background` + `exec_poll` |
| Running a dev server | `exec_background` (it never finishes, poll to check logs) |
| Parallel builds in one workspace | Multiple `exec_background` calls, poll each |

### Workflow: build in background, edit in foreground

```
1. exec_background: "cargo build --release"      -> job_id: 1
2. file_edit: fix a typo in README.md             (no waiting)
3. file_write: add a new test file                (no waiting)
4. exec_poll: job_id 1                            -> check if build finished
5. If still running, do more work, poll again later
```

---

## Port Forwarding

When a workspace runs a web server or API, forward its port to the host:

```json
{
  "tool": "port_forward",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "guest_port": 8080
  }
}
```

Response includes the auto-assigned `host_port`. The service is now reachable at `localhost:<host_port>` from the host. You can also specify an explicit host port (must be >= 1024):

```json
{
  "tool": "port_forward",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "guest_port": 8080,
    "host_port": 9090
  }
}
```

Remove it when done:

```json
{
  "tool": "port_forward_remove",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "guest_port": 8080
  }
}
```

---

## File Transfer (Host <-> Guest)

Transfer files between the host filesystem and the workspace VM. Host paths must be within the configured transfer directory (default: `/var/lib/agentiso/transfers`).

**Upload host file into VM:**

```json
{
  "tool": "file_upload",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "host_path": "/var/lib/agentiso/transfers/dataset.csv",
    "guest_path": "/app/data/dataset.csv"
  }
}
```

**Download VM file to host:**

```json
{
  "tool": "file_download",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "guest_path": "/app/output/results.json",
    "host_path": "/var/lib/agentiso/transfers/results.json"
  }
}
```

---

## Pattern 6: Git Workflow

**Use case:** Clone a repository, make changes, check status, and commit -- all inside the workspace.

### Sequence

**Step 1: Create workspace with internet access and clone**

```json
{
  "tool": "workspace_create",
  "arguments": {
    "name": "git-work",
    "allow_internet": true
  }
}
```

```json
{
  "tool": "git_clone",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "url": "https://github.com/user/repo.git",
    "path": "/workspace/repo"
  }
}
```

**Step 2: Make changes and check status**

```json
{
  "tool": "file_edit",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/workspace/repo/README.md",
    "old_string": "# Old Title",
    "new_string": "# New Title"
  }
}
```

```json
{
  "tool": "workspace_git_status",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/workspace/repo"
  }
}
```

Response:

```json
{
  "branch": "main",
  "staged": [],
  "modified": ["README.md"],
  "untracked": [],
  "dirty": true
}
```

**Step 3: Commit and push**

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "command": "cd /workspace/repo && git add -A && git commit -m 'Update title' && git push",
    "timeout_secs": 30
  }
}
```

### Key points

- Use `workspace_git_status` to get structured status instead of parsing `git status` output.
- The `dirty` field is a quick check for whether there are any uncommitted changes.
- For private repos, inject credentials via `set_env` (e.g., `GIT_ASKPASS` or a token in the URL).

---

## Pattern 7: Snapshot Size Monitoring

**Use case:** Track disk usage across snapshots to manage storage.

### Sequence

**Step 1: List snapshots with size info**

```json
{
  "tool": "snapshot_list",
  "arguments": {
    "workspace_id": "a1b2c3d4-..."
  }
}
```

Response now includes per-snapshot disk usage:

```json
{
  "snapshots": [
    {
      "name": "after-install",
      "created_at": "2026-02-19T...",
      "used_bytes": 52428800,
      "referenced_bytes": 1073741824
    }
  ]
}
```

- `used_bytes`: space uniquely held by this snapshot (would be freed on delete)
- `referenced_bytes`: total data the snapshot refers to (shared + unique)

**Step 2: Compare snapshot to current state**

```json
{
  "tool": "snapshot_diff",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "snapshot_name": "after-install"
  }
}
```

This returns size-level diff information (block-level on zvols, not file-level).

### Key points

- Use `snapshot_list` to identify snapshots consuming the most space.
- Delete old snapshots you no longer need to reclaim `used_bytes`.
- Snapshots with dependent forks cannot be deleted -- destroy the forks first.

---

## Quick Reference

### Lifecycle (6 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `workspace_create` | -- | `name`, `base_image`, `vcpus`, `memory_mb`, `disk_gb`, `allow_internet` |
| `workspace_destroy` | `workspace_id` | -- |
| `workspace_start` | `workspace_id` | -- |
| `workspace_stop` | `workspace_id` | -- |
| `workspace_list` | -- | `state_filter` |
| `workspace_info` | `workspace_id` | -- |

### Execution (7 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `exec` | `workspace_id`, `command` | `timeout_secs`, `workdir`, `env`, `max_output_bytes` |
| `exec_background` | `workspace_id`, `command` | `workdir`, `env` |
| `exec_poll` | `workspace_id`, `job_id` | -- |
| `workspace_git_status` | `workspace_id` | `path` |
| `file_write` | `workspace_id`, `path`, `content` | `mode` |
| `file_read` | `workspace_id`, `path` | `offset`, `limit` |
| `file_edit` | `workspace_id`, `path`, `old_string`, `new_string` | -- |

### File Operations (3 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `file_list` | `workspace_id`, `path` | -- |
| `file_upload` | `workspace_id`, `host_path`, `guest_path` | -- |
| `file_download` | `workspace_id`, `host_path`, `guest_path` | -- |

### Snapshots (5 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `snapshot_create` | `workspace_id`, `name` | `include_memory` |
| `snapshot_restore` | `workspace_id`, `snapshot_name` | -- |
| `snapshot_list` | `workspace_id` | -- |
| `snapshot_delete` | `workspace_id`, `snapshot_name` | -- |
| `snapshot_diff` | `workspace_id`, `snapshot_name` | -- |

### Forking (1 tool)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `workspace_fork` | `workspace_id`, `snapshot_name` | `new_name` |

### Networking (4 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `port_forward` | `workspace_id`, `guest_port` | `host_port` |
| `port_forward_remove` | `workspace_id`, `guest_port` | -- |
| `workspace_ip` | `workspace_id` | -- |
| `network_policy` | `workspace_id` | `allow_internet`, `allow_inter_vm`, `allowed_ports` |

---

## Common Mistakes

**Forgetting to enable internet before `apk add` or `pip install`.**
New workspaces have internet disabled by default. Either pass `allow_internet: true` to `workspace_create`, or call `network_policy` with `allow_internet: true` after creation.

**Not destroying workspaces.** Each running workspace consumes RAM and CPU. Destroy workspaces when you are done. If you need them later, stop them instead (`workspace_stop` frees memory but keeps the disk).

**Using `exec` for commands that take > 30 seconds.** The default timeout is 30s. Either increase `timeout_secs` or use `exec_background` for long commands.

**Forking from live state.** Always `snapshot_create` first, then `workspace_fork` from that snapshot. You cannot fork without a snapshot.

**Not snapshotting before risky operations.** If you are about to run `rm -rf`, `DROP TABLE`, or any destructive command, snapshot first. Restoring a snapshot takes under a second.

**Using `file_write` when `file_edit` would be better.** If you only need to change one line, `file_edit` avoids rewriting the entire file and reduces the chance of losing content you did not read.

**Not reading a file before editing it.** Always `file_read` first so you know the exact string to match in `old_string`. Guessing leads to "string not found" errors.
