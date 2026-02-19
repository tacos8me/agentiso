#!/usr/bin/env bash
#
# build-alpine-opencode.sh - Build the alpine-opencode image.
#
# Extends the base alpine-dev image with:
#   - OpenCode v1.2.6 musl binary (AI coding agent CLI)
#   - git, curl, nodejs, npm, python3, py3-pip
#   - Default opencode config (Anthropic provider, claude-sonnet-4)
#
# Prerequisites:
#   - Base image must exist: agentiso/agentiso/base/alpine-dev@latest
#   - Run setup-e2e.sh first if the base image doesn't exist
#   - Must be run as root (for ZFS and mount operations)
#
# Usage:
#   sudo ./images/build-alpine-opencode.sh
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Configuration
ZFS_POOL="agentiso"
DATASET_PREFIX="agentiso"
BASE_ZVOL="${ZFS_POOL}/${DATASET_PREFIX}/base/alpine-dev"
TARGET_ZVOL="${ZFS_POOL}/${DATASET_PREFIX}/base/alpine-opencode"
IMAGE_SIZE="2G"
MOUNT_POINT="/mnt/alpine-opencode-build"
OPENCODE_VERSION="1.2.6"
OPENCODE_URL="https://github.com/anomalyco/opencode/releases/download/v${OPENCODE_VERSION}/opencode-linux-x64-baseline-musl"

# Ensure running as root
if [[ $EUID -ne 0 ]]; then
    echo "ERROR: This script must be run with sudo"
    exit 1
fi

# Ensure base image exists
if ! zfs list "${BASE_ZVOL}@latest" &>/dev/null; then
    echo "ERROR: Base image snapshot not found: ${BASE_ZVOL}@latest"
    echo "Run setup-e2e.sh first to create the base image."
    exit 1
fi

echo "=== Building alpine-opencode image ==="
echo "Base:   ${BASE_ZVOL}@latest"
echo "Target: ${TARGET_ZVOL}"
echo ""

# ---------------------------------------------------------------
# Step 1: Create or recreate the target zvol from base
# ---------------------------------------------------------------
echo "--- Step 1: Clone base image ---"

if zfs list "$TARGET_ZVOL" &>/dev/null; then
    echo "Target zvol $TARGET_ZVOL already exists, destroying..."
    if zfs list "${TARGET_ZVOL}@latest" &>/dev/null; then
        zfs destroy -R "${TARGET_ZVOL}@latest"
    fi
    zfs destroy "$TARGET_ZVOL"
fi

echo "Cloning ${BASE_ZVOL}@latest -> ${TARGET_ZVOL}..."
zfs clone "${BASE_ZVOL}@latest" "${TARGET_ZVOL}"

# Wait for device node
sleep 1
udevadm settle 2>/dev/null || sleep 2

echo "OK"
echo ""

# ---------------------------------------------------------------
# Step 2: Mount and customize
# ---------------------------------------------------------------
echo "--- Step 2: Install opencode and packages ---"

mkdir -p "$MOUNT_POINT"
mount "/dev/zvol/${TARGET_ZVOL}" "$MOUNT_POINT"

cleanup() {
    echo "Cleaning up..."
    umount "$MOUNT_POINT" 2>/dev/null || true
}
trap cleanup EXIT

# Copy DNS config for package installation
cp /etc/resolv.conf "$MOUNT_POINT/etc/resolv.conf"

echo "Installing additional packages..."
chroot "$MOUNT_POINT" /bin/sh -c '
    apk update --quiet
    apk add --quiet --no-progress git curl nodejs npm python3 py3-pip
' 2>&1 | tail -5

echo "Downloading opencode v${OPENCODE_VERSION}..."
TMPBIN=$(mktemp)
curl -fSL -o "$TMPBIN" "$OPENCODE_URL"
chmod 755 "$TMPBIN"
install -m 0755 "$TMPBIN" "$MOUNT_POINT/usr/local/bin/opencode"
rm -f "$TMPBIN"

echo "Verifying opencode binary..."
if chroot "$MOUNT_POINT" /usr/local/bin/opencode --version 2>/dev/null; then
    echo "  opencode binary OK"
else
    echo "  WARNING: opencode --version failed (may need different arch or deps)"
fi

echo "Creating default opencode config..."
mkdir -p "$MOUNT_POINT/root/.config/opencode"
cat > "$MOUNT_POINT/root/.config/opencode/config.jsonc" << 'CONFIG'
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-20250514"
}
CONFIG

echo "OK"
echo ""

# ---------------------------------------------------------------
# Step 3: Unmount and snapshot
# ---------------------------------------------------------------
echo "--- Step 3: Snapshot ---"

umount "$MOUNT_POINT"
trap - EXIT  # Disable cleanup trap since we unmounted successfully

echo "Creating snapshot ${TARGET_ZVOL}@latest..."
zfs snapshot "${TARGET_ZVOL}@latest"

echo ""
echo "=== alpine-opencode image ready ==="
echo ""
echo "ZFS:"
zfs list -r "${TARGET_ZVOL}"
echo ""
echo "To create a workspace from this image, use:"
echo "  agentiso create --image alpine-opencode"
