pub mod bridge;
pub mod dhcp;
pub mod nftables;

use std::net::Ipv4Addr;

use anyhow::{Context, Result};
use tracing::{info, instrument, warn};

#[allow(unused_imports)]
pub use bridge::{tap_device_name, BridgeManager};
pub use dhcp::IpAllocator;
#[allow(unused_imports)]
pub use nftables::{NetworkPolicy, NftablesManager, PortForwardRule};

/// Result of setting up networking for a new workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceNetworkSetup {
    /// Name of the TAP device (e.g. "tap-abc12345")
    pub tap_device: String,
    /// Allocated guest IP address
    pub guest_ip: Ipv4Addr,
    /// Gateway IP (bridge IP)
    pub gateway_ip: Ipv4Addr,
}

/// High-level network manager for workspace lifecycle operations.
///
/// Coordinates the bridge, IP allocator, and nftables managers to provide
/// a simple interface for workspace networking setup and teardown.
#[allow(dead_code)]
pub struct NetworkManager {
    bridge: BridgeManager,
    nftables: NftablesManager,
    ip_allocator: IpAllocator,
    gateway_ip: Ipv4Addr,
    bridge_subnet: String,
}

#[allow(dead_code)]
impl NetworkManager {
    /// Create a new NetworkManager with default settings.
    ///
    /// Bridge: `br-agentiso` with IP `10.99.0.1/16`.
    /// Subnet: `10.99.0.0/16`.
    pub fn new() -> Self {
        let gateway_ip = Ipv4Addr::new(10, 99, 0, 1);
        let bridge_name = "br-agentiso".to_string();
        let bridge_subnet = "10.99.0.0/16".to_string();

        Self {
            bridge: BridgeManager::new(bridge_name.clone(), "10.99.0.1/16".to_string()),
            nftables: NftablesManager::new(bridge_name, bridge_subnet.clone(), gateway_ip),
            ip_allocator: IpAllocator::new(gateway_ip),
            gateway_ip,
            bridge_subnet,
        }
    }

    /// Create a new NetworkManager with custom settings.
    pub fn with_config(
        bridge_name: String,
        bridge_cidr: String,
        bridge_subnet: String,
        gateway_ip: Ipv4Addr,
    ) -> Self {
        Self {
            bridge: BridgeManager::new(bridge_name.clone(), bridge_cidr),
            nftables: NftablesManager::new(bridge_name, bridge_subnet.clone(), gateway_ip),
            ip_allocator: IpAllocator::new(gateway_ip),
            gateway_ip,
            bridge_subnet,
        }
    }

    /// Access the bridge manager.
    pub fn bridge(&self) -> &BridgeManager {
        &self.bridge
    }

    /// Access the nftables manager.
    pub fn nftables(&self) -> &NftablesManager {
        &self.nftables
    }

    /// Access the IP allocator (mutable).
    pub fn ip_allocator_mut(&mut self) -> &mut IpAllocator {
        &mut self.ip_allocator
    }

    /// Access the IP allocator (immutable).
    pub fn ip_allocator(&self) -> &IpAllocator {
        &self.ip_allocator
    }

    /// Gateway IP address (bridge IP).
    pub fn gateway_ip(&self) -> Ipv4Addr {
        self.gateway_ip
    }

    /// Initialize networking: ensure bridge exists, enable IP forwarding, init nftables.
    ///
    /// Should be called once at startup.
    pub async fn init(&self) -> Result<()> {
        self.bridge
            .ensure_bridge()
            .await
            .context("failed to ensure bridge")?;

        bridge::enable_ip_forwarding()
            .await
            .context("failed to enable IP forwarding")?;

        if let Err(e) = bridge::ensure_iptables_forward(self.bridge.bridge_name()).await {
            warn!(error = %e, "failed to add iptables FORWARD rules (VM internet may not work if host has iptables FORWARD DROP policy)");
        }

        if let Err(e) = bridge::ensure_iptables_nat(self.bridge.bridge_name(), &self.bridge_subnet).await {
            warn!(error = %e, "failed to add iptables NAT MASQUERADE rule (VM internet may not work if nftables masquerade is overridden by iptables)");
        }

        self.nftables
            .init()
            .await
            .context("failed to initialize nftables")?;

        info!("network manager initialized");
        Ok(())
    }

    /// Set up networking for a new workspace.
    ///
    /// 1. Allocate an IP address
    /// 2. Create a TAP device and attach to bridge
    /// 3. Apply default firewall rules
    ///
    /// Returns the TAP device name, allocated IP, and gateway IP.
    #[instrument(skip(self))]
    pub async fn setup_workspace(
        &mut self,
        workspace_id: &str,
        policy: &NetworkPolicy,
    ) -> Result<WorkspaceNetworkSetup> {
        // Allocate IP
        let guest_ip = self
            .ip_allocator
            .allocate()
            .context("failed to allocate IP for workspace")?;

        // Create TAP device
        let tap_device = match self.bridge.create_tap(workspace_id).await {
            Ok(tap) => tap,
            Err(e) => {
                // Release the IP if TAP creation fails
                self.ip_allocator.release(guest_ip);
                return Err(e.context("failed to create TAP device"));
            }
        };

        // Apply firewall rules (is_new=true: skip remove_workspace_rules for new workspaces)
        if let Err(e) = self
            .nftables
            .apply_workspace_rules(workspace_id, guest_ip, policy, true)
            .await
        {
            // Clean up on failure
            self.ip_allocator.release(guest_ip);
            let _ = self.bridge.destroy_tap(workspace_id).await;
            return Err(e.context("failed to apply firewall rules"));
        }

        info!(
            workspace_id = %workspace_id,
            tap = %tap_device,
            ip = %guest_ip,
            "workspace network setup complete"
        );

        Ok(WorkspaceNetworkSetup {
            tap_device,
            guest_ip,
            gateway_ip: self.gateway_ip,
        })
    }

