# agentiso

QEMU microvm workspace manager for AI agents. Exposes isolated, snapshotable Linux VMs as MCP tools over stdio transport. AI agents create workspaces, execute commands, read and write files, snapshot state, and fork environments — all without Docker or SSH.

## Quick Start

```bash
# 1. Install prerequisites (Ubuntu/Debian)
sudo apt install qemu-system-x86_64 zfsutils-linux nftables musl-tools
rustup target add x86_64-unknown-linux-musl

# 2. Build host binary and guest agent
cargo build --release
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# 3. Set up environment (ZFS datasets, Alpine base image, kernel+initrd)
sudo ./scripts/setup-e2e.sh

# 4. Copy and edit configuration
cp config.example.toml /etc/agentiso/config.toml

# 5. Verify all prerequisites pass
./target/release/agentiso check --config config.toml
```

## MCP Integration

Add agentiso as an MCP server in your OpenCode configuration:

```json
{
  "mcpServers": {
    "agentiso": {
      "command": "/usr/local/bin/agentiso",
      "args": ["serve", "--config", "/etc/agentiso/config.toml"]
    }
  }
}
```

The server reads MCP protocol from stdin and writes to stdout. It is launched by the MCP client, not run directly. See [Configuration Reference](docs/configuration.md) for all config options.

## Tools

agentiso exposes **40 MCP tools** across seven categories:

- **Workspace lifecycle** (8) — create, destroy, start, stop, list, info, IP, logs
- **Execution & files** (11) — exec, background jobs, file read/write/edit/list, upload/download, set_env
- **Snapshots & forks** (5) — create, restore, list, delete snapshots; fork workspaces
- **Networking** (3) — port forwarding, network policy
- **Session management** (2) — adopt workspaces after restart
- **Vault** (8) — Obsidian-style markdown vault: read, write, search, list, delete, frontmatter, tags, replace
- **Orchestration** (3) — git clone, batch fork, workspace prepare

See [Tool Reference](docs/tools.md) for the full table with parameters and examples.

## Documentation

- [Configuration Reference](docs/configuration.md) — all TOML config sections and defaults
- [Tool Reference](docs/tools.md) — complete MCP tool table with parameters
- [Agent Workflow Guide](docs/workflows.md) — patterns for using agentiso in agent loops
- [Architecture](docs/architecture.md) — system design, vsock protocol, ZFS layout
- [Troubleshooting](docs/troubleshooting.md) — common issues and fixes

## Development

```bash
# Unit tests (no root needed) — 505 tests
cargo test

# E2E tests (root required, needs setup-e2e.sh first) — 26 tests
sudo ./scripts/e2e-test.sh

# MCP integration tests (full lifecycle over stdio) — 26 steps
sudo ./scripts/test-mcp-integration.sh

# State persistence tests (root required) — 10 tests
sudo ./scripts/test-state-persistence.sh
```

See [CLAUDE.md](CLAUDE.md) for full build instructions, project structure, and conventions.

## License

TBD
