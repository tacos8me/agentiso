#!/bin/bash
# Comprehensive network diagnostics for agentiso VMs
# Run as: sudo ./scripts/diagnose-network.sh
set -e

echo "=== agentiso Network Diagnostics ==="
echo

# 1. Bridge status
echo "--- Bridge ---"
ip addr show br-agentiso 2>/dev/null || echo "MISSING: br-agentiso does not exist"
echo

# 2. TAP devices
echo "--- TAP devices on bridge ---"
bridge link show br-agentiso 2>/dev/null | grep tap || echo "No TAP devices on bridge"
echo

# 3. ARP table
echo "--- ARP table (br-agentiso) ---"
ip neigh show dev br-agentiso
echo

# 4. IP forwarding
echo "--- IP Forwarding ---"
sysctl net.ipv4.ip_forward
echo

# 5. bridge-nf-call-iptables (global + per-bridge)
echo "--- bridge-nf-call-iptables ---"
sysctl net.bridge.bridge-nf-call-iptables 2>/dev/null || echo "global: not loaded"
for br in br-agentiso docker0; do
    val=$(cat /sys/class/net/$br/bridge/nf_call_iptables 2>/dev/null)
    if [ -n "$val" ]; then
        echo "  $br: nf_call_iptables=$val (0=disabled, 1=enabled)"
    fi
done
echo

# 6. iptables FORWARD chain
echo "--- iptables FORWARD chain (first 20 rules) ---"
iptables -L FORWARD -v -n --line-numbers 2>&1 | head -25
echo

# 7. iptables NAT/POSTROUTING
echo "--- iptables NAT POSTROUTING ---"
iptables -t nat -L POSTROUTING -v -n 2>&1 | head -15
echo

# 8. nftables rules
echo "--- nftables agentiso table ---"
nft list table inet agentiso 2>/dev/null || echo "No agentiso nftables table found"
echo

# 9. Workspace state
echo "--- Workspace state ---"
STATE=/var/lib/agentiso/state.json
if [ -f "$STATE" ]; then
    python3 -c "
import json
with open('$STATE') as f:
    state = json.load(f)
for wid, ws in state.get('workspaces', {}).items():
    n = ws.get('network', {})
    print(f\"  {wid[:8]} ip={n.get('ip')} internet={n.get('allow_internet')} tap={ws.get('tap_device')} state={ws.get('state')}\")
" 2>/dev/null || echo "Failed to parse state.json"
else
    echo "No state file found"
fi
echo

# 10. Host ping to VMs
echo "--- Host ping to VMs ---"
for ip in $(python3 -c "
import json
with open('$STATE') as f:
    state = json.load(f)
for ws in state.get('workspaces', {}).values():
    if ws.get('state') == 'running':
        print(ws['network']['ip'])
" 2>/dev/null); do
    echo -n "  ping $ip: "
    ping -c 1 -W 1 "$ip" >/dev/null 2>&1 && echo "OK" || echo "FAIL"
done
echo

# 11. Guest network config (via exec through vsock - need running server)
echo "--- Guest diagnostics (via MCP exec) ---"
echo "  Run these in your MCP client:"
echo "    exec: ip addr show eth0"
echo "    exec: ip route show"
echo "    exec: ping -c 1 10.99.0.1   (gateway)"
echo "    exec: ping -c 1 1.1.1.1     (internet)"
echo "    exec: cat /etc/resolv.conf"
echo

# 12. tcpdump hint
echo "--- Quick tcpdump check ---"
echo "  To trace packets live, run in another terminal:"
echo "    sudo tcpdump -i br-agentiso -n icmp"
echo "  Then ping from guest. If packets appear but no reply, it's iptables."
echo "  If no packets appear at all, guest network is misconfigured."
echo
echo "=== Done ==="
