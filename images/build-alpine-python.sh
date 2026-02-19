#!/usr/bin/env bash
#
# build-alpine-python.sh - Build the alpine-python toolchain image.
#
# Extends the base alpine-dev image with:
#   - Python 3 with pip and venv
#   - Common packages: numpy, requests
#   - Build dependencies for native extensions (gcc, musl-dev, libffi-dev)
#
# Prerequisites:
#   - Base image must exist: agentiso/agentiso/base/alpine-dev@latest
#   - Run setup-e2e.sh first if the base image doesn't exist
#   - Must be run as root (for ZFS and mount operations)
#
# Usage:
#   sudo ./images/build-alpine-python.sh
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Configuration
ZFS_POOL="agentiso"
DATASET_PREFIX="agentiso"
BASE_ZVOL="${ZFS_POOL}/${DATASET_PREFIX}/base/alpine-dev"
TARGET_ZVOL="${ZFS_POOL}/${DATASET_PREFIX}/base/alpine-python"
MOUNT_POINT="/mnt/alpine-python-build"

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

echo "=== Building alpine-python image ==="
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
echo "--- Step 2: Install Python toolchain ---"

mkdir -p "$MOUNT_POINT"
mount "/dev/zvol/${TARGET_ZVOL}" "$MOUNT_POINT"

cleanup() {
    echo "Cleaning up..."
    umount "$MOUNT_POINT" 2>/dev/null || true
}
trap cleanup EXIT

# Copy DNS config for package installation
cp /etc/resolv.conf "$MOUNT_POINT/etc/resolv.conf"

echo "Installing Python packages and build dependencies..."
chroot "$MOUNT_POINT" /bin/sh -c '
    apk update --quiet
    apk add --quiet --no-progress \
        python3 \
        python3-dev \
        py3-pip \
        py3-virtualenv \
        gcc \
        musl-dev \
        libffi-dev \
        openssl-dev
' 2>&1 | tail -5

echo "Installing common Python packages via pip..."
chroot "$MOUNT_POINT" /bin/sh -c '
    pip3 install --break-system-packages --quiet numpy requests
'

echo "Verifying Python installation..."
chroot "$MOUNT_POINT" /bin/sh -c '
    python3 --version
    pip3 --version
    python3 -c "import numpy; print(f\"numpy {numpy.__version__}\")"
    python3 -c "import requests; print(f\"requests {requests.__version__}\")"
'

echo "OK"
echo ""

# ---------------------------------------------------------------
# Step 3: Unmount and snapshot
# ---------------------------------------------------------------
echo "--- Step 3: Snapshot ---"

umount "$MOUNT_POINT"
trap - EXIT

echo "Creating snapshot ${TARGET_ZVOL}@latest..."
zfs snapshot "${TARGET_ZVOL}@latest"

echo ""
echo "=== alpine-python image ready ==="
echo ""
echo "ZFS:"
zfs list -r "${TARGET_ZVOL}"
echo ""
echo "Includes: python3, pip, venv, numpy, requests, gcc, musl-dev"
