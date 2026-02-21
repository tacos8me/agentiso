#!/usr/bin/env bash
#
# agentiso e2e environment setup
# Run with: sudo ./scripts/setup-e2e.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# Configuration
ZFS_POOL="agentiso"
DATASET_PREFIX="agentiso"
ALPINE_VERSION="3.20.6"
IMAGE_SIZE="2G"
GUEST_AGENT="$PROJECT_DIR/target/x86_64-unknown-linux-musl/release/agentiso-guest"
MOUNT_POINT="/mnt/alpine-build"

echo "=== agentiso e2e setup ==="
echo "Project: $PROJECT_DIR"
echo ""

# Ensure running as root
if [[ $EUID -ne 0 ]]; then
    echo "ERROR: This script must be run with sudo"
    exit 1
fi

# Ensure guest agent binary exists
if [[ ! -f "$GUEST_AGENT" ]]; then
    echo "ERROR: Guest agent not found at $GUEST_AGENT"
    echo "Build it first: cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest"
    exit 1
fi

# ---------------------------------------------------------------
# Step 1: Copy host kernel + initramfs
# ---------------------------------------------------------------
echo "--- Step 1: Kernel ---"
mkdir -p /var/lib/agentiso

HOST_KERNEL="/boot/vmlinuz-$(uname -r)"
HOST_INITRD="/boot/initrd.img-$(uname -r)"

if [[ ! -f "$HOST_KERNEL" ]]; then
    echo "ERROR: Host kernel not found at $HOST_KERNEL"
    exit 1
fi

# Copy host kernel (bzImage format — QEMU handles this natively)
if [[ -f /var/lib/agentiso/vmlinuz ]] && cmp -s "$HOST_KERNEL" /var/lib/agentiso/vmlinuz; then
    echo "Kernel already up-to-date, skipping copy"
else
    echo "Copying host kernel: $HOST_KERNEL"
    cp "$HOST_KERNEL" /var/lib/agentiso/vmlinuz
fi

# Copy initramfs (needed because distro kernels have virtio as modules)
if [[ -f "$HOST_INITRD" ]]; then
    if [[ -f /var/lib/agentiso/initrd.img ]] && cmp -s "$HOST_INITRD" /var/lib/agentiso/initrd.img; then
        echo "Initramfs already up-to-date, skipping copy"
    else
        echo "Copying host initramfs: $HOST_INITRD"
        cp "$HOST_INITRD" /var/lib/agentiso/initrd.img
    fi
else
    echo "WARNING: No initramfs at $HOST_INITRD — virtio modules may not load"
fi

echo "Kernel: $(ls -lh /var/lib/agentiso/vmlinuz | awk '{print $5}') ($(uname -r))"
[[ -f /var/lib/agentiso/initrd.img ]] && echo "Initrd: $(ls -lh /var/lib/agentiso/initrd.img | awk '{print $5}')"
echo ""

# ---------------------------------------------------------------
# Step 2: Create runtime directories
# ---------------------------------------------------------------
echo "--- Step 2: Runtime dirs ---"
mkdir -p /var/lib/agentiso /var/lib/agentiso/vault /run/agentiso
# Make accessible to the user who will run agentiso
if [[ -n "${SUDO_USER:-}" ]]; then
    chown "$SUDO_USER:$SUDO_USER" /var/lib/agentiso /var/lib/agentiso/vault /run/agentiso
fi
echo "OK"
echo ""

# ---------------------------------------------------------------
# Step 3: Build Alpine base image
# ---------------------------------------------------------------
echo "--- Step 3: Alpine base image ---"
WORKDIR=$(mktemp -d)
IMAGE="$WORKDIR/alpine-dev.raw"
ROOTFS_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VERSION%.*}/releases/x86_64/alpine-minirootfs-${ALPINE_VERSION}-x86_64.tar.gz"

