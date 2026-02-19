use anyhow::{bail, Context, Result};
use tokio::process::Command;
use tracing::{debug, info, instrument};

use std::net::Ipv4Addr;

/// Network policy for a workspace.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct NetworkPolicy {
    /// Allow outbound internet access (NAT)
    pub allow_internet: bool,
    /// Allow traffic to/from other VMs on the bridge
    pub allow_inter_vm: bool,
    /// Specific TCP ports allowed for inbound connections from outside
    pub allowed_ports: Vec<u16>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            allow_internet: true,
            allow_inter_vm: false,
            allowed_ports: Vec::new(),
        }
    }
}

/// Port forward rule: host_port -> guest_ip:guest_port.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PortForwardRule {
    pub host_port: u16,
    pub guest_ip: Ipv4Addr,
    pub guest_port: u16,
    pub workspace_id: String,
}

/// nftables firewall manager.
///
/// Manages per-workspace firewall rules including isolation, NAT, and port forwarding.
/// Uses the `nft` CLI to apply rules.
///
/// Table structure:
/// ```text
/// table inet agentiso {
///     chain forward {
///         type filter hook forward priority 0; policy drop;
///         # per-workspace rules inserted here
///     }
///     chain postrouting {
///         type nat hook postrouting priority 100;
///         # NAT masquerade rules
///     }
///     chain prerouting {
///         type nat hook prerouting priority -100;
///         # DNAT port forwarding rules
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct NftablesManager {
    /// Bridge device name (for matching traffic)
    bridge_name: String,
    /// Bridge subnet in CIDR form (e.g. "10.99.0.0/16")
    bridge_subnet: String,
    /// Gateway IP address (bridge IP, e.g. "10.99.0.1")
    gateway_ip: Ipv4Addr,
}

impl NftablesManager {
    pub fn new(bridge_name: String, bridge_subnet: String, gateway_ip: Ipv4Addr) -> Self {
        Self {
            bridge_name,
            bridge_subnet,
            gateway_ip,
        }
    }

    /// Initialize the agentiso nftables table with base chains.
    ///
    /// Idempotent: flushes and recreates the table on each call.
    #[instrument(skip(self))]
    pub async fn init(&self) -> Result<()> {
        info!("initializing nftables rules");

        // Delete existing table if present (ignore error if it doesn't exist)
        let _ = run_nft(&["delete", "table", "inet", "agentiso"]).await;

        // Create the base ruleset
        let ruleset = format!(
            r#"
table inet agentiso {{
    chain forward {{
        type filter hook forward priority 0; policy drop;

        # Allow established/related connections
        ct state established,related accept

        # Allow host -> VM (bridge subnet)
        iifname != "{bridge}" oifname "{bridge}" ip daddr {subnet} accept

        # Allow VM -> host (bridge IP)
        iifname "{bridge}" oifname != "{bridge}" ip saddr {subnet} ip daddr {gateway} accept
    }}

    chain postrouting {{
        type nat hook postrouting priority 100; policy accept;
    }}

    chain prerouting {{
        type nat hook prerouting priority -100; policy accept;
    }}
}}
"#,
            bridge = self.bridge_name,
            subnet = self.bridge_subnet,
            gateway = self.gateway_ip,
        );

        run_nft_stdin(&ruleset)
            .await
            .context("failed to initialize nftables base ruleset")?;

        info!("nftables base rules initialized");
        Ok(())
    }

