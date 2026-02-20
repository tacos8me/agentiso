pub mod agent_card;
pub mod message_relay;
pub mod task_board;

use std::sync::Arc;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::workspace::{TeamLifecycleState, TeamState, WorkspaceManager};

pub use agent_card::{AgentCard, AgentEndpoints, AgentStatus};
pub use message_relay::MessageRelay;

/// Role definition for team creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleDef {
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// Team status report returned by team_status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamStatusReport {
    pub name: String,
    pub state: TeamLifecycleState,
    pub members: Vec<MemberStatus>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Status of a single team member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberStatus {
    pub name: String,
    pub workspace_id: String,
    pub ip: Option<String>,
    pub workspace_state: String,
    pub agent_status: AgentStatus,
}

/// Manages multi-agent team lifecycle: creation, status, and teardown.
pub struct TeamManager {
    workspace_manager: Arc<WorkspaceManager>,
    config: Arc<Config>,
    relay: Arc<MessageRelay>,
}

impl TeamManager {
    pub fn new(workspace_manager: Arc<WorkspaceManager>, config: Arc<Config>, relay: Arc<MessageRelay>) -> Self {
        Self {
            workspace_manager,
            config,
            relay,
        }
    }

    /// Get a reference to the message relay.
    #[allow(dead_code)]
    pub fn relay(&self) -> &Arc<MessageRelay> {
        &self.relay
    }

    /// Validate a team name: alphanumeric + hyphens, 1-64 chars.
    fn validate_name(name: &str) -> Result<()> {
        if name.is_empty() || name.len() > 64 {
            bail!("team name must be 1-64 characters, got {}", name.len());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            bail!(
                "team name must contain only alphanumeric characters and hyphens: '{}'",
                name
            );
        }
        if name.starts_with('-') || name.ends_with('-') {
            bail!("team name must not start or end with a hyphen: '{}'", name);
        }
        Ok(())
    }

