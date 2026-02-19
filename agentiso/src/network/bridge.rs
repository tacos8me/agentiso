use anyhow::{bail, Context, Result};
use tokio::process::Command;
use tracing::{debug, info, instrument};

/// Bridge and TAP device manager.
///
/// Manages the `br-agentiso` bridge and per-workspace TAP devices.
/// All operations shell out to the `ip` command.
#[derive(Debug, Clone)]
pub struct BridgeManager {
    /// Name of the bridge device (default: "br-agentiso")
    bridge_name: String,
    /// IP address of the bridge in CIDR notation (default: "10.99.0.1/16")
    bridge_cidr: String,
}

#[allow(dead_code)]
impl BridgeManager {
    pub fn new(bridge_name: String, bridge_cidr: String) -> Self {
        Self {
            bridge_name,
            bridge_cidr,
        }
    }

    /// Returns the bridge device name.
    pub fn bridge_name(&self) -> &str {
        &self.bridge_name
    }

    /// Ensure the bridge device exists and is configured.
    ///
    /// Creates the bridge if it doesn't exist, assigns the IP, and brings it up.
    /// Also disables `nf_call_iptables` on the bridge so that Docker/kube-router
    /// iptables rules don't interfere with our traffic (nftables handles all filtering).
    /// Idempotent: safe to call multiple times.
    #[instrument(skip(self))]
    pub async fn ensure_bridge(&self) -> Result<()> {
        if self.bridge_exists().await? {
            debug!(bridge = %self.bridge_name, "bridge already exists");
            // Ensure it's up
            self.set_link_up(&self.bridge_name).await?;
            // Ensure nf_call_iptables is disabled (idempotent)
            self.disable_nf_call_iptables().await;
            return Ok(());
        }

        info!(bridge = %self.bridge_name, cidr = %self.bridge_cidr, "creating bridge");

        // Create the bridge
        run_ip(&["link", "add", &self.bridge_name, "type", "bridge"])
            .await
            .with_context(|| format!("failed to create bridge {}", self.bridge_name))?;

        // Assign IP address
        run_ip(&[
            "addr", "add", &self.bridge_cidr, "dev", &self.bridge_name,
        ])
        .await
        .with_context(|| {
            format!(
                "failed to assign {} to {}",
                self.bridge_cidr, self.bridge_name
            )
        })?;

        // Bring it up
        self.set_link_up(&self.bridge_name).await?;

        // Disable nf_call_iptables so Docker/kube-router iptables rules
        // don't interfere with traffic on this bridge. Our nftables rules
        // still apply (they use nf_tables hooks, not bridge-nf-call).
        self.disable_nf_call_iptables().await;

        info!(bridge = %self.bridge_name, "bridge created and up");
        Ok(())
    }

    /// Create a TAP device for a workspace and attach it to the bridge.
    ///
    /// The TAP device name is `tap-{short_id}` where short_id is the first 8 chars
    /// of the workspace UUID.
    ///
    /// Returns the TAP device name.
    #[instrument(skip(self))]
    pub async fn create_tap(&self, workspace_id: &str) -> Result<String> {
        let tap_name = tap_device_name(workspace_id);

        debug!(tap = %tap_name, "creating TAP device");

        // Create TAP device
        run_ip(&["tuntap", "add", &tap_name, "mode", "tap"])
            .await
            .with_context(|| format!("failed to create TAP device {}", tap_name))?;

        // Attach to bridge
        run_ip(&["link", "set", &tap_name, "master", &self.bridge_name])
            .await
            .with_context(|| {
                format!(
                    "failed to attach {} to bridge {}",
                    tap_name, self.bridge_name
                )
            })?;

        // Bring it up
        self.set_link_up(&tap_name).await?;

        info!(tap = %tap_name, bridge = %self.bridge_name, "TAP device created and attached");
        Ok(tap_name)
    }

    /// Destroy a TAP device for a workspace.
    #[instrument(skip(self))]
    pub async fn destroy_tap(&self, workspace_id: &str) -> Result<()> {
        let tap_name = tap_device_name(workspace_id);

        debug!(tap = %tap_name, "destroying TAP device");

        // Bring it down first, ignore errors if already down
        let _ = run_ip(&["link", "set", &tap_name, "down"]).await;

        // Delete the device
        run_ip(&["link", "del", &tap_name])
            .await
            .with_context(|| format!("failed to delete TAP device {}", tap_name))?;

        info!(tap = %tap_name, "TAP device destroyed");
        Ok(())
    }

