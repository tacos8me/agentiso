# OpenCode MCP Configuration for agentiso

## Config file location

OpenCode looks for MCP server configuration in a JSON file. The location depends on your platform:

| Platform | Path |
|----------|------|
| Linux | `~/.config/opencode/mcp.json` |
| macOS | `~/Library/Application Support/opencode/mcp.json` |
| Windows | `%APPDATA%\opencode\mcp.json` |

If the file does not exist, create it.

## Adding agentiso

Merge the following into your config file under the `mcpServers` key. If the file is empty, paste the entire block:

```json
{
  "mcpServers": {
    "agentiso": {
      "command": "/usr/local/bin/agentiso",
      "args": ["serve", "--config", "/etc/agentiso/config.toml"],
      "env": {}
    }
  }
}
```

If you already have other MCP servers configured, add only the `"agentiso": { ... }` entry inside the existing `"mcpServers"` object.

## Requirements before connecting

1. **The command must be an absolute path.** OpenCode does not use your shell's PATH. If you installed agentiso somewhere other than `/usr/local/bin/agentiso`, update the `command` field accordingly.

   ```bash
   which agentiso          # find the path
   realpath $(which agentiso)  # resolve symlinks
   ```

2. **The config file must exist and be valid.** The path in `--config` must point to a working `config.toml`. If you are using the default install location, edit `/etc/agentiso/config.toml` to match your ZFS pool name, kernel path, and bridge settings.

3. **The ZFS base image must be set up.** Run `setup-e2e.sh` once before first use:

   ```bash
   # From the agentiso project root
   cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest
   sudo ./scripts/setup-e2e.sh
   ```

4. **The bridge must be up.** agentiso assumes `br-agentiso` (or the name in your config) exists and has the configured gateway IP:

   ```bash
   ip link show br-agentiso
   ip addr show br-agentiso
   ```

5. **agentiso must run as root** (or a user with access to `/dev/kvm`, TAP creation, ZFS, and nftables). When launched via OpenCode, it inherits the permissions of the OpenCode process. For a production setup, use the systemd service instead:

   ```bash
   sudo systemctl enable --now agentiso
   # Then point OpenCode at a socket or use a proxy; see deploy/agentiso.service
   ```

   For development, you can run agentiso manually in a separate terminal:

   ```bash
   sudo ./target/release/agentiso serve --config config.toml
   ```

   and configure OpenCode to connect to it, or run OpenCode itself with elevated permissions.

## Verifying the connection

After updating the config file, restart OpenCode. You should see agentiso appear in the MCP tools list. You can test it by asking the agent to create a workspace:

```
Use agentiso to create a new workspace named "test"
```

If it fails, check the agentiso process logs. When run from OpenCode, stderr output goes to the MCP client logs (visible in OpenCode's developer/diagnostic view).

## Development setup (no system install)

For local development, point `command` at the development build:

```json
{
  "mcpServers": {
    "agentiso": {
      "command": "/path/to/agentiso/target/release/agentiso",
      "args": ["serve", "--config", "/path/to/agentiso/config.toml"],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

`RUST_LOG` accepts standard `tracing` filter syntax (e.g. `debug`, `agentiso=trace`).
