# Agent Workflow Guide

How to use agentiso effectively as an AI agent. This guide covers the most common patterns, with concrete tool call sequences you can follow.

## The 8 Tools That Cover 90% of Use Cases

You have 31 tools available. In practice, these 8 handle almost everything:

| Tool | What it does |
|------|-------------|
| `workspace_create` | Spin up a fresh isolated VM |
| `exec` | Run a shell command and get stdout/stderr/exit_code |
| `file_write` | Write a file into the VM (full content replace) |
| `file_read` | Read a file from the VM |
| `file_list` | List directory contents |
| `snapshot(action="create")` | Save a named checkpoint |
| `snapshot(action="restore")` | Roll back to a checkpoint |
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

- By default, workspaces do **not** have internet access (`default_allow_internet = false`, secure by default). To create a sandbox with internet, pass `allow_internet: true` at creation time:

```json
{
  "tool": "workspace_create",
  "arguments": {
    "name": "sandbox-with-net",
    "allow_internet": true
  }
}
```

  Or enable it later via `network_policy` (guest DNS is automatically reconfigured via vsock, so DNS resolution works immediately):

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
  "tool": "snapshot",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "create",
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
  "tool": "snapshot",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "restore",
    "name": "after-npm-install"
  }
}
```

The workspace disk is now exactly as it was after Step 2. The VM restarts with a clean slate.

**Step 5: Try a different approach**

Write new code, test again. Snapshot again when it works:

```json
{
  "tool": "snapshot",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "create",
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
  "tool": "snapshot",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "create",
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
- Fork from a snapshot, not from live state. Always `snapshot(action="create")` first, then `workspace_fork` from that snapshot.
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
  "tool": "snapshot",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "create",
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
    "action": "start",
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
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "poll",
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

### Killing a background job

```json
{
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "kill",
    "job_id": 1
  }
}
```

### When to use background execution

| Scenario | Use |
|----------|-----|
| Command finishes in < 120s | `exec` (synchronous, simpler) |
| Long build (> 120s) | `exec_background(action="start")` + `exec_background(action="poll")` |
| Running a dev server | `exec_background(action="start")` (it never finishes, poll to check logs) |
| Parallel builds in one workspace | Multiple `exec_background(action="start")` calls, poll each |

### Workflow: build in background, edit in foreground

```
1. exec_background(action="start"): "cargo build --release"  -> job_id: 1
2. file_edit: fix a typo in README.md                        (no waiting)
3. file_write: add a new test file                           (no waiting)
4. exec_background(action="poll"): job_id 1                  -> check if build finished
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
    "action": "add",
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
    "action": "add",
    "guest_port": 8080,
    "host_port": 9090
  }
}
```

Remove it when done:

```json
{
  "tool": "port_forward",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "remove",
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
  "tool": "file_transfer",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "direction": "upload",
    "host_path": "/var/lib/agentiso/transfers/dataset.csv",
    "guest_path": "/app/data/dataset.csv"
  }
}
```

**Download VM file to host:**

```json
{
  "tool": "file_transfer",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "direction": "download",
    "host_path": "/var/lib/agentiso/transfers/results.json",
    "guest_path": "/app/output/results.json"
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
  "tool": "git_status",
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

**Step 3: Review the diff**

```json
{
  "tool": "git_diff",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/workspace/repo"
  }
}
```

**Step 4: Commit changes**

```json
{
  "tool": "git_commit",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/workspace/repo",
    "message": "Update title",
    "add_all": true
  }
}
```

**Step 5: Push to remote**

```json
{
  "tool": "git_push",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "path": "/workspace/repo"
  }
}
```

### Key points

- Use `git_status` to get structured status instead of parsing `git status` output.
- Use `git_diff` to review changes before committing (`staged: true` for staged changes).
- Use `git_commit` with `add_all: true` to stage and commit in one step, or stage specific files first via `exec`.
- Use `git_push` with `set_upstream: true` for new branches.
- The `dirty` field from `git_status` is a quick check for whether there are any uncommitted changes.
- For private repos, inject credentials via `set_env` (e.g., `GIT_ASKPASS` or a token in the URL).

---

## Pattern 7: Snapshot Size Monitoring

**Use case:** Track disk usage across snapshots to manage storage.

### Sequence

**Step 1: List snapshots with size info**

```json
{
  "tool": "snapshot",
  "arguments": {
    "workspace_id": "a1b2c3d4-...",
    "action": "list"
  }
}
```

Response includes per-snapshot metadata:

```json
{
  "snapshots": [
    {
      "name": "after-install",
      "created_at": "2026-02-19T...",
      "has_memory": false,
      "parent": null
    }
  ]
}
```

**Planned (not yet implemented):** `used_bytes` and `referenced_bytes` fields will be added in a future release to show per-snapshot disk usage.

### Key points

- Use `snapshot(action="list")` to see all snapshots for a workspace.
- Delete old snapshots you no longer need with `snapshot(action="delete")` to reclaim disk space.
- Snapshots with dependent forks cannot be deleted -- destroy the forks first.

---

## Pattern 8: Multi-Agent Team

**Use case:** Coordinate multiple agents working on different parts of a problem, each in its own isolated workspace VM. Team members can communicate with each other via their VMs' network.

### Sequence

**Step 1: Create a team with named roles**

```json
{
  "tool": "team",
  "arguments": {
    "action": "create",
    "name": "review-team",
    "roles": [
      {"name": "coder", "role": "developer", "skills": ["rust", "python"]},
      {"name": "reviewer", "role": "code_review", "description": "Reviews PRs and runs tests"},
      {"name": "researcher", "role": "research", "skills": ["web_search"]}
    ]
  }
}
```

Each role gets its own workspace VM. Intra-team nftables rules allow team members to communicate with each other while remaining isolated from other workspaces. Agent cards are written to the vault at `teams/{name}/cards/{member}.json`.

**Step 2: Check team status**

```json
{
  "tool": "team",
  "arguments": {
    "action": "status",
    "name": "review-team"
  }
}
```

Returns member details including workspace IDs, IPs, workspace state, and agent status.

**Step 3: Work in team member workspaces**

Use `exec`, `file_write`, `git_clone`, etc. with each member's `workspace_id`. Each member workspace is a full independent VM.

**Step 4: Coordinate via vault-backed task board**

Use the `vault` tool to create task files with YAML frontmatter for the team's task board:

```json
{
  "tool": "vault",
  "arguments": {
    "action": "write",
    "path": "teams/review-team/tasks/task-001.md",
    "content": "---\nstatus: pending\npriority: high\nassigned_to: coder\ndepends_on: []\n---\n# Implement feature X\n\nWrite the implementation for feature X."
  }
}
```

Search for available tasks:

```json
{
  "tool": "vault",
  "arguments": {
    "action": "search",
    "query": "status: pending",
    "path_prefix": "teams/review-team/tasks"
  }
}
```

Update task status as work progresses:

```json
{
  "tool": "vault",
  "arguments": {
    "action": "frontmatter",
    "path": "teams/review-team/tasks/task-001.md",
    "frontmatter_action": "set",
    "key": "status",
    "value": "completed"
  }
}
```

**Step 5: Tear down the team when done**

```json
{
  "tool": "team",
  "arguments": {
    "action": "destroy",
    "name": "review-team"
  }
}
```

All member workspace VMs are destroyed in parallel, nftables rules are cleaned up, and team state is removed.

### Key points

- Each team member gets a full workspace VM. The team tool handles provisioning all members at once.
- Team members can reach each other by IP (shown in `team status`), while non-team workspaces remain isolated.
- Use the vault for team coordination: task boards, shared notes, and agent cards.
- The `team list` action shows all teams with member counts and creation timestamps.
- Destroy teams when done to free all member VM resources.

---

## Pattern 9: Parallel Coding Swarm

**Use case:** Distribute a large task across N worker VMs that all start from the same golden snapshot. Each worker implements its piece independently and in parallel. After all workers finish, merge their changes into a single workspace. This is the highest-throughput pattern for parallelizable coding work.

### Sequence

**Step 1: Prepare a golden image with the repo and dependencies**

```json
{
  "tool": "workspace_prepare",
  "arguments": {
    "name": "golden-myproject",
    "git_url": "https://github.com/org/myproject.git",
    "setup_commands": [
      "cd /workspace && npm install",
      "cd /workspace && npm run build"
    ]
  }
}
```

Response includes `workspace_id` and `snapshot_name` (the auto-created snapshot). Save both.

**Step 2: Fork 5 workers from the golden snapshot**

```json
{
  "tool": "workspace_fork",
  "arguments": {
    "workspace_id": "golden-myproject",
    "snapshot_name": "prepared",
    "count": 5,
    "name_prefix": "worker"
  }
}
```

Response returns a list of `{workspace_id, name}` for worker-1 through worker-5. Save all IDs.

**Step 3: Inject API keys into each worker**

Repeat for each worker:

```json
{
  "tool": "set_env",
  "arguments": {
    "workspace_id": "worker-1",
    "vars": {
      "ANTHROPIC_API_KEY": "sk-ant-...",
      "OPENAI_API_KEY": "sk-..."
    }
  }
}
```

Keys are injected via vsock and never written to disk.

**Step 4: Start background tasks on each worker**

Repeat for each worker with its specific task:

```json
{
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "worker-1",
    "action": "start",
    "command": "cd /workspace && git checkout -b feat/auth && opencode run 'implement JWT auth in src/auth.ts, add tests' 2>&1 | tee /tmp/task.log"
  }
}
```

Save the `job_id` returned for each worker.

**Step 5: Poll all workers for completion**

Poll each worker periodically. All 5 can be polled in parallel:

```json
{
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "worker-1",
    "action": "poll",
    "job_id": 1
  }
}
```

When `running: false` and `exit_code: 0`, that worker is done. If `exit_code` is non-zero, check stdout/stderr for errors and decide whether to retry or skip.

**Step 6: Merge all workers into a target workspace**

```json
{
  "tool": "workspace_merge",
  "arguments": {
    "source_workspaces": ["worker-1", "worker-2", "worker-3", "worker-4", "worker-5"],
    "target_workspace": "golden-myproject",
    "strategy": "branch-per-source",
    "path": "/workspace",
    "commit_message": "Merge parallel swarm results"
  }
}
```

Response shows per-source success/failure and commit counts. If conflicts occur, the merge response reports them; resolve manually via `exec` in the target workspace.

**Step 7: Destroy workers**

```json
{
  "tool": "workspace_destroy",
  "arguments": { "workspace_id": "worker-1" }
}
```

Repeat for all 5 workers. Keep the golden workspace if you need it for future swarm runs.

### Watch out for

- **Rate limits.** Creating 5 workspaces rapidly may hit the default 5/min create limit. Increase `create_per_minute` in config or use `workspace_fork` with `count` (batch fork is a single rate-limited call).
- **Memory pressure.** Each worker VM uses its configured memory (default 512 MiB). 5 workers = 2.5 GiB minimum. Monitor host memory.
- **Merge conflicts.** Workers that touch the same files will conflict. Assign non-overlapping file scopes to each worker when possible. Use `branch-per-source` strategy to get one branch per worker so you can resolve conflicts incrementally.
- **Exec timeouts.** The default exec timeout is 120s. For long-running tasks, always use `exec_background` and poll.

---

## Pattern 10: Team-Based Code Review

**Use case:** One agent writes code while two reviewers independently review different modules. The team tool sets up isolated VMs with intra-team networking and a message relay for coordination.

### Sequence

**Step 1: Create a team with author and reviewer roles**

```json
{
  "tool": "team",
  "arguments": {
    "action": "create",
    "name": "review-squad",
    "roles": [
      {"name": "author", "role": "developer", "skills": ["rust", "typescript"]},
      {"name": "reviewer-1", "role": "code_review", "description": "Reviews backend modules"},
      {"name": "reviewer-2", "role": "code_review", "description": "Reviews frontend modules"}
    ]
  }
}
```

Response includes `workspace_id` for each member. Save them.

**Step 2: Clone the repo in the author's workspace**

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "review-squad.author",
    "command": "cd /workspace && git clone https://github.com/org/repo.git && cd repo && git checkout -b feat/new-feature"
  }
}
```

