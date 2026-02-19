# Vault Integration — Phase 1 Design

**Date**: 2026-02-19
**Status**: In Progress
**Depends on**: OpenCode integration sprint (complete)

## Goal

Add native Obsidian vault tools to agentiso so the orchestrator can search,
read, and write a markdown knowledge base. Enrich worker prompts with vault
context before dispatching. No external dependencies (no Node.js, no Obsidian
desktop).

## Architecture

```
Host Claude (orchestrator)
  │
  ├── vault_search "auth patterns"    ─→ ripgrep-style search on /mnt/vault/
  ├── vault_read "conventions/rust.md" ─→ reads file, parses frontmatter
  │
  ├── orchestrate --task-file tasks.toml
  │     │
  │     ├── Resolves vault_context queries per task
  │     ├── Appends vault notes to each worker prompt
  │     │
  │     ├── Worker 1: "Implement auth. CONTEXT: <vault notes>"
  │     └── Worker 2: "Add tests. CONTEXT: <vault notes>"
  │
  └── Post-task: collect worker results, write to vault/output/
```

The vault is a directory of markdown files on the host filesystem. agentiso
reads/writes them directly using `tokio::fs` and the `ignore` crate for
directory walking. No separate process, no IPC.

## New MCP Tools (8 tools)

| Tool | Description | Params |
|------|-------------|--------|
| `vault_read` | Read a note by path | `path`, `format` (markdown/json) |
| `vault_search` | Full-text/regex search | `query`, `regex`, `path_prefix`, `tag`, `max_results` |
| `vault_list` | List notes in a directory | `path`, `recursive` |
| `vault_write` | Create/update a note | `path`, `content`, `mode` (overwrite/append/prepend) |
| `vault_frontmatter` | Get/set/delete YAML frontmatter | `path`, `action`, `key`, `value` |
| `vault_tags` | Add/remove/list tags | `path`, `action`, `tag` |
| `vault_replace` | Search-and-replace in a note | `path`, `search`, `replace`, `regex` |
| `vault_delete` | Delete a note | `path`, `confirm` |

All tools require vault to be configured (`[vault]` section in config.toml).
Path traversal prevented: all paths resolved relative to vault root, `..` rejected.

## Configuration

```toml
[vault]
# Enable vault tools. Default: false.
enabled = false

# Path to the vault root directory on the host filesystem.
path = "/mnt/vault"

# File extensions to include in search/list (default: ["md"])
extensions = ["md"]

# Directories to exclude from search/list
exclude_dirs = [".obsidian", ".trash", ".git"]
```

Added to `Config` struct in `config.rs` as `vault: VaultConfig`.

## Orchestration Enhancement

The `TaskDef` in `orchestrate.rs` gains an optional `vault_context` field:

```toml
[[tasks]]
name = "implement-auth"
prompt = "Implement user authentication"

[[tasks.vault_context]]
query = "auth"
kind = "search"    # full-text search, inject matching note excerpts

[[tasks.vault_context]]
query = "conventions/rust-style.md"
kind = "read"      # read entire note, inject as context
```

The orchestrator resolves these queries before dispatching each worker,
appending results to the prompt as a `## Project Knowledge Base` section.

## Implementation Modules

| File | Description | Owner |
|------|-------------|-------|
| `agentiso/src/mcp/vault.rs` | Vault module: VaultManager, search, read, frontmatter | vault-tools agent |
| `agentiso/src/mcp/tools.rs` | 8 new `#[tool]` handlers | vault-tools agent |
| `agentiso/src/config.rs` | `VaultConfig` struct | config agent |
| `config.example.toml` | Vault config section | config agent |
| `agentiso/src/workspace/orchestrate.rs` | `vault_context` in TaskDef + resolution | orchestrate agent |
| `agentiso/Cargo.toml` | Add `ignore`, `serde_yaml` deps | config agent |

## Dependencies (Rust crates)

- `ignore` — ripgrep's directory walker with gitignore-style exclusion
- `serde_yaml` — YAML frontmatter parsing
- `regex` — already a transitive dependency

No new system dependencies. No new processes. Single binary.

## Security

- Path traversal: all paths resolved with `canonicalize()`, must stay within vault root
- `.obsidian/` and `.trash/` excluded by default
- Write tools create parent directories as needed (no arbitrary mkdir)
- No shell invocation — pure Rust filesystem ops

## Testing

- Unit tests for: frontmatter parsing, tag extraction, path validation, search
- Config validation test: vault path must exist if enabled
- Integration: vault_read/write/search/list round-trip tests
