# Network Security Audit Report

**Auditor**: sec-network
**Date**: 2026-02-24
**Scope**: Network isolation, nftables rules, bridge configuration, inter-VM communication, MCP bridge authentication, port forwarding, IP forwarding scope

**Files Reviewed**:
- `agentiso/src/network/mod.rs` (NetworkManager, high-level orchestration)
- `agentiso/src/network/bridge.rs` (BridgeManager, TAP devices, IP forwarding, iptables rules)
- `agentiso/src/network/nftables.rs` (NftablesManager, per-workspace rules, team rules, MCP bridge rules)
- `agentiso/src/network/dhcp.rs` (IpAllocator)
- `agentiso/src/mcp/bridge.rs` (HTTP MCP bridge, auth middleware)
- `agentiso/src/mcp/auth.rs` (AuthManager, bridge tokens, session management)
- `agentiso/src/mcp/tools.rs` (port_forward, network_policy tool handlers)
- `agentiso/src/vm/mod.rs` (VmManager, QEMU launch, vsock)
- `agentiso/src/vm/microvm.rs` (QEMU command line construction)
- `agentiso/src/config.rs` (NetworkConfig, McpBridgeConfig, DashboardConfig)
- `config.example.toml`

---

## Summary

Overall the network isolation architecture is well-designed. The nftables forward chain has a **default-drop policy**, per-workspace rules use comment-based identification for clean lifecycle management, and IP forwarding is scoped per-interface rather than globally. The MCP bridge uses per-workspace bearer tokens with proper validation middleware.

However, several findings range from medium to high severity, primarily around: (1) lack of source-IP anti-spoofing rules on the bridge, (2) unrestricted DNAT prerouting that can redirect external traffic, (3) inter-VM isolation bypass via the `allow_inter_vm` flag granting bidirectional access when only unidirectional was intended, (4) the input chain's default-accept policy allowing VMs to probe host services, and (5) missing ARP-level protections.

**Findings**: 2 High, 5 Medium, 4 Low, 3 Info

---

## Findings

### NET-01: No Anti-Spoofing Rules on Bridge (Source IP Validation Missing)

**Severity**: HIGH

**File**: `agentiso/src/network/nftables.rs`, lines 91-125 (base ruleset) and lines 139-204 (workspace rules)

**Description**: The nftables ruleset does not enforce that a VM's outbound traffic uses the IP address that was allocated to it. A compromised VM can spoof its source IP to match another VM's IP, effectively impersonating that VM. This bypasses all per-workspace forward rules which match on `ip saddr`.

**Exploit scenario**:
1. VM-A is allocated IP 10.99.0.2, VM-B is allocated 10.99.0.3.
2. VM-B has internet access enabled; VM-A does not.
3. VM-A crafts packets with source IP 10.99.0.3.
4. The forward chain rule `ip saddr 10.99.0.3 ... accept` matches VM-A's spoofed packets, granting it internet access.
5. Alternatively, if team rules exist for 10.99.0.3 allowing traffic to 10.99.0.4, VM-A can reach 10.99.0.4 by spoofing as 10.99.0.3.

**Recommendation**: Add per-workspace anti-spoofing rules in the forward chain that tie each TAP interface to its allocated IP address. This ensures traffic entering from a specific TAP device can only have the correct source IP.

**Fix snippet** (add to `apply_workspace_rules` in `nftables.rs`, after line 151):

```rust
// Anti-spoofing: drop traffic from this VM's TAP that doesn't have the correct source IP.
// The TAP device name encodes the workspace ID, binding interface to IP.
let tap_name = crate::network::bridge::tap_device_name(workspace_id);
rules.push_str(&format!(
    "insert rule inet agentiso forward iifname \"{tap}\" ip saddr != {ip} drop comment \"ws-{id}-antispoof\"\n",
    tap = tap_name,
    ip = guest_ip,
    id = workspace_id,
));
```

---