    /// List all TAP devices attached to this bridge.
    ///
    /// Returns the names of interfaces that match `tap-*` and are attached
    /// to the bridge.
    pub async fn list_tap_devices(&self) -> Result<Vec<String>> {
        let output = Command::new("ip")
            .args(["link", "show", "master", &self.bridge_name])
            .output()
            .await
            .context("failed to run ip link show master")?;

        if !output.status.success() {
            // Bridge may not exist yet
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut taps = Vec::new();

        // Parse `ip link show` output. Each interface has a line like:
        // 5: tap-abc12345: <BROADCAST,MULTICAST,UP,LOWER_UP> ...
        for line in stdout.lines() {
            let line = line.trim();
            // Match lines with interface index prefix
            if let Some(colon_pos) = line.find(':') {
                // Check if the part before first colon is a number (interface index)
                let index_part = line[..colon_pos].trim();
                if index_part.parse::<u32>().is_ok() {
                    // The interface name is between the first and second colon
                    let rest = &line[colon_pos + 1..];
                    if let Some(name_end) = rest.find(':') {
                        let name = rest[..name_end].trim();
                        if name.starts_with("tap-") {
                            taps.push(name.to_string());
                        }
                    }
                }
            }
        }

        Ok(taps)
    }

    /// Check if the bridge device exists.
    async fn bridge_exists(&self) -> Result<bool> {
        let output = Command::new("ip")
            .args(["link", "show", &self.bridge_name])
            .output()
            .await
            .context("failed to run ip link show")?;

        Ok(output.status.success())
    }

    /// Bring a network link up.
    async fn set_link_up(&self, device: &str) -> Result<()> {
        run_ip(&["link", "set", device, "up"])
            .await
            .with_context(|| format!("failed to bring up {}", device))
    }

    /// Disable `nf_call_iptables` on this bridge specifically.
    ///
    /// When Docker/kube-router sets the global `bridge-nf-call-iptables=1`,
    /// bridged frames are pushed through iptables FORWARD where they may be
    /// dropped by policies we don't control. Disabling it per-bridge lets
    /// nftables be the sole filtering layer for our traffic without affecting
    /// Docker's isolation on other bridges.
    ///
    /// Best-effort: logs a warning on failure (old kernels may not support it).
    async fn disable_nf_call_iptables(&self) {
        let path = format!("/sys/class/net/{}/bridge/nf_call_iptables", self.bridge_name);
        match tokio::fs::write(&path, "0\n").await {
            Ok(_) => debug!(bridge = %self.bridge_name, "disabled nf_call_iptables on bridge"),
            Err(e) => debug!(bridge = %self.bridge_name, error = %e,
                "could not disable per-bridge nf_call_iptables (may not be supported)"),
        }
    }
}

/// Generate the TAP device name for a workspace.
///
/// Uses the first 8 characters of the workspace ID to keep it under the
/// 15-character Linux interface name limit.
pub fn tap_device_name(workspace_id: &str) -> String {
    let short_id = &workspace_id[..workspace_id.len().min(8)];
    format!("tap-{}", short_id)
}

/// Enable IP forwarding on the host. Required for NAT to work.
pub async fn enable_ip_forwarding() -> Result<()> {
    let output = Command::new("sysctl")
        .args(["-w", "net.ipv4.ip_forward=1"])
        .output()
        .await
        .context("failed to run sysctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("failed to enable IP forwarding: {}", stderr.trim());
    }

    debug!("IP forwarding enabled");
    Ok(())
}

/// Ensure iptables ACCEPT rules exist for bridge traffic.
///
/// Hosts running Docker, kube-router, or flannel often have an iptables
/// FORWARD chain with policy DROP. Without explicit ACCEPT rules for
/// our bridge, all VM traffic is silently dropped at the iptables layer
/// even when nftables rules allow it.
///
/// We add standard interface-match rules for routed (NAT) traffic.
/// L2 bridged traffic (host <-> VM on the same subnet) does not need
/// FORWARD rules because it is handled by the bridge's own forwarding
/// path. (Note: physdev rules are NOT used here because `--physdev-in`
/// and `--physdev-out` match bridge *port* names — i.e. individual TAP
/// devices — not the bridge interface itself.)
///
/// Rules added (all idempotent):
///   -I FORWARD -i <bridge> -j ACCEPT
///   -I FORWARD -o <bridge> -j ACCEPT
pub async fn ensure_iptables_forward(bridge_name: &str) -> Result<()> {
    // Routed traffic rules (for NAT / internet access)
    for (flag, direction) in [("-i", "outbound"), ("-o", "inbound")] {
        ensure_iptables_rule(&[flag, bridge_name, "-j", "ACCEPT"], bridge_name, direction).await?;
    }

    Ok(())
}

/// Add a single iptables FORWARD rule idempotently.
async fn ensure_iptables_rule(rule_args: &[&str], bridge_name: &str, direction: &str) -> Result<()> {
    let mut check_args = vec!["-C", "FORWARD"];
    check_args.extend_from_slice(rule_args);

    let check = Command::new("iptables")
        .args(&check_args)
        .output()
        .await
        .context("failed to run iptables -C (is the iptables binary installed?)")?;

    if !check.status.success() {
        let mut add_args = vec!["-I", "FORWARD", "1"];
        add_args.extend_from_slice(rule_args);

        let add = Command::new("iptables")
            .args(&add_args)
            .output()
            .await
            .context("failed to run iptables -I")?;

        if !add.status.success() {
            let stderr = String::from_utf8_lossy(&add.stderr);
            bail!(
                "failed to add iptables FORWARD {} rule for {}: {}",
                direction,
                bridge_name,
                stderr.trim()
            );
        }
        info!(bridge = %bridge_name, direction, "added iptables FORWARD ACCEPT rule");
    } else {
        debug!(bridge = %bridge_name, direction, "iptables FORWARD ACCEPT rule already exists");
    }

    Ok(())
}

/// Remove iptables FORWARD ACCEPT rules for bridge traffic.
///
/// Counterpart to [`ensure_iptables_forward`]. Best-effort: does not fail
/// if rules are already absent or if `iptables` is unavailable.
pub async fn cleanup_iptables_forward(bridge_name: &str) -> Result<()> {
    // Remove routed traffic rules
    for (flag, direction) in [("-i", "outbound"), ("-o", "inbound")] {
        cleanup_iptables_rule(&[flag, bridge_name, "-j", "ACCEPT"], bridge_name, direction).await;
    }
    Ok(())
}

/// Ensure an iptables NAT MASQUERADE rule exists for the bridge subnet.
///
/// On hosts with Docker or kube-router, nftables masquerade rules may be
/// silently overridden by iptables nat rules. Adding an iptables MASQUERADE
/// rule as a belt-and-suspenders approach ensures outbound NAT works
/// regardless of nftables/iptables interaction quirks.
///
/// Rule added (idempotent):
///   -t nat -I POSTROUTING 1 -s <subnet> ! -o <bridge> -j MASQUERADE
pub async fn ensure_iptables_nat(bridge_name: &str, subnet: &str) -> Result<()> {
    // Check if rule already exists
    let check = Command::new("iptables")
        .args([
            "-t", "nat", "-C", "POSTROUTING",
            "-s", subnet,
            "!", "-o", bridge_name,
            "-j", "MASQUERADE",
        ])
        .output()
        .await
        .context("failed to run iptables -t nat -C (is iptables installed?)")?;

    if !check.status.success() {
        // Rule doesn't exist, add it
        let add = Command::new("iptables")
            .args([
                "-t", "nat", "-I", "POSTROUTING", "1",
                "-s", subnet,
                "!", "-o", bridge_name,
                "-j", "MASQUERADE",
            ])
            .output()
            .await
            .context("failed to run iptables -t nat -I")?;

        if !add.status.success() {
            let stderr = String::from_utf8_lossy(&add.stderr);
            bail!(
                "failed to add iptables NAT MASQUERADE rule for {} via {}: {}",
                subnet,
                bridge_name,
                stderr.trim()
            );
        }
        info!(bridge = %bridge_name, subnet = %subnet, "added iptables NAT MASQUERADE rule");
    } else {
        debug!(bridge = %bridge_name, subnet = %subnet, "iptables NAT MASQUERADE rule already exists");
    }

    Ok(())
}

/// Remove the iptables NAT MASQUERADE rule for the bridge subnet.
///
/// Counterpart to [`ensure_iptables_nat`]. Best-effort: does not fail
/// if the rule is already absent or if `iptables` is unavailable.
pub async fn cleanup_iptables_nat(bridge_name: &str, subnet: &str) -> Result<()> {
    let check = match Command::new("iptables")
        .args([
            "-t", "nat", "-C", "POSTROUTING",
            "-s", subnet,
            "!", "-o", bridge_name,
            "-j", "MASQUERADE",
        ])
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            debug!(bridge = %bridge_name, error = %e, "iptables not available for NAT cleanup");
            return Ok(());
        }
    };

    if !check.status.success() {
        debug!(bridge = %bridge_name, subnet = %subnet, "iptables NAT MASQUERADE rule does not exist, nothing to remove");
        return Ok(());
    }

    match Command::new("iptables")
        .args([
            "-t", "nat", "-D", "POSTROUTING",
            "-s", subnet,
            "!", "-o", bridge_name,
            "-j", "MASQUERADE",
        ])
        .output()
        .await
    {
        Ok(o) if o.status.success() => {
            debug!(bridge = %bridge_name, subnet = %subnet, "removed iptables NAT MASQUERADE rule");
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            debug!(bridge = %bridge_name, subnet = %subnet, stderr = %stderr.trim(), "failed to remove iptables NAT MASQUERADE rule");
        }
        Err(e) => {
            debug!(bridge = %bridge_name, error = %e, "failed to run iptables -t nat -D");
        }
    }

    Ok(())
}

