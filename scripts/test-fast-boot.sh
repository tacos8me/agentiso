#!/usr/bin/env bash
#
# Test fast boot mode vs standard OpenRC boot.
# Measures time from QEMU spawn to guest agent vsock readiness.
#
# Run with: sudo ./scripts/test-fast-boot.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# Configuration
ZFS_POOL="agentiso"
DATASET_PREFIX="agentiso"
KERNEL="/var/lib/agentiso/vmlinuz"
INITRD_STANDARD="/var/lib/agentiso/initrd.img"
INITRD_FAST="/var/lib/agentiso/initrd-fast.img"
BRIDGE="br-agentiso"
VSOCK_CID=100
GUEST_PORT=5000

# Ensure running as root
if [[ $EUID -ne 0 ]]; then
    echo "ERROR: This script must be run with sudo"
    exit 1
fi

# ---------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------

cleanup_vm() {
    local pid="${1:-}"
    local tap="${2:-}"
    local zvol_ds="${3:-}"
    local run_dir="${4:-}"

    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill -9 "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    if [[ -n "$tap" ]]; then
        ip link del "$tap" 2>/dev/null || true
    fi
    if [[ -n "$zvol_ds" ]]; then
        zfs destroy -f "$zvol_ds" 2>/dev/null || true
    fi
    if [[ -n "$run_dir" ]]; then
        rm -rf "$run_dir" 2>/dev/null || true
    fi
}

# Time a full boot cycle: clone → TAP → QEMU → vsock ready
# Args: $1=test_name $2=initrd_path $3=extra_append
boot_and_measure() {
    local test_name="$1"
    local initrd_path="$2"
    local extra_append="${3:-}"
    local ws_id="fast-test-$$"
    local zvol_ds="${ZFS_POOL}/${DATASET_PREFIX}/workspaces/ws-${ws_id}"
    local zvol_dev="/dev/zvol/${zvol_ds}"
    local tap_dev="tap-${ws_id:0:8}"
    local run_dir="/tmp/agentiso-fast-test-${ws_id}"
    local qmp_sock="${run_dir}/qmp.sock"
    local log_file="/tmp/agentiso-${test_name}.log"
    local qemu_pid=""

    mkdir -p "$run_dir"

    # Cleanup on exit
    trap "cleanup_vm '$qemu_pid' '$tap_dev' '$zvol_ds' '$run_dir'" EXIT

    echo ""
    echo "=== $test_name ==="

    # 1. ZFS clone
    local t0=$(date +%s%N)
    zfs clone "${ZFS_POOL}/${DATASET_PREFIX}/base/alpine-dev@latest" "$zvol_ds"
    udevadm settle 2>/dev/null || sleep 1
    local t_zfs=$(date +%s%N)

    # 2. TAP setup
    ip tuntap add "$tap_dev" mode tap 2>/dev/null
    ip link set "$tap_dev" master "$BRIDGE" 2>/dev/null
    ip link set "$tap_dev" up 2>/dev/null
    local t_tap=$(date +%s%N)

    # 3. Build QEMU command
    local append="console=ttyS0 root=/dev/vda rw quiet${extra_append}"
    local initrd_args=""
    [[ -f "$initrd_path" ]] && initrd_args="-initrd $initrd_path"

    # 4. Launch QEMU
    qemu-system-x86_64 \
        -M microvm,rtc=on \
        -cpu host -enable-kvm \
        -m 512 -smp 2 \
        -kernel "$KERNEL" \
        $initrd_args \
        -append "$append" \
        -drive id=root,file="$zvol_dev",format=raw,if=none \
        -device virtio-blk-device,drive=root \
        -netdev tap,id=net0,ifname="$tap_dev",script=no,downscript=no \
        -device virtio-net-device,netdev=net0 \
        -device vhost-vsock-device,guest-cid="$VSOCK_CID" \
        -chardev socket,id=qmp0,path="$qmp_sock",server=on,wait=off \
        -mon chardev=qmp0,mode=control \
        -chardev stdio,id=serial0 \
        -device isa-serial,chardev=serial0 \
        -nographic -nodefaults \
        </dev/null &>"$log_file" &
    qemu_pid=$!

    # Update trap with actual PID
    trap "cleanup_vm '$qemu_pid' '$tap_dev' '$zvol_ds' '$run_dir'" EXIT

    local t_qemu=$(date +%s%N)

    # 5. Wait for guest agent via vsock
    local booted=false
    for i in $(seq 1 30); do
        if ! kill -0 "$qemu_pid" 2>/dev/null; then
            echo "  QEMU died during boot!"
            tail -20 "$log_file"
            break
        fi

        if timeout 1 python3 -c "
import socket
s = socket.socket(40, socket.SOCK_STREAM)
s.settimeout(0.5)
s.connect(($VSOCK_CID, $GUEST_PORT))
s.close()
" 2>/dev/null; then
            booted=true
            break
        fi
        sleep 0.2
    done

    local t_ready=$(date +%s%N)

    # Calculate timings (in milliseconds)
    local ms_zfs=$(( (t_zfs - t0) / 1000000 ))
    local ms_tap=$(( (t_tap - t_zfs) / 1000000 ))
    local ms_qemu=$(( (t_qemu - t_tap) / 1000000 ))
    local ms_boot=$(( (t_ready - t_qemu) / 1000000 ))
    local ms_total=$(( (t_ready - t0) / 1000000 ))

    if $booted; then
        echo "  RESULT: Guest agent ready!"
        echo ""
        echo "  Breakdown:"
        echo "    ZFS clone:    ${ms_zfs}ms"
        echo "    TAP setup:    ${ms_tap}ms"
        echo "    QEMU spawn:   ${ms_qemu}ms"
        echo "    Kernel+init:  ${ms_boot}ms"
        echo "    ─────────────────────"
        echo "    TOTAL:        ${ms_total}ms"
    else
        echo "  FAIL: Guest agent not reachable after 30s"
        echo "  Last 30 lines of QEMU log:"
        tail -30 "$log_file"
    fi

    # Cleanup
    cleanup_vm "$qemu_pid" "$tap_dev" "$zvol_ds" "$run_dir"
    qemu_pid=""
    trap - EXIT

    echo ""
}

# ---------------------------------------------------------------
# Pre-flight checks
# ---------------------------------------------------------------

echo "=== agentiso fast boot test ==="
echo ""

# Check files exist
for f in "$KERNEL" "$INITRD_STANDARD"; do
    if [[ ! -f "$f" ]]; then
        echo "ERROR: $f not found. Run setup-e2e.sh first."
        exit 1
    fi
done

if [[ ! -f "$INITRD_FAST" ]]; then
    echo "Custom initrd not found at $INITRD_FAST."
    echo "Building it now..."
    "$SCRIPT_DIR/../images/kernel/build-initrd.sh" "$INITRD_FAST"
fi

echo "Kernel:          $(ls -lh "$KERNEL" | awk '{print $5}')"
echo "Standard initrd: $(ls -lh "$INITRD_STANDARD" | awk '{print $5}')"
echo "Fast initrd:     $(ls -lh "$INITRD_FAST" | awk '{print $5}')"

# ---------------------------------------------------------------
# Test 1: Standard boot (OpenRC)
# ---------------------------------------------------------------

boot_and_measure "STANDARD (OpenRC)" "$INITRD_STANDARD" ""

# ---------------------------------------------------------------
# Test 2: Fast boot (custom initrd + init-fast)
# ---------------------------------------------------------------

boot_and_measure "FAST (custom initrd + init-fast)" "$INITRD_FAST" " init=/sbin/init-fast"

echo "=== Done ==="