### NET-02: DNAT Port Forwarding Rules Apply to All Interfaces (Not Scoped to Bridge)

**Severity**: HIGH

**File**: `agentiso/src/network/nftables.rs`, lines 210-245 (add_port_forward) and lines 741-757 (generate_port_forward_rules)

**Description**: The DNAT prerouting rules do not restrict which network interface the traffic arrives on. The rule:

```
add rule inet agentiso prerouting tcp dport {host_port} dnat ip to {ip}:{guest_port}
```

This matches traffic arriving on **any** interface (external-facing NICs, loopback, other bridges). An attacker on the network can reach VM services by connecting to the host's public IP on the forwarded port.

**Exploit scenario**:
1. A workspace forwards guest port 8080 to host port 9090.
2. The host has a public IP 203.0.113.5 on eth0.
3. An external attacker connects to 203.0.113.5:9090.
4. nftables DNAT redirects the connection to the VM at 10.99.0.2:8080.
5. The attacker now has direct access to the VM's service, bypassing any intended isolation.

**Recommendation**: Scope DNAT rules to only match traffic arriving on the bridge interface (for host-to-VM access) or localhost:

```rust
// Scope DNAT to bridge + localhost only
let rules = format!(
    concat!(
        "add rule inet agentiso prerouting iifname \"lo\" tcp dport {host_port} dnat ip to {ip}:{guest_port} comment \"ws-{id}-pf-{guest_port}\"\n",
        "add rule inet agentiso prerouting iifname \"{bridge}\" tcp dport {host_port} dnat ip to {ip}:{guest_port} comment \"ws-{id}-pf-bridge-{guest_port}\"\n",
        "add rule inet agentiso forward ip daddr {ip} tcp dport {guest_port} accept comment \"ws-{id}-pf-fwd-{guest_port}\"\n",
    ),
    // ...
);
```

Alternatively, add an `iifname != "eth0"` exclusion or bind specifically to `lo` and `br-agentiso`.

---

### NET-03: Input Chain Default-Accept Allows VMs to Probe Host Services

**Severity**: MEDIUM

**File**: `agentiso/src/network/nftables.rs`, lines 93-98 (base ruleset input chain)

**Description**: The input chain has `policy accept`:

```nft
chain input {
    type filter hook input priority 0; policy accept;
    # Per-workspace MCP bridge rules are inserted here
}
```

This means **all** VMs can reach **any** host service listening on the bridge IP (10.99.0.1), not just the MCP bridge port. Services like SSH (22), the dashboard (7070/8080), metrics, or any other daemon listening on 0.0.0.0 or the bridge IP are accessible from every VM.

The MCP bridge rules in the input chain are **additive allow** rules, but since the policy is already accept, they are redundant -- all traffic is already accepted.

**Exploit scenario**:
1. A VM scans the bridge IP 10.99.0.1 and finds SSH on port 22.
2. The VM attempts brute-force login to the host's SSH server.
3. Or: the VM accesses the dashboard API on port 7070 without auth (dashboard default has empty admin_token).

**Recommendation**: Change the input chain to `policy drop` and explicitly allow only required traffic:

```nft
chain input {
    type filter hook input priority 0; policy drop;

    # Allow established/related
    ct state established,related accept

    # Allow ICMP (ping) from bridge subnet
    iifname "{bridge}" ip saddr {subnet} icmp type echo-request accept

    # Allow vsock traffic (handled at kernel level, not IP)
    # Per-workspace MCP bridge rules are inserted here

    # Allow all non-bridge traffic (don't break host networking)
    iifname != "{bridge}" accept
}
```

This ensures VMs can only reach explicitly allowed ports on the host while preserving normal host networking.

---

### NET-04: Inter-VM Rule Is Unidirectional But Grants Bidirectional Access via conntrack

**Severity**: MEDIUM

**File**: `agentiso/src/network/nftables.rs`, lines 170-177 (inter-VM workspace rules) and line 103 (conntrack established accept)