echo "Downloading Alpine $ALPINE_VERSION minirootfs..."
curl -fSL -o "$WORKDIR/rootfs.tar.gz" "$ROOTFS_URL"

echo "Creating ${IMAGE_SIZE} ext4 image..."
truncate -s "$IMAGE_SIZE" "$IMAGE"
mkfs.ext4 -F -q "$IMAGE"

echo "Mounting and populating rootfs..."
mkdir -p "$MOUNT_POINT"
mount -o loop "$IMAGE" "$MOUNT_POINT"

# Cleanup trap
cleanup() {
    echo "Cleaning up..."
    umount "$MOUNT_POINT" 2>/dev/null || true
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

tar xzf "$WORKDIR/rootfs.tar.gz" -C "$MOUNT_POINT"

echo "Copying guest agent binary..."
cp "$GUEST_AGENT" "$MOUNT_POINT/usr/local/bin/agentiso-guest"
chmod 755 "$MOUNT_POINT/usr/local/bin/agentiso-guest"

echo "Setting up DNS for chroot..."
cp /etc/resolv.conf "$MOUNT_POINT/etc/resolv.conf"

echo "Installing packages in chroot (this takes a minute)..."
chroot "$MOUNT_POINT" /bin/sh -c '
    apk update --quiet
    apk add --quiet --no-progress build-base git python3 nodejs npm openrc
' 2>&1 | tail -3

echo "Copying kernel modules for vsock and virtio-console..."
KVER="$(uname -r)"
KMOD_SRC="/lib/modules/$KVER"
KMOD_DST="$MOUNT_POINT/lib/modules/$KVER"
mkdir -p "$KMOD_DST/kernel/net/vmw_vsock" "$KMOD_DST/kernel/drivers/char/virtio"

# Helper: copy and decompress module (Ubuntu ships .ko.zst, Alpine kmod can't read zstd)
copy_mod() {
    local mod_name="$1"
    local dest_dir="$2"
    local src
    src=$(find "$KMOD_SRC" -name "${mod_name}.ko*" 2>/dev/null | head -1)
    if [[ -z "$src" ]]; then
        echo "  WARNING: module $mod_name not found"
        return
    fi
    if [[ "$src" == *.zst ]]; then
        zstd -dq "$src" -o "$dest_dir/${mod_name}.ko"
    elif [[ "$src" == *.xz ]]; then
        xz -dkc "$src" > "$dest_dir/${mod_name}.ko"
    elif [[ "$src" == *.gz ]]; then
        gzip -dkc "$src" > "$dest_dir/${mod_name}.ko"
    else
        cp "$src" "$dest_dir/"
    fi
}

# vsock modules
for mod in vsock vmw_vsock_virtio_transport_common vmw_vsock_virtio_transport; do
    copy_mod "$mod" "$KMOD_DST/kernel/net/vmw_vsock"
done

# virtio-console module
copy_mod virtio_console "$KMOD_DST/kernel/drivers/char/virtio"

# Copy modules metadata and regenerate deps
cp "$KMOD_SRC/modules.order" "$KMOD_DST/" 2>/dev/null || true
cp "$KMOD_SRC/modules.builtin" "$KMOD_DST/" 2>/dev/null || true
cp "$KMOD_SRC/modules.builtin.modinfo" "$KMOD_DST/" 2>/dev/null || true
depmod -b "$MOUNT_POINT" "$KVER" 2>/dev/null || true

# Auto-load vsock modules on boot
mkdir -p "$MOUNT_POINT/etc/modules-load.d"
cat > "$MOUNT_POINT/etc/modules-load.d/agentiso.conf" << 'MODEOF'
vsock
vmw_vsock_virtio_transport
virtio_console
MODEOF

# Add a boot script to load modules (Alpine uses modprobe, not systemd modules-load)
cat > "$MOUNT_POINT/etc/local.d/agentiso-modules.start" << 'MODSEOF'
#!/bin/sh
KDIR="/lib/modules/$(uname -r)/kernel"
insmod "$KDIR/net/vmw_vsock/vsock.ko" 2>/dev/null || true
insmod "$KDIR/net/vmw_vsock/vmw_vsock_virtio_transport_common.ko" 2>/dev/null || true
insmod "$KDIR/net/vmw_vsock/vmw_vsock_virtio_transport.ko" 2>/dev/null || true
insmod "$KDIR/drivers/char/virtio/virtio_console.ko" 2>/dev/null || true
MODSEOF
chmod +x "$MOUNT_POINT/etc/local.d/agentiso-modules.start"

echo "Configuring guest agent service..."
cat > "$MOUNT_POINT/etc/init.d/agentiso-guest" << 'INITEOF'
#!/sbin/openrc-run
name="agentiso-guest"
command="/usr/local/bin/agentiso-guest"
command_background=true
pidfile="/run/agentiso-guest.pid"
output_log="/var/log/agentiso-guest.log"
error_log="/var/log/agentiso-guest.log"
start_pre() {
    # Load vsock modules before starting the agent (insmod with explicit
    # paths — Alpine minirootfs has busybox insmod but not full kmod).
    # The guest agent also loads modules itself as a fallback.
    KDIR="/lib/modules/$(uname -r)/kernel/net/vmw_vsock"
    for mod in vsock.ko vmw_vsock_virtio_transport_common.ko vmw_vsock_virtio_transport.ko; do
        if insmod "$KDIR/$mod" 2>&1; then
            einfo "Loaded $mod"
        else
            ewarn "insmod $KDIR/$mod failed (may already be loaded)"
        fi
    done
}
depend() {
    after localmount
}
INITEOF
chmod +x "$MOUNT_POINT/etc/init.d/agentiso-guest"
chroot "$MOUNT_POINT" rc-update add agentiso-guest default 2>/dev/null
chroot "$MOUNT_POINT" rc-update add local default 2>/dev/null

echo "Configuring console and init..."
cat > "$MOUNT_POINT/etc/inittab" << 'INITTAB'
::sysinit:/sbin/openrc sysinit
::sysinit:/sbin/openrc boot
::wait:/sbin/openrc default
ttyS0::respawn:/sbin/getty 115200 ttyS0
::ctrlaltdel:/sbin/reboot
::shutdown:/sbin/openrc shutdown
INITTAB

echo "agentiso-vm" > "$MOUNT_POINT/etc/hostname"
echo "root:agentiso" | chpasswd -R "$MOUNT_POINT" 2>/dev/null || true

# Set up fstab
cat > "$MOUNT_POINT/etc/fstab" << 'FSTAB'
/dev/vda    /    ext4    defaults,noatime    0 1
FSTAB

echo "Installing init-fast shim..."
cat > "$MOUNT_POINT/sbin/init-fast" << 'INITFAST'
#!/bin/sh
# Minimal init shim — bypasses OpenRC for fast boot.
# Mount essential filesystems if not already from initrd.
mountpoint -q /proc || mount -t proc proc /proc
mountpoint -q /sys  || mount -t sysfs sysfs /sys
mountpoint -q /dev  || mount -t devtmpfs devtmpfs /dev
mount -t tmpfs -o size=256m tmpfs /tmp
mount -t tmpfs -o size=64m tmpfs /run
# Mount /dev/pts (pty allocation) and /dev/shm (POSIX shared memory)
mkdir -p /dev/pts
mount -t devpts devpts /dev/pts
mkdir -p /dev/shm
mount -t tmpfs -o size=64m tmpfs /dev/shm
# Minimal /etc/hosts for localhost resolution (npm, dev servers)
echo "127.0.0.1 localhost" > /etc/hosts
echo "::1 localhost" >> /etc/hosts
# Ensure vsock modules loaded (may already be from initrd)
KDIR="/lib/modules/$(uname -r)/kernel/net/vmw_vsock"
insmod "$KDIR/vsock.ko" 2>/dev/null
insmod "$KDIR/vmw_vsock_virtio_transport_common.ko" 2>/dev/null
insmod "$KDIR/vmw_vsock_virtio_transport.ko" 2>/dev/null
# Reap zombies in background
trap 'wait' CHLD
# Exec guest agent (becomes main process under PID 1 shell)
exec /usr/local/bin/agentiso-guest
INITFAST
chmod 755 "$MOUNT_POINT/sbin/init-fast"

echo "Unmounting image..."
umount "$MOUNT_POINT"
trap - EXIT  # Disable cleanup trap since we unmounted successfully

# ---------------------------------------------------------------
# Step 4: Write image to ZFS zvol
# ---------------------------------------------------------------
echo ""
echo "--- Step 4: ZFS base image ---"

ZVOL_PATH="${ZFS_POOL}/${DATASET_PREFIX}/base/alpine-dev"

# Check if zvol already exists
if zfs list "$ZVOL_PATH" &>/dev/null; then
    echo "Zvol $ZVOL_PATH already exists."
    # Check for snapshot — destroy recursively to handle any dependent clones.
    # WARNING: `zfs destroy -R` cascades to ALL clones of this snapshot,
    # including alpine-opencode and any workspace zvols derived from it.
    # After this runs you MUST rebuild alpine-opencode if it existed.
    if zfs list "${ZVOL_PATH}@latest" &>/dev/null; then
        echo "Snapshot ${ZVOL_PATH}@latest exists. Destroying (with dependents) and recreating..."
        echo "WARNING: This cascades to ALL clones (including alpine-opencode)."
        zfs destroy -R "${ZVOL_PATH}@latest"
        echo "WARNING: alpine-opencode was destroyed by cascade. Rebuild with: sudo ./images/build-alpine-opencode.sh"
    fi
    echo "Writing image to existing zvol..."
    dd if="$IMAGE" of="/dev/zvol/${ZVOL_PATH}" bs=4M status=progress 2>&1
else
    echo "Creating zvol $ZVOL_PATH..."
    zfs create -V "$IMAGE_SIZE" "$ZVOL_PATH"
    # Wait for device node
    sleep 1
    udevadm settle 2>/dev/null || sleep 2
    echo "Writing image to zvol..."
    dd if="$IMAGE" of="/dev/zvol/${ZVOL_PATH}" bs=4M status=progress 2>&1
fi

echo "Creating snapshot ${ZVOL_PATH}@latest..."
zfs snapshot "${ZVOL_PATH}@latest"

# Clean up temp image
rm -f "$IMAGE"
rm -rf "$WORKDIR"

echo ""
echo "--- Step 4b: Build fast initrd ---"
"$SCRIPT_DIR/../images/kernel/build-initrd.sh" /var/lib/agentiso/initrd-fast.img

# ---------------------------------------------------------------
# Step 5: Verify
# ---------------------------------------------------------------
echo ""
echo "=== Verification ==="
echo ""
echo "Kernel:"
ls -lh /var/lib/agentiso/vmlinuz
[[ -f /var/lib/agentiso/initrd.img ]] && ls -lh /var/lib/agentiso/initrd.img
echo ""
echo "ZFS:"
zfs list -r "${ZFS_POOL}/${DATASET_PREFIX}/base"
echo ""
echo "Zvol device:"
ls -la "/dev/zvol/${ZVOL_PATH}"
echo ""
echo "Guest agent:"
ls -lh "$GUEST_AGENT"
echo ""
echo "=== Setup complete! ==="
echo ""
echo "NOTE: If alpine-opencode existed before this run, it was destroyed by the"
echo "      cascade 'zfs destroy -R' on the base snapshot. Rebuild it with:"
echo "  sudo ./images/build-alpine-opencode.sh"
echo ""
echo "Next: run the e2e test or start agentiso:"
echo "  cargo build --release"
echo "  ./target/release/agentiso serve --config config.toml"
