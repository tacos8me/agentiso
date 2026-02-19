//! Team MCP tool handler.
//!
//! Provides a bundled "team" tool with create/destroy/status/list actions,
//! dispatching to TeamManager methods.

use rmcp::ErrorData as McpError;
use rmcp::model::*;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::info;

use crate::team::RoleDef;

use super::tools::AgentisoServer;

// ---------------------------------------------------------------------------
// Parameter struct
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct TeamParams {
    /// Action: "create", "destroy", "status", "list"
    pub action: String,
    /// Team name (required for create, destroy, status)
    #[serde(default)]
    pub name: Option<String>,
    /// Role definitions for team members (for create)
    #[serde(default)]
    pub roles: Option<Vec<RoleDefParam>>,
    /// Maximum VMs for the team (for create, default: 10)
    #[serde(default)]
    pub max_vms: Option<u32>,
    /// Base snapshot to use for team workspaces (for create, optional)
    #[serde(default)]
    pub base_snapshot: Option<String>,
}

/// Role definition parameter (mirrors team::RoleDef but with JsonSchema).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct RoleDefParam {
    /// Member name (used as workspace name suffix)
    pub name: String,
    /// Role description (e.g. "researcher", "coder", "reviewer")
    pub role: String,
    /// Skills/capabilities this agent has
    #[serde(default)]
    pub skills: Vec<String>,
    /// Human-readable description of this member's purpose
    #[serde(default)]
    pub description: String,
}

impl From<RoleDefParam> for RoleDef {
    fn from(p: RoleDefParam) -> Self {
        RoleDef {
            name: p.name,
            role: p.role,
            skills: p.skills,
            description: p.description,
        }
    }
}

// ---------------------------------------------------------------------------
// Handler implementation
// ---------------------------------------------------------------------------

impl AgentisoServer {
    pub(crate) async fn handle_team(
        &self,
        params: TeamParams,
    ) -> Result<CallToolResult, McpError> {
        let team_manager = self.team_manager.as_ref().ok_or_else(|| {
            McpError::invalid_request(
                "Team management is not available. The server was started without team support."
                    .to_string(),
                None,
            )
        })?;

        match params.action.as_str() {
            "create" => {
                let name = params.name.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'name' is required for action 'create'".to_string(),
                        None,
                    )
                })?;
                let roles = params.roles.ok_or_else(|| {
                    McpError::invalid_params(
                        "'roles' is required for action 'create'. Provide an array of \
                         {name, role, skills?, description?} objects."
                            .to_string(),
                        None,
                    )
                })?;

                if roles.is_empty() {
                    return Err(McpError::invalid_params(
                        "'roles' must contain at least one role definition".to_string(),
                        None,
                    ));
                }

                let max_vms = params.max_vms.unwrap_or(10);
                let base_snapshot = params.base_snapshot.as_deref();

                info!(
                    tool = "team",
                    action = "create",
                    name = %name,
                    role_count = roles.len(),
                    max_vms,
                    "tool call"
                );

                let role_defs: Vec<RoleDef> = roles.into_iter().map(|r| r.into()).collect();

                let team_state = team_manager
                    .create_team(name, role_defs, base_snapshot, max_vms)
                    .await
                    .map_err(|e| {
                        McpError::internal_error(
                            format!("failed to create team '{}': {:#}", name, e),
                            None,
                        )
                    })?;

