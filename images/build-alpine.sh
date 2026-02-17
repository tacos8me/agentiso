#!/usr/bin/env bash
#
# build-alpine.sh - Build the agentiso Alpine Linux base image.
#
# Creates a minimal Alpine rootfs with dev tools, installs the agentiso-guest
# agent binary, and writes it as a raw ext4 disk image.
#
# Two modes:
#   1. Docker mode (default): Uses Docker to build the rootfs. Works on any
#      Linux host with Docker installed. Does NOT require root.
#   2. Native mode (--native): Uses apk directly with losetup/chroot.
#      Requires root and apk-tools on the host (i.e., Alpine Linux host).
#
# Prerequisites:
#   - Docker (for Docker mode) OR apk-tools + root (for native mode)
#   - agentiso-guest binary already built:
#     cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest
#
# Usage:
#   ./images/build-alpine.sh [--native] [output-image-path]
#
# The output is a raw ext4 disk image. To import into ZFS:
#   zfs create -V 4G tank/agentiso/base/alpine-dev
#   dd if=images/alpine-dev.raw of=/dev/zvol/tank/agentiso/base/alpine-dev bs=4M
#   zfs snapshot tank/agentiso/base/alpine-dev@latest

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Parse arguments
NATIVE_MODE=false
OUTPUT=""
for arg in "$@"; do
    case "$arg" in
        --native) NATIVE_MODE=true ;;
        *) OUTPUT="$arg" ;;
    esac
done
OUTPUT="${OUTPUT:-$PROJECT_ROOT/images/alpine-dev.raw}"

IMAGE_SIZE="4G"
IMAGE_SIZE_BYTES=$((4 * 1024 * 1024 * 1024))
ALPINE_VERSION="3.20"
ALPINE_MIRROR="https://dl-cdn.alpinelinux.org/alpine"

GUEST_AGENT="$PROJECT_ROOT/target/x86_64-unknown-linux-musl/release/agentiso-guest"
if [ ! -f "$GUEST_AGENT" ]; then
    echo "ERROR: Guest agent binary not found at $GUEST_AGENT"
    echo "Build it first: cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest"
    exit 1
fi

# Shared configuration files used by both modes.

write_fstab() {
    local root="$1"
    cat > "$root/etc/fstab" << 'FSTAB'
/dev/vda    /       ext4    defaults,noatime    0 1
proc        /proc   proc    defaults            0 0
sysfs       /sys    sysfs   defaults            0 0
devtmpfs    /dev    devtmpfs defaults           0 0
FSTAB
}

write_inittab() {
    local root="$1"
    cat > "$root/etc/inittab" << 'INITTAB'
::sysinit:/sbin/openrc sysinit
::sysinit:/sbin/openrc boot
::wait:/sbin/openrc default
::shutdown:/sbin/openrc shutdown

# Serial console for microvm (hvc0)
hvc0::respawn:/sbin/getty 115200 hvc0

::ctrlaltdel:/sbin/reboot
INITTAB
}

write_interfaces() {
    local root="$1"
    mkdir -p "$root/etc/network"
    cat > "$root/etc/network/interfaces" << 'IFACES'
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet manual
IFACES
}

write_guest_agent_initscript() {
    local root="$1"
    cat > "$root/etc/init.d/agentiso-guest" << 'INITSCRIPT'
#!/sbin/openrc-run

name="agentiso-guest"
description="agentiso guest agent"
command="/usr/local/bin/agentiso-guest"
command_background=true
pidfile="/run/${RC_SVCNAME}.pid"
output_log="/var/log/agentiso-guest.log"
error_log="/var/log/agentiso-guest.log"

depend() {
    need localmount
    after bootmisc
}
INITSCRIPT
    chmod 0755 "$root/etc/init.d/agentiso-guest"
}

# ============================================================================
# Docker mode: build rootfs inside a Docker container, export as tarball,
# then pack into a raw ext4 image.
# ============================================================================

