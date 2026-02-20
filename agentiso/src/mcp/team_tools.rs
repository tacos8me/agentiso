//! Team MCP tool handler.
//!
//! Provides a bundled "team" tool with create/destroy/status/list/message/receive
//! actions, dispatching to TeamManager methods and MessageRelay.

use rmcp::ErrorData as McpError;
use rmcp::model::*;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{info, warn};

use crate::team::RoleDef;

use super::rate_limit;
use super::tools::AgentisoServer;

/// Truncate a string at a UTF-8 safe boundary, appending "... (truncated)" if needed.
fn safe_truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Walk backwards to find a char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... (truncated)", &s[..end])
}

// ---------------------------------------------------------------------------
// Parameter struct
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct TeamParams {
    /// Action: "create", "destroy", "status", "list", "message", "receive"
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
    /// Parent team name for creating a sub-team (optional, for create action)
    #[serde(default)]
    pub parent_team: Option<String>,
    /// Sender agent name (required for message action)
    #[serde(default)]
    pub agent: Option<String>,
    /// Recipient agent name or "*" for broadcast (required for message action)
    #[serde(default)]
    pub to: Option<String>,
    /// Message content (required for message action)
    #[serde(default)]
    pub content: Option<String>,
    /// Message type hint: "text", "task_update", "request", "response" (for message action, default "text")
    #[serde(default)]
    pub message_type: Option<String>,
    /// Max messages to retrieve (for receive action, default 10)
    #[serde(default)]
    pub limit: Option<u32>,
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

                let parent_team = params.parent_team.as_deref();
                let team_state = team_manager
                    .create_team(name, role_defs, base_snapshot, max_vms, parent_team)
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

            "message" => {
                let name = params.name.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'name' is required for action 'message' (team name)".to_string(),
                        None,
                    )
                })?;
                let to = params.to.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'to' is required for action 'message'. Provide a recipient agent name or \"*\" for broadcast."
                            .to_string(),
                        None,
                    )
                })?;
                let content = params.content.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'content' is required for action 'message'".to_string(),
                        None,
                    )
                })?;
                let from = params.agent.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'agent' is required for action 'message' (sender agent name)".to_string(),
                        None,
                    )
                })?;
                let message_type = params.message_type.as_deref().unwrap_or("text");

                // Rate limit check
                if let Err(e) = self.rate_limiter.check(rate_limit::CATEGORY_TEAM_MESSAGE) {
                    return Err(McpError::invalid_request(e, None));
                }

                info!(
                    tool = "team",
                    action = "message",
                    team = %name,
                    from = %from,
                    to = %to,
                    message_type = %message_type,
                    "tool call"
                );

                match self.message_relay.send(name, from, to, content, message_type).await {
                    Ok(message_id) => {
                        // Push to guest via relay vsock (fire-and-forget).
                        // Determine recipients: direct -> [to], broadcast -> all except sender.
                        let recipients: Vec<String> = if to == "*" {
                            self.message_relay
                                .team_agents(name)
                                .await
                                .into_iter()
                                .filter(|a| a != from)
                                .collect()
                        } else {
                            vec![to.to_string()]
                        };

                        for recipient in &recipients {
                            if let Some(ws_id) =
                                self.message_relay.workspace_id(name, recipient).await
                            {
                                match self.workspace_manager.relay_client_arc(&ws_id).await {
                                    Ok(Some(relay_client)) => {
                                        let mut client = relay_client.lock().await;
                                        if let Err(e) = client
                                            .send_team_message(from, content, message_type)
                                            .await
                                        {
                                            warn!(
                                                error = %e,
                                                team = %name,
                                                recipient = %recipient,
                                                "relay push to guest failed (message stored in host inbox)"
                                            );
                                        }
                                    }
                                    Ok(None) => {
                                        // No relay connection — workspace not part of a team or
                                        // relay not connected yet. Message is still in host inbox.
                                    }
                                    Err(e) => {
                                        warn!(
                                            error = %e,
                                            team = %name,
                                            recipient = %recipient,
                                            "failed to get relay client for push"
                                        );
                                    }
                                }
                            }
                        }

                        let result = serde_json::json!({
                            "action": "message",
                            "message_id": message_id,
                            "status": "delivered",
                            "from": from,
                            "to": to,
                        });
                        Ok(CallToolResult::success(vec![Content::text(
                            serde_json::to_string_pretty(&result).unwrap(),
                        )]))
                    }
                    Err(e) => Err(McpError::invalid_request(e, None)),
                }
            }

            "receive" => {
                let name = params.name.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'name' is required for action 'receive' (team name)".to_string(),
                        None,
                    )
                })?;
                let agent = params.agent.as_deref().ok_or_else(|| {
                    McpError::invalid_params(
                        "'agent' is required for action 'receive' (agent name to receive messages for)"
                            .to_string(),
                        None,
                    )
                })?;
                let limit = params.limit.unwrap_or(10) as usize;

                info!(
                    tool = "team",
                    action = "receive",
                    team = %name,
                    agent = %agent,
                    limit,
                    "tool call"
                );

                match self.message_relay.receive(name, agent, limit).await {
                    Ok(messages) => {
                        // Filter out task_assignment messages — those are handled
                        // exclusively by the guest daemon, not by message polling.
                        let messages: Vec<_> = messages
                            .into_iter()
                            .filter(|m| m.message_type != "task_assignment")
                            .collect();
                        let count = messages.len();
                        let mut result = serde_json::json!({
                            "action": "receive",
                            "agent": agent,
                            "messages": messages,
                            "count": count,
                        });

                        // Also poll guest daemon for completed task results
                        if let Some(ws_id) =
                            self.message_relay.workspace_id(name, agent).await
                        {
                            match self.workspace_manager.vsock_client_arc(&ws_id).await {
                                Ok(vsock_arc) => {
                                    let mut client = vsock_arc.lock().await;
                                    match client.poll_daemon_results(limit as u32).await {
                                        Ok(daemon_resp) => {
                                            if !daemon_resp.results.is_empty()
                                                || daemon_resp.pending_tasks > 0
                                            {
                                                let daemon_results: Vec<serde_json::Value> =
                                                    daemon_resp
                                                        .results
                                                        .iter()
                                                        .map(|r| {
                                                            serde_json::json!({
                                                                "task_id": r.task_id,
                                                                "success": r.success,
                                                                "exit_code": r.exit_code,
                                                                "stdout": safe_truncate(&r.stdout, 4096),
                                                                "stderr": safe_truncate(&r.stderr, 2048),
                                                                "elapsed_secs": r.elapsed_secs,
                                                                "source_message_id": r.source_message_id,
                                                            })
                                                        })
                                                        .collect();

                                                result["daemon_results"] =
                                                    serde_json::json!(daemon_results);
                                                result["daemon_pending_tasks"] =
                                                    serde_json::json!(daemon_resp.pending_tasks);
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                error = %e,
                                                team = %name,
                                                agent = %agent,
                                                "failed to poll daemon results"
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        error = %e,
                                        team = %name,
                                        agent = %agent,
                                        "failed to get vsock client for daemon poll"
                                    );
                                }
                            }
                        }

                        Ok(CallToolResult::success(vec![Content::text(
                            serde_json::to_string_pretty(&result).unwrap(),
                        )]))
                    }
                    Err(e) => Err(McpError::invalid_request(e, None)),
                }
            }

            other => Err(McpError::invalid_params(
                format!(
                    "unknown team action '{}'. Valid actions: create, destroy, status, list, message, receive",
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

    #[test]
    fn test_team_params_message() {
        let json = serde_json::json!({
            "action": "message",
            "agent": "alice",
            "to": "bob",
            "content": "hello world",
            "message_type": "task_update"
        });
        let params: TeamParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "message");
        assert_eq!(params.agent.as_deref(), Some("alice"));
        assert_eq!(params.to.as_deref(), Some("bob"));
        assert_eq!(params.content.as_deref(), Some("hello world"));
        assert_eq!(params.message_type.as_deref(), Some("task_update"));
    }

    #[test]
    fn test_team_params_message_broadcast() {
        let json = serde_json::json!({
            "action": "message",
            "agent": "lead",
            "to": "*",
            "content": "start sprint"
        });
        let params: TeamParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "message");
        assert_eq!(params.to.as_deref(), Some("*"));
        // message_type defaults to None (handler uses "text")
        assert!(params.message_type.is_none());
    }

    #[test]
    fn test_team_params_receive() {
        let json = serde_json::json!({
            "action": "receive",
            "agent": "bob",
            "limit": 5
        });
        let params: TeamParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "receive");
        assert_eq!(params.agent.as_deref(), Some("bob"));
        assert_eq!(params.limit, Some(5));
    }

    #[test]
    fn test_team_params_receive_defaults() {
        let json = serde_json::json!({
            "action": "receive",
            "agent": "bob"
        });
        let params: TeamParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.action, "receive");
        assert_eq!(params.agent.as_deref(), Some("bob"));
        // limit defaults to None (handler uses 10)
        assert!(params.limit.is_none());
    }

    #[test]
    fn test_safe_truncate_short_string() {
        assert_eq!(safe_truncate("hello", 10), "hello");
    }

    #[test]
    fn test_safe_truncate_at_boundary() {
        let s = "a".repeat(100);
        let result = safe_truncate(&s, 50);
        assert!(result.starts_with(&"a".repeat(50)));
        assert!(result.ends_with("... (truncated)"));
    }

    #[test]
    fn test_safe_truncate_multibyte() {
        // 3-byte UTF-8 chars: each is 3 bytes
        let s = "\u{2603}\u{2603}\u{2603}\u{2603}"; // 4 snowmen, 12 bytes total
        // Truncate at byte 4 — falls in middle of 2nd char (bytes 3..6)
        let result = safe_truncate(s, 4);
        // Should back up to byte 3 (end of first snowman)
        assert!(result.starts_with("\u{2603}"));
        assert!(result.ends_with("... (truncated)"));
    }
}
