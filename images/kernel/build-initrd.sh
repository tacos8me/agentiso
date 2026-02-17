#!/usr/bin/env bash
#
# Build a minimal initrd (~1MB) for fast VM boot.
# Contains only busybox + vsock kernel modules.
#
# Output: /var/lib/agentiso/initrd-fast.img
#
set -euo pipefail

OUTPUT="${1:-/var/lib/agentiso/initrd-fast.img}"
KVER="$(uname -r)"
KMOD_SRC="/lib/modules/$KVER"
WORKDIR=$(mktemp -d)

echo "Building minimal initrd for kernel $KVER..."

# Create directory structure
mkdir -p "$WORKDIR"/{bin,lib/modules,dev,proc,sys,mnt/root}

# Copy busybox (statically linked)
BUSYBOX=$(which busybox)
if [[ ! -f "$BUSYBOX" ]]; then
    echo "ERROR: busybox not found"
    exit 1
fi
cp "$BUSYBOX" "$WORKDIR/bin/busybox"

# Create symlinks for required applets
for cmd in sh mount insmod switch_root; do
    ln -sf busybox "$WORKDIR/bin/$cmd"
done

# Copy and decompress vsock modules
for mod_name in vsock vmw_vsock_virtio_transport_common vmw_vsock_virtio_transport; do
    src=$(find "$KMOD_SRC" -name "${mod_name}.ko*" 2>/dev/null | head -1)
    if [[ -z "$src" ]]; then
        echo "WARNING: module $mod_name not found"
        continue
    fi
    if [[ "$src" == *.zst ]]; then
        zstd -dq "$src" -o "$WORKDIR/lib/modules/${mod_name}.ko"
    elif [[ "$src" == *.xz ]]; then
        xz -dkc "$src" > "$WORKDIR/lib/modules/${mod_name}.ko"
    elif [[ "$src" == *.gz ]]; then
        gzip -dkc "$src" > "$WORKDIR/lib/modules/${mod_name}.ko"
    else
        cp "$src" "$WORKDIR/lib/modules/"
    fi
done

# Create init script
cat > "$WORKDIR/init" << 'INITEOF'
#!/bin/busybox sh
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
# vsock modules
/bin/busybox insmod /lib/modules/vsock.ko 2>/dev/null
/bin/busybox insmod /lib/modules/vmw_vsock_virtio_transport_common.ko 2>/dev/null
/bin/busybox insmod /lib/modules/vmw_vsock_virtio_transport.ko 2>/dev/null
# Mount root and switch
/bin/busybox mount -t ext4 /dev/vda /mnt/root
exec /bin/busybox switch_root /mnt/root /sbin/init-fast
INITEOF
chmod 755 "$WORKDIR/init"

# Create cpio archive compressed with gzip
echo "Creating compressed cpio archive..."
(cd "$WORKDIR" && find . | cpio -o -H newc --quiet | gzip -9) > "$OUTPUT"

# Cleanup
rm -rf "$WORKDIR"

SIZE=$(ls -lh "$OUTPUT" | awk '{print $5}')
echo "Built: $OUTPUT ($SIZE)"