    /// Update the network policy for a running workspace.
    #[instrument(skip(self))]
    pub async fn update_policy(
        &self,
        workspace_id: &str,
        guest_ip: Ipv4Addr,
        policy: &NetworkPolicy,
    ) -> Result<()> {
        self.nftables
            .apply_workspace_rules(workspace_id, guest_ip, policy, false)
            .await
            .context("failed to update workspace network policy")
    }

    /// Add a port forwarding rule for a workspace.
    #[instrument(skip(self))]
    pub async fn add_port_forward(
        &self,
        workspace_id: &str,
        guest_ip: Ipv4Addr,
        guest_port: u16,
        host_port: u16,
    ) -> Result<()> {
        self.nftables
            .add_port_forward(workspace_id, guest_ip, guest_port, host_port)
            .await
            .context("failed to add port forward")
    }

    /// Remove a port forwarding rule for a workspace.
    #[instrument(skip(self))]
    pub async fn remove_port_forward(
        &self,
        workspace_id: &str,
        guest_port: u16,
    ) -> Result<()> {
        self.nftables
            .remove_port_forward(workspace_id, guest_port)
            .await
            .context("failed to remove port forward")
    }

    /// Ensure networking is set up for a workspace that already has an IP allocated.
    ///
    /// Used when restarting a stopped workspace (e.g. after server restart) where the
    /// TAP device and nftables rules may no longer exist but the IP is still allocated.
    ///
    /// 1. (Re-)create the TAP device and attach to bridge
    /// 2. (Re-)apply firewall rules
    #[instrument(skip(self))]
    pub async fn ensure_workspace_network(
        &self,
        workspace_id: &str,
        guest_ip: Ipv4Addr,
        policy: &NetworkPolicy,
    ) -> Result<String> {
        // Destroy existing TAP if present (best-effort, ignore errors)
        let _ = self.bridge.destroy_tap(workspace_id).await;

        // Create TAP device
        let tap_device = self
            .bridge
            .create_tap(workspace_id)
            .await
            .context("failed to create TAP device for workspace restart")?;

        // Apply firewall rules (is_new=false: remove old rules first for existing workspaces)
        if let Err(e) = self
            .nftables
            .apply_workspace_rules(workspace_id, guest_ip, policy, false)
            .await
        {
            let _ = self.bridge.destroy_tap(workspace_id).await;
            return Err(e.context("failed to apply firewall rules for workspace restart"));
        }

        info!(
            workspace_id = %workspace_id,
            tap = %tap_device,
            ip = %guest_ip,
            "workspace network ensured for restart"
        );

        Ok(tap_device)
    }

    /// Tear down all networking for a workspace.
    ///
    /// 1. Remove all nftables rules
    /// 2. Destroy the TAP device
    /// 3. Release the IP address
    #[instrument(skip(self))]
    pub async fn cleanup_workspace(
        &mut self,
        workspace_id: &str,
        guest_ip: Ipv4Addr,
    ) -> Result<()> {
        // Remove firewall rules (best-effort, continue on error)
        if let Err(e) = self.nftables.remove_workspace_rules(workspace_id).await {
            tracing::warn!(
                workspace_id = %workspace_id,
                error = %e,
                "failed to remove nftables rules during cleanup"
            );
        }

        // Destroy TAP device (best-effort)
        if let Err(e) = self.bridge.destroy_tap(workspace_id).await {
            tracing::warn!(
                workspace_id = %workspace_id,
                error = %e,
                "failed to destroy TAP device during cleanup"
            );
        }

        // Release IP
        self.ip_allocator.release(guest_ip);

        info!(workspace_id = %workspace_id, "workspace network cleanup complete");
        Ok(())
    }

    /// Restore a previously allocated IP into the allocator (for state recovery).
    pub fn restore_ip(&mut self, ip: Ipv4Addr) -> Result<()> {
        self.ip_allocator.mark_allocated(ip)
    }

    /// Shut down the network manager, cleaning up global resources.
    ///
    /// Removes the iptables FORWARD ACCEPT rules that were added at startup.
    /// Per-workspace resources (TAP devices, nftables rules, IPs) should already
    /// have been cleaned up via [`cleanup_workspace`] before calling this.
    ///
    /// Best-effort: logs warnings but does not fail.
    pub async fn shutdown(&self) {
        info!("shutting down network manager");

        if let Err(e) = bridge::cleanup_iptables_forward(self.bridge.bridge_name()).await {
            warn!(error = %e, "failed to clean up iptables FORWARD rules during shutdown");
        }

        if let Err(e) = bridge::cleanup_iptables_nat(self.bridge.bridge_name(), &self.bridge_subnet).await {
            warn!(error = %e, "failed to clean up iptables NAT MASQUERADE rule during shutdown");
        }

        info!("network manager shutdown complete");
    }
}
