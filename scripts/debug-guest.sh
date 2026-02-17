#!/usr/bin/env bash
# Debug: boot VM, wait, kill, mount zvol, check logs
set -euo pipefail

if [[ $EUID -ne 0 ]]; then echo "Run with sudo"; exit 1; fi

ZFS_POOL="agentiso"
ZVOL_BASE="${ZFS_POOL}/agentiso/base/alpine-dev@latest"
ZVOL_WS="${ZFS_POOL}/agentiso/workspaces/ws-debug"
ZVOL_DEV="/dev/zvol/${ZVOL_WS}"
TAP_DEV="tap-debug"
KERNEL="/var/lib/agentiso/vmlinuz"
INITRD="/var/lib/agentiso/initrd.img"
QMP_SOCK="/tmp/agentiso-debug-qmp.sock"
MNT="/mnt/agentiso-debug"

cleanup() {
    kill "$QEMU_PID" 2>/dev/null || true
    wait "$QEMU_PID" 2>/dev/null || true
    umount "$MNT" 2>/dev/null || true
    ip link del "$TAP_DEV" 2>/dev/null || true
    rm -f "$QMP_SOCK"
    zfs destroy -R "$ZVOL_WS" 2>/dev/null || true
    rmdir "$MNT" 2>/dev/null || true
}
trap cleanup EXIT

# Setup
zfs list "$ZVOL_WS" &>/dev/null && zfs destroy -R "$ZVOL_WS"
zfs clone "$ZVOL_BASE" "$ZVOL_WS"
sleep 1; udevadm settle 2>/dev/null || sleep 2

ip link del "$TAP_DEV" 2>/dev/null || true
ip tuntap add "$TAP_DEV" mode tap
ip link set "$TAP_DEV" master br-agentiso
ip link set "$TAP_DEV" up

INITRD_ARGS=""
[[ -f "$INITRD" ]] && INITRD_ARGS="-initrd $INITRD"

# Boot VM in background
echo "=== Booting VM ==="
qemu-system-x86_64 \
    -M microvm,rtc=on \
    -cpu host -enable-kvm \
    -m 512 -smp 2 \
    -kernel "$KERNEL" \
    $INITRD_ARGS \
    -append "console=ttyS0 root=/dev/vda rw quiet" \
    -drive id=root,file="$ZVOL_DEV",format=raw,if=none \
    -device virtio-blk-device,drive=root \
    -netdev tap,id=net0,ifname="$TAP_DEV",script=no,downscript=no \
    -device virtio-net-device,netdev=net0 \
    -device vhost-vsock-device,guest-cid=100 \
    -chardev socket,id=qmp0,path="$QMP_SOCK",server=on,wait=off \
    -mon chardev=qmp0,mode=control \
    -chardev stdio,id=serial0 \
    -device isa-serial,chardev=serial0 \
    -nographic -nodefaults \
    </dev/null &>/tmp/agentiso-debug-qemu.log &
QEMU_PID=$!

echo "QEMU PID: $QEMU_PID"
echo "Waiting 15s for VM to boot..."
sleep 15

if ! kill -0 "$QEMU_PID" 2>/dev/null; then
    echo "QEMU died! Log:"
    tail -30 /tmp/agentiso-debug-qemu.log
    exit 1
fi

echo ""
echo "=== Attempting vsock connection ==="
timeout 2 bash -c "echo -n '' | socat -d -d - VSOCK-CONNECT:100:5000" 2>&1 || echo "(vsock connection failed)"

echo ""
echo "=== Killing VM ==="
kill "$QEMU_PID" 2>/dev/null || true
wait "$QEMU_PID" 2>/dev/null || true
QEMU_PID=""
sleep 1

echo ""
echo "=== Mounting zvol to inspect guest filesystem ==="
mkdir -p "$MNT"
mount -o ro "$ZVOL_DEV" "$MNT"

echo ""
echo "--- Guest agent log ---"
cat "$MNT/var/log/agentiso-guest.log" 2>/dev/null || echo "(no log file)"

echo ""
echo "--- Check vsock modules exist ---"
find "$MNT/lib/modules/" -name "vsock*" -o -name "vmw_vsock*" 2>/dev/null || echo "(no modules found)"

echo ""
echo "--- Check init script ---"
grep -A5 "start_pre" "$MNT/etc/init.d/agentiso-guest" 2>/dev/null || echo "(no start_pre)"

echo ""
echo "--- Last 20 lines of QEMU log ---"
tail -20 /tmp/agentiso-debug-qemu.log

echo ""
echo "=== Done ==="