                let info = serde_json::json!({
                    "action": "create",
                    "name": team_state.name,
                    "state": format!("{:?}", team_state.state),
                    "member_count": team_state.member_workspace_ids.len(),
                    "member_workspace_ids": team_state.member_workspace_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                    "max_vms": team_state.max_vms,
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&info).unwrap(),
                )]))
            }

            "destroy" => {
                let name = params.name.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'name' is required for action 'destroy'".to_string(),
                        None,
                    )
                })?;

                info!(tool = "team", action = "destroy", name = %name, "tool call");

                team_manager.destroy_team(name).await.map_err(|e| {
                    McpError::internal_error(
                        format!("failed to destroy team '{}': {:#}", name, e),
                        None,
                    )
                })?;

                let info = serde_json::json!({
                    "action": "destroy",
                    "name": name,
                    "success": true,
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&info).unwrap(),
                )]))
            }

            "status" => {
                let name = params.name.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'name' is required for action 'status'".to_string(),
                        None,
                    )
                })?;

                info!(tool = "team", action = "status", name = %name, "tool call");

                let report = team_manager.team_status(name).await.map_err(|e| {
                    McpError::internal_error(
                        format!("failed to get status for team '{}': {:#}", name, e),
                        None,
                    )
                })?;

                let info = serde_json::json!({
                    "action": "status",
                    "name": report.name,
                    "state": format!("{:?}", report.state),
                    "created_at": report.created_at.to_rfc3339(),
                    "members": report.members.iter().map(|m| {
                        serde_json::json!({
                            "name": m.name,
                            "workspace_id": m.workspace_id,
                            "ip": m.ip,
                            "workspace_state": m.workspace_state,
                            "agent_status": format!("{:?}", m.agent_status),
                        })
                    }).collect::<Vec<_>>(),
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&info).unwrap(),
                )]))
            }

            "list" => {
                info!(tool = "team", action = "list", "tool call");

                let teams = team_manager.list_teams().await.map_err(|e| {
                    McpError::internal_error(
                        format!("failed to list teams: {:#}", e),
                        None,
                    )
                })?;

                let info = serde_json::json!({
                    "action": "list",
                    "teams": teams.iter().map(|t| {
                        serde_json::json!({
                            "name": t.name,
                            "state": format!("{:?}", t.state),
                            "member_count": t.member_workspace_ids.len(),
                            "created_at": t.created_at.to_rfc3339(),
                            "max_vms": t.max_vms,
                        })
                    }).collect::<Vec<_>>(),
                    "count": teams.len(),
                });

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&info).unwrap(),
                )]))
            }

            other => Err(McpError::invalid_params(
                format!(
                    "unknown team action '{}'. Valid actions: create, destroy, status, list",
                    other
                ),
                None,
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_team_params_create() {
        let json = serde_json::json!({
            "action": "create",
            "name": "my-team",
            "roles": [
                {"name": "coder", "role": "developer"},
                {"name": "reviewer", "role": "review", "skills": ["code_review"], "description": "Reviews PRs"}
            ],
            "max_vms": 5
        });
        let params: TeamParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "create");
        assert_eq!(params.name.as_deref(), Some("my-team"));
        assert_eq!(params.roles.as_ref().unwrap().len(), 2);
        assert_eq!(params.max_vms, Some(5));
    }

    #[test]
    fn test_team_params_destroy() {
        let json = serde_json::json!({
            "action": "destroy",
            "name": "my-team"
        });
        let params: TeamParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "destroy");
        assert_eq!(params.name.as_deref(), Some("my-team"));
    }

    #[test]
    fn test_team_params_status() {
        let json = serde_json::json!({
            "action": "status",
            "name": "my-team"
        });
        let params: TeamParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "status");
        assert_eq!(params.name.as_deref(), Some("my-team"));
    }

    #[test]
    fn test_team_params_list() {
        let json = serde_json::json!({
            "action": "list"
        });
        let params: TeamParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "list");
        assert!(params.name.is_none());
    }

    #[test]
    fn test_team_params_missing_action() {
        let json = serde_json::json!({
            "name": "my-team"
        });
        assert!(serde_json::from_value::<TeamParams>(json).is_err());
    }

    #[test]
    fn test_role_def_param_full() {
        let json = serde_json::json!({
            "name": "researcher",
            "role": "research",
            "skills": ["web_search", "analysis"],
            "description": "Researches topics"
        });
        let param: RoleDefParam = serde_json::from_value(json).unwrap();
        assert_eq!(param.name, "researcher");
        assert_eq!(param.role, "research");
        assert_eq!(param.skills.len(), 2);
        assert_eq!(param.description, "Researches topics");
    }

    #[test]
    fn test_role_def_param_minimal() {
        let json = serde_json::json!({
            "name": "coder",
            "role": "dev"
        });
        let param: RoleDefParam = serde_json::from_value(json).unwrap();
        assert_eq!(param.name, "coder");
        assert_eq!(param.role, "dev");
        assert!(param.skills.is_empty());
        assert!(param.description.is_empty());
    }

    #[test]
    fn test_role_def_param_to_role_def() {
        let param = RoleDefParam {
            name: "coder".to_string(),
            role: "dev".to_string(),
            skills: vec!["rust".to_string()],
            description: "Writes code".to_string(),
        };
        let role_def: RoleDef = param.into();
        assert_eq!(role_def.name, "coder");
        assert_eq!(role_def.role, "dev");
        assert_eq!(role_def.skills, vec!["rust"]);
        assert_eq!(role_def.description, "Writes code");
    }
}