**Description**: When `allow_inter_vm = true` for workspace A, only workspace A gets a forward rule:

```nft
iifname "br-agentiso" ip saddr {ip_a} oifname "br-agentiso" accept
```

This allows A to initiate connections to any other VM on the bridge. Once A establishes a connection to VM-B, the base rule `ct state established,related accept` allows VM-B's responses back to VM-A. This means VM-A has effective access to **all** VMs, not just VMs that also have `allow_inter_vm = true`.

More critically, if VM-A with `allow_inter_vm=true` sends a packet to VM-B (which has `allow_inter_vm=false`), VM-B can respond because the conntrack rule handles the return path. VM-B is supposed to be isolated, but VM-A can probe and communicate with it.

**Exploit scenario**:
1. VM-A has `allow_inter_vm = true`. VM-B has `allow_inter_vm = false`.
2. VM-A connects to VM-B on port 22. The forward rule for A matches (saddr=A, bridge-to-bridge).
3. VM-B's SSH response is allowed by `ct state established,related accept`.
4. VM-A now has a bidirectional TCP connection to an "isolated" VM-B.

**Recommendation**: Change inter-VM rules to require explicit destination matching. Only allow inter-VM traffic between workspaces that both have `allow_inter_vm = true`, or between workspaces on the same team. One approach:

```rust
// Instead of blanket allow to any bridge destination:
// Only allow to VMs that also have allow_inter_vm=true
// This requires tracking which VMs have the flag, or using a different approach.
// Simplest fix: add a per-VM DROP rule for unwanted inbound bridge traffic.
// In apply_workspace_rules, when allow_inter_vm=false:
rules.push_str(&format!(
    "insert rule inet agentiso forward iifname \"{bridge}\" oifname \"{bridge}\" ip daddr {ip} drop comment \"ws-{id}-isolate\"\n",
    bridge = self.bridge_name,
    ip = guest_ip,
    id = workspace_id,
));
```

This inserts a DROP rule for bridge-to-bridge traffic destined for isolated VMs, preventing other VMs from reaching them even if those other VMs have `allow_inter_vm=true`.

---

### NET-05: Team Rules Use String IPs Without Validation

**Severity**: MEDIUM

**File**: `agentiso/src/network/nftables.rs`, lines 689-701 (generate_team_rules) and lines 709-737 (generate_nested_team_rules)

**Description**: The `generate_team_rules` and `generate_nested_team_rules` functions accept `&[String]` for member IPs and directly interpolate them into nftables rule strings without any validation. If an attacker can control the IP string (e.g., through a compromised team member or malicious input), they could inject nftables rule syntax.

The current call chain passes IPs from the workspace state, which are generated by the `IpAllocator` and stored as `Ipv4Addr` types, then converted to strings. The conversion path appears safe today, but the function signature accepts arbitrary strings, making it fragile.

**Exploit scenario**:
If a future code change passes unsanitized user input as an IP string:
```
member_ips = ["10.99.0.2 accept; add rule inet agentiso forward accept comment \"pwned"]
```
This would inject an arbitrary accept rule into the forward chain.

**Recommendation**: Change the function signatures to accept `&[Ipv4Addr]` instead of `&[String]`, or validate the IP format before interpolation:

```rust
pub fn generate_team_rules(team_id: &str, member_ips: &[Ipv4Addr]) -> Vec<String> {
    // Use Display trait on Ipv4Addr -- guaranteed to produce valid IP notation
}
```

Or add validation:
```rust
for ip_str in member_ips {
    ip_str.parse::<Ipv4Addr>()
        .with_context(|| format!("invalid IP in team rules: {}", ip_str))?;
}
```

---

### NET-06: No ARP Protections on Bridge

**Severity**: MEDIUM

**File**: `agentiso/src/network/bridge.rs` (entire file -- no ARP protection present)