/// Remove a single iptables FORWARD rule if it exists. Best-effort.
async fn cleanup_iptables_rule(rule_args: &[&str], bridge_name: &str, direction: &str) {
    let mut check_args = vec!["-C", "FORWARD"];
    check_args.extend_from_slice(rule_args);

    let check = match Command::new("iptables").args(&check_args).output().await {
        Ok(o) => o,
        Err(e) => {
            debug!(bridge = %bridge_name, direction, error = %e, "iptables not available for cleanup");
            return;
        }
    };

    if !check.status.success() {
        debug!(bridge = %bridge_name, direction, "iptables rule does not exist, nothing to remove");
        return;
    }

    let mut del_args = vec!["-D", "FORWARD"];
    del_args.extend_from_slice(rule_args);

    match Command::new("iptables").args(&del_args).output().await {
        Ok(o) if o.status.success() => {
            debug!(bridge = %bridge_name, direction, "removed iptables FORWARD rule");
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            debug!(bridge = %bridge_name, direction, stderr = %stderr.trim(), "failed to remove iptables rule");
        }
        Err(e) => {
            debug!(bridge = %bridge_name, direction, error = %e, "failed to run iptables -D");
        }
    }
}

/// Run an `ip` command and check for success.
async fn run_ip(args: &[&str]) -> Result<()> {
    debug!(args = ?args, "running ip command");

    let output = Command::new("ip")
        .args(args)
        .output()
        .await
        .context("failed to execute ip command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ip {} failed: {}", args.first().unwrap_or(&""), stderr.trim());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tap_device_name_standard_uuid() {
        // A typical UUID short_id: first 8 chars
        let name = tap_device_name("abc12345-6789-0000-0000-000000000000");
        assert_eq!(name, "tap-abc12345");
    }

    #[test]
    fn test_tap_device_name_short_id() {
        // Already shortened to 8 chars
        let name = tap_device_name("abc12345");
        assert_eq!(name, "tap-abc12345");
    }

    #[test]
    fn test_tap_device_name_very_short_id() {
        // Very short ID (edge case)
        let name = tap_device_name("ab");
        assert_eq!(name, "tap-ab");
    }

    #[test]
    fn test_tap_device_name_length_within_limit() {
        // Linux interface names must be <= 15 chars
        // "tap-" (4) + 8 chars = 12, which is under 15
        let name = tap_device_name("abcdefgh");
        assert!(name.len() <= 15, "TAP name '{}' exceeds 15 char limit", name);
    }

    #[test]
    fn test_bridge_manager_name() {
        let bm = BridgeManager::new("br-agentiso".to_string(), "10.99.0.1/16".to_string());
        assert_eq!(bm.bridge_name(), "br-agentiso");
    }

    #[test]
    fn test_bridge_manager_custom_name() {
        let bm = BridgeManager::new("br-custom".to_string(), "192.168.1.1/24".to_string());
        assert_eq!(bm.bridge_name(), "br-custom");
    }
}
