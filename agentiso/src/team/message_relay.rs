//! Host-side message relay for inter-agent team communication.
//!
//! Maintains a per-agent bounded inbox. Messages are routed by agent name
//! within a team. Broadcast ("*") fans out to all agents except the sender.
//!
//! Agent keys are scoped by team to prevent name collisions across teams
//! (e.g., two teams both having a "coder" agent).

use std::collections::{HashMap, VecDeque};

use tokio::sync::RwLock;
use uuid::Uuid;

use agentiso_protocol::TeamMessageEnvelope;

/// Maximum messages per agent inbox before oldest are dropped.
const DEFAULT_INBOX_CAPACITY: usize = 100;

/// Maximum message content size (256 KiB) to prevent unbounded memory usage.
const MAX_CONTENT_SIZE: usize = 256 * 1024;

/// A registered agent in the relay.
struct AgentMailbox {
    /// Team this agent belongs to.
    team: String,
    /// Agent name (without team prefix).
    agent_name: String,
    /// Workspace ID for this agent (used by future push delivery).
    #[allow(dead_code)]
    workspace_id: Uuid,
    /// Bounded inbox — oldest messages dropped when full.
    inbox: VecDeque<TeamMessageEnvelope>,
    /// Maximum inbox size.
    capacity: usize,
}

/// Composite key for agent lookup: "team:agent_name".
fn agent_key(team: &str, agent_name: &str) -> String {
    format!("{}:{}", team, agent_name)
}

/// Routes messages between agents within teams.
pub struct MessageRelay {
    /// "team:agent_name" -> mailbox. Protected by RwLock for concurrent reads.
    agents: RwLock<HashMap<String, AgentMailbox>>,
}