    /// Apply per-workspace firewall rules based on the network policy.
    ///
    /// Each workspace gets uniquely-commented rules so they can be identified and removed.
    #[instrument(skip(self))]
    pub async fn apply_workspace_rules(
        &self,
        workspace_id: &str,
        guest_ip: Ipv4Addr,
        policy: &NetworkPolicy,
        is_new: bool,
    ) -> Result<()> {
        // Only remove existing rules if this is an existing workspace
        if !is_new {
            self.remove_workspace_rules(workspace_id).await?;
        }

        let mut rules = String::new();

        // Internet access (NAT masquerade for this VM's IP)
        if policy.allow_internet {
            rules.push_str(&format!(
                "add rule inet agentiso forward iifname \"{bridge}\" ip saddr {ip} oifname != \"{bridge}\" accept comment \"ws-{id}-internet\"\n",
                bridge = self.bridge_name,
                ip = guest_ip,
                id = workspace_id,
            ));
            rules.push_str(&format!(
                "add rule inet agentiso postrouting ip saddr {ip} oifname != \"{bridge}\" masquerade comment \"ws-{id}-nat\"\n",
                ip = guest_ip,
                bridge = self.bridge_name,
                id = workspace_id,
            ));
        }

        // Inter-VM traffic
        if policy.allow_inter_vm {
            rules.push_str(&format!(
                "add rule inet agentiso forward iifname \"{bridge}\" ip saddr {ip} oifname \"{bridge}\" accept comment \"ws-{id}-intervm\"\n",
                bridge = self.bridge_name,
                ip = guest_ip,
                id = workspace_id,
            ));
        }

        // Allowed inbound ports
        for port in &policy.allowed_ports {
            rules.push_str(&format!(
                "add rule inet agentiso forward ip daddr {ip} tcp dport {port} accept comment \"ws-{id}-allow-{port}\"\n",
                ip = guest_ip,
                port = port,
                id = workspace_id,
            ));
        }

        if !rules.is_empty() {
            run_nft_stdin(&rules)
                .await
                .with_context(|| format!("failed to apply rules for workspace {}", workspace_id))?;
        }

        info!(
            workspace_id = %workspace_id,
            guest_ip = %guest_ip,
            internet = policy.allow_internet,
            inter_vm = policy.allow_inter_vm,
            "workspace firewall rules applied"
        );

        Ok(())
    }

    /// Add a port forwarding rule (DNAT) for a workspace.
    ///
    /// Forwards traffic arriving on `host_port` to `guest_ip:guest_port`.
    #[instrument(skip(self))]
    pub async fn add_port_forward(
        &self,
        workspace_id: &str,
        guest_ip: Ipv4Addr,
        guest_port: u16,
        host_port: u16,
    ) -> Result<()> {
        let rules = format!(
            concat!(
                "add rule inet agentiso prerouting tcp dport {host_port} dnat to {ip}:{guest_port} comment \"ws-{id}-pf-{guest_port}\"\n",
                "add rule inet agentiso forward ip daddr {ip} tcp dport {guest_port} accept comment \"ws-{id}-pf-fwd-{guest_port}\"\n",
            ),
            host_port = host_port,
            ip = guest_ip,
            guest_port = guest_port,
            id = workspace_id,
        );

        run_nft_stdin(&rules)
            .await
            .with_context(|| {
                format!(
                    "failed to add port forward {}->{}:{} for workspace {}",
                    host_port, guest_ip, guest_port, workspace_id
                )
            })?;

        info!(
            workspace_id = %workspace_id,
            host_port = host_port,
            guest_port = guest_port,
            "port forward added"
        );

        Ok(())
    }

    /// Remove a port forwarding rule for a specific guest port.
    #[instrument(skip(self))]
    pub async fn remove_port_forward(
        &self,
        workspace_id: &str,
        guest_port: u16,
    ) -> Result<()> {
        let pf_comment = format!("ws-{}-pf-{}", workspace_id, guest_port);
        let fwd_comment = format!("ws-{}-pf-fwd-{}", workspace_id, guest_port);

        self.delete_rules_by_comment("prerouting", &pf_comment).await?;
        self.delete_rules_by_comment("forward", &fwd_comment).await?;

        info!(
            workspace_id = %workspace_id,
            guest_port = guest_port,
            "port forward removed"
        );

        Ok(())
    }

