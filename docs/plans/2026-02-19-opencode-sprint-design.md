# OpenCode Integration Sprint — Design

**Date**: 2026-02-19
**Status**: In Progress
**Depends on**: P0-P1 sprint (complete), subnet fix (complete)

## Goal

Enable agentiso to orchestrate parallel AI coding agents by running OpenCode
inside VMs. Workers fork from golden snapshots, execute tasks via `opencode run`,
and report results back through vsock.

## Architecture

```
agentiso orchestrate "task description"
  ├── Planner (host-side or dedicated VM)
  │   ├── Decompose task into N subtasks
  │   └── For each subtask:
  │       ├── Fork worker VM from golden snapshot (instant ZFS CoW)
  │       ├── Inject API key via SetEnv guest RPC
  │       ├── opencode run "subtask prompt" --format json
  │       ├── Collect result via vsock
  │       └── Destroy worker VM
  └── Merge/review results
```

## Sprint Tasks

### P2 Remaining (from prior sprint)

| # | Task | Owner | Description |
|---|------|-------|-------------|
| 1 | Prometheus metrics | mcp-server | `/metrics` endpoint: workspace count, boot latency histogram, exec count, error rate |
| 2 | Pre-baked toolchain images | guest-agent | `alpine-rust`, `alpine-python`, `alpine-node` image build scripts |

### OpenCode Integration (new)

| # | Task | Owner | Description |
|---|------|-------|-------------|
| 3 | SetEnv guest agent RPC | guest-agent | New `SetEnv` protocol variant: set environment variables in guest (for API key injection) |
| 4 | Alpine-opencode base image | guest-agent | Image build script: Alpine + opencode musl binary + git + common dev tools |
| 5 | Golden snapshot workflow | workspace-core | `workspace_prepare` MCP tool: create workspace, clone repo, install deps, snapshot as "golden" |
| 6 | Batch fork + dispatch | workspace-core | `workspace_batch_fork` MCP tool: fork N workers from snapshot, return workspace IDs |
| 7 | OpenCode run wrapper | vm-engine | Execute `opencode run "prompt" --format json` in VM via guest agent exec, parse JSON output |
| 8 | Orchestrate CLI command | workspace-core | `agentiso orchestrate` CLI: read task file, fork workers, dispatch, collect, report |
| 9 | Integration tests | storage-net | E2e tests: golden snapshot → fork → opencode run → collect → destroy |

## Key Design Decisions

### `opencode run` over `opencode serve`
- Stateless, clean lifecycle per invocation
- No SQLite concurrency issues
- No memory leak from long-lived server
- Better error boundaries (process exit = task done)

### One VM per worker
- Full filesystem isolation (ZFS CoW clone)
- No shared state between workers
- Clean environment per task
- Workers never communicate with each other

### SetEnv via vsock (not disk)
- API keys never written to ZFS zvol (survives snapshot)
- Injected at runtime into process environment
- Cleared on VM destroy

### Golden snapshot pattern
- Clone repo once, install deps once
- ZFS snapshot captures ready-to-work state
- Fork is instant (~50ms) via ZFS clone
- Workers start with warm environment

## OpenCode Binary Details

- **Version**: v1.2.6
- **Binary**: `opencode-linux-x64-baseline-musl`
- **Source**: `https://github.com/anomalyco/opencode/releases`
- **Size**: ~50MB compressed
- **Runtime deps**: git (for repo operations)
- **Config**: `~/.config/opencode/config.jsonc` (provider, model, MCP servers)

## Scaling Model

- 10-20 concurrent VMs per API key (rate limits are the bottleneck)
- Each VM: 1-2 vCPUs, 512MB-1GB RAM, 2GB disk
- Fork latency: ~50ms (ZFS clone) + ~500ms (VM boot)
- Task latency: dominated by LLM API call time (30s-300s)

## File Ownership

| Agent | New files |
|-------|-----------|
| guest-agent | `protocol/src/lib.rs` (SetEnv), `images/build-alpine-opencode.sh` |
| vm-engine | `agentiso/src/vm/opencode.rs` |
| workspace-core | `agentiso/src/workspace/orchestrate.rs`, CLI subcommand |
| mcp-server | `workspace_prepare`, `workspace_batch_fork` tool definitions |
| storage-net | `scripts/test-opencode-integration.sh` |