**Step 3: Author writes code, then notifies reviewers**

After the author finishes writing code and commits:

```json
{
  "tool": "team",
  "arguments": {
    "action": "message",
    "name": "review-squad",
    "agent": "author",
    "to": "reviewer-1",
    "content": "Ready for review. Focus on src/api/ and src/db/. Run: git diff main..HEAD -- src/api/ src/db/"
  }
}
```

```json
{
  "tool": "team",
  "arguments": {
    "action": "message",
    "name": "review-squad",
    "agent": "author",
    "to": "reviewer-2",
    "content": "Ready for review. Focus on src/ui/ and src/components/. Run: git diff main..HEAD -- src/ui/ src/components/"
  }
}
```

**Step 4: Reviewers pick up messages and review**

Each reviewer receives their message:

```json
{
  "tool": "team",
  "arguments": {
    "action": "receive",
    "name": "review-squad",
    "agent": "reviewer-1"
  }
}
```

Then runs the review in their own workspace (the repo is cloned from the author's workspace via git over the intra-team network, or by using `workspace_merge` to pull changes):

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "review-squad.reviewer-1",
    "command": "cd /workspace/repo && git diff main..HEAD -- src/api/ src/db/"
  }
}
```

**Step 5: Reviewers send feedback back**

```json
{
  "tool": "team",
  "arguments": {
    "action": "message",
    "name": "review-squad",
    "agent": "reviewer-1",
    "to": "author",
    "content": "LGTM on src/api/. Issue in src/db/pool.rs:42 — connection pool not closed on error path. Suggest adding defer/drop guard."
  }
}
```

**Step 6: Tear down when done**

```json
{
  "tool": "team",
  "arguments": {
    "action": "destroy",
    "name": "review-squad"
  }
}
```

### Watch out for

- **Repo sharing.** Each team member has its own VM. Clone the repo separately in each reviewer's workspace, or use `workspace_merge` to push the author's branch to reviewers. Team VMs can communicate over the network (IPs shown in `team status`).
- **Message polling.** The `receive` action returns pending messages and clears them. Poll periodically if the reviewer is waiting for work.
- **Team memory.** Each team member is a full VM. A 3-member team uses 3x the memory of a single workspace.

---

## Pattern 11: Fork-and-Merge Feature Development

**Use case:** Develop 3 independent features in parallel from the same codebase, then merge them all into a single workspace. Each feature branch is isolated so developers cannot interfere with each other.

### Sequence

**Step 1: Prepare the golden image**

```json
{
  "tool": "workspace_prepare",
  "arguments": {
    "name": "golden-app",
    "git_url": "https://github.com/org/app.git",
    "setup_commands": ["cd /workspace && npm install"]
  }
}
```

**Step 2: Fork 3 feature workers**

```json
{
  "tool": "workspace_fork",
  "arguments": {
    "workspace_id": "golden-app",
    "snapshot_name": "prepared",
    "new_name": "feat-auth"
  }
}
```

```json
{
  "tool": "workspace_fork",
  "arguments": {
    "workspace_id": "golden-app",
    "snapshot_name": "prepared",
    "new_name": "feat-payments"
  }
}
```

```json
{
  "tool": "workspace_fork",
  "arguments": {
    "workspace_id": "golden-app",
    "snapshot_name": "prepared",
    "new_name": "feat-notifications"
  }
}
```

**Step 3: Each worker creates a feature branch and implements its feature**

For each worker, create a branch and write code:

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "feat-auth",
    "command": "cd /workspace && git checkout -b feat/auth"
  }
}
```

