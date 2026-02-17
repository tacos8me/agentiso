#!/usr/bin/env bash
#
# agentiso install script
# Run as root from the project root: sudo ./deploy/install.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_DIR/target/release/agentiso"
CONFIG_SRC="$PROJECT_DIR/config.toml"

if [[ $EUID -ne 0 ]]; then
    echo "ERROR: This script must be run as root (sudo ./deploy/install.sh)"
    exit 1
fi

if [[ ! -f "$BINARY" ]]; then
    echo "ERROR: Binary not found at $BINARY"
    echo "Build it first: cargo build --release"
    exit 1
fi

echo "Installing agentiso..."

# Install binary
install -m 755 "$BINARY" /usr/local/bin/agentiso
echo "  Binary -> /usr/local/bin/agentiso"

# Install config (only if not already present — don't overwrite customizations)
mkdir -p /etc/agentiso
if [[ ! -f /etc/agentiso/config.toml ]]; then
    cp "$CONFIG_SRC" /etc/agentiso/config.toml
    echo "  Config -> /etc/agentiso/config.toml"
else
    echo "  Config /etc/agentiso/config.toml already exists, skipping (edit manually)"
fi

# Create runtime and state directories
mkdir -p /var/lib/agentiso /run/agentiso /var/lib/agentiso/transfers
echo "  Directories: /var/lib/agentiso, /run/agentiso, /var/lib/agentiso/transfers"

# Install systemd service
cp "$SCRIPT_DIR/agentiso.service" /etc/systemd/system/agentiso.service
systemctl daemon-reload
echo "  Service -> /etc/systemd/system/agentiso.service"

echo ""
echo "Installation complete."
echo ""
echo "Next steps:"
echo "  1. Edit /etc/agentiso/config.toml for your environment"
echo "  2. Ensure the bridge is up: sudo ip link add br-agentiso type bridge"
echo "  3. Run setup if you haven't: sudo ./scripts/setup-e2e.sh"
echo "  4. Enable and start: sudo systemctl enable --now agentiso"
echo "  5. Check logs: sudo journalctl -u agentiso -f"
