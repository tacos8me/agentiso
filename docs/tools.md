# MCP Tool Reference

agentiso exposes 43 MCP tools over stdio transport. All tools that operate on a workspace accept `workspace_id` as either a UUID or a human-readable workspace name.

## Workspace Lifecycle

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `workspace_create` | Create and start a new isolated workspace VM. Returns workspace ID and connection details. | _(none)_ | `name`, `base_image`, `vcpus`, `memory_mb`, `disk_gb`, `allow_internet` |
| `workspace_destroy` | Stop and permanently destroy a workspace VM and all its storage. | `workspace_id` | _(none)_ |
| `workspace_start` | Boot a stopped workspace VM. | `workspace_id` | _(none)_ |
| `workspace_stop` | Gracefully stop a running workspace VM. The workspace can be started again later. | `workspace_id` | _(none)_ |
| `workspace_list` | List all workspaces visible to this session. Owned workspaces show `"owned": true`; orphaned ones show `"owned": false`. | _(none)_ | `state_filter` |
| `workspace_info` | Get detailed information about a workspace including snapshots, network config, and disk usage. | `workspace_id` | _(none)_ |
| `workspace_ip` | Get the IP address of a workspace VM. | `workspace_id` | _(none)_ |
| `workspace_logs` | Retrieve QEMU console output and/or stderr logs for debugging boot or runtime issues. | `workspace_id` | `log_type`, `max_lines` |

## Execution

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `exec` | Execute a shell command inside a running workspace VM. Returns stdout, stderr, and exit code. | `workspace_id`, `command` | `timeout_secs`, `workdir`, `env`, `max_output_bytes` |
| `exec_background` | Start a shell command in the background inside a workspace VM. Returns a `job_id` for polling. | `workspace_id`, `command` | `workdir`, `env` |
| `exec_poll` | Poll a background job. Returns whether the job is still running, its exit code (if finished), and stdout/stderr. | `workspace_id`, `job_id` | _(none)_ |
| `exec_kill` | Kill a background job by sending it a signal. | `workspace_id`, `job_id` | `signal` |
| `set_env` | Set persistent environment variables inside a workspace VM. Applied to all subsequent `exec` and `exec_background` calls. Per-command env vars override these. | `workspace_id`, `vars` | _(none)_ |

## File Operations

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `file_write` | Write a file inside a running workspace VM. | `workspace_id`, `path`, `content` | `mode` |
| `file_read` | Read a file from inside a running workspace VM. Returns text content; rejects binary files. | `workspace_id`, `path` | `offset`, `limit` |
| `file_edit` | Edit a file by replacing an exact string match. The first occurrence of `old_string` is replaced with `new_string`. | `workspace_id`, `path`, `old_string`, `new_string` | _(none)_ |
| `file_list` | List files and directories at a given path inside a workspace VM. | `workspace_id`, `path` | _(none)_ |
| `file_upload` | Upload a file from the host filesystem into a workspace VM. The `host_path` must be within the configured transfer directory. | `workspace_id`, `host_path`, `guest_path` | _(none)_ |
| `file_download` | Download a file from a workspace VM to the host filesystem. The `host_path` must be within the configured transfer directory. | `workspace_id`, `host_path`, `guest_path` | _(none)_ |

## Snapshots & Forking

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `snapshot_create` | Create a named snapshot (checkpoint) of a workspace's disk state. Optionally includes VM memory state. | `workspace_id`, `name` | `include_memory` |
| `snapshot_restore` | Restore a workspace to a previously created snapshot. The workspace is stopped and restarted. Newer snapshots are removed. | `workspace_id`, `snapshot_name` | _(none)_ |
| `snapshot_list` | List all snapshots for a workspace, showing the snapshot tree with parent relationships. | `workspace_id` | _(none)_ |
| `snapshot_delete` | Delete a named snapshot from a workspace. | `workspace_id`, `snapshot_name` | _(none)_ |
| `workspace_fork` | Fork (clone) a new independent workspace from an existing workspace's snapshot. | `workspace_id`, `snapshot_name` | `new_name` |

## Networking

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `port_forward` | Forward a host port to a guest port in a workspace VM. Returns the assigned host port. Host ports below 1024 are rejected. | `workspace_id`, `guest_port` | `host_port` |
| `port_forward_remove` | Remove a port forwarding rule from a workspace. | `workspace_id`, `guest_port` | _(none)_ |
| `network_policy` | Set the network isolation policy for a workspace: internet access, inter-VM communication, and allowed inbound ports. | `workspace_id` | `allow_internet`, `allow_inter_vm`, `allowed_ports` |

## Session Management

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `workspace_adopt` | Adopt an orphaned workspace into the current session. Use after a server restart to reclaim ownership. | `workspace_id` | _(none)_ |
| `workspace_adopt_all` | Adopt all orphaned workspaces into the current session. Purges stale sessions first. | _(none)_ | _(none)_ |

## Helper Tools

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `git_clone` | Clone a git repository into a running workspace VM. Returns the clone path and HEAD commit SHA. | `workspace_id`, `url` | `path`, `branch`, `depth` |

## Orchestration

Tools for preparing golden images and batch-forking parallel worker VMs.

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `workspace_prepare` | Create a "golden" workspace ready for mass forking. Optionally clones a git repo and runs setup commands, then creates a snapshot named "golden". | `name` | `base_image`, `git_url`, `setup_commands` |
| `workspace_batch_fork` | Fork N worker VMs from a workspace snapshot in parallel. Best-effort: successful workers are returned alongside any errors. Count must be 1-20. | `workspace_id`, `count` | `snapshot_name`, `name_prefix` |

## Vault

Obsidian-style markdown knowledge base tools. These require `[vault]` to be enabled in `config.toml` and do not require a workspace.

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `vault_read` | Read a note from the vault by path. Returns content and parsed YAML frontmatter. | `path` | `format` |
| `vault_write` | Create or update a note in the vault. Supports overwrite, append, and prepend modes. | `path`, `content` | `mode` |
| `vault_search` | Search vault notes for a query string or regex pattern. Returns matching lines with context. | `query` | `regex`, `path_prefix`, `tag`, `max_results` |
| `vault_list` | List notes and directories in the vault. | _(none)_ | `path`, `recursive` |
| `vault_delete` | Delete a note from the vault. Requires `confirm=true` to prevent accidental deletion. | `path`, `confirm` | _(none)_ |
| `vault_frontmatter` | Get, set, or delete YAML frontmatter keys on a vault note. | `path`, `action` | `key`, `value` |
| `vault_tags` | List, add, or remove tags on a vault note. | `path`, `action` | `tag` |
| `vault_replace` | Search and replace text within a vault note. Returns the number of replacements made. | `path`, `search`, `replace` | `regex` |
| `vault_move` | Move or rename a note within the vault. Creates parent directories if needed. Path traversal protection on both paths. | `path`, `new_path` | `overwrite` |
| `vault_batch_read` | Read multiple notes in a single call. Returns array of results with per-file error handling. Partial failures don't abort. | `paths` (max 10) | `include_content`, `include_frontmatter` |
| `vault_stats` | Get vault overview: total notes, folders, size in bytes, and recently modified files sorted by mtime. | _(none)_ | `recent_count` |
