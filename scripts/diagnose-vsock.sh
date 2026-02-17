#!/usr/bin/env bash
# One-shot vsock diagnostic: boots VM, tests vsock, inspects guest filesystem
set -euo pipefail

if [[ $EUID -ne 0 ]]; then echo "Run with sudo"; exit 1; fi

ZFS_POOL="agentiso"
ZVOL_BASE="${ZFS_POOL}/agentiso/base/alpine-dev@latest"
ZVOL_WS="${ZFS_POOL}/agentiso/workspaces/ws-diag"
ZVOL_DEV="/dev/zvol/${ZVOL_WS}"
TAP_DEV="tap-diag"
KERNEL="/var/lib/agentiso/vmlinuz"
INITRD="/var/lib/agentiso/initrd.img"
QMP_SOCK="/tmp/agentiso-diag-qmp.sock"
MNT="/mnt/agentiso-diag"
VSOCK_CID=101
GUEST_PORT=5000

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

echo "=== Booting VM (CID=$VSOCK_CID) ==="
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
    -device vhost-vsock-device,guest-cid="$VSOCK_CID" \
    -chardev socket,id=qmp0,path="$QMP_SOCK",server=on,wait=off \
    -mon chardev=qmp0,mode=control \
    -chardev stdio,id=serial0 \
    -device isa-serial,chardev=serial0 \
    -nographic -nodefaults \
    </dev/null &>/tmp/agentiso-diag-qemu.log &
QEMU_PID=$!

echo "QEMU PID: $QEMU_PID"
echo "Waiting 20s for full boot..."
sleep 20

if ! kill -0 "$QEMU_PID" 2>/dev/null; then
    echo "QEMU died!"
    cat /tmp/agentiso-diag-qemu.log
    exit 1
fi

echo ""
echo "=== Test 1: socat vsock (verbose) ==="
timeout 3 bash -c "echo -n '' | socat -d -d - VSOCK-CONNECT:${VSOCK_CID}:${GUEST_PORT}" 2>&1 || echo "(socat exited with $?)"

echo ""
echo "=== Test 2: Python vsock connect ==="
timeout 5 python3 -c "
import socket, struct, json, sys
try:
    s = socket.socket(40, socket.SOCK_STREAM)
    s.settimeout(3)
    print('Connecting to CID=${VSOCK_CID} port=${GUEST_PORT}...')
    s.connect((${VSOCK_CID}, ${GUEST_PORT}))
    print('Connected!')
    msg = json.dumps({'type':'Ping'}).encode()
    frame = struct.pack('>I', len(msg)) + msg
    print(f'Sending {len(frame)} bytes: {frame}')
    s.sendall(frame)
    print('Sent, reading response...')
    raw_len = s.recv(4)
    if len(raw_len) < 4:
        print(f'Short read on length: got {len(raw_len)} bytes')
        sys.exit(1)
    resp_len = struct.unpack('>I', raw_len)[0]
    print(f'Response length: {resp_len}')
    resp = s.recv(resp_len)
    print(f'Response: {resp.decode()}')
    s.close()
    print('SUCCESS')
except Exception as e:
    print(f'FAILED: {e}')
" 2>&1 || echo "(python exited with $?)"

echo ""
echo "=== Killing VM ==="
kill "$QEMU_PID" 2>/dev/null || true
wait "$QEMU_PID" 2>/dev/null || true
QEMU_PID=""
sleep 1

echo ""
echo "=== Mounting zvol for inspection ==="
mkdir -p "$MNT"
mount -o ro "$ZVOL_DEV" "$MNT"

echo ""
echo "--- Guest agent log ---"
cat "$MNT/var/log/agentiso-guest.log" 2>/dev/null || echo "(no log file)"

echo ""
echo "--- Guest agent binary info ---"
file "$MNT/usr/local/bin/agentiso-guest" 2>/dev/null
ls -la "$MNT/usr/local/bin/agentiso-guest" 2>/dev/null
md5sum "$MNT/usr/local/bin/agentiso-guest" 2>/dev/null

echo ""
echo "--- Host agent binary info ---"
PROJ="/mnt/nvme-1/projects/agentiso"
ls -la "$PROJ/target/x86_64-unknown-linux-musl/release/agentiso-guest" 2>/dev/null
md5sum "$PROJ/target/x86_64-unknown-linux-musl/release/agentiso-guest" 2>/dev/null

echo ""
echo "--- Vsock modules on guest filesystem ---"
KVER=$(uname -r)
echo "Kernel: $KVER"
ls -la "$MNT/lib/modules/$KVER/kernel/net/vmw_vsock/" 2>/dev/null || echo "(no vsock modules dir)"
file "$MNT/lib/modules/$KVER/kernel/net/vmw_vsock/"*.ko 2>/dev/null || echo "(no .ko files)"

echo ""
echo "--- Init script (start_pre) ---"
grep -A10 "start_pre" "$MNT/etc/init.d/agentiso-guest" 2>/dev/null || echo "(no start_pre)"

echo ""
echo "--- /etc/modules-load.d/ ---"
cat "$MNT/etc/modules-load.d/agentiso.conf" 2>/dev/null || echo "(no modules-load.d conf)"

echo ""
echo "--- local.d modules script ---"
cat "$MNT/etc/local.d/agentiso-modules.start" 2>/dev/null || echo "(no local.d script)"

echo ""
echo "--- QEMU console log (last 20 lines) ---"
tail -20 /tmp/agentiso-diag-qemu.log

echo ""
echo "=== Done ==="