**Description**: There are no ebtables/nftables bridge-family rules to prevent ARP spoofing on the `br-agentiso` bridge. A malicious VM can send gratuitous ARP replies to:
1. Claim the gateway IP (10.99.0.1), becoming a man-in-the-middle for all VM traffic.
2. Claim another VM's IP, redirecting that VM's traffic to itself.
3. Poison the ARP cache of other VMs on the bridge.

QEMU's virtio-net provides a virtual network interface to the guest, and the guest has full control over the MAC/ARP packets it sends through it.

**Exploit scenario**:
1. VM-A sends a gratuitous ARP: "10.99.0.1 is at [VM-A's MAC]".
2. All other VMs on the bridge update their ARP cache.
3. Traffic intended for the gateway (internet access) flows to VM-A instead.
4. VM-A can inspect, modify, or drop the traffic.

**Recommendation**: Add ebtables or nftables bridge-family rules to enforce MAC-IP binding:

```bash
# Per-workspace: bind MAC to IP on the TAP device
nft add rule bridge agentiso forward iifname "tap-XXXX" ether saddr != 52:54:00:00:XX:XX drop
nft add rule bridge agentiso forward iifname "tap-XXXX" arp saddr ip != 10.99.0.X drop
```

Or use QEMU's built-in MAC filtering by setting `queues=` and `macaddr=` constraints at the TAP level. Additionally, consider enabling `proxy_arp` on the bridge with static ARP entries.

---

### NET-07: MCP Bridge Token Does Not Scope to Specific Tools

**Severity**: MEDIUM

**File**: `agentiso/src/mcp/bridge.rs`, lines 64-165 (start_bridge, session creation)

**Description**: When a VM connects to the MCP bridge with a valid bearer token, the bridge creates a full `AgentisoServer` instance with access to all 31 MCP tools. The token validates that the request came from a workspace, but the resulting MCP session can then invoke any tool, including `workspace_create`, `workspace_destroy`, `workspace_fork`, `team`, `swarm_run`, etc.

The per-workspace scoping doc says "each bridge session gets its own AgentisoServer with a unique session ID" and "workspace ownership is adopted when the connecting OpenCode client makes its first tool call." However, the bridge session can create new workspaces, fork existing ones, or even create teams -- potentially escalating from a single workspace to controlling many VMs.

**Exploit scenario**:
1. A compromised VM obtains its MCP bridge token (from its config.jsonc).
2. It calls `workspace_create` via the bridge to spawn additional VMs.
3. It calls `workspace_fork` with count=20 to create a swarm of VMs.
4. It exhausts host resources (VMs, memory, disk) despite being a single workspace.

**Recommendation**: The MCP bridge sessions should have a restricted tool whitelist. Only tools relevant to the originating workspace should be exposed (e.g., `exec`, `file_read`, `file_write`, `file_list`, `file_edit`, `file_transfer`, `git_*`). Workspace lifecycle tools (`workspace_create`, `workspace_fork`, `workspace_destroy`, `team`, `swarm_run`) should be blocked for bridge sessions:

```rust
// In AgentisoServer, add a flag for bridge mode
pub is_bridge_session: bool,

// In tool handlers, check:
if self.is_bridge_session {
    return Err(McpError::invalid_request("tool not available via bridge", None));
}
```

---

### NET-08: Port Forward Does Not Validate Host Port Conflicts

**Severity**: LOW

**File**: `agentiso/src/mcp/tools.rs`, lines 1584-1661 (port_forward handler)

**Description**: The `port_forward` tool rejects privileged ports (< 1024) but does not check whether the requested host port conflicts with agentiso's own services: the dashboard (default 7070 or 8080), the MCP bridge (default 3100), or the metrics endpoint. A user could forward a guest port to 3100, shadowing the MCP bridge and intercepting bridge traffic from other VMs.