    /// Create a new team with the given roles.
    ///
    /// If `parent_team` is specified, this creates a sub-team under the parent.
    /// Sub-teams inherit budget from the parent, respect nesting depth limits,
    /// and are cascade-destroyed when the parent is destroyed.
    pub async fn create_team(
        &self,
        name: &str,
        roles: Vec<RoleDef>,
        _base_snapshot: Option<&str>,
        max_vms: u32,
        parent_team: Option<&str>,
    ) -> Result<TeamState> {
        // Validate name
        Self::validate_name(name)?;

        // Check team doesn't already exist
        if self.workspace_manager.get_team(name).await?.is_some() {
            bail!("team '{}' already exists", name);
        }

        // Determine nesting depth and validate parent
        let nesting_depth = if let Some(parent_name) = parent_team {
            let parent = self
                .workspace_manager
                .get_team(parent_name)
                .await?
                .ok_or_else(|| anyhow::anyhow!("parent team '{}' not found", parent_name))?;

            let depth = parent.nesting_depth + 1;
            if depth > self.config.resources.max_nesting_depth {
                bail!(
                    "sub-team '{}' would exceed max nesting depth ({} > {})",
                    name,
                    depth,
                    self.config.resources.max_nesting_depth
                );
            }

            // Check parent has budget for the new VMs
            let parent_used = parent.member_workspace_ids.len() as u32;
            let roles_count = roles.len() as u32;
            if parent_used + roles_count > parent.max_vms {
                bail!(
                    "parent team '{}' budget exceeded ({} used + {} new > {} max_vms)",
                    parent_name,
                    parent_used,
                    roles_count,
                    parent.max_vms
                );
            }

            depth
        } else {
            0
        };

        // Check per-team VM cap
        let roles_count = roles.len() as u32;
        if roles_count > self.config.resources.max_vms_per_team {
            bail!(
                "team '{}' requests {} VMs which exceeds per-team cap ({})",
                name,
                roles_count,
                self.config.resources.max_vms_per_team
            );
        }

        // Check global VM cap
        let existing_count = self.workspace_manager.list().await?.len() as u32;
        if existing_count + roles_count > self.config.resources.max_total_vms {
            bail!(
                "creating {} VMs for team '{}' would exceed global VM cap ({} existing + {} new > {})",
                roles_count, name, existing_count, roles_count, self.config.resources.max_total_vms
            );
        }

        // Also check legacy max_workspaces limit
        if existing_count + roles_count > self.config.resources.max_workspaces {
            bail!(
                "creating {} workspaces for team '{}' would exceed max_workspaces limit ({} existing + {} new > {})",
                roles_count, name, existing_count, roles_count, self.config.resources.max_workspaces
            );
        }

        // Create team state in Creating state
        let team_state = TeamState {
            name: name.to_string(),
            state: TeamLifecycleState::Creating,
            member_workspace_ids: Vec::new(),
            created_at: chrono::Utc::now(),
            parent_team: parent_team.map(|s| s.to_string()),
            max_vms,
            nesting_depth,
        };
        self.workspace_manager
            .register_team(team_state.clone())
            .await?;

        // Create vault directories for the team
        if self.config.vault.enabled {
            if let Some(vault) = crate::mcp::vault::VaultManager::with_scope(
                &self.config.vault,
                &format!("teams/{}", name),
            ) {
                // Write placeholder files to create subdirs
                for subdir in &["cards", "tasks", "notes"] {
                    let path = format!("{}/.gitkeep", subdir);
                    vault
                        .write_note(&path, "", crate::mcp::vault::WriteMode::Overwrite)
                        .await
                        .ok();
                }
            }
        }

        // Create workspaces for each role
        let mut member_ids = Vec::with_capacity(roles.len());
        let mut member_ips = Vec::new();

        for role in &roles {
            // Use a short random suffix to avoid name collisions with leftover
            // workspaces from a previous team that wasn't fully cleaned up.
            let suffix = &uuid::Uuid::new_v4().to_string()[..4];
            let ws_name = format!("{}-{}-{}", name, role.name, suffix);
            let result = self
                .workspace_manager
                .create(crate::workspace::CreateParams {
                    name: Some(ws_name),
                    base_image: None,
                    vcpus: None,
                    memory_mb: None,
                    disk_gb: None,
                    allow_internet: None,
                })
                .await;

            match result {
                Ok(create_result) => {
                    let ws = &create_result.workspace;
                    let ws_id = ws.id;
                    let ip = ws.network.ip.to_string();

                    // Set team_id on the workspace
                    self.workspace_manager
                        .set_workspace_team_id(ws_id, Some(name.to_string()))
                        .await?;

                    // Register agent in message relay
                    self.relay.register(&role.name, name, ws_id).await;

                    member_ids.push(ws_id);
                    member_ips.push(ip.clone());

                    // Write AgentCard to vault
                    if self.config.vault.enabled {
                        if let Some(vault) = crate::mcp::vault::VaultManager::with_scope(
                            &self.config.vault,
                            &format!("teams/{}", name),
                        ) {
                            let card = AgentCard {
                                name: role.name.clone(),
                                role: role.role.clone(),
                                description: role.description.clone(),
                                skills: role.skills.clone(),
                                endpoints: AgentEndpoints {
                                    vsock_cid: ws.vsock_cid,
                                    ip: ip.clone(),
                                    http_port: 8080,
                                },
                                status: AgentStatus::Initializing,
                                team: name.to_string(),
                                workspace_id: ws_id.to_string(),
                            };
                            let card_json =
                                serde_json::to_string_pretty(&card).unwrap_or_default();
                            let card_path = format!("cards/{}.json", role.name);
                            vault
                                .write_note(
                                    &card_path,
                                    &card_json,
                                    crate::mcp::vault::WriteMode::Overwrite,
                                )
                                .await
                                .ok();
                        }
                    }
                }
                Err(e) => {
                    // Rollback: destroy already-created workspaces
                    tracing::error!(
                        role = %role.name,
                        error = %e,
                        "failed to create workspace for team role, rolling back"
                    );
                    // Rollback: unregister from relay
                    for r in &roles[..member_ids.len()] {
                        self.relay.unregister(&r.name, name).await;
                    }
                    for id in &member_ids {
                        if let Err(e2) = self.workspace_manager.destroy(*id).await {
                            tracing::warn!(
                                id = %id,
                                error = %e2,
                                "failed to destroy workspace during team creation rollback"
                            );
                        }
                    }
                    self.workspace_manager.remove_team(name).await?;
                    return Err(e.context(format!(
                        "failed to create workspace for team role '{}'",
                        role.name
                    )));
                }
            }
        }

        // Apply intra-team nftables rules
        if member_ips.len() > 1 {
            let nw = self.workspace_manager.network_manager().await;
            nw.nftables()
                .apply_team_rules(name, &member_ips)
                .await
                .ok();
        }

        // Apply parent-child nftables rules for nested teams
        if let Some(parent_name) = parent_team {
            let parent = self.workspace_manager.get_team(parent_name).await?;
            if let Some(parent_state) = parent {
                // Collect parent member IPs
                let mut parent_ips = Vec::new();
                for ws_id in &parent_state.member_workspace_ids {
                    if let Ok(ws) = self.workspace_manager.get(*ws_id).await {
                        parent_ips.push(ws.network.ip.to_string());
                    }
                }
                if !parent_ips.is_empty() && !member_ips.is_empty() {
                    let nw = self.workspace_manager.network_manager().await;
                    nw.nftables()
                        .apply_nested_team_rules(parent_name, name, &parent_ips, &member_ips)
                        .await
                        .ok();
                }
            }
        }

        // Connect relay vsock for each team member
        {
            let mut vm_manager = self.workspace_manager.vm_manager().await;
            for ws_id in &member_ids {
                if let Err(e) = vm_manager.connect_relay(ws_id).await {
                    tracing::warn!(
                        workspace_id = %ws_id,
                        error = %e,
                        "failed to connect relay vsock (team messaging may not work)"
                    );
                }
            }
        }

        // Update team state to Ready with member IDs
        let final_state = TeamState {
            name: name.to_string(),
            state: TeamLifecycleState::Ready,
            member_workspace_ids: member_ids,
            created_at: team_state.created_at,
            parent_team: parent_team.map(|s| s.to_string()),
            max_vms,
            nesting_depth,
        };
        self.workspace_manager
            .register_team(final_state.clone())
            .await?;

        Ok(final_state)
    }