build_docker() {
    echo "==> Building Alpine rootfs using Docker"

    WORKDIR=$(mktemp -d /tmp/agentiso-build.XXXXXX)
    trap 'rm -rf "$WORKDIR"' EXIT

    # Create a Dockerfile that installs everything we need.
    cat > "$WORKDIR/Dockerfile" << DOCKERFILE
FROM alpine:${ALPINE_VERSION}

# Base system packages
RUN apk add --no-cache \\
    alpine-base \\
    openrc \\
    busybox-initscripts \\
    iproute2 \\
    iptables \\
    ca-certificates \\
    tzdata

# Dev tools
RUN apk add --no-cache \\
    build-base \\
    git \\
    python3 \\
    py3-pip \\
    nodejs \\
    npm \\
    curl \\
    wget \\
    jq \\
    bash \\
    openssh-client \\
    vim \\
    strace

# Clean up apk cache
RUN rm -rf /var/cache/apk/*

# Remove root password (agent-only access via vsock)
RUN passwd -d root

# Create workspace directory
RUN mkdir -p /workspace
DOCKERFILE

    echo "==> Building Docker image"
    docker build -t agentiso-rootfs-builder "$WORKDIR"

    # Copy guest agent binary into a temp container
    echo "==> Creating container and installing guest agent"
    CONTAINER_ID=$(docker create agentiso-rootfs-builder /bin/true)

    docker cp "$GUEST_AGENT" "$CONTAINER_ID:/usr/local/bin/agentiso-guest"

    # Export the full filesystem as a tarball
    echo "==> Exporting rootfs tarball"
    docker export "$CONTAINER_ID" > "$WORKDIR/rootfs.tar"
    docker rm "$CONTAINER_ID" > /dev/null

    # Create a raw ext4 image from the tarball
    echo "==> Creating raw disk image ($IMAGE_SIZE)"
    truncate -s "$IMAGE_SIZE" "$OUTPUT"
    mkfs.ext4 -q -L agentiso-root "$OUTPUT"

    # Mount and populate. This requires root for losetup/mount.
    # If we don't have root, use debugfs or fuse2fs as fallback.
    if [ "$(id -u)" = "0" ]; then
        MOUNTPOINT="$WORKDIR/mnt"
        mkdir -p "$MOUNTPOINT"
        mount -o loop "$OUTPUT" "$MOUNTPOINT"

        echo "==> Extracting rootfs into image"
        tar xf "$WORKDIR/rootfs.tar" -C "$MOUNTPOINT"

        # Apply configuration
        echo "==> Configuring system"
        write_fstab "$MOUNTPOINT"
        echo "agentiso" > "$MOUNTPOINT/etc/hostname"
        write_inittab "$MOUNTPOINT"
        write_interfaces "$MOUNTPOINT"
        write_guest_agent_initscript "$MOUNTPOINT"
        chmod 0755 "$MOUNTPOINT/usr/local/bin/agentiso-guest"

        # Configure services via chroot
        mount -t proc proc "$MOUNTPOINT/proc"
        chroot "$MOUNTPOINT" rc-update del networking boot 2>/dev/null || true
        chroot "$MOUNTPOINT" rc-update del hwclock boot 2>/dev/null || true
        chroot "$MOUNTPOINT" rc-update add devfs sysinit 2>/dev/null || true
        chroot "$MOUNTPOINT" rc-update add mdev sysinit 2>/dev/null || true
        chroot "$MOUNTPOINT" rc-update add hostname boot 2>/dev/null || true
        chroot "$MOUNTPOINT" rc-update add agentiso-guest default 2>/dev/null || true
        umount "$MOUNTPOINT/proc"

        umount "$MOUNTPOINT"
    elif command -v fuse2fs &>/dev/null; then
        MOUNTPOINT="$WORKDIR/mnt"
        mkdir -p "$MOUNTPOINT"
        fuse2fs "$OUTPUT" "$MOUNTPOINT" -o fakeroot

        echo "==> Extracting rootfs into image"
        tar xf "$WORKDIR/rootfs.tar" -C "$MOUNTPOINT"

        echo "==> Configuring system"
        write_fstab "$MOUNTPOINT"
        echo "agentiso" > "$MOUNTPOINT/etc/hostname"
        write_inittab "$MOUNTPOINT"
        write_interfaces "$MOUNTPOINT"
        write_guest_agent_initscript "$MOUNTPOINT"
        chmod 0755 "$MOUNTPOINT/usr/local/bin/agentiso-guest"

        # Write OpenRC symlinks manually (since we can't chroot without root)
        mkdir -p "$MOUNTPOINT/etc/runlevels/sysinit"
        mkdir -p "$MOUNTPOINT/etc/runlevels/boot"
        mkdir -p "$MOUNTPOINT/etc/runlevels/default"
        ln -sf /etc/init.d/devfs "$MOUNTPOINT/etc/runlevels/sysinit/devfs" 2>/dev/null || true
        ln -sf /etc/init.d/mdev "$MOUNTPOINT/etc/runlevels/sysinit/mdev" 2>/dev/null || true
        ln -sf /etc/init.d/hostname "$MOUNTPOINT/etc/runlevels/boot/hostname" 2>/dev/null || true
        ln -sf /etc/init.d/agentiso-guest "$MOUNTPOINT/etc/runlevels/default/agentiso-guest" 2>/dev/null || true

        fusermount -u "$MOUNTPOINT"
    else
        echo "==> No root and no fuse2fs; using Docker to populate the image"
        # Use a privileged Docker container to mount and populate the image
        docker run --rm --privileged \
            -v "$OUTPUT:/image.raw" \
            -v "$WORKDIR/rootfs.tar:/rootfs.tar" \
            -v "$GUEST_AGENT:/guest-agent:ro" \
            alpine:${ALPINE_VERSION} sh -c '
                apk add --no-cache e2fsprogs
                mkdir -p /mnt
                mount -o loop /image.raw /mnt
                tar xf /rootfs.tar -C /mnt
                install -m 0755 /guest-agent /mnt/usr/local/bin/agentiso-guest
                echo "agentiso" > /mnt/etc/hostname
                mkdir -p /mnt/workspace
                # Write OpenRC runlevel symlinks
                mkdir -p /mnt/etc/runlevels/sysinit /mnt/etc/runlevels/boot /mnt/etc/runlevels/default
                ln -sf /etc/init.d/devfs /mnt/etc/runlevels/sysinit/devfs
                ln -sf /etc/init.d/mdev /mnt/etc/runlevels/sysinit/mdev
                ln -sf /etc/init.d/hostname /mnt/etc/runlevels/boot/hostname
                umount /mnt
            '

        # We still need to write config files. Re-mount via Docker.
        # The config files were written by the Dockerfile, but we need
        # to overwrite some for microvm use.
        docker run --rm --privileged \
            -v "$OUTPUT:/image.raw" \
            alpine:${ALPINE_VERSION} sh -c '
                apk add --no-cache e2fsprogs
                mkdir -p /mnt
                mount -o loop /image.raw /mnt

                # fstab
                cat > /mnt/etc/fstab << EOF2
/dev/vda    /       ext4    defaults,noatime    0 1
proc        /proc   proc    defaults            0 0
sysfs       /sys    sysfs   defaults            0 0
devtmpfs    /dev    devtmpfs defaults           0 0
EOF2

                # inittab for microvm serial console
                cat > /mnt/etc/inittab << EOF2
::sysinit:/sbin/openrc sysinit
::sysinit:/sbin/openrc boot
::wait:/sbin/openrc default
::shutdown:/sbin/openrc shutdown
hvc0::respawn:/sbin/getty 115200 hvc0
::ctrlaltdel:/sbin/reboot
EOF2

                # Network interfaces
                mkdir -p /mnt/etc/network
                cat > /mnt/etc/network/interfaces << EOF2
auto lo
iface lo inet loopback
auto eth0
iface eth0 inet manual
EOF2

                # Guest agent init script
                cat > /mnt/etc/init.d/agentiso-guest << EOF2
#!/sbin/openrc-run
name="agentiso-guest"
description="agentiso guest agent"
command="/usr/local/bin/agentiso-guest"
command_background=true
pidfile="/run/\${RC_SVCNAME}.pid"
output_log="/var/log/agentiso-guest.log"
error_log="/var/log/agentiso-guest.log"
depend() {
    need localmount
    after bootmisc
}
EOF2
                chmod 0755 /mnt/etc/init.d/agentiso-guest
                ln -sf /etc/init.d/agentiso-guest /mnt/etc/runlevels/default/agentiso-guest

                # Remove networking and hwclock from boot
                rm -f /mnt/etc/runlevels/boot/networking 2>/dev/null || true
                rm -f /mnt/etc/runlevels/boot/hwclock 2>/dev/null || true

                umount /mnt
            '
    fi

    # Clean up Docker image
    docker rmi agentiso-rootfs-builder > /dev/null 2>&1 || true

    echo "==> Done: $OUTPUT"
    echo "    Image size: $(du -h "$OUTPUT" | cut -f1)"
    echo ""
    echo "    To import into ZFS:"
    echo "    zfs create -V ${IMAGE_SIZE} tank/agentiso/base/alpine-dev"
    echo "    dd if=$OUTPUT of=/dev/zvol/tank/agentiso/base/alpine-dev bs=4M"
    echo "    zfs snapshot tank/agentiso/base/alpine-dev@latest"
}

# ============================================================================
# Native mode: uses apk directly with losetup/chroot. Requires root + Alpine.
# ============================================================================

build_native() {
    if [ "$(id -u)" != "0" ]; then
        echo "ERROR: Native mode requires root. Run with sudo or use Docker mode (default)."
        exit 1
    fi

    if ! command -v apk &>/dev/null; then
        echo "ERROR: Native mode requires apk (Alpine package manager)."
        echo "Use Docker mode instead: ./images/build-alpine.sh"
        exit 1
    fi

    WORKDIR=$(mktemp -d /tmp/agentiso-build.XXXXXX)
    MOUNTPOINT="$WORKDIR/rootfs"

    cleanup() {
        set +e
        if mountpoint -q "$MOUNTPOINT/proc" 2>/dev/null; then
            umount "$MOUNTPOINT/proc"
        fi
        if mountpoint -q "$MOUNTPOINT/sys" 2>/dev/null; then
            umount "$MOUNTPOINT/sys"
        fi
        if mountpoint -q "$MOUNTPOINT/dev" 2>/dev/null; then
            umount "$MOUNTPOINT/dev"
        fi
        if mountpoint -q "$MOUNTPOINT" 2>/dev/null; then
            umount "$MOUNTPOINT"
        fi
        if [ -n "${LOOP_DEV:-}" ]; then
            losetup -d "$LOOP_DEV" 2>/dev/null
        fi
        rm -rf "$WORKDIR"
    }
    trap 'cleanup' EXIT

    echo "==> Creating raw disk image ($IMAGE_SIZE)"
    truncate -s "$IMAGE_SIZE" "$OUTPUT"

    echo "==> Setting up loop device"
    LOOP_DEV=$(losetup --find --show "$OUTPUT")
    echo "    Loop device: $LOOP_DEV"

    echo "==> Creating ext4 filesystem"
    mkfs.ext4 -q -L agentiso-root "$LOOP_DEV"

    echo "==> Mounting filesystem"
    mkdir -p "$MOUNTPOINT"
    mount "$LOOP_DEV" "$MOUNTPOINT"

    echo "==> Installing Alpine base system"
    mkdir -p "$MOUNTPOINT/etc/apk"
    echo "$ALPINE_MIRROR/v$ALPINE_VERSION/main" > "$MOUNTPOINT/etc/apk/repositories"
    echo "$ALPINE_MIRROR/v$ALPINE_VERSION/community" >> "$MOUNTPOINT/etc/apk/repositories"

    apk add --root "$MOUNTPOINT" --initdb --no-cache \
        alpine-base \
        openrc \
        busybox-initscripts \
        iproute2 \
        iptables \
        ca-certificates \
        tzdata

    echo "==> Installing dev tools"
    apk add --root "$MOUNTPOINT" --no-cache \
        build-base \
        git \
        python3 \
        py3-pip \
        nodejs \
        npm \
        curl \
        wget \
        jq \
        bash \
        openssh-client \
        vim \
        strace

    echo "==> Configuring system"
    mount -t proc proc "$MOUNTPOINT/proc"
    mount -t sysfs sysfs "$MOUNTPOINT/sys"
    mount --bind /dev "$MOUNTPOINT/dev"

    write_fstab "$MOUNTPOINT"
    echo "agentiso" > "$MOUNTPOINT/etc/hostname"
    write_inittab "$MOUNTPOINT"
    write_interfaces "$MOUNTPOINT"

    # Disable unnecessary services
    chroot "$MOUNTPOINT" rc-update del networking boot 2>/dev/null || true
    chroot "$MOUNTPOINT" rc-update del hwclock boot 2>/dev/null || true

    # Enable essential services
    chroot "$MOUNTPOINT" rc-update add devfs sysinit
    chroot "$MOUNTPOINT" rc-update add mdev sysinit
    chroot "$MOUNTPOINT" rc-update add hostname boot

    echo "==> Installing agentiso-guest agent"
    install -m 0755 "$GUEST_AGENT" "$MOUNTPOINT/usr/local/bin/agentiso-guest"
    write_guest_agent_initscript "$MOUNTPOINT"
    chroot "$MOUNTPOINT" rc-update add agentiso-guest default

    # Set root password to empty (agent-only access via vsock)
    chroot "$MOUNTPOINT" passwd -d root

    # Create workspace directory
    mkdir -p "$MOUNTPOINT/workspace"

    echo "==> Unmounting"
    umount "$MOUNTPOINT/proc"
    umount "$MOUNTPOINT/sys"
    umount "$MOUNTPOINT/dev"
    umount "$MOUNTPOINT"
    losetup -d "$LOOP_DEV"
    unset LOOP_DEV

    echo "==> Done: $OUTPUT"
    echo "    Image size: $(du -h "$OUTPUT" | cut -f1)"
    echo ""
    echo "    To import into ZFS:"
    echo "    zfs create -V ${IMAGE_SIZE} tank/agentiso/base/alpine-dev"
    echo "    dd if=$OUTPUT of=/dev/zvol/tank/agentiso/base/alpine-dev bs=4M"
    echo "    zfs snapshot tank/agentiso/base/alpine-dev@latest"
}

# ============================================================================
# Main
# ============================================================================

if [ "$NATIVE_MODE" = "true" ]; then
    build_native
else
    build_docker
fi