**Exploit scenario**:
1. User forwards guest port 80 to host port 3100.
2. nftables DNAT rule intercepts traffic to 3100 and sends it to the VM.
3. Other VMs trying to connect to the MCP bridge at 10.99.0.1:3100 have their traffic redirected to the attacker's VM.

**Recommendation**: Maintain a blocklist of reserved host ports (dashboard port, MCP bridge port, metrics port, vsock port range) and reject port_forward requests targeting them:

```rust
const RESERVED_PORTS: &[u16] = &[3100, 7070, 8080]; // from config

if RESERVED_PORTS.contains(&host_port) {
    return Err(McpError::invalid_params(
        format!("host_port {} is reserved for agentiso services", host_port),
        None,
    ));
}
```

---

### NET-09: iptables Belt-and-Suspenders Rules Are Overly Broad

**Severity**: LOW

**File**: `agentiso/src/network/bridge.rs`, lines 273-280 (ensure_iptables_forward)

**Description**: The iptables FORWARD rules added for Docker/kube-router compatibility are:

```
-I FORWARD -i br-agentiso -j ACCEPT
-I FORWARD -o br-agentiso -j ACCEPT
```

These rules accept **all** traffic through the bridge in iptables, regardless of source/destination. While nftables is the primary filtering layer, these iptables rules create a fallback path where traffic is accepted even if nftables rules change or are temporarily removed (e.g., during `init()` which deletes and recreates the table).

During the window between `delete table inet agentiso` and `run_nft_stdin(&ruleset)` in `NftablesManager::init()`, the iptables ACCEPT rules are the only firewall -- and they allow everything.

**Exploit scenario**:
1. A race condition or crash occurs during `init()` between line 88 (delete table) and line 127 (create new table).
2. During this window, no nftables rules exist.
3. The iptables `-i br-agentiso -j ACCEPT` rule allows all traffic to/from VMs.
4. Isolated VMs can temporarily reach the internet and each other.

**Recommendation**: Make the iptables rules source-subnet-scoped:

```bash
iptables -I FORWARD -i br-agentiso -s 10.99.0.0/16 -j ACCEPT
iptables -I FORWARD -o br-agentiso -d 10.99.0.0/16 -j ACCEPT
```

Or consider removing the iptables rules entirely and relying solely on nftables, with proper documentation that Docker/kube-router users must configure exceptions.

---

### NET-10: workspace_id Used in nftables Comments Without Sanitization

**Severity**: LOW

**File**: `agentiso/src/network/nftables.rs`, lines 155-186 (apply_workspace_rules -- comment generation)

**Description**: The `workspace_id` parameter is interpolated directly into nftables rule comments (e.g., `comment "ws-{id}-internet"`). Workspace IDs are UUIDs generated by the system (via `Uuid::new_v4()`), so in the normal flow they contain only hex characters and hyphens. However, the nftables functions accept `&str` and do not validate the ID format.

The `workspace_id` passed to nftables functions is the first 8 chars of the UUID (via `workspace_id.to_string()[..8]` in workspace lifecycle), which is safe. But the function signatures don't enforce this -- a future caller could pass unsanitized input.

The nftables comment field within double quotes could theoretically allow injection if the ID contained `"` characters, though nft's comment parser may reject this.

**Recommendation**: Add a debug assertion or validation in the nftables functions:

```rust
fn validate_nft_identifier(id: &str) -> Result<()> {
    anyhow::ensure!(
        id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "invalid characters in nftables identifier: {:?}", id
    );
    Ok(())
}
```

---

### NET-11: DNS Exfiltration When Internet Disabled

**Severity**: LOW

**File**: `agentiso/src/network/nftables.rs`, lines 154-167 (internet rules); `agentiso/src/config.rs`, lines 173-175 (dns_servers)

**Description**: When internet access is disabled for a workspace, the forward rule for outbound traffic is simply not added. However, the base ruleset contains:

```nft
iifname "{bridge}" oifname != "{bridge}" ip saddr {subnet} ip daddr {gateway} accept
```

