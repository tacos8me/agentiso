# MCP Tool Reference

agentiso exposes 31 MCP tools over stdio transport. All tools that operate on a workspace accept `workspace_id` as either a UUID or a human-readable workspace name.

**Rate limiting:** All tool calls are subject to token-bucket rate limiting (enabled by default). Tools are grouped into categories by cost: **create** (workspace_create, workspace_fork — 5/min), **exec** (exec, exec_background — 60/min), and **default** (all other tools — 120/min). See [Configuration Reference](configuration.md#rate_limit) to adjust or disable limits.

## Workspace Lifecycle

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `workspace_create` | Create and start a new isolated workspace VM. Returns workspace ID, connection details, `boot_time_ms`, and `from_pool` (whether a warm pool VM was used). | _(none)_ | `name`, `base_image`, `vcpus`, `memory_mb`, `disk_gb`, `allow_internet` |
| `workspace_destroy` | Stop and permanently destroy a workspace VM and all its storage. | `workspace_id` | _(none)_ |
| `workspace_start` | Boot a stopped workspace VM. | `workspace_id` | _(none)_ |
| `workspace_stop` | Gracefully stop a running workspace VM. The workspace can be started again later. | `workspace_id` | _(none)_ |
| `workspace_list` | List all workspaces visible to this session. Owned workspaces show `"owned": true`; orphaned ones show `"owned": false`. | _(none)_ | `state_filter` |
| `workspace_info` | Get detailed information about a workspace including snapshots, network config, IP address, and disk usage. | `workspace_id` | _(none)_ |

## Execution

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `exec` | Execute a shell command inside a running workspace VM. Returns stdout, stderr, and exit code. | `workspace_id`, `command` | `timeout_secs`, `workdir`, `env`, `max_output_bytes` |
| `exec_background` | Manage background jobs inside a workspace VM. Use the `action` parameter to start, poll, or kill a job. See sub-actions below. | `workspace_id`, `action` | _(varies by action)_ |
| `set_env` | Set persistent environment variables inside a workspace VM. Applied to all subsequent `exec` and `exec_background` calls. Per-command env vars override these. | `workspace_id`, `vars` | _(none)_ |
| `exec_parallel` | Execute commands across multiple workspaces concurrently. Takes a single command string (applied to all workspaces) or a per-workspace array. Returns a results array with per-workspace `exit_code`, `stdout`, `stderr`, and a summary. | `workspace_ids`, `commands` | `timeout_secs`, `workdir`, `env`, `max_output_bytes` |

### `exec_background` sub-actions

| Action | Description | Additional Required Params | Additional Optional Params |
|--------|-------------|---------------------------|---------------------------|
| `start` | Start a shell command in the background. Returns a `job_id` for polling. | `command` | `workdir`, `env` |
| `poll` | Poll a background job. Returns whether the job is still running, its exit code (if finished), and stdout/stderr. | `job_id` | _(none)_ |
| `kill` | Kill a background job by sending it a signal. | `job_id` | `signal` |

## File Operations

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `file_write` | Write a file inside a running workspace VM. | `workspace_id`, `path`, `content` | `mode` |
| `file_read` | Read a file from inside a running workspace VM. Returns text content; rejects binary files. | `workspace_id`, `path` | `offset`, `limit` |
| `file_edit` | Edit a file by replacing an exact string match. The first occurrence of `old_string` is replaced with `new_string`. | `workspace_id`, `path`, `old_string`, `new_string` | _(none)_ |
| `file_list` | List files and directories at a given path inside a workspace VM. | `workspace_id`, `path` | _(none)_ |
| `file_transfer` | Transfer a file between the host filesystem and a workspace VM. Use `direction` to select upload or download. The `host_path` must be within the configured transfer directory. | `workspace_id`, `direction`, `host_path`, `guest_path` | _(none)_ |

### `file_transfer` directions

| Direction | Description |
|-----------|-------------|
| `upload` | Upload a file from the host filesystem into a workspace VM. |
| `download` | Download a file from a workspace VM to the host filesystem. |

## Snapshots & Forking

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `snapshot` | Unified snapshot management tool. Use the `action` parameter to select the operation. See sub-actions below. | `workspace_id`, `action` | _(varies by action)_ |
| `workspace_fork` | Fork (clone) new independent workspace(s) from an existing workspace's snapshot. Response includes `forked_from` lineage (source workspace and snapshot name). When `count` > 1, forks N workers in parallel (best-effort, 1-20). | `workspace_id`, `snapshot_name` | `new_name`, `count`, `name_prefix` |

### `snapshot` sub-actions

| Action | Description | Additional Required Params | Additional Optional Params |
|--------|-------------|---------------------------|---------------------------|
| `create` | Create a named snapshot (checkpoint) of a workspace's disk state. Optionally includes VM memory state. | `name` | `include_memory` |
| `restore` | Restore a workspace to a previously created snapshot. The workspace is stopped and restarted. Newer snapshots are removed. | `name` | _(none)_ |
| `list` | List all snapshots for a workspace, showing the snapshot tree with parent relationships. | _(none)_ | _(none)_ |
| `delete` | Delete a named snapshot from a workspace. Checks for dependent clones before deleting. | `name` | _(none)_ |

## Networking

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `port_forward` | Manage port forwarding for a workspace VM. Use `action` to add or remove a forwarding rule. When adding, returns the assigned host port. Host ports below 1024 are rejected. | `workspace_id`, `action`, `guest_port` | `host_port` |
| `network_policy` | Set the network isolation policy for a workspace: internet access, inter-VM communication, and allowed inbound ports. Reconfigures guest DNS via vsock when toggling internet access. | `workspace_id` | `allow_internet`, `allow_inter_vm`, `allowed_ports` |

### `port_forward` actions

| Action | Description |
|--------|-------------|
| `add` | Forward a host port to a guest port. Returns the assigned host port. |
| `remove` | Remove an existing port forwarding rule for the given guest port. |

## Session Management

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `workspace_adopt` | Adopt orphaned workspace(s) into the current session. Use after a server restart to reclaim ownership. Omit `workspace_id` to adopt all orphaned workspaces (purges stale sessions first). | _(none)_ | `workspace_id` |

## Git

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `git_clone` | Clone a git repository into a running workspace VM. Returns the clone path and HEAD commit SHA. | `workspace_id`, `url` | `path`, `branch`, `depth` |
| `git_status` | Get structured git status for a repository in a workspace. Returns branch name, staged files, modified files, untracked files, and dirty flag. | `workspace_id` | `path` (default `/workspace`) |
| `git_commit` | Stage and commit changes in a workspace repository. | `workspace_id`, `path`, `message` | `add_all`, `author_name`, `author_email` |
| `git_push` | Push commits to a remote repository from a workspace. | `workspace_id`, `path` | `remote`, `branch`, `force`, `set_upstream`, `timeout_secs` |
| `git_diff` | Show uncommitted or staged changes in a workspace repository. | `workspace_id`, `path` | `staged`, `stat`, `file_path`, `max_bytes` |
| `workspace_merge` | Merge changes from one or more source workspaces into a target workspace using git format-patch/am. Supports three strategies: `sequential` (apply patches in order), `branch-per-source` (create a branch per source, then merge), and `cherry-pick` (cherry-pick individual commits). Returns per-source results with success/failure and commit counts. | `source_workspaces`, `target_workspace`, `strategy` | `path`, `commit_message` |

## Orchestration

Tools for preparing golden images ready for mass forking.

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `workspace_prepare` | Create a "golden" workspace ready for mass forking. Optionally clones a git repo and runs setup commands, then creates a snapshot named "golden". | `name` | `base_image`, `git_url`, `setup_commands` |
| `swarm_run` | End-to-end parallel orchestration: fork workers from a snapshot, inject env vars, execute commands, optionally merge results via git, and optionally clean up workers. Returns per-task results with exit codes and output. | `golden_workspace`, `snapshot_name`, `tasks` | `env_vars`, `merge_strategy`, `merge_target`, `max_parallel`, `timeout_secs`, `cleanup`, `allow_internet` |

### `swarm_run` parameters

| Field | Required | Description |
|-------|----------|-------------|
| `golden_workspace` | yes | Workspace ID or name of the golden image to fork from |
| `snapshot_name` | yes | Snapshot name to fork from |
| `tasks` | yes | Array of task objects, each with `name` (string), `command` (string), and optional `workdir` (string) |
| `env_vars` | no | Map of environment variables to inject into all workers |
| `merge_strategy` | no | Git merge strategy: `sequential`, `branch-per-source`, or `cherry-pick`. If omitted, no merge is performed. |
| `merge_target` | no | Workspace to merge results into (defaults to `golden_workspace`) |
| `max_parallel` | no | Maximum concurrent workers (default: 4, max: 20) |
| `timeout_secs` | no | Per-task execution timeout in seconds (default: 600) |
| `cleanup` | no | Whether to destroy worker workspaces after completion (default: `true`) |
| `allow_internet` | no | Enable internet access on forked workers (default: inherits from golden workspace) |

## Diagnostics

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `workspace_logs` | Retrieve QEMU console output and/or stderr logs for debugging boot or runtime issues. | `workspace_id` | `log_type`, `max_lines` |

## Vault

Obsidian-style markdown knowledge base tool. Requires `[vault]` to be enabled in `config.toml` and does not require a workspace.

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `vault` | Unified vault management tool. Use the `action` parameter to select the operation. See sub-actions below. | `action` | _(varies by action)_ |

### `vault` sub-actions

| Action | Description | Additional Required Params | Additional Optional Params |
|--------|-------------|---------------------------|---------------------------|
| `read` | Read a note from the vault by path. Returns content and parsed YAML frontmatter. | `path` | `format` |
| `write` | Create or update a note in the vault. Supports overwrite, append, and prepend modes. | `path`, `content` | `mode` |
| `search` | Search vault notes for a query string or regex pattern. Returns matching lines with context. | `query` | `regex`, `path_prefix`, `tag`, `max_results` |
| `list` | List notes and directories in the vault. | _(none)_ | `path`, `recursive` |
| `delete` | Delete a note from the vault. Requires `confirm=true` to prevent accidental deletion. | `path`, `confirm` | _(none)_ |
| `frontmatter` | Get, set, or delete YAML frontmatter keys on a vault note. | `path`, `frontmatter_action` | `key`, `value` |
| `tags` | List, add, or remove tags on a vault note. | `path`, `tags_action` | `tag` |
| `replace` | Search and replace text within a vault note. Returns the number of replacements made. | `path`, `old_string`, `new_string` | `regex` |
| `move` | Move or rename a note within the vault. Creates parent directories if needed. Path traversal protection on both paths. | `path`, `new_path` | `overwrite` |
| `batch_read` | Read multiple notes in a single call. Returns array of results with per-file error handling. Partial failures don't abort. | `paths` (max 10) | `include_content`, `include_frontmatter` |
| `stats` | Get vault overview: total notes, folders, size in bytes, and recently modified files sorted by mtime. | _(none)_ | `recent_count` |

## Teams

Multi-agent team lifecycle management. Each team member gets its own isolated workspace VM with intra-team nftables rules allowing communication between members.

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `team` | Unified team management tool. Use the `action` parameter to select the operation. See sub-actions below. | `action` | _(varies by action)_ |

### `team` sub-actions

| Action | Description | Additional Required Params | Additional Optional Params |
|--------|-------------|---------------------------|---------------------------|
| `create` | Create a new team with named roles. Each role gets its own workspace VM. Agent cards are written to the vault at `teams/{name}/cards/{member}.json`. Intra-team nftables rules allow member-to-member communication. When `parent_team` is set, creates a sub-team with budget inheritance and nesting depth enforcement. | `name`, `roles` | `max_vms`, `base_snapshot`, `parent_team` |
| `destroy` | Destroy a team: cascade-destroy any sub-teams, tear down all member workspace VMs in parallel, remove nftables rules, and clean up state. | `name` | _(none)_ |
| `status` | Get a team's current state, member details (IPs, workspace state, agent status), and creation timestamp. | `name` | _(none)_ |
| `list` | List all teams with their state, member count, max VMs, and creation timestamp. | _(none)_ | _(none)_ |
| `message` | Send a message from one agent to another within a team. Use `to: "*"` to broadcast to all team members except the sender. Rate limited (50 burst, 300/min). Content max 256 KiB. | `name`, `agent`, `to`, `content` | `message_type` |
| `receive` | Drain messages from an agent's inbox. Messages are removed after retrieval (pull model). | `name`, `agent` | `limit` (default 10) |

### Daemon task execution via `message_type: "task_assignment"`

The `message` action supports a special `message_type` called `task_assignment`. When a message with this type is sent, the guest daemon inside the target agent's VM picks it up, executes the command, and stores the result. Task assignment messages are **not** returned by `receive` -- instead, results appear as `daemon_results` in the receive response.

**Sending a task assignment:**

```json
{
  "tool": "team",
  "arguments": {
    "action": "message",
    "name": "my-team",
    "agent": "coordinator",
    "to": "worker-1",
    "content": "{\"task_id\":\"task-001\",\"command\":\"cd /workspace && cargo test\",\"workdir\":\"/workspace\",\"env\":{\"RUST_LOG\":\"debug\"},\"timeout_secs\":120}",
    "message_type": "task_assignment"
  }
}
```

The `content` field must be a JSON string conforming to the `TaskAssignmentPayload` schema:

| Field | Required | Type | Default | Description |
|-------|----------|------|---------|-------------|
| `task_id` | yes | string | -- | Unique identifier for this task (used to correlate results) |
| `command` | yes | string | -- | Shell command to execute (run via `sh -c`) |
| `workdir` | no | string | `null` | Working directory for execution |
| `env` | no | object | `{}` | Environment variables to set for the command |
| `timeout_secs` | no | integer | `300` | Maximum execution time in seconds |

**Receiving results:**

When calling `team(action="receive")`, the response includes daemon results alongside regular messages:

```json
{
  "messages": [],
  "count": 0,
  "daemon_results": [
    {
      "task_id": "task-001",
      "success": true,
      "exit_code": 0,
      "stdout": "test result: ok. 42 passed\n",
      "stderr": "",
      "elapsed_secs": 12,
      "source_message_id": "msg-abc-123"
    }
  ],
  "daemon_pending_tasks": 0
}
```

`DaemonTaskResult` schema:

| Field | Type | Description |
|-------|------|-------------|
| `task_id` | string | The `task_id` from the original `TaskAssignmentPayload` |
| `success` | boolean | Whether the command exited with code 0 |
| `exit_code` | integer | Process exit code |
| `stdout` | string | Standard output (UTF-8 safe truncated) |
| `stderr` | string | Standard error (UTF-8 safe truncated) |
| `elapsed_secs` | integer | Wall-clock execution time in seconds |
| `source_message_id` | string | The `message_id` of the original team message that carried this task |

The `daemon_pending_tasks` field in the receive response indicates how many tasks are still executing in the guest daemon. Use this to decide whether to poll again.

**Key behaviors:**

- The guest daemon polls its inbox every 2 seconds for new task assignments.
- Up to 4 tasks execute concurrently per agent (semaphore-gated).
- Task assignment messages are filtered out of regular `receive` results -- they never appear in the `messages` array.
- Results are collected via `PollDaemonResults` over the main vsock channel (port 5000).

### `roles` parameter format

Each role in the `roles` array is an object with:

| Field | Required | Description |
|-------|----------|-------------|
| `name` | yes | Member name (used as workspace name suffix: `{team}-{name}`) |
| `role` | yes | Role description (e.g. "coder", "tester", "researcher") |
| `skills` | no | Array of skill strings for this agent |
| `description` | no | Human-readable description of this member's purpose |