    /// Remove all nftables rules for a workspace (cleanup on destroy).
    #[instrument(skip(self))]
    pub async fn remove_workspace_rules(&self, workspace_id: &str) -> Result<()> {
        let comment_prefix = format!("ws-{}-", workspace_id);

        for chain in ["forward", "postrouting", "prerouting"] {
            self.delete_rules_by_comment_prefix(chain, &comment_prefix)
                .await?;
        }

        debug!(workspace_id = %workspace_id, "workspace rules removed");
        Ok(())
    }

    /// Delete rules from a chain that have a specific comment.
    async fn delete_rules_by_comment(&self, chain: &str, comment: &str) -> Result<()> {
        let handles = self.find_rule_handles(chain, comment).await?;
        for handle in handles {
            run_nft(&[
                "delete", "rule", "inet", "agentiso", chain, "handle", &handle.to_string(),
            ])
            .await
            .with_context(|| {
                format!("failed to delete rule handle {} in chain {}", handle, chain)
            })?;
        }
        Ok(())
    }

    /// Delete rules from a chain whose comment starts with the given prefix.
    async fn delete_rules_by_comment_prefix(
        &self,
        chain: &str,
        prefix: &str,
    ) -> Result<()> {
        let handles = self.find_rule_handles_by_prefix(chain, prefix).await?;
        for handle in handles {
            run_nft(&[
                "delete", "rule", "inet", "agentiso", chain, "handle", &handle.to_string(),
            ])
            .await
            .with_context(|| {
                format!("failed to delete rule handle {} in chain {}", handle, chain)
            })?;
        }
        Ok(())
    }

    /// Find rule handles in a chain that match a comment exactly.
    async fn find_rule_handles(&self, chain: &str, comment: &str) -> Result<Vec<u64>> {
        self.find_rule_handles_impl(chain, |c| c == comment).await
    }

    /// Find rule handles in a chain whose comment starts with a prefix.
    async fn find_rule_handles_by_prefix(
        &self,
        chain: &str,
        prefix: &str,
    ) -> Result<Vec<u64>> {
        self.find_rule_handles_impl(chain, |c| c.starts_with(prefix))
            .await
    }

    /// Find rule handles in a chain matching a comment predicate.
    ///
    /// Uses `nft -a list chain inet agentiso {chain}` and parses the output.
    async fn find_rule_handles_impl<F>(
        &self,
        chain: &str,
        comment_matches: F,
    ) -> Result<Vec<u64>>
    where
        F: Fn(&str) -> bool,
    {
        let output = run_nft_output(&[
            "-a", "list", "chain", "inet", "agentiso", chain,
        ])
        .await
        .unwrap_or_default();

        let mut handles = Vec::new();

        for line in output.lines() {
            let line = line.trim();

            // Look for lines with 'comment "..."' and '# handle N'
            if let Some(comment) = extract_comment(line) {
                if comment_matches(&comment) {
                    if let Some(handle) = extract_handle(line) {
                        handles.push(handle);
                    }
                }
            }
        }

        Ok(handles)
    }
}

/// Extract the comment value from an nft rule line.
/// E.g., from `... comment "ws-abc-internet" # handle 5` returns "ws-abc-internet"
fn extract_comment(line: &str) -> Option<String> {
    let marker = "comment \"";
    let start = line.find(marker)? + marker.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

/// Extract the handle number from an nft rule line.
/// E.g., from `... # handle 5` returns 5
fn extract_handle(line: &str) -> Option<u64> {
    let marker = "# handle ";
    let start = line.find(marker)? + marker.len();
    line[start..].split_whitespace().next()?.parse().ok()
}

/// Run an `nft` command and check for success.
async fn run_nft(args: &[&str]) -> Result<()> {
    debug!(args = ?args, "running nft command");

    let output = Command::new("nft")
        .args(args)
        .output()
        .await
        .context("failed to execute nft command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nft failed: {}", stderr.trim());
    }

    Ok(())
}