This allows VMs to reach the gateway IP (host) on any port. If the host runs a DNS resolver (e.g., systemd-resolved on 127.0.0.53, or a caching resolver on 0.0.0.0), VMs could resolve DNS names even when "offline." DNS could be used as a covert exfiltration channel (DNS tunneling) with ~50 bytes per query.

The code does reconfigure guest DNS when internet is toggled (setting nameserver to a non-routable address), but a compromised guest can override `/etc/resolv.conf` and point to the gateway.

**Exploit scenario**:
1. VM has internet disabled. Guest DNS is set to a non-routable address.
2. Compromised VM overrides `/etc/resolv.conf` to point to 10.99.0.1.
3. If the host runs a DNS resolver, the VM can resolve external domains.
4. Using DNS tunneling tools (e.g., iodine, dnscat2), the VM exfiltrates data.

**Recommendation**: This is partially mitigated by the guest DNS reconfiguration. For defense-in-depth, block DNS (port 53) from VMs to the gateway in the forward chain when internet is disabled:

```rust
if !policy.allow_internet {
    // Block DNS to gateway when internet is disabled
    rules.push_str(&format!(
        "insert rule inet agentiso forward iifname \"{bridge}\" ip saddr {ip} ip daddr {gateway} tcp dport 53 drop comment \"ws-{id}-no-dns\"\n",
        bridge = self.bridge_name,
        ip = guest_ip,
        gateway = self.gateway_ip,
        id = workspace_id,
    ));
    rules.push_str(&format!(
        "insert rule inet agentiso forward iifname \"{bridge}\" ip saddr {ip} ip daddr {gateway} udp dport 53 drop comment \"ws-{id}-no-dns-udp\"\n",
        bridge = self.bridge_name,
        ip = guest_ip,
        gateway = self.gateway_ip,
        id = workspace_id,
    ));
}
```

Note: the current VM-to-host rule is in the FORWARD chain, but DNS to the gateway IP would actually be INPUT chain traffic (local delivery to host). The base rule `ip saddr {subnet} ip daddr {gateway} accept` in the forward chain would match only if the traffic is being forwarded, not if it is locally delivered. VM-to-gateway-IP traffic goes through INPUT, which has policy accept (see NET-03). Fixing NET-03 would also address this.

---

### NET-12: Bridge IP Forwarding Not Disabled on Shutdown

**Severity**: INFO

**File**: `agentiso/src/network/bridge.rs`, lines 239-254 (enable_ip_forwarding); `agentiso/src/network/mod.rs`, lines 336-348 (shutdown)

**Description**: The `enable_ip_forwarding` function sets `net.ipv4.conf.br-agentiso.forwarding=1` at startup. The `shutdown()` method removes iptables rules but does not disable IP forwarding on the bridge interface. After agentiso exits, the bridge interface remains with forwarding enabled.

This is very minor since the nftables table is also not explicitly removed at shutdown (the table persists until reboot or manual cleanup), and without VMs running there's no traffic to forward.

**Recommendation**: Add `sysctl -w net.ipv4.conf.br-agentiso.forwarding=0` to the shutdown sequence for completeness. Low priority.

---

### NET-13: MAC Address Derivation Is Predictable

**Severity**: INFO

**File**: `agentiso/src/vm/mod.rs`, lines 908-914 (mac_from_ip)

**Description**: VM MAC addresses are deterministically derived from guest IPs: `52:54:00:00:{third_octet}:{fourth_octet}`. This means knowing a VM's IP reveals its MAC address. An attacker who knows the IP allocation pattern (sequential from 10.99.0.2) can predict MACs for all VMs.

This is only informational because MAC prediction alone doesn't enable attacks -- the ARP spoofing attack (NET-06) is the real concern, and it doesn't require knowing target MACs.