```json
{
  "tool": "file_write",
  "arguments": {
    "workspace_id": "feat-auth",
    "path": "/workspace/src/auth.ts",
    "content": "export function authenticate(token: string): boolean {\n  // JWT verification logic\n  return verifyJWT(token);\n}\n"
  }
}
```

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "feat-auth",
    "command": "cd /workspace && git add -A && git commit -m 'feat: add JWT authentication'",
    "timeout_secs": 30
  }
}
```

Repeat the write-test-commit cycle for feat-payments and feat-notifications.

**Step 4: Run tests in each worker to validate**

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "feat-auth",
    "command": "cd /workspace && npm test",
    "timeout_secs": 120
  }
}
```

Check `exit_code: 0` on each before proceeding to merge.

**Step 5: Merge all features into the golden workspace using branch-per-source**

```json
{
  "tool": "workspace_merge",
  "arguments": {
    "source_workspaces": ["feat-auth", "feat-payments", "feat-notifications"],
    "target_workspace": "golden-app",
    "strategy": "branch-per-source",
    "path": "/workspace",
    "commit_message": "Merge feature branches"
  }
}
```

The response reports per-source results:

```json
{
  "results": [
    {"source_id": "feat-auth", "success": true, "commits_applied": 3},
    {"source_id": "feat-payments", "success": true, "commits_applied": 2},
    {"source_id": "feat-notifications", "success": false, "error": "merge conflict in src/index.ts", "commits_applied": 0}
  ]
}
```