    /// Destroy a team: cascade-destroy sub-teams, tear down all member
    /// workspaces in parallel, remove nftables rules, and remove team state.
    pub async fn destroy_team(&self, name: &str) -> Result<()> {
        let team = self
            .workspace_manager
            .get_team(name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("team '{}' not found", name))?;

        // Cascade-destroy any sub-teams whose parent_team matches this team
        let all_teams = self.workspace_manager.list_teams().await;
        for child in &all_teams {
            if child.parent_team.as_deref() == Some(name) {
                if let Err(e) = Box::pin(self.destroy_team(&child.name)).await {
                    tracing::warn!(
                        child = %child.name,
                        error = %e,
                        "failed to cascade-destroy sub-team"
                    );
                }
            }
        }

        // Unregister all team agents from message relay
        self.relay.unregister_team(name).await;

        // Destroy workspaces in parallel
        let mut join_set = tokio::task::JoinSet::new();
        for ws_id in team.member_workspace_ids.clone() {
            let wm = self.workspace_manager.clone();
            join_set.spawn(async move { wm.destroy(ws_id).await });
        }

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "failed to destroy team member workspace");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "team member destroy task panicked");
                }
            }
        }

        // Remove nested team nftables rules if this is a sub-team
        if let Some(ref parent_name) = team.parent_team {
            let nw = self.workspace_manager.network_manager().await;
            nw.nftables()
                .remove_nested_team_rules(parent_name, name)
                .await
                .ok();
        }

        // Remove intra-team nftables rules
        let nw = self.workspace_manager.network_manager().await;
        nw.nftables().remove_team_rules(name).await.ok();

        // Remove team from persisted state
        self.workspace_manager.remove_team(name).await?;

        Ok(())
    }

    /// Get a status report for a team.
    pub async fn team_status(&self, name: &str) -> Result<TeamStatusReport> {
        let team = self
            .workspace_manager
            .get_team(name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("team '{}' not found", name))?;

        let mut members = Vec::new();
        for ws_id in &team.member_workspace_ids {
            match self.workspace_manager.get(*ws_id).await {
                Ok(ws) => {
                    // Extract role name from workspace name (team-role pattern)
                    let member_name = ws
                        .name
                        .strip_prefix(&format!("{}-", name))
                        .unwrap_or(&ws.name)
                        .to_string();
                    members.push(MemberStatus {
                        name: member_name,
                        workspace_id: ws_id.to_string(),
                        ip: Some(ws.network.ip.to_string()),
                        workspace_state: ws.state.to_string(),
                        agent_status: AgentStatus::Ready,
                    });
                }
                Err(_) => {
                    members.push(MemberStatus {
                        name: ws_id.to_string(),
                        workspace_id: ws_id.to_string(),
                        ip: None,
                        workspace_state: "unknown".to_string(),
                        agent_status: AgentStatus::Failed,
                    });
                }
            }
        }

        Ok(TeamStatusReport {
            name: team.name.clone(),
            state: team.state.clone(),
            members,
            created_at: team.created_at,
        })
    }

    /// List all teams.
    pub async fn list_teams(&self) -> Result<Vec<TeamState>> {
        Ok(self.workspace_manager.list_teams().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_valid_names() {
        assert!(TeamManager::validate_name("my-team").is_ok());
        assert!(TeamManager::validate_name("team1").is_ok());
        assert!(TeamManager::validate_name("a").is_ok());
        assert!(TeamManager::validate_name("team-alpha-1").is_ok());
        assert!(TeamManager::validate_name(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(TeamManager::validate_name("").is_err());
    }

    #[test]
    fn validate_name_rejects_too_long() {
        assert!(TeamManager::validate_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn validate_name_rejects_special_chars() {
        assert!(TeamManager::validate_name("my team").is_err());
        assert!(TeamManager::validate_name("my_team").is_err());
        assert!(TeamManager::validate_name("team/alpha").is_err());
        assert!(TeamManager::validate_name("team.alpha").is_err());
    }

    #[test]
    fn validate_name_rejects_leading_trailing_hyphen() {
        assert!(TeamManager::validate_name("-team").is_err());
        assert!(TeamManager::validate_name("team-").is_err());
    }

    #[test]
    fn role_def_serde_roundtrip() {
        let role = RoleDef {
            name: "researcher".to_string(),
            role: "research".to_string(),
            skills: vec!["web_search".to_string()],
            description: "Researches topics".to_string(),
        };
        let json = serde_json::to_string(&role).unwrap();
        let deserialized: RoleDef = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "researcher");
        assert_eq!(deserialized.skills.len(), 1);
    }

    #[test]
    fn role_def_defaults() {
        let json = r#"{"name": "coder", "role": "dev"}"#;
        let role: RoleDef = serde_json::from_str(json).unwrap();
        assert_eq!(role.name, "coder");
        assert!(role.skills.is_empty());
        assert!(role.description.is_empty());
    }

    #[test]
    fn team_status_report_serde_roundtrip() {
        let report = TeamStatusReport {
            name: "my-team".to_string(),
            state: TeamLifecycleState::Ready,
            members: vec![MemberStatus {
                name: "coder".to_string(),
                workspace_id: "abcd1234".to_string(),
                ip: Some("10.99.0.2".to_string()),
                workspace_state: "running".to_string(),
                agent_status: AgentStatus::Ready,
            }],
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: TeamStatusReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "my-team");
        assert_eq!(deserialized.members.len(), 1);
        assert_eq!(deserialized.members[0].name, "coder");
    }

    #[test]
    fn team_state_nesting_depth_default() {
        // TeamState should default nesting_depth to 0 when deserialized from
        // JSON that lacks the field (backwards compatibility).
        let json = r#"{
            "name": "old-team",
            "state": "Ready",
            "member_workspace_ids": [],
            "created_at": "2026-01-01T00:00:00Z",
            "parent_team": null,
            "max_vms": 10
        }"#;
        let ts: TeamState = serde_json::from_str(json).unwrap();
        assert_eq!(ts.nesting_depth, 0);
    }

    #[test]
    fn team_state_nesting_depth_roundtrip() {
        let ts = TeamState {
            name: "sub-team".to_string(),
            state: TeamLifecycleState::Ready,
            member_workspace_ids: vec![],
            created_at: chrono::Utc::now(),
            parent_team: Some("parent".to_string()),
            max_vms: 5,
            nesting_depth: 2,
        };
        let json = serde_json::to_string(&ts).unwrap();
        let deserialized: TeamState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.nesting_depth, 2);
        assert_eq!(deserialized.parent_team.as_deref(), Some("parent"));
    }

    #[test]
    fn member_status_with_none_ip() {
        let ms = MemberStatus {
            name: "dead-agent".to_string(),
            workspace_id: "00000000".to_string(),
            ip: None,
            workspace_state: "unknown".to_string(),
            agent_status: AgentStatus::Failed,
        };
        let json = serde_json::to_string(&ms).unwrap();
        let deserialized: MemberStatus = serde_json::from_str(&json).unwrap();
        assert!(deserialized.ip.is_none());
        assert_eq!(deserialized.agent_status, AgentStatus::Failed);
    }
}
