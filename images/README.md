# agentiso Images

Build scripts for VM base images. Each image is stored as a ZFS zvol under
`agentiso/agentiso/base/` with a `@latest` snapshot that workspaces are cloned from.

## Available Images

| Image | Script | Description |
|-------|--------|-------------|
| `alpine-dev` | `build-alpine.sh` | Base Alpine Linux with dev tools (git, python3, nodejs, build-base) |
| `alpine-opencode` | `build-alpine-opencode.sh` | Alpine + [OpenCode](https://github.com/anomalyco/opencode) AI coding agent |
| `alpine-rust` | `build-alpine-rust.sh` | Alpine + Rust stable toolchain (rustup, cargo, clippy, rustfmt) |
| `alpine-python` | `build-alpine-python.sh` | Alpine + Python 3 (pip, venv, numpy, requests) |
| `alpine-node` | `build-alpine-node.sh` | Alpine + Node.js LTS (npm, yarn) |

## Build Order

All toolchain images extend `alpine-dev`, so you must build the base first:

```bash
# 1. Build base image (done automatically by setup-e2e.sh)
sudo ./scripts/setup-e2e.sh

# 2. Build any toolchain image (each is independent)
sudo ./images/build-alpine-opencode.sh
sudo ./images/build-alpine-rust.sh
sudo ./images/build-alpine-python.sh
sudo ./images/build-alpine-node.sh
```

## How It Works

Each script:
1. Clones the `alpine-dev@latest` ZFS zvol (instant copy-on-write)
2. Mounts the clone and installs packages via chroot
3. Unmounts and creates a `@latest` snapshot

Workspaces are then created by cloning a `@latest` snapshot, giving each VM
its own writable copy of the filesystem with near-zero overhead.

## ZFS Layout

```
agentiso/agentiso/base/
  alpine-dev          # Base image (2GB zvol)
  alpine-dev@latest   # Snapshot (source for all clones)
  alpine-opencode     # Clone of alpine-dev + opencode
  alpine-opencode@latest
  alpine-rust         # Clone of alpine-dev + rust toolchain
  alpine-rust@latest
  alpine-python       # Clone of alpine-dev + python toolchain
  alpine-python@latest
  alpine-node         # Clone of alpine-dev + node toolchain
  alpine-node@latest
```

## Kernel

The kernel and initramfs are separate from the rootfs images. They live at:
- `/var/lib/agentiso/vmlinuz` — Host kernel (bzImage)
- `/var/lib/agentiso/initrd.img` — Host initramfs (for virtio modules)
- `/var/lib/agentiso/initrd-fast.img` — Minimal initramfs (fast boot)

See `images/kernel/` for kernel/initrd build scripts.
