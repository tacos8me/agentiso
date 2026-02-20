#!/bin/bash
# cleanup-workspaces.sh — Destroy all workspaces, teams, and orphan resources.
# Usage: sudo ./scripts/cleanup-workspaces.sh
set -euo pipefail

echo "=== agentiso workspace cleanup ==="

# 1. Kill any QEMU processes
echo "Killing QEMU processes..."
pkill -f 'qemu-system.*agentiso' 2>/dev/null || true
sleep 1

# 2. Destroy ZFS workspace datasets
echo "Destroying ZFS workspace datasets..."
for ds in $(zfs list -H -o name -r agentiso/agentiso/workspaces 2>/dev/null | tail -n +2); do
    echo "  zfs destroy -r $ds"
    zfs destroy -r "$ds" 2>/dev/null || true
done

# 3. Destroy ZFS fork datasets
echo "Destroying ZFS fork datasets..."
for ds in $(zfs list -H -o name -r agentiso/agentiso/forks 2>/dev/null | tail -n +2); do
    echo "  zfs destroy -r $ds"
    zfs destroy -r "$ds" 2>/dev/null || true
done

# 4. Clear state file (preserve structure, zero out workspaces/teams)
STATE_FILE="/var/lib/agentiso/state.json"
if [ -f "$STATE_FILE" ]; then
    echo "Clearing state file..."
    python3 -c "
import json
with open('$STATE_FILE') as f:
    state = json.load(f)
state['workspaces'] = {}
state['teams'] = {}
with open('$STATE_FILE', 'w') as f:
    json.dump(state, f, indent=2)
print('  State cleared: 0 workspaces, 0 teams')
"
fi

# 5. Clean up orphan TAP devices
echo "Cleaning up TAP devices..."
for tap in $(ip -o link show 2>/dev/null | grep -oP 'tap-[0-9a-f]+' | sort -u); do
    echo "  deleting $tap"
    ip link delete "$tap" 2>/dev/null || true
done

echo "=== Done ==="