impl MessageRelay {
    pub fn new() -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
        }
    }

    /// Register an agent with the relay.
    pub async fn register(&self, agent_name: &str, team: &str, workspace_id: Uuid) {
        let mut agents = self.agents.write().await;
        agents.insert(
            agent_key(team, agent_name),
            AgentMailbox {
                team: team.to_string(),
                agent_name: agent_name.to_string(),
                workspace_id,
                inbox: VecDeque::new(),
                capacity: DEFAULT_INBOX_CAPACITY,
            },
        );
    }

    /// Unregister an agent (e.g., on team destroy).
    pub async fn unregister(&self, agent_name: &str, team: &str) {
        let mut agents = self.agents.write().await;
        agents.remove(&agent_key(team, agent_name));
    }

    /// Unregister all agents belonging to a team.
    pub async fn unregister_team(&self, team: &str) {
        let mut agents = self.agents.write().await;
        agents.retain(|_, mailbox| mailbox.team != team);
    }

    /// Send a message from one agent to another (or broadcast with to="*").
    ///
    /// Both sender and recipient must be in the same team. The `team` parameter
    /// scopes the lookup to prevent cross-team name collisions.
    ///
    /// Returns the message_id on success, or an error if sender/recipient
    /// is not registered or not in the same team.
    pub async fn send(
        &self,
        team: &str,
        from: &str,
        to: &str,
        content: &str,
        message_type: &str,
    ) -> Result<String, String> {
        // Validate content size
        if content.len() > MAX_CONTENT_SIZE {
            return Err(format!(
                "message content too large: {} bytes (max {} bytes)",
                content.len(),
                MAX_CONTENT_SIZE
            ));
        }

        let mut agents = self.agents.write().await;

        // Validate sender exists in this team
        let sender_key = agent_key(team, from);
        if !agents.contains_key(&sender_key) {
            return Err(format!(
                "sender '{}' not registered in relay for team '{}'",
                from, team
            ));
        }

        let message_id = Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();

        if to == "*" {
            // Broadcast to all team members except sender
            let recipient_keys: Vec<String> = agents
                .iter()
                .filter(|(_, mb)| mb.team == team && mb.agent_name != from)
                .map(|(key, _)| key.clone())
                .collect();

            for key in &recipient_keys {
                if let Some(mailbox) = agents.get_mut(key) {
                    let envelope = TeamMessageEnvelope {
                        message_id: message_id.clone(),
                        from: from.to_string(),
                        to: mailbox.agent_name.clone(),
                        content: content.to_string(),
                        message_type: message_type.to_string(),
                        timestamp: timestamp.clone(),
                    };
                    if mailbox.inbox.len() >= mailbox.capacity {
                        mailbox.inbox.pop_front(); // drop oldest — O(1) with VecDeque
                    }
                    mailbox.inbox.push_back(envelope);
                }
            }
        } else {
            // Direct message — recipient must be in the same team
            let recipient_key = agent_key(team, to);
            let recipient = agents.get_mut(&recipient_key).ok_or_else(|| {
                format!(
                    "recipient '{}' not registered in relay for team '{}'",
                    to, team
                )
            })?;

            let envelope = TeamMessageEnvelope {
                message_id: message_id.clone(),
                from: from.to_string(),
                to: to.to_string(),
                content: content.to_string(),
                message_type: message_type.to_string(),
                timestamp,
            };

            if recipient.inbox.len() >= recipient.capacity {
                recipient.inbox.pop_front(); // drop oldest — O(1) with VecDeque
            }
            recipient.inbox.push_back(envelope);
        }

        Ok(message_id)
    }

    /// Drain up to `limit` messages from an agent's inbox.
    pub async fn receive(
        &self,
        team: &str,
        agent_name: &str,
        limit: usize,
    ) -> Result<Vec<TeamMessageEnvelope>, String> {
        let mut agents = self.agents.write().await;
        let key = agent_key(team, agent_name);
        let mailbox = agents.get_mut(&key).ok_or_else(|| {
            format!(
                "agent '{}' not registered in relay for team '{}'",
                agent_name, team
            )
        })?;

        let count = limit.min(mailbox.inbox.len());
        let messages: Vec<TeamMessageEnvelope> = mailbox.inbox.drain(..count).collect();
        Ok(messages)
    }

    /// Get the workspace_id for a registered agent.
    #[allow(dead_code)] // Used by future push delivery
    pub async fn workspace_id(&self, team: &str, agent_name: &str) -> Option<Uuid> {
        let agents = self.agents.read().await;
        agents.get(&agent_key(team, agent_name)).map(|mb| mb.workspace_id)
    }

    /// List all agent names in a team.
    #[allow(dead_code)] // Used by diagnostics and future team status
    pub async fn team_agents(&self, team: &str) -> Vec<String> {
        let agents = self.agents.read().await;
        agents
            .iter()
            .filter(|(_, mb)| mb.team == team)
            .map(|(_, mb)| mb.agent_name.clone())
            .collect()
    }

    /// Get the inbox length for an agent (for diagnostics).
    #[allow(dead_code)] // Used by diagnostics
    pub async fn inbox_len(&self, team: &str, agent_name: &str) -> Option<usize> {
        let agents = self.agents.read().await;
        agents.get(&agent_key(team, agent_name)).map(|mb| mb.inbox.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_send_direct_message() {
        let relay = MessageRelay::new();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        relay.register("alice", "team1", id_a).await;
        relay.register("bob", "team1", id_b).await;

        let msg_id = relay.send("team1", "alice", "bob", "hello", "text").await.unwrap();
        assert!(!msg_id.is_empty());

        let msgs = relay.receive("team1", "bob", 10).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].from, "alice");
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[0].message_type, "text");
    }

    #[tokio::test]
    async fn broadcast_message() {
        let relay = MessageRelay::new();
        relay.register("lead", "team1", Uuid::new_v4()).await;
        relay.register("coder", "team1", Uuid::new_v4()).await;
        relay.register("tester", "team1", Uuid::new_v4()).await;

        relay.send("team1", "lead", "*", "start sprint", "text").await.unwrap();

        // lead should NOT receive their own broadcast
        let lead_msgs = relay.receive("team1", "lead", 10).await.unwrap();
        assert_eq!(lead_msgs.len(), 0);

        // coder and tester should each get 1 message
        let coder_msgs = relay.receive("team1", "coder", 10).await.unwrap();
        assert_eq!(coder_msgs.len(), 1);
        assert_eq!(coder_msgs[0].content, "start sprint");

        let tester_msgs = relay.receive("team1", "tester", 10).await.unwrap();
        assert_eq!(tester_msgs.len(), 1);
    }

    #[tokio::test]
    async fn same_name_different_teams_no_collision() {
        let relay = MessageRelay::new();
        relay.register("coder", "team1", Uuid::new_v4()).await;
        relay.register("coder", "team2", Uuid::new_v4()).await;
        relay.register("lead", "team1", Uuid::new_v4()).await;
        relay.register("lead", "team2", Uuid::new_v4()).await;

        // Send in team1 — should not affect team2
        relay.send("team1", "lead", "coder", "hello team1", "text").await.unwrap();

        let team1_msgs = relay.receive("team1", "coder", 10).await.unwrap();
        assert_eq!(team1_msgs.len(), 1);
        assert_eq!(team1_msgs[0].content, "hello team1");

        // team2 coder should have no messages
        let team2_msgs = relay.receive("team2", "coder", 10).await.unwrap();
        assert_eq!(team2_msgs.len(), 0);
    }

    #[tokio::test]
    async fn cross_team_send_rejected() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team2", Uuid::new_v4()).await;

        // alice (team1) tries to send to bob — but bob is in team2, not team1
        let result = relay.send("team1", "alice", "bob", "hi", "text").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not registered"));
    }

    #[tokio::test]
    async fn unknown_sender_rejected() {
        let relay = MessageRelay::new();
        relay.register("bob", "team1", Uuid::new_v4()).await;

        let result = relay.send("team1", "ghost", "bob", "hi", "text").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("sender"));
    }

    #[tokio::test]
    async fn unknown_recipient_rejected() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;

        let result = relay.send("team1", "alice", "ghost", "hi", "text").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("recipient"));
    }

    #[tokio::test]
    async fn content_size_limit_enforced() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;

        let huge = "x".repeat(MAX_CONTENT_SIZE + 1);
        let result = relay.send("team1", "alice", "bob", &huge, "text").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));

        // Exactly at limit should work
        let at_limit = "x".repeat(MAX_CONTENT_SIZE);
        let result = relay.send("team1", "alice", "bob", &at_limit, "text").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn inbox_capacity_drops_oldest() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;

        // Send more than DEFAULT_INBOX_CAPACITY messages
        for i in 0..(DEFAULT_INBOX_CAPACITY + 5) {
            relay.send("team1", "alice", "bob", &format!("msg-{}", i), "text").await.unwrap();
        }

        let msgs = relay.receive("team1", "bob", DEFAULT_INBOX_CAPACITY + 10).await.unwrap();
        assert_eq!(msgs.len(), DEFAULT_INBOX_CAPACITY);
        // Oldest should have been dropped, so first message should be msg-5
        assert_eq!(msgs[0].content, "msg-5");
    }

    #[tokio::test]
    async fn receive_drains_inbox() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;

        relay.send("team1", "alice", "bob", "msg1", "text").await.unwrap();
        relay.send("team1", "alice", "bob", "msg2", "text").await.unwrap();
        relay.send("team1", "alice", "bob", "msg3", "text").await.unwrap();

        // Receive 2 of 3
        let batch1 = relay.receive("team1", "bob", 2).await.unwrap();
        assert_eq!(batch1.len(), 2);
        assert_eq!(batch1[0].content, "msg1");
        assert_eq!(batch1[1].content, "msg2");

        // Remaining 1
        let batch2 = relay.receive("team1", "bob", 10).await.unwrap();
        assert_eq!(batch2.len(), 1);
        assert_eq!(batch2[0].content, "msg3");

        // Empty now
        let batch3 = relay.receive("team1", "bob", 10).await.unwrap();
        assert_eq!(batch3.len(), 0);
    }

    #[tokio::test]
    async fn unregister_team_removes_all_members() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;
        relay.register("carol", "team2", Uuid::new_v4()).await;

        relay.unregister_team("team1").await;

        assert!(relay.workspace_id("team1", "alice").await.is_none());
        assert!(relay.workspace_id("team1", "bob").await.is_none());
        // carol is in team2, should still exist
        assert!(relay.workspace_id("team2", "carol").await.is_some());
    }

    #[tokio::test]
    async fn team_agents_returns_correct_list() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;
        relay.register("carol", "team2", Uuid::new_v4()).await;

        let mut agents = relay.team_agents("team1").await;
        agents.sort();
        assert_eq!(agents, vec!["alice", "bob"]);
    }

    #[tokio::test]
    async fn inbox_len_tracking() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;

        assert_eq!(relay.inbox_len("team1", "bob").await, Some(0));
        relay.send("team1", "alice", "bob", "hi", "text").await.unwrap();
        assert_eq!(relay.inbox_len("team1", "bob").await, Some(1));
    }
}
