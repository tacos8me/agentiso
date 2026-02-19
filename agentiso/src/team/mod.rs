pub mod agent_card;
pub mod task_board;

use std::sync::Arc;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::workspace::{TeamLifecycleState, TeamState, WorkspaceManager};

pub use agent_card::{AgentCard, AgentEndpoints, AgentStatus};
pub use task_board::{BoardTask, TaskBoard, TaskPriority, TaskStatus, TaskUpdate};

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
}

impl TeamManager {
    pub fn new(workspace_manager: Arc<WorkspaceManager>, config: Arc<Config>) -> Self {
        Self {
            workspace_manager,
            config,
        }
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
    pub async fn create_team(
        &self,
        name: &str,
        roles: Vec<RoleDef>,
        _base_snapshot: Option<&str>,
        max_vms: u32,
    ) -> Result<TeamState> {
        // Validate name
        Self::validate_name(name)?;

        // Check team doesn't already exist
        if self.workspace_manager.get_team(name).await?.is_some() {
            bail!("team '{}' already exists", name);
        }

        // Check VM cap
        let existing_count = self.workspace_manager.list().await?.len() as u32;
        let roles_count = roles.len() as u32;
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
            parent_team: None,
            max_vms,
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
            let ws_name = format!("{}-{}", name, role.name);
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

        // Update team state to Ready with member IDs
        let final_state = TeamState {
            name: name.to_string(),
            state: TeamLifecycleState::Ready,
            member_workspace_ids: member_ids,
            created_at: team_state.created_at,
            parent_team: None,
            max_vms,
        };
        self.workspace_manager
            .register_team(final_state.clone())
            .await?;

        Ok(final_state)
    }

    /// Destroy a team: tear down all member workspaces in parallel,
    /// remove nftables rules, and remove team state.
    pub async fn destroy_team(&self, name: &str) -> Result<()> {
        let team = self
            .workspace_manager
            .get_team(name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("team '{}' not found", name))?;

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

        // Remove nftables rules
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
