#!/usr/bin/env bash
#
# build-alpine-rust.sh - Build the alpine-rust toolchain image.
#
# Extends the base alpine-dev image with:
#   - Rust stable toolchain (via rustup)
#   - cargo, rustc, rustfmt, clippy
#   - Build essentials: gcc, musl-dev, make, pkgconf
#
# Prerequisites:
#   - Base image must exist: agentiso/agentiso/base/alpine-dev@latest
#   - Run setup-e2e.sh first if the base image doesn't exist
#   - Must be run as root (for ZFS and mount operations)
#
# Usage:
#   sudo ./images/build-alpine-rust.sh
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Configuration
ZFS_POOL="agentiso"
DATASET_PREFIX="agentiso"
BASE_ZVOL="${ZFS_POOL}/${DATASET_PREFIX}/base/alpine-dev"
TARGET_ZVOL="${ZFS_POOL}/${DATASET_PREFIX}/base/alpine-rust"
MOUNT_POINT="/mnt/alpine-rust-build"

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

echo "=== Building alpine-rust image ==="
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
echo "--- Step 2: Install Rust toolchain ---"

mkdir -p "$MOUNT_POINT"
mount "/dev/zvol/${TARGET_ZVOL}" "$MOUNT_POINT"

cleanup() {
    echo "Cleaning up..."
    umount "$MOUNT_POINT" 2>/dev/null || true
}
trap cleanup EXIT

# Copy DNS config for package installation
cp /etc/resolv.conf "$MOUNT_POINT/etc/resolv.conf"

echo "Installing build dependencies..."
chroot "$MOUNT_POINT" /bin/sh -c '
    apk update --quiet
    apk add --quiet --no-progress \
        build-base \
        gcc \
        musl-dev \
        make \
        pkgconf \
        openssl-dev \
        curl
' 2>&1 | tail -5

echo "Installing rustup and stable toolchain (this takes a few minutes)..."
chroot "$MOUNT_POINT" /bin/sh -c '
    curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --default-toolchain stable --profile default
'

echo "Verifying Rust installation..."
chroot "$MOUNT_POINT" /bin/sh -c '
    . /root/.cargo/env
    rustc --version
    cargo --version
    rustfmt --version
'

# Make cargo available system-wide via profile script
cat > "$MOUNT_POINT/etc/profile.d/rust.sh" << 'RUSTENV'
# Rust toolchain
if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi
RUSTENV
chmod 644 "$MOUNT_POINT/etc/profile.d/rust.sh"

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
echo "=== alpine-rust image ready ==="
echo ""
echo "ZFS:"
zfs list -r "${TARGET_ZVOL}"
echo ""
echo "Includes: rustc (stable), cargo, rustfmt, clippy, gcc, musl-dev, make"