**Recommendation**: No action required. The deterministic MAC scheme is standard QEMU practice and simplifies debugging. If ARP spoofing protections (NET-06) are implemented, MAC predictability becomes irrelevant.

---

### NET-14: Dashboard Default Binds to 0.0.0.0 Without Auth

**Severity**: INFO

**File**: `agentiso/src/config.rs`, lines 503-513 (DashboardConfig default); `config.example.toml`, lines 229-250

**Description**: The `config.example.toml` shows `bind_addr = "0.0.0.0"` for the dashboard with the comment "Use '127.0.0.1' for localhost-only access, or '0.0.0.0' to listen on all interfaces (set admin_token if exposed)." The compiled default is `127.0.0.1` which is safe, but the example file suggests `0.0.0.0` with `admin_token = ""`.

A user copying the example config verbatim gets a dashboard exposed on all interfaces with no authentication, providing full REST API access to workspace management.

**Recommendation**: Update the example config to use `127.0.0.1` as the shown bind address, or generate a random admin token during `agentiso init`. Document that `bind_addr = "0.0.0.0"` requires a non-empty `admin_token`.

---

## Positive Findings (Things Done Well)

1. **Forward chain default-drop**: The nftables forward chain has `policy drop`, which is the correct default for a security-sensitive setup. Traffic is only allowed by explicit rules.

2. **Per-interface IP forwarding**: Using `net.ipv4.conf.br-agentiso.forwarding=1` instead of global `net.ipv4.ip_forward=1` is a good practice that limits the blast radius.

3. **Comment-based rule lifecycle**: Using nftables comments like `ws-{id}-internet` for rule identification enables clean per-workspace cleanup without affecting other workspaces. The `delete_rules_by_comment_prefix` approach is robust.

4. **MCP bridge bearer token auth**: The bridge middleware properly validates `Authorization: Bearer <token>` on every request. Tokens are per-workspace UUIDs, generated securely, and revoked on workspace destroy.

5. **Privileged port blocking**: Port forwarding rejects host ports < 1024, preventing VMs from binding to SSH, HTTP, or other system service ports.

6. **Bridge nf_call_iptables disabled**: Disabling `nf_call_iptables` on the bridge prevents Docker/kube-router iptables rules from interfering with agentiso's nftables-based filtering.

7. **Workspace name validation**: Workspace names are validated to alphanumeric + hyphen/underscore/dot, preventing injection in filesystem paths and nftables comments.

8. **Team name validation**: Team names are restricted to alphanumeric + hyphens (1-64 chars), preventing injection in team-related nftables rule comments.

9. **Force-adopt stale session check**: The 60-second activity threshold prevents live sessions from having their workspaces stolen, reducing the risk of active workspace hijacking.

10. **vsock-only guest communication**: Removing the TCP fallback from the guest agent eliminates an entire network attack surface. All host-guest communication uses vsock (kernel-level, not routable).

---

## Priority Fix Order

1. **NET-01** (High): Anti-spoofing rules -- prevents IP impersonation, which undermines all per-IP firewall rules
2. **NET-02** (High): Scope DNAT to bridge/localhost -- prevents external access to VM services
3. **NET-03** (Medium): Input chain default-drop -- prevents VMs from probing host services
4. **NET-04** (Medium): Inter-VM isolation fix -- prevents bidirectional access to "isolated" VMs
5. **NET-06** (Medium): ARP protections -- prevents L2 man-in-the-middle attacks
6. **NET-05** (Medium): Type-safe IP params in team rules -- defense-in-depth against injection
7. **NET-07** (Medium): Bridge tool scoping -- prevents workspace privilege escalation
8. **NET-08** (Low): Reserved port blocklist -- prevents service shadowing
9. **NET-09** (Low): Scope iptables rules -- reduces blast radius during nftables reinit
10. **NET-10** (Low): Validate nftables identifiers -- defense-in-depth
11. **NET-11** (Low): DNS blocking when offline -- prevents DNS tunneling exfiltration