/// Run an `nft` command with input piped via stdin.
async fn run_nft_stdin(input: &str) -> Result<()> {
    debug!(input_len = input.len(), "running nft with stdin input");

    let mut child = Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn nft")?;

    use tokio::io::AsyncWriteExt;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).await?;
        // Drop stdin to close it, signaling EOF
    }

    let output = child
        .wait_with_output()
        .await
        .context("failed to wait for nft")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nft -f - failed: {}", stderr.trim());
    }

    Ok(())
}

/// Run an `nft` command and return its stdout.
async fn run_nft_output(args: &[&str]) -> Result<String> {
    debug!(args = ?args, "running nft command for output");

    let output = Command::new("nft")
        .args(args)
        .output()
        .await
        .context("failed to execute nft command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("nft failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Generate the nftables base ruleset string for initialization.
#[allow(dead_code)]
pub(crate) fn generate_base_ruleset(bridge_name: &str, bridge_subnet: &str, gateway_ip: Ipv4Addr) -> String {
    format!(
        r#"
table inet agentiso {{
    chain forward {{
        type filter hook forward priority 0; policy drop;

        # Allow established/related connections
        ct state established,related accept

        # Allow host -> VM (bridge subnet)
        iifname != "{bridge}" oifname "{bridge}" ip daddr {subnet} accept

        # Allow VM -> host (bridge IP)
        iifname "{bridge}" oifname != "{bridge}" ip saddr {subnet} ip daddr {gateway} accept
    }}

    chain postrouting {{
        type nat hook postrouting priority 100; policy accept;
    }}

    chain prerouting {{
        type nat hook prerouting priority -100; policy accept;
    }}
}}
"#,
        bridge = bridge_name,
        subnet = bridge_subnet,
        gateway = gateway_ip,
    )
}

/// Generate workspace firewall rules string based on the network policy.
#[allow(dead_code)]
pub(crate) fn generate_workspace_rules(
    bridge_name: &str,
    workspace_id: &str,
    guest_ip: Ipv4Addr,
    policy: &NetworkPolicy,
) -> String {
    let mut rules = String::new();

    if policy.allow_internet {
        rules.push_str(&format!(
            "add rule inet agentiso forward iifname \"{bridge}\" ip saddr {ip} oifname != \"{bridge}\" accept comment \"ws-{id}-internet\"\n",
            bridge = bridge_name,
            ip = guest_ip,
            id = workspace_id,
        ));
        rules.push_str(&format!(
            "add rule inet agentiso postrouting ip saddr {ip} oifname != \"{bridge}\" masquerade comment \"ws-{id}-nat\"\n",
            ip = guest_ip,
            bridge = bridge_name,
            id = workspace_id,
        ));
    }

    if policy.allow_inter_vm {
        rules.push_str(&format!(
            "add rule inet agentiso forward iifname \"{bridge}\" ip saddr {ip} oifname \"{bridge}\" accept comment \"ws-{id}-intervm\"\n",
            bridge = bridge_name,
            ip = guest_ip,
            id = workspace_id,
        ));
    }

    for port in &policy.allowed_ports {
        rules.push_str(&format!(
            "add rule inet agentiso forward ip daddr {ip} tcp dport {port} accept comment \"ws-{id}-allow-{port}\"\n",
            ip = guest_ip,
            port = port,
            id = workspace_id,
        ));
    }

    rules
}

/// Generate port forwarding (DNAT) rules string.
#[allow(dead_code)]
pub(crate) fn generate_port_forward_rules(
    workspace_id: &str,
    guest_ip: Ipv4Addr,
    guest_port: u16,
    host_port: u16,
) -> String {
    format!(
        concat!(
            "add rule inet agentiso prerouting tcp dport {host_port} dnat to {ip}:{guest_port} comment \"ws-{id}-pf-{guest_port}\"\n",
            "add rule inet agentiso forward ip daddr {ip} tcp dport {guest_port} accept comment \"ws-{id}-pf-fwd-{guest_port}\"\n",
        ),
        host_port = host_port,
        ip = guest_ip,
        guest_port = guest_port,
        id = workspace_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_comment_standard() {
        let line = r#"        ip saddr 10.99.0.2 oifname != "br-agentiso" masquerade comment "ws-abc12345-nat" # handle 7"#;
        let comment = extract_comment(line).unwrap();
        assert_eq!(comment, "ws-abc12345-nat");
    }

    #[test]
    fn test_extract_comment_none() {
        let line = "        ct state established,related accept # handle 4";
        assert!(extract_comment(line).is_none());
    }

    #[test]
    fn test_extract_handle_standard() {
        let line = r#"        ip saddr 10.99.0.2 accept comment "ws-abc-internet" # handle 5"#;
        let handle = extract_handle(line).unwrap();
        assert_eq!(handle, 5);
    }

    #[test]
    fn test_extract_handle_large_number() {
        let line = r#"        accept comment "rule" # handle 123456"#;
        let handle = extract_handle(line).unwrap();
        assert_eq!(handle, 123456);
    }

    #[test]
    fn test_extract_handle_none() {
        let line = "        ct state established,related accept";
        assert!(extract_handle(line).is_none());
    }

    #[test]
    fn test_default_network_policy() {
        let policy = NetworkPolicy::default();
        assert!(policy.allow_internet);
        assert!(!policy.allow_inter_vm);
        assert!(policy.allowed_ports.is_empty());
    }

    #[test]
    fn test_generate_base_ruleset_contains_chains() {
        let gw = Ipv4Addr::new(10, 99, 0, 1);
        let ruleset = generate_base_ruleset("br-agentiso", "10.99.0.0/16", gw);
        assert!(ruleset.contains("table inet agentiso"));
        assert!(ruleset.contains("chain forward"));
        assert!(ruleset.contains("chain postrouting"));
        assert!(ruleset.contains("chain prerouting"));
        assert!(ruleset.contains("policy drop"));
        assert!(ruleset.contains("br-agentiso"));
        assert!(ruleset.contains("10.99.0.0/16"));
        assert!(ruleset.contains("ip daddr 10.99.0.1"));
    }

    #[test]
    fn test_generate_base_ruleset_custom_gateway() {
        let gw = Ipv4Addr::new(172, 16, 0, 1);
        let ruleset = generate_base_ruleset("br-custom", "172.16.0.0/16", gw);
        assert!(ruleset.contains("ip daddr 172.16.0.1"));
        assert!(!ruleset.contains("10.99.0.1"));
    }

    #[test]
    fn test_generate_workspace_rules_internet_allowed() {
        let policy = NetworkPolicy {
            allow_internet: true,
            allow_inter_vm: false,
            allowed_ports: Vec::new(),
        };
        let ip = Ipv4Addr::new(10, 99, 0, 5);
        let rules = generate_workspace_rules("br-agentiso", "abc12345", ip, &policy);

        assert!(rules.contains("ws-abc12345-internet"));
        assert!(rules.contains("ws-abc12345-nat"));
        assert!(rules.contains("masquerade"));
        assert!(rules.contains("10.99.0.5"));
        assert!(!rules.contains("intervm"));
    }

    #[test]
    fn test_generate_workspace_rules_inter_vm_allowed() {
        let policy = NetworkPolicy {
            allow_internet: false,
            allow_inter_vm: true,
            allowed_ports: Vec::new(),
        };
        let ip = Ipv4Addr::new(10, 99, 0, 3);
        let rules = generate_workspace_rules("br-agentiso", "xyz99999", ip, &policy);

        assert!(!rules.contains("internet"));
        assert!(!rules.contains("masquerade"));
        assert!(rules.contains("ws-xyz99999-intervm"));
        assert!(rules.contains("10.99.0.3"));
    }

    #[test]
    fn test_generate_workspace_rules_both_allowed() {
        let policy = NetworkPolicy {
            allow_internet: true,
            allow_inter_vm: true,
            allowed_ports: Vec::new(),
        };
        let ip = Ipv4Addr::new(10, 99, 1, 100);
        let rules = generate_workspace_rules("br-agentiso", "test1234", ip, &policy);

        assert!(rules.contains("ws-test1234-internet"));
        assert!(rules.contains("ws-test1234-nat"));
        assert!(rules.contains("ws-test1234-intervm"));
    }

    #[test]
    fn test_generate_workspace_rules_nothing_allowed() {
        let policy = NetworkPolicy {
            allow_internet: false,
            allow_inter_vm: false,
            allowed_ports: Vec::new(),
        };
        let ip = Ipv4Addr::new(10, 99, 0, 2);
        let rules = generate_workspace_rules("br-agentiso", "nope0000", ip, &policy);
        assert!(rules.is_empty());
    }

    #[test]
    fn test_generate_workspace_rules_with_allowed_ports() {
        let policy = NetworkPolicy {
            allow_internet: false,
            allow_inter_vm: false,
            allowed_ports: vec![80, 443, 8080],
        };
        let ip = Ipv4Addr::new(10, 99, 0, 5);
        let rules = generate_workspace_rules("br-agentiso", "ports123", ip, &policy);

        assert!(rules.contains("ws-ports123-allow-80"));
        assert!(rules.contains("ws-ports123-allow-443"));
        assert!(rules.contains("ws-ports123-allow-8080"));
        assert!(rules.contains("ip daddr 10.99.0.5 tcp dport 80"));
        assert!(rules.contains("ip daddr 10.99.0.5 tcp dport 443"));
        assert!(rules.contains("ip daddr 10.99.0.5 tcp dport 8080"));
        // No internet or inter-vm rules
        assert!(!rules.contains("internet"));
        assert!(!rules.contains("intervm"));
    }

    #[test]
    fn test_generate_port_forward_rules() {
        let ip = Ipv4Addr::new(10, 99, 0, 7);
        let rules = generate_port_forward_rules("abc12345", ip, 8080, 8080);

        assert!(rules.contains("dnat to 10.99.0.7:8080"));
        assert!(rules.contains("tcp dport 8080"));
        assert!(rules.contains("ws-abc12345-pf-8080"));
        assert!(rules.contains("ws-abc12345-pf-fwd-8080"));
    }

    #[test]
    fn test_generate_port_forward_different_ports() {
        let ip = Ipv4Addr::new(10, 99, 0, 10);
        let rules = generate_port_forward_rules("def00000", ip, 3000, 9090);

        // DNAT rule: prerouting, host_port 9090 -> guest_ip:3000
        assert!(rules.contains("tcp dport 9090"));
        assert!(rules.contains("dnat to 10.99.0.10:3000"));
        // Forward rule: allow guest_port 3000
        assert!(rules.contains("ip daddr 10.99.0.10 tcp dport 3000"));
    }

    #[test]
    fn test_extract_comment_and_handle_together() {
        let line = r#"        iifname "br-agentiso" ip saddr 10.99.0.2 oifname != "br-agentiso" accept comment "ws-abc-internet" # handle 12"#;
        assert_eq!(extract_comment(line).unwrap(), "ws-abc-internet");
        assert_eq!(extract_handle(line).unwrap(), 12);
    }

    #[test]
    fn test_port_forward_rule_struct() {
        let rule = PortForwardRule {
            host_port: 8080,
            guest_ip: Ipv4Addr::new(10, 99, 0, 5),
            guest_port: 80,
            workspace_id: "abc12345".to_string(),
        };
        assert_eq!(rule.host_port, 8080);
        assert_eq!(rule.guest_port, 80);
        assert_eq!(rule.guest_ip, Ipv4Addr::new(10, 99, 0, 5));
    }
}
