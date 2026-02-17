#!/usr/bin/env bash
# Quick manual boot test - run with sudo, Ctrl-A X to exit QEMU
set -euo pipefail

ZFS_POOL="agentiso"
ZVOL_BASE="${ZFS_POOL}/agentiso/base/alpine-dev@latest"
ZVOL_WS="${ZFS_POOL}/agentiso/workspaces/ws-manualtest"
TAP_DEV="tap-manualt"
KERNEL="/var/lib/agentiso/vmlinuz"
INITRD="/var/lib/agentiso/initrd.img"

# Setup
zfs list "$ZVOL_WS" &>/dev/null && zfs destroy -r "$ZVOL_WS"
zfs clone "$ZVOL_BASE" "$ZVOL_WS"
sleep 1; udevadm settle 2>/dev/null || sleep 2

ip link del "$TAP_DEV" 2>/dev/null || true
ip tuntap add "$TAP_DEV" mode tap
ip link set "$TAP_DEV" master br-agentiso
ip link set "$TAP_DEV" up

echo "Booting QEMU microvm... (Ctrl-A X to exit)"
echo ""

INITRD_ARGS=""
[[ -f "$INITRD" ]] && INITRD_ARGS="-initrd $INITRD"

qemu-system-x86_64 \
    -M microvm,rtc=on \
    -cpu host -enable-kvm \
    -m 512 -smp 2 \
    -nodefaults -no-user-config \
    -kernel "$KERNEL" \
    $INITRD_ARGS \
    -append "console=ttyS0 root=/dev/vda rw earlyprintk=ttyS0" \
    -drive id=root,file="/dev/zvol/${ZVOL_WS}",format=raw,if=none \
    -device virtio-blk-device,drive=root \
    -netdev tap,id=net0,ifname="$TAP_DEV",script=no,downscript=no \
    -device virtio-net-device,netdev=net0 \
    -chardev stdio,id=serial0,mux=on \
    -device isa-serial,chardev=serial0 \
    -mon chardev=serial0 \
    -nographic

# Cleanup
echo ""
echo "Cleaning up..."
ip link del "$TAP_DEV" 2>/dev/null || true
zfs destroy -r "$ZVOL_WS" 2>/dev/null || true
echo "Done"
