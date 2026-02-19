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
    /// IP address of the bridge in CIDR notation (default: "10.42.0.1/16")
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
    /// Idempotent: safe to call multiple times.
    #[instrument(skip(self))]
    pub async fn ensure_bridge(&self) -> Result<()> {
        if self.bridge_exists().await? {
            debug!(bridge = %self.bridge_name, "bridge already exists");
            // Ensure it's up
            self.set_link_up(&self.bridge_name).await?;
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
/// This adds two idempotent rules:
///   -A FORWARD -i <bridge> -j ACCEPT   (outbound from VMs)
///   -A FORWARD -o <bridge> -j ACCEPT   (return traffic to VMs)
pub async fn ensure_iptables_forward(bridge_name: &str) -> Result<()> {
    // Check if rules already exist before adding (idempotent).
    for (flag, direction) in [("-i", "outbound"), ("-o", "inbound")] {
        let check = Command::new("iptables")
            .args(["-C", "FORWARD", flag, bridge_name, "-j", "ACCEPT"])
            .output()
            .await
            .context("failed to run iptables -C (is the iptables binary installed?)")?;

        if !check.status.success() {
            // Rule doesn't exist, add it.
            let add = Command::new("iptables")
                .args(["-I", "FORWARD", "1", flag, bridge_name, "-j", "ACCEPT"])
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
    }

    Ok(())
}

/// Remove iptables FORWARD ACCEPT rules for bridge traffic.
///
/// This is the counterpart to [`ensure_iptables_forward`]. It checks whether
/// each rule exists with `iptables -C` and, if so, removes it with `iptables -D`.
///
/// Best-effort: does not fail if rules are already absent or if `iptables` is
/// unavailable.
pub async fn cleanup_iptables_forward(bridge_name: &str) -> Result<()> {
    for (flag, direction) in [("-i", "outbound"), ("-o", "inbound")] {
        // Check if the rule exists.
        let check = match Command::new("iptables")
            .args(["-C", "FORWARD", flag, bridge_name, "-j", "ACCEPT"])
            .output()
            .await
        {
            Ok(output) => output,
            Err(e) => {
                debug!(
                    bridge = %bridge_name,
                    direction,
                    error = %e,
                    "iptables not available, skipping FORWARD rule cleanup"
                );
                return Ok(());
            }
        };

        if !check.status.success() {
            debug!(
                bridge = %bridge_name,
                direction,
                "iptables FORWARD ACCEPT rule does not exist, nothing to remove"
            );
            continue;
        }

        // Rule exists, remove it.
        let remove = match Command::new("iptables")
            .args(["-D", "FORWARD", flag, bridge_name, "-j", "ACCEPT"])
            .output()
            .await
        {
            Ok(output) => output,
            Err(e) => {
                debug!(
                    bridge = %bridge_name,
                    direction,
                    error = %e,
                    "failed to run iptables -D, skipping"
                );
                continue;
            }
        };

        if !remove.status.success() {
            let stderr = String::from_utf8_lossy(&remove.stderr);
            debug!(
                bridge = %bridge_name,
                direction,
                stderr = %stderr.trim(),
                "failed to remove iptables FORWARD ACCEPT rule"
            );
        } else {
            debug!(
                bridge = %bridge_name,
                direction,
                "removed iptables FORWARD ACCEPT rule"
            );
        }
    }

    Ok(())
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
        let bm = BridgeManager::new("br-agentiso".to_string(), "10.42.0.1/16".to_string());
        assert_eq!(bm.bridge_name(), "br-agentiso");
    }

    #[test]
    fn test_bridge_manager_custom_name() {
        let bm = BridgeManager::new("br-custom".to_string(), "192.168.1.1/24".to_string());
        assert_eq!(bm.bridge_name(), "br-custom");
    }
}
