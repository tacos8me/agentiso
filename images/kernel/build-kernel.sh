#!/usr/bin/env bash
#
# build-kernel.sh - Build a minimal Linux kernel for agentiso microvm.
#
# Produces a stripped vmlinux binary with only the drivers needed for
# QEMU microvm: virtio-blk, virtio-net, virtio-vsock, and ext4.
# No modules, no initrd - boots directly into the root filesystem.
#
# Prerequisites:
#   - Build dependencies: gcc, make, flex, bison, libelf-dev, bc, libssl-dev
#   - Internet access to download kernel source
#
# Usage:
#   ./images/kernel/build-kernel.sh [kernel-version]
#
# Output:
#   images/kernel/vmlinux

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL_VERSION="${1:-6.6.75}"
KERNEL_MAJOR=$(echo "$KERNEL_VERSION" | cut -d. -f1)
KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_MAJOR}.x/linux-${KERNEL_VERSION}.tar.xz"
BUILD_DIR="$SCRIPT_DIR/build"
OUTPUT="$SCRIPT_DIR/vmlinux"
NPROC=$(nproc)

# Check for required build tools
for tool in gcc make flex bison bc; do
    if ! command -v "$tool" &>/dev/null; then
        echo "ERROR: Required build tool '$tool' not found."
        echo "Install with: sudo apt install build-essential flex bison bc libelf-dev libssl-dev"
        exit 1
    fi
done

echo "==> Building Linux kernel $KERNEL_VERSION for agentiso microvm"

mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

if [ ! -d "linux-$KERNEL_VERSION" ]; then
    echo "==> Downloading kernel source"
    if [ ! -f "linux-${KERNEL_VERSION}.tar.xz" ]; then
        curl -L -o "linux-${KERNEL_VERSION}.tar.xz" "$KERNEL_URL"
    fi
    echo "==> Extracting"
    tar xf "linux-${KERNEL_VERSION}.tar.xz"
fi

cd "linux-$KERNEL_VERSION"

echo "==> Generating minimal config"
make allnoconfig

# Write the microvm kernel config fragment
cat > agentiso.config << 'KCONFIG'
# Base
CONFIG_64BIT=y
CONFIG_SMP=y
CONFIG_PRINTK=y
CONFIG_BUG=y
CONFIG_SYSFS=y
CONFIG_PROC_FS=y
CONFIG_TMPFS=y
CONFIG_DEVTMPFS=y
CONFIG_DEVTMPFS_MOUNT=y
CONFIG_BINFMT_ELF=y
CONFIG_BINFMT_SCRIPT=y

# Kernel features
CONFIG_HIGH_RES_TIMERS=y
CONFIG_NO_HZ_IDLE=y
CONFIG_POSIX_TIMERS=y
CONFIG_FUTEX=y
CONFIG_EPOLL=y
CONFIG_SIGNALFD=y
CONFIG_TIMERFD=y
CONFIG_EVENTFD=y
CONFIG_SHMEM=y
CONFIG_AIO=y
CONFIG_IO_URING=y
CONFIG_ADVISE_SYSCALLS=y
CONFIG_MEMBARRIER=y
CONFIG_KALLSYMS=y
CONFIG_MULTIUSER=y
CONFIG_SYSVIPC=y
CONFIG_POSIX_MQUEUE=y
CONFIG_CROSS_MEMORY_ATTACH=y
CONFIG_INOTIFY_USER=y
CONFIG_FHANDLE=y

# Block layer
CONFIG_BLOCK=y

# TTY / Console
CONFIG_TTY=y
CONFIG_VT=n
CONFIG_SERIAL_8250=n
CONFIG_HVC_DRIVER=y
CONFIG_VIRTIO_CONSOLE=y

# Filesystem
CONFIG_EXT4_FS=y
CONFIG_EXT4_USE_FOR_EXT2=y

# Pseudo-filesystems
CONFIG_PROC_SYSCTL=y
CONFIG_KERNFS=y

# PCI (needed for virtio-pci)
CONFIG_PCI=y
CONFIG_PCI_HOST_GENERIC=y

# Virtio (all built-in, no modules)
CONFIG_VIRTIO=y
CONFIG_VIRTIO_PCI=y
CONFIG_VIRTIO_BLK=y
CONFIG_VIRTIO_NET=y
CONFIG_VIRTIO_MMIO=y

# Vsock support (guest side)
CONFIG_VSOCKETS=y
CONFIG_VIRTIO_VSOCKETS=y
CONFIG_VIRTIO_VSOCKETS_COMMON=y
# VHOST_VSOCK is for the host side, not needed in guest kernel
CONFIG_VHOST_VSOCK=n

# Networking
CONFIG_NET=y
CONFIG_INET=y
CONFIG_PACKET=y
CONFIG_UNIX=y
CONFIG_NETFILTER=y
CONFIG_IP_NF_IPTABLES=y
CONFIG_IP_NF_FILTER=y
CONFIG_NF_CONNTRACK=y
CONFIG_NF_NAT=y

# RTC (microvm needs rtc=on)
CONFIG_RTC_CLASS=y
CONFIG_RTC_HCTOSYS=y

# cgroups (for resource limits)
CONFIG_CGROUPS=y
CONFIG_CGROUP_CPUACCT=y
CONFIG_CGROUP_SCHED=y
CONFIG_MEMCG=y
CONFIG_BLK_CGROUP=y
CONFIG_CGROUP_PIDS=y

# Security
CONFIG_SECCOMP=y
CONFIG_SECCOMP_FILTER=y

# No modules - everything built-in
CONFIG_MODULES=n

# No initrd
CONFIG_BLK_DEV_INITRD=n

# Kernel compression
CONFIG_KERNEL_XZ=y
KCONFIG

echo "==> Merging config fragment"
scripts/kconfig/merge_config.sh -m .config agentiso.config

# Run olddefconfig to resolve any missing dependencies
make olddefconfig

echo "==> Building kernel (using $NPROC cores)"
make -j"$NPROC" vmlinux

echo "==> Stripping debug symbols"
strip --strip-debug vmlinux

echo "==> Copying output"
cp vmlinux "$OUTPUT"

VMLINUX_SIZE=$(du -h "$OUTPUT" | cut -f1)
echo "==> Done: $OUTPUT ($VMLINUX_SIZE)"
echo "    Install with: sudo cp $OUTPUT /var/lib/agentiso/vmlinux"