**Step 6: Handle merge conflicts**

If a source fails, resolve conflicts manually in the target workspace:

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "golden-app",
    "command": "cd /workspace && git diff --name-only --diff-filter=U"
  }
}
```

Read the conflicting file, edit it, then complete the merge:

```json
{
  "tool": "exec",
  "arguments": {
    "workspace_id": "golden-app",
    "command": "cd /workspace && git add src/index.ts && git commit -m 'resolve: merge conflict in index.ts'"
  }
}
```

**Step 7: Clean up workers**

Destroy all 3 feature workspaces once the merge is complete.

### Watch out for

- **Non-overlapping files.** The cleanest parallel development assigns each worker to different files/directories. If workers modify the same files, expect merge conflicts.
- **Strategy choice.** `branch-per-source` gives you one branch per worker in the target, which makes conflict resolution easier because you can see which source caused the conflict. `sequential` applies patches in order (first source wins on conflicts). `cherry-pick` applies individual commits and stops at the first conflict.
- **Test after merge.** Always run the full test suite in the target workspace after merging to catch integration issues.
- **Snapshot before merge.** Create a snapshot of the target workspace before merging so you can roll back if the merge goes badly.

---

## Pattern 12: Research Swarm with Comparison

**Use case:** Explore multiple approaches to the same problem in parallel. Each worker implements a different solution. After all workers finish, compare their results and pick the best approach.

### Sequence

**Step 1: Prepare the golden image with the project**

```json
{
  "tool": "workspace_prepare",
  "arguments": {
    "name": "golden-cache-research",
    "git_url": "https://github.com/org/app.git",
    "setup_commands": [
      "cd /workspace && pip install -r requirements.txt",
      "cd /workspace && pip install redis pytest-benchmark"
    ]
  }
}
```

**Step 2: Fork 3 research workers**

```json
{
  "tool": "workspace_fork",
  "arguments": {
    "workspace_id": "golden-cache-research",
    "snapshot_name": "prepared",
    "count": 3,
    "name_prefix": "approach"
  }
}
```

Returns approach-1, approach-2, approach-3 with their workspace IDs.

**Step 3: Assign each worker a different approach**

Worker 1 -- Redis-based caching:

```json
{
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "approach-1",
    "action": "start",
    "command": "cd /workspace && git checkout -b approach/redis && python implement_cache.py --strategy=redis && python benchmark.py --output=/tmp/results.json 2>&1"
  }
}
```

Worker 2 -- In-memory LRU cache:

```json
{
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "approach-2",
    "action": "start",
    "command": "cd /workspace && git checkout -b approach/lru && python implement_cache.py --strategy=lru && python benchmark.py --output=/tmp/results.json 2>&1"
  }
}
```

Worker 3 -- File-based cache with SQLite:

```json
{
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "approach-3",
    "action": "start",
    "command": "cd /workspace && git checkout -b approach/sqlite && python implement_cache.py --strategy=sqlite && python benchmark.py --output=/tmp/results.json 2>&1"
  }
}
```

**Step 4: Poll until all workers complete**

Poll each in parallel:

```json
{
  "tool": "exec_background",
  "arguments": {
    "workspace_id": "approach-1",
    "action": "poll",
    "job_id": 1
  }
}
```

Wait until all 3 have `running: false`.

**Step 5: Read results from each worker**

```json
{
  "tool": "file_read",
  "arguments": {
    "workspace_id": "approach-1",
    "path": "/tmp/results.json"
  }
}
```

```json
{
  "tool": "file_read",
  "arguments": {
    "workspace_id": "approach-2",
    "path": "/tmp/results.json"
  }
}
```

```json
{
  "tool": "file_read",
  "arguments": {
    "workspace_id": "approach-3",
    "path": "/tmp/results.json"
  }
}
```

Compare the benchmark results (latency, throughput, memory usage) across all 3 approaches.

**Step 6: Keep the winning approach, merge it into the golden workspace**

Suppose approach-2 (LRU) wins:

```json
{
  "tool": "workspace_merge",
  "arguments": {
    "source_workspaces": ["approach-2"],
    "target_workspace": "golden-cache-research",
    "strategy": "sequential",
    "path": "/workspace"
  }
}
```

**Step 7: Destroy all research workers**

```json
{
  "tool": "workspace_destroy",
  "arguments": { "workspace_id": "approach-1" }
}
```

```json
{
  "tool": "workspace_destroy",
  "arguments": { "workspace_id": "approach-2" }
}
```

```json
{
  "tool": "workspace_destroy",
  "arguments": { "workspace_id": "approach-3" }
}
```

### Watch out for

- **Standardized output.** All workers should produce results in the same format (e.g., a JSON file at a known path) so you can compare them programmatically. Define the output contract before forking.
- **Resource contention.** If approaches have different resource needs (e.g., Redis requires a running server), account for that in the setup. Use `exec_background` to start services before the main task.
- **No merge needed for losers.** Only merge the winning approach. Destroy the others to reclaim resources.
- **Snapshot the target first.** Before merging the winner, snapshot the target so you can roll back if the merge introduces unexpected issues.

---

## Pattern 13: Daemon Task Execution

**Use case:** Delegate shell commands to team member VMs asynchronously. The guest daemon inside each VM picks up task assignments, executes them with concurrency control, and stores results for later collection. This is the preferred pattern for fire-and-forget task delegation within teams.

### Sequence

**Step 1: Create a team**

```json
{
  "tool": "team",
  "arguments": {
    "action": "create",
    "name": "build-team",
    "roles": [
      {"name": "coordinator", "role": "orchestrator"},
      {"name": "worker-1", "role": "builder", "skills": ["rust"]},
      {"name": "worker-2", "role": "builder", "skills": ["python"]}
    ]
  }
}
```

**Step 2: Send task assignments to workers**

Send a `task_assignment` message to each worker. The `content` field is a JSON string with the `TaskAssignmentPayload` schema.

```json
{
  "tool": "team",
  "arguments": {
    "action": "message",
    "name": "build-team",
    "agent": "coordinator",
    "to": "worker-1",
    "content": "{\"task_id\":\"rust-tests\",\"command\":\"cd /workspace && cargo test --release 2>&1\",\"workdir\":\"/workspace\",\"env\":{\"RUST_BACKTRACE\":\"1\"},\"timeout_secs\":120}",
    "message_type": "task_assignment"
  }
}
```

```json
{
  "tool": "team",
  "arguments": {
    "action": "message",
    "name": "build-team",
    "agent": "coordinator",
    "to": "worker-2",
    "content": "{\"task_id\":\"python-tests\",\"command\":\"cd /workspace && pytest -v 2>&1\",\"timeout_secs\":60}",
    "message_type": "task_assignment"
  }
}
```

You can send up to 4 tasks per worker concurrently -- additional tasks queue until a slot frees up.

**Step 3: Wait briefly, then receive results**

The guest daemon polls its inbox every 2 seconds, so there is a small delay before tasks start executing. Poll each worker's results via `receive`:

```json
{
  "tool": "team",
  "arguments": {
    "action": "receive",
    "name": "build-team",
    "agent": "worker-1"
  }
}
```

Response when a task has completed:

```json
{
  "messages": [],
  "count": 0,
  "daemon_results": [
    {
      "task_id": "rust-tests",
      "success": true,
      "exit_code": 0,
      "stdout": "test result: ok. 42 passed; 0 failed\n",
      "stderr": "",
      "elapsed_secs": 35,
      "source_message_id": "msg-abc-123"
    }
  ],
  "daemon_pending_tasks": 0
}
```

If `daemon_pending_tasks` is greater than 0, tasks are still running -- poll again after a delay.

If the `daemon_results` array is empty and `daemon_pending_tasks` is 0, no tasks have been assigned or all results have already been collected.

**Step 4: Tear down the team**

```json
{
  "tool": "team",
  "arguments": {
    "action": "destroy",
    "name": "build-team"
  }
}
```

### Key points

- **Task assignments bypass the regular message inbox.** Messages with `message_type: "task_assignment"` are consumed exclusively by the guest daemon. They never appear in the `messages` array of a `receive` response.
- **Concurrency is semaphore-gated.** Each agent VM runs up to 4 tasks concurrently. If you send more than 4, the extras queue and execute as slots free up.
- **Daemon polls every 2 seconds.** There is a short delay between sending a task and execution starting. Do not poll for results immediately after sending.
- **Results are one-shot.** Once `daemon_results` are returned by `receive`, they are removed from the daemon's outbox. Poll once and save the results.
- **Use unique `task_id` values.** The `task_id` in the `TaskAssignmentPayload` is returned verbatim in the `DaemonTaskResult`, allowing you to correlate results with assignments.
- **Output is truncated.** Both stdout and stderr are UTF-8 safe truncated to prevent excessive memory use.
- **Default timeout is 300 seconds.** Override with `timeout_secs` in the payload. Tasks that exceed the timeout are killed.

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

### Execution (4 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `exec` | `workspace_id`, `command` | `timeout_secs`, `workdir`, `env`, `max_output_bytes` |
| `exec_background` | `workspace_id`, `action` | `command`, `job_id`, `workdir`, `env`, `signal` (varies by action) |
| `set_env` | `workspace_id`, `vars` | -- |
| `exec_parallel` | `workspace_ids`, `commands` | `timeout_secs`, `workdir`, `env`, `max_output_bytes` |

### File Operations (5 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `file_write` | `workspace_id`, `path`, `content` | `mode` |
| `file_read` | `workspace_id`, `path` | `offset`, `limit` |
| `file_edit` | `workspace_id`, `path`, `old_string`, `new_string` | -- |
| `file_list` | `workspace_id`, `path` | -- |
| `file_transfer` | `workspace_id`, `direction`, `host_path`, `guest_path` | -- |

### Snapshots & Forking (2 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `snapshot` | `workspace_id`, `action` | `name`, `include_memory` (varies by action) |
| `workspace_fork` | `workspace_id`, `snapshot_name` | `new_name`, `count`, `name_prefix` |

### Git (6 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `git_clone` | `workspace_id`, `url` | `path`, `branch`, `depth` |
| `git_status` | `workspace_id` | `path` |
| `git_commit` | `workspace_id`, `path`, `message` | `add_all`, `author_name`, `author_email` |
| `git_push` | `workspace_id`, `path` | `remote`, `branch`, `force`, `set_upstream`, `timeout_secs` |
| `git_diff` | `workspace_id`, `path` | `staged`, `stat`, `file_path`, `max_bytes` |
| `workspace_merge` | `source_workspaces`, `target_workspace`, `strategy` | `path`, `commit_message` |

### Networking (2 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `port_forward` | `workspace_id`, `action`, `guest_port` | `host_port` |
| `network_policy` | `workspace_id` | `allow_internet`, `allow_inter_vm`, `allowed_ports` |

### Session (1 tool)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `workspace_adopt` | -- | `workspace_id` |

### Orchestration (2 tools)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `workspace_prepare` | `name` | `base_image`, `git_url`, `setup_commands` |
| `swarm_run` | `golden_workspace`, `snapshot_name`, `tasks` | `env_vars`, `merge_strategy`, `merge_target`, `max_parallel`, `timeout_secs`, `cleanup` |

### Diagnostics (1 tool)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `workspace_logs` | `workspace_id` | `log_type`, `max_lines` |

### Vault (1 tool)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `vault` | `action` | varies by action (read/write/search/list/delete/frontmatter/tags/replace/move/batch_read/stats) |

### Teams (1 tool)

| Tool | Required params | Optional params |
|------|----------------|-----------------|
| `team` | `action` | varies by action (create/destroy/status/list/message/receive) |

---

## Common Mistakes

**Forgetting to enable internet when needed.**
New workspaces have internet disabled by default (secure by default). If your workflow requires internet access (e.g., `git clone`, `apk add`, `npm install`), pass `allow_internet: true` to `workspace_create`, or call `network_policy` with `allow_internet: true` after creation. When toggling via `network_policy`, guest DNS is automatically reconfigured via vsock so DNS resolution works immediately.

**Not destroying workspaces.** Each running workspace consumes RAM and CPU. Destroy workspaces when you are done. If you need them later, stop them instead (`workspace_stop` frees memory but keeps the disk).

**Using `exec` for commands that take > 30 seconds.** The default timeout is 120s (increased from 30s). Use `exec_background` for commands expected to run longer.

**Hitting rate limits during batch operations.** Rate limiting is enabled by default (create: 5/min, exec: 60/min, default: 120/min). If your workflow creates many workspaces or runs many exec calls in quick succession, you may hit the limit. Increase the relevant `*_per_minute` value in `[rate_limit]` config, or set `enabled = false` to disable rate limiting entirely. See [Configuration Reference](docs/configuration.md#rate_limit).

**Forking from live state.** Always `snapshot(action="create")` first, then `workspace_fork` from that snapshot. You cannot fork without a snapshot.

**Not snapshotting before risky operations.** If you are about to run `rm -rf`, `DROP TABLE`, or any destructive command, call `snapshot(action="create")` first. Restoring with `snapshot(action="restore")` takes under a second.

**Using `file_write` when `file_edit` would be better.** If you only need to change one line, `file_edit` avoids rewriting the entire file and reduces the chance of losing content you did not read.

**Not reading a file before editing it.** Always `file_read` first so you know the exact string to match in `old_string`. Guessing leads to "string not found" errors.
