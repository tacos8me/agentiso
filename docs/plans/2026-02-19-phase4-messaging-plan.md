# Phase 4: Inter-Agent Messaging Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable agents running in team VM workspaces to send messages to each other via a host-side relay over vsock, with an MCP tool for external orchestration.

**Architecture:** Host-side `MessageRelay` routes messages between VMs using a dedicated vsock relay connection (port 5001) per VM, separate from the existing request/response channel (port 5000). This solves C1 (exec holds vsock mutex for 125s). Guest agents listen on a second vsock port for pushed messages and expose a minimal HTTP API (port 8080) for direct peer communication.

**Tech Stack:** Rust, tokio, serde, vsock, axum (guest HTTP), nftables

---

## Dependency Graph

```
Task 1 (protocol types) ─┬─→ Task 2 (MessageRelay)  ─┬─→ Task 5 (wire relay into team) ─→ Task 7 (MCP tool)
                          │                            │
                          ├─→ Task 3 (dual vsock)    ─┘
                          │
                          └─→ Task 4 (guest relay)   ─→ Task 6 (guest HTTP API)
                                                                                           ─→ Task 8 (e2e tests)
```

**Agent assignments:**
- `guest-agent`: Tasks 1, 4, 6
- `vm-engine`: Task 3
- `workspace-core`: Tasks 2, 5
- `mcp-server`: Tasks 7, 8

---

### Task 1: Protocol Types for Team Messaging

**Owner:** `guest-agent`
**Files:**
- Modify: `protocol/src/lib.rs`

**Step 1: Add the relay port constant and message types**

Add after `pub const GUEST_AGENT_PORT: u32 = 5000;` (line 5):

```rust
/// Vsock port for the message relay channel (separate from request/response).
pub const RELAY_PORT: u32 = 5001;
```

Add new request/response variants. In the `GuestRequest` enum (after `TaskClaim`):

```rust
    /// Send a message to another agent in the same team.
    TeamMessage(TeamMessageRequest),

    /// Receive pending messages (guest polls host for messages).
    TeamReceive(TeamReceiveRequest),
```

In the `GuestResponse` enum (after `TaskClaimed`):

```rust
    /// Message was accepted by the relay for delivery.
    TeamMessageDelivered(TeamMessageDeliveredResponse),

    /// Pending messages retrieved from the relay inbox.
    TeamMessages(TeamMessagesResponse),

    /// A message pushed from the host relay to the guest.
    TeamMessagePush(TeamMessagePushResponse),
```

Add the struct definitions (before the `GuestResponse` enum, near the other request structs):

```rust
// --- Team messaging (Phase 4) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessageRequest {
    /// Recipient agent name, or "*" for broadcast to all team members.
    pub to: String,
    /// Message content (freeform text or JSON string).
    pub content: String,
    /// Message type hint: "text", "task_update", "request", "response".
    #[serde(default = "default_message_type")]
    pub message_type: String,
}

fn default_message_type() -> String {
    "text".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamReceiveRequest {
    /// Maximum messages to retrieve (default 10).
    #[serde(default = "default_receive_limit")]
    pub limit: u32,
}

fn default_receive_limit() -> u32 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessageDeliveredResponse {
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessageEnvelope {
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub content: String,
    pub message_type: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessagesResponse {
    pub messages: Vec<TeamMessageEnvelope>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessagePushResponse {
    pub message: TeamMessageEnvelope,
}
```

**Step 2: Write tests for the new types**

Add to the `#[cfg(test)] mod tests` section:

```rust
    // -----------------------------------------------------------------------
    // Team messaging round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn test_team_message_request_roundtrip() {
        let req = GuestRequest::TeamMessage(TeamMessageRequest {
            to: "coder".to_string(),
            content: "please review PR #5".to_string(),
            message_type: "request".to_string(),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::TeamMessage(tm) = rt {
            assert_eq!(tm.to, "coder");
            assert_eq!(tm.content, "please review PR #5");
            assert_eq!(tm.message_type, "request");
        } else {
            panic!("expected TeamMessage variant");
        }
    }

    #[test]
    fn test_team_message_default_type() {
        let json = r#"{"type":"TeamMessage","to":"lead","content":"done"}"#;
        let req: GuestRequest = serde_json::from_str(json).unwrap();
        if let GuestRequest::TeamMessage(tm) = req {
            assert_eq!(tm.message_type, "text");
        } else {
            panic!("expected TeamMessage variant");
        }
    }

    #[test]
    fn test_team_receive_request_roundtrip() {
        let req = GuestRequest::TeamReceive(TeamReceiveRequest { limit: 5 });
        let rt = roundtrip_request(&req);
        if let GuestRequest::TeamReceive(tr) = rt {
            assert_eq!(tr.limit, 5);
        } else {
            panic!("expected TeamReceive variant");
        }
    }

    #[test]
    fn test_team_receive_default_limit() {
        let json = r#"{"type":"TeamReceive"}"#;
        let req: GuestRequest = serde_json::from_str(json).unwrap();
        if let GuestRequest::TeamReceive(tr) = req {
            assert_eq!(tr.limit, 10);
        } else {
            panic!("expected TeamReceive variant");
        }
    }

    #[test]
    fn test_team_message_delivered_roundtrip() {
        let resp = GuestResponse::TeamMessageDelivered(TeamMessageDeliveredResponse {
            message_id: "msg-001".to_string(),
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::TeamMessageDelivered(d) = rt {
            assert_eq!(d.message_id, "msg-001");
        } else {
            panic!("expected TeamMessageDelivered variant");
        }
    }

    #[test]
    fn test_team_messages_response_roundtrip() {
        let resp = GuestResponse::TeamMessages(TeamMessagesResponse {
            messages: vec![TeamMessageEnvelope {
                message_id: "msg-001".to_string(),
                from: "lead".to_string(),
                to: "coder".to_string(),
                content: "start task 3".to_string(),
                message_type: "text".to_string(),
                timestamp: "2026-02-19T21:00:00Z".to_string(),
            }],
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::TeamMessages(ms) = rt {
            assert_eq!(ms.messages.len(), 1);
            assert_eq!(ms.messages[0].from, "lead");
        } else {
            panic!("expected TeamMessages variant");
        }
    }

    #[test]
    fn test_team_message_push_roundtrip() {
        let resp = GuestResponse::TeamMessagePush(TeamMessagePushResponse {
            message: TeamMessageEnvelope {
                message_id: "msg-002".to_string(),
                from: "reviewer".to_string(),
                to: "coder".to_string(),
                content: "LGTM".to_string(),
                message_type: "response".to_string(),
                timestamp: "2026-02-19T21:05:00Z".to_string(),
            },
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::TeamMessagePush(p) = rt {
            assert_eq!(p.message.from, "reviewer");
            assert_eq!(p.message.content, "LGTM");
        } else {
            panic!("expected TeamMessagePush variant");
        }
    }

    #[test]
    fn test_team_message_envelope_serde() {
        let env = TeamMessageEnvelope {
            message_id: "msg-abc".to_string(),
            from: "a".to_string(),
            to: "*".to_string(),
            content: "broadcast".to_string(),
            message_type: "text".to_string(),
            timestamp: "2026-02-19T22:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&env).unwrap();
        let rt: TeamMessageEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(rt.to, "*");
        assert_eq!(rt.message_id, "msg-abc");
    }
```

**Step 3: Run tests**

Run: `cargo test -p agentiso-protocol`
Expected: All existing tests pass + 8 new tests pass.

**Step 4: Commit**

```bash
git add protocol/src/lib.rs
git commit -m "feat(protocol): add TeamMessage/TeamReceive types for Phase 4 messaging"
```

---

### Task 2: MessageRelay Host Module

**Owner:** `workspace-core`
**Files:**
- Create: `agentiso/src/team/message_relay.rs`
- Modify: `agentiso/src/team/mod.rs` (add `pub mod message_relay;`)

**Step 1: Write the MessageRelay tests first**

Create `agentiso/src/team/message_relay.rs`:

```rust
//! Host-side message relay for inter-agent team communication.
//!
//! Maintains a per-agent bounded inbox. Messages are routed by agent name
//! within a team. Broadcast ("*") fans out to all agents except the sender.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use agentiso_protocol::TeamMessageEnvelope;

/// Maximum messages per agent inbox before oldest are dropped.
const DEFAULT_INBOX_CAPACITY: usize = 100;

/// A registered agent in the relay.
struct AgentMailbox {
    /// Team this agent belongs to.
    team: String,
    /// Workspace ID for this agent.
    workspace_id: Uuid,
    /// Bounded inbox — oldest messages dropped when full.
    inbox: Vec<TeamMessageEnvelope>,
    /// Maximum inbox size.
    capacity: usize,
}

/// Routes messages between agents within teams.
pub struct MessageRelay {
    /// Agent name -> mailbox. Protected by RwLock for concurrent reads.
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
            agent_name.to_string(),
            AgentMailbox {
                team: team.to_string(),
                workspace_id,
                inbox: Vec::new(),
                capacity: DEFAULT_INBOX_CAPACITY,
            },
        );
    }

    /// Unregister an agent (e.g., on team destroy).
    pub async fn unregister(&self, agent_name: &str) {
        let mut agents = self.agents.write().await;
        agents.remove(agent_name);
    }

    /// Unregister all agents belonging to a team.
    pub async fn unregister_team(&self, team: &str) {
        let mut agents = self.agents.write().await;
        agents.retain(|_, mailbox| mailbox.team != team);
    }

    /// Send a message from one agent to another (or broadcast with to="*").
    ///
    /// Returns the message_id on success, or an error if sender/recipient
    /// is not registered or not in the same team.
    pub async fn send(
        &self,
        from: &str,
        to: &str,
        content: &str,
        message_type: &str,
    ) -> Result<String, String> {
        let mut agents = self.agents.write().await;

        // Validate sender exists
        let sender_team = match agents.get(from) {
            Some(mailbox) => mailbox.team.clone(),
            None => return Err(format!("sender '{}' not registered in relay", from)),
        };

        let message_id = Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();

        if to == "*" {
            // Broadcast to all team members except sender
            let recipients: Vec<String> = agents
                .iter()
                .filter(|(name, mb)| mb.team == sender_team && *name != from)
                .map(|(name, _)| name.clone())
                .collect();

            for recipient_name in &recipients {
                if let Some(mailbox) = agents.get_mut(recipient_name) {
                    let envelope = TeamMessageEnvelope {
                        message_id: message_id.clone(),
                        from: from.to_string(),
                        to: recipient_name.clone(),
                        content: content.to_string(),
                        message_type: message_type.to_string(),
                        timestamp: timestamp.clone(),
                    };
                    if mailbox.inbox.len() >= mailbox.capacity {
                        mailbox.inbox.remove(0); // drop oldest
                    }
                    mailbox.inbox.push(envelope);
                }
            }
        } else {
            // Direct message
            let recipient = agents.get_mut(to).ok_or_else(|| {
                format!("recipient '{}' not registered in relay", to)
            })?;

            if recipient.team != sender_team {
                return Err(format!(
                    "cannot send cross-team message (sender team '{}', recipient team '{}')",
                    sender_team, recipient.team
                ));
            }

            let envelope = TeamMessageEnvelope {
                message_id: message_id.clone(),
                from: from.to_string(),
                to: to.to_string(),
                content: content.to_string(),
                message_type: message_type.to_string(),
                timestamp,
            };

            if recipient.inbox.len() >= recipient.capacity {
                recipient.inbox.remove(0); // drop oldest
            }
            recipient.inbox.push(envelope);
        }

        Ok(message_id)
    }

    /// Drain up to `limit` messages from an agent's inbox.
    pub async fn receive(&self, agent_name: &str, limit: usize) -> Result<Vec<TeamMessageEnvelope>, String> {
        let mut agents = self.agents.write().await;
        let mailbox = agents.get_mut(agent_name).ok_or_else(|| {
            format!("agent '{}' not registered in relay", agent_name)
        })?;

        let count = limit.min(mailbox.inbox.len());
        let messages: Vec<TeamMessageEnvelope> = mailbox.inbox.drain(..count).collect();
        Ok(messages)
    }

    /// Get the workspace_id for a registered agent.
    pub async fn workspace_id(&self, agent_name: &str) -> Option<Uuid> {
        let agents = self.agents.read().await;
        agents.get(agent_name).map(|mb| mb.workspace_id)
    }

    /// List all agents in a team.
    pub async fn team_agents(&self, team: &str) -> Vec<String> {
        let agents = self.agents.read().await;
        agents
            .iter()
            .filter(|(_, mb)| mb.team == team)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Get the inbox length for an agent (for diagnostics).
    pub async fn inbox_len(&self, agent_name: &str) -> Option<usize> {
        let agents = self.agents.read().await;
        agents.get(agent_name).map(|mb| mb.inbox.len())
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

        let msg_id = relay.send("alice", "bob", "hello", "text").await.unwrap();
        assert!(!msg_id.is_empty());

        let msgs = relay.receive("bob", 10).await.unwrap();
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

        relay.send("lead", "*", "start sprint", "text").await.unwrap();

        // lead should NOT receive their own broadcast
        let lead_msgs = relay.receive("lead", 10).await.unwrap();
        assert_eq!(lead_msgs.len(), 0);

        // coder and tester should each get 1 message
        let coder_msgs = relay.receive("coder", 10).await.unwrap();
        assert_eq!(coder_msgs.len(), 1);
        assert_eq!(coder_msgs[0].content, "start sprint");

        let tester_msgs = relay.receive("tester", 10).await.unwrap();
        assert_eq!(tester_msgs.len(), 1);
    }

    #[tokio::test]
    async fn cross_team_message_rejected() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team2", Uuid::new_v4()).await;

        let result = relay.send("alice", "bob", "hi", "text").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cross-team"));
    }

    #[tokio::test]
    async fn unknown_sender_rejected() {
        let relay = MessageRelay::new();
        relay.register("bob", "team1", Uuid::new_v4()).await;

        let result = relay.send("ghost", "bob", "hi", "text").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("sender"));
    }

    #[tokio::test]
    async fn unknown_recipient_rejected() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;

        let result = relay.send("alice", "ghost", "hi", "text").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("recipient"));
    }

    #[tokio::test]
    async fn inbox_capacity_drops_oldest() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;

        // Send more than DEFAULT_INBOX_CAPACITY messages
        for i in 0..(DEFAULT_INBOX_CAPACITY + 5) {
            relay.send("alice", "bob", &format!("msg-{}", i), "text").await.unwrap();
        }

        let msgs = relay.receive("bob", DEFAULT_INBOX_CAPACITY + 10).await.unwrap();
        assert_eq!(msgs.len(), DEFAULT_INBOX_CAPACITY);
        // Oldest should have been dropped, so first message should be msg-5
        assert_eq!(msgs[0].content, "msg-5");
    }

    #[tokio::test]
    async fn receive_drains_inbox() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;

        relay.send("alice", "bob", "msg1", "text").await.unwrap();
        relay.send("alice", "bob", "msg2", "text").await.unwrap();
        relay.send("alice", "bob", "msg3", "text").await.unwrap();

        // Receive 2 of 3
        let batch1 = relay.receive("bob", 2).await.unwrap();
        assert_eq!(batch1.len(), 2);
        assert_eq!(batch1[0].content, "msg1");
        assert_eq!(batch1[1].content, "msg2");

        // Remaining 1
        let batch2 = relay.receive("bob", 10).await.unwrap();
        assert_eq!(batch2.len(), 1);
        assert_eq!(batch2[0].content, "msg3");

        // Empty now
        let batch3 = relay.receive("bob", 10).await.unwrap();
        assert_eq!(batch3.len(), 0);
    }

    #[tokio::test]
    async fn unregister_team_removes_all_members() {
        let relay = MessageRelay::new();
        relay.register("alice", "team1", Uuid::new_v4()).await;
        relay.register("bob", "team1", Uuid::new_v4()).await;
        relay.register("carol", "team2", Uuid::new_v4()).await;

        relay.unregister_team("team1").await;

        assert!(relay.workspace_id("alice").await.is_none());
        assert!(relay.workspace_id("bob").await.is_none());
        // carol is in team2, should still exist
        assert!(relay.workspace_id("carol").await.is_some());
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

        assert_eq!(relay.inbox_len("bob").await, Some(0));
        relay.send("alice", "bob", "hi", "text").await.unwrap();
        assert_eq!(relay.inbox_len("bob").await, Some(1));
    }
}
```

**Step 2: Add module declaration**

In `agentiso/src/team/mod.rs`, add after `pub mod task_board;`:

```rust
pub mod message_relay;
```

And add the re-export:
```rust
pub use message_relay::MessageRelay;
```

**Step 3: Run tests**

Run: `cargo test -p agentiso -- team::message_relay`
Expected: 10 tests pass.

**Step 4: Commit**

```bash
git add agentiso/src/team/message_relay.rs agentiso/src/team/mod.rs
git commit -m "feat(team): add MessageRelay with per-agent bounded inbox"
```

---

### Task 3: Dual Vsock Connection in VmHandle

**Owner:** `vm-engine`
**Files:**
- Modify: `agentiso/src/vm/mod.rs:27-45` (VmHandle struct)
- Modify: `agentiso/src/vm/mod.rs:116-268` (launch method)
- Modify: `agentiso/src/vm/mod.rs:550-565` (accessor methods)

**Step 1: Add relay field to VmHandle**

In `agentiso/src/vm/mod.rs`, add to the `VmHandle` struct (after the `vsock` field, line 40):

```rust
    /// Dedicated vsock client for the message relay channel.
    ///
    /// Connects to the guest on RELAY_PORT (5001). Separate from the main
    /// `vsock` connection so that message delivery is never blocked by
    /// long-running exec operations that hold the main vsock mutex.
    /// `None` if relay connection was not established (non-team workspace).
    pub vsock_relay: Option<Arc<Mutex<VsockClient>>>,
```

**Step 2: Update launch() to optionally connect relay**

The relay connection should NOT be established during launch (most workspaces aren't team members). Instead, add a new method `connect_relay()` that can be called after a workspace is assigned to a team:

```rust
    /// Establish a dedicated relay vsock connection for team messaging.
    ///
    /// Call this after the workspace is assigned to a team. The relay
    /// connection uses RELAY_PORT (5001) and is stored separately from
    /// the main vsock connection.
    pub async fn connect_relay(&mut self, workspace_id: &Uuid) -> Result<()> {
        let handle = self.get_mut(workspace_id)?;
        if handle.vsock_relay.is_some() {
            return Ok(()); // already connected
        }

        let cid = handle.config.vsock_cid;
        let relay_port = agentiso_protocol::RELAY_PORT;

        debug!(
            workspace = %workspace_id,
            cid,
            port = relay_port,
            "connecting relay vsock"
        );

        let vsock = VsockClient::connect_and_wait(
            cid,
            relay_port,
            std::time::Duration::from_secs(10),
        )
        .await
        .context("failed to connect relay vsock")?;

        handle.vsock_relay = Some(Arc::new(Mutex::new(vsock)));
        info!(workspace = %workspace_id, "relay vsock connected");
        Ok(())
    }
```

**Step 3: Add relay accessor method**

Add after `vsock_client_arc()` (around line 553):

```rust
    /// Get an `Arc<Mutex<VsockClient>>` for a workspace's relay channel.
    ///
    /// Returns `None` if the relay channel is not connected (workspace is
    /// not part of a team).
    pub fn relay_client_arc(&self, workspace_id: &Uuid) -> Result<Option<Arc<Mutex<VsockClient>>>> {
        let handle = self.get(workspace_id)?;
        Ok(handle.vsock_relay.as_ref().map(Arc::clone))
    }
```

**Step 4: Update VmHandle construction in launch()**

In the `self.vms.insert()` call (around line 255), add the relay field:

```rust
            VmHandle {
                workspace_id,
                process: child,
                qmp,
                vsock: Arc::new(Mutex::new(vsock)),
                vsock_relay: None,  // Connected later via connect_relay() for team workspaces
                config: vm_config,
                pid,
            },
```

**Step 5: Write tests**

Add to `mod tests`:

```rust
    #[test]
    fn test_relay_port_constant() {
        assert_eq!(agentiso_protocol::RELAY_PORT, 5001);
        assert_ne!(agentiso_protocol::RELAY_PORT, agentiso_protocol::GUEST_AGENT_PORT);
    }
```

**Step 6: Run tests**

Run: `cargo test -p agentiso -- vm::tests`
Expected: All existing VM tests pass + 1 new test.

**Step 7: Commit**

```bash
git add agentiso/src/vm/mod.rs
git commit -m "feat(vm): add dual vsock — relay channel on port 5001 for team messaging"
```

---

### Task 4: Guest-Side Relay Listener

**Owner:** `guest-agent`
**Files:**
- Modify: `guest-agent/src/main.rs`

**Step 1: Add relay message handling to handle_request()**

In the `handle_request()` function (line 971), add match arms for the new message types:

```rust
        GuestRequest::TeamMessage(_) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: "TeamMessage must be sent to the host relay, not handled by guest".to_string(),
        }),
        GuestRequest::TeamReceive(_) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: "TeamReceive must be sent to the host relay, not handled by guest".to_string(),
        }),
```

These are host-side operations (like VaultRead/TaskClaim) — the guest rejects them if received directly.

**Step 2: Add a relay listener on port 5001**

Add a second listener in `main()`. The relay listener accepts connections from the host and receives `TeamMessagePush` responses (the host pushes messages to the guest as needed). Add a local message inbox:

```rust
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

/// In-memory inbox for messages pushed from the host relay.
static MESSAGE_INBOX: std::sync::OnceLock<Arc<TokioMutex<Vec<agentiso_protocol::TeamMessageEnvelope>>>> =
    std::sync::OnceLock::new();

fn message_inbox() -> &'static Arc<TokioMutex<Vec<agentiso_protocol::TeamMessageEnvelope>>> {
    MESSAGE_INBOX.get_or_init(|| Arc::new(TokioMutex::new(Vec::new())))
}
```

In main(), after starting the primary listener, spawn the relay listener:

```rust
    // Start relay listener on port 5001 (best-effort; non-fatal if bind fails)
    tokio::spawn(async {
        match listen(agentiso_protocol::RELAY_PORT).await {
            Ok(listener) => {
                info!(port = agentiso_protocol::RELAY_PORT, "relay listener started");
                match listener {
                    Listener::Vsock(vsock) => {
                        loop {
                            match vsock.accept().await {
                                Ok((stream, peer_cid)) => {
                                    let peer = format!("relay:vsock:cid={peer_cid}");
                                    let (reader, writer) = tokio::io::split(stream);
                                    tokio::spawn(async move {
                                        handle_relay_connection(reader, writer, peer).await;
                                    });
                                }
                                Err(e) => {
                                    warn!(error = %e, "relay accept failed");
                                }
                            }
                        }
                    }
                    Listener::Tcp(tcp) => {
                        loop {
                            match tcp.accept().await {
                                Ok((stream, addr)) => {
                                    let peer = format!("relay:tcp:{addr}");
                                    let (reader, writer) = stream.into_split();
                                    tokio::spawn(async move {
                                        handle_relay_connection(reader, writer, peer).await;
                                    });
                                }
                                Err(e) => {
                                    warn!(error = %e, "relay TCP accept failed");
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to start relay listener (team messaging disabled)");
            }
        }
    });
```

Add the relay connection handler:

```rust
/// Handle a relay connection from the host.
///
/// The host sends GuestRequest messages and expects GuestResponse back.
/// For TeamMessage requests received on this channel, the guest stores
/// them in the local message inbox (they were relayed from another agent).
async fn handle_relay_connection(
    mut reader: impl AsyncReadExt + Unpin,
    mut writer: impl AsyncWriteExt + Unpin,
    peer: String,
) {
    info!(peer = %peer, "new relay connection");

    loop {
        let req: GuestRequest = match read_message(&mut reader).await {
            Ok(r) => r,
            Err(e) => {
                let is_disconnect = e.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .map(|io_err| matches!(
                            io_err.kind(),
                            std::io::ErrorKind::UnexpectedEof
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::BrokenPipe
                        ))
                        .unwrap_or(false)
                });
                if is_disconnect {
                    info!(peer = %peer, "relay connection closed");
                } else {
                    warn!(peer = %peer, error = %e, "failed to read relay message");
                }
                return;
            }
        };

        // On the relay channel, we handle Ping (for keepalive) and
        // use a special convention: host sends TeamMessage as a "push"
        // to deliver a message from another agent. The guest stores it.
        let response = match req {
            GuestRequest::Ping => handle_ping().await,
            GuestRequest::TeamMessage(tm) => {
                // Host is pushing a message from another agent to us
                let envelope = agentiso_protocol::TeamMessageEnvelope {
                    message_id: uuid::Uuid::new_v4().to_string(),
                    from: tm.to.clone(), // the 'to' field is repurposed as 'from' in push context
                    to: "self".to_string(),
                    content: tm.content,
                    message_type: tm.message_type,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };
                let mut inbox = message_inbox().lock().await;
                // Cap at 1000 messages
                if inbox.len() >= 1000 {
                    inbox.remove(0);
                }
                inbox.push(envelope);
                GuestResponse::Ok
            }
            _ => GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: "relay channel only accepts Ping and TeamMessage".to_string(),
            }),
        };

        if let Err(e) = write_message(&mut writer, &response).await {
            error!(peer = %peer, error = %e, "failed to write relay response");
            return;
        }
    }
}
```

**Step 3: Add guest-agent deps for uuid and chrono**

In `guest-agent/Cargo.toml`, add:

```toml
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"], default-features = false }
```

**Step 4: Run tests**

Run: `cargo test -p agentiso-guest`
Expected: All existing guest tests pass + build succeeds.

**Step 5: Commit**

```bash
git add guest-agent/src/main.rs guest-agent/Cargo.toml
git commit -m "feat(guest): add relay listener on port 5001 for team message delivery"
```

---

### Task 5: Wire MessageRelay into Team Lifecycle

**Owner:** `workspace-core`
**Files:**
- Modify: `agentiso/src/team/mod.rs` (TeamManager gets Arc<MessageRelay>)
- Modify: `agentiso/src/workspace/mod.rs` (WorkspaceManager exposes relay)
- Modify: `agentiso/src/mcp/tools.rs` (AgentisoServer stores Arc<MessageRelay>)

**Step 1: Add MessageRelay to TeamManager**

In `agentiso/src/team/mod.rs`, update `TeamManager`:

```rust
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
    pub fn relay(&self) -> &Arc<MessageRelay> {
        &self.relay
    }
```

**Step 2: Register agents in relay on team create**

In `create_team()`, after the workspace is created and its `team_id` is set (around line 163), register with the relay:

```rust
                    // Register agent in message relay
                    self.relay.register(&role.name, name, ws_id).await;
```

And on rollback (in the `Err(e)` branch), unregister:

```rust
                    // Rollback: unregister from relay
                    for role in &roles[..member_ids.len()] {
                        self.relay.unregister(&role.name).await;
                    }
```

**Step 3: Unregister on team destroy**

In `destroy_team()`, before destroying workspaces, unregister from relay:

```rust
        // Unregister all team agents from message relay
        self.relay.unregister_team(name).await;
```

**Step 4: Connect relay vsock for each team member**

After all workspaces are created and registered, connect the relay vsock:

```rust
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
```

**Step 5: Update AgentisoServer to pass relay when creating TeamManager**

In `agentiso/src/mcp/tools.rs`, find where `TeamManager::new()` is called and add the relay parameter. The `AgentisoServer` struct should hold an `Arc<MessageRelay>`:

```rust
    message_relay: Arc<crate::team::MessageRelay>,
```

Initialize it in the server constructor:

```rust
    let message_relay = Arc::new(crate::team::MessageRelay::new());
```

Pass it when creating TeamManager:

```rust
    TeamManager::new(workspace_manager.clone(), config.clone(), message_relay.clone())
```

**Step 6: Run tests**

Run: `cargo test -p agentiso`
Expected: All tests pass (the TeamManager unit tests don't call create_team, so the relay parameter is just passed through).

**Step 7: Commit**

```bash
git add agentiso/src/team/mod.rs agentiso/src/workspace/mod.rs agentiso/src/mcp/tools.rs
git commit -m "feat(team): wire MessageRelay into team create/destroy lifecycle"
```

---

### Task 6: Guest HTTP API (Minimal)

**Owner:** `guest-agent`
**Files:**
- Modify: `guest-agent/Cargo.toml` (add axum deps)
- Modify: `guest-agent/src/main.rs` (add HTTP server)

**Step 1: Add axum dependencies**

In `guest-agent/Cargo.toml`:

```toml
axum = "0.8"
```

**Step 2: Add HTTP API routes**

In `guest-agent/src/main.rs`, add the HTTP server module:

```rust
use axum::{routing::get, routing::post, Json, Router};

/// Start the HTTP API server on port 8080 (best-effort, non-fatal).
async fn start_http_api() {
    let app = Router::new()
        .route("/health", get(http_health))
        .route("/messages", get(http_get_messages))
        .route("/messages", post(http_post_message));

    let addr = "0.0.0.0:8080";
    info!(addr, "starting HTTP API");
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            if let Err(e) = axum::serve(listener, app).await {
                error!(error = %e, "HTTP API server error");
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to bind HTTP API on {}", addr);
        }
    }
}

async fn http_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ready",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": uptime_secs(),
    }))
}

async fn http_get_messages() -> Json<serde_json::Value> {
    let mut inbox = message_inbox().lock().await;
    let messages: Vec<_> = inbox.drain(..).collect();
    Json(serde_json::json!({
        "messages": messages,
    }))
}

async fn http_post_message(
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    // Accept messages posted directly by peers
    let envelope = agentiso_protocol::TeamMessageEnvelope {
        message_id: uuid::Uuid::new_v4().to_string(),
        from: body.get("from").and_then(|v| v.as_str()).unwrap_or("unknown").to_string(),
        to: "self".to_string(),
        content: body.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        message_type: body.get("message_type").and_then(|v| v.as_str()).unwrap_or("text").to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    let mut inbox = message_inbox().lock().await;
    if inbox.len() >= 1000 {
        inbox.remove(0);
    }
    inbox.push(envelope);

    Json(serde_json::json!({ "status": "accepted" }))
}
```

**Step 3: Spawn HTTP server in main()**

In `main()`, before the primary listener loop, spawn the HTTP API:

```rust
    // Start HTTP API (best-effort, non-fatal)
    tokio::spawn(start_http_api());
```

**Step 4: Run tests and build**

Run: `cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest`
Expected: Builds successfully with axum.

Run: `cargo test -p agentiso-guest`
Expected: All existing tests pass.

**Step 5: Commit**

```bash
git add guest-agent/Cargo.toml guest-agent/src/main.rs
git commit -m "feat(guest): add minimal HTTP API on port 8080 (/health, /messages)"
```

---

### Task 7: MCP team_message Tool + Rate Limiting

**Owner:** `mcp-server`
**Files:**
- Modify: `agentiso/src/mcp/team_tools.rs` (add message/receive actions)
- Modify: `agentiso/src/mcp/rate_limit.rs` (add team_message category)
- Modify: `agentiso/src/config.rs` (add team_message_per_minute)

**Step 1: Add rate limit category**

In `agentiso/src/mcp/rate_limit.rs`, add:

```rust
pub const CATEGORY_TEAM_MESSAGE: &str = "team_message";
```

In `RateLimiter::new()`, add after the default bucket:

```rust
        // Team message: burst 50, refill at team_message_per_minute rate
        buckets.insert(
            CATEGORY_TEAM_MESSAGE.to_string(),
            TokenBucket::new(50, config.team_message_per_minute),
        );
```

In `agentiso/src/config.rs`, add to `RateLimitConfig`:

```rust
    /// Maximum team_message calls per minute.
    #[serde(default = "default_team_message_per_minute")]
    pub team_message_per_minute: u32,
```

```rust
fn default_team_message_per_minute() -> u32 {
    300
}
```

And update the `Default` impl for `RateLimitConfig` to include `team_message_per_minute: 300`.

**Step 2: Add message/receive actions to TeamParams**

In `agentiso/src/mcp/team_tools.rs`, update `TeamParams`:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct TeamParams {
    /// Action: "create", "destroy", "status", "list", "message", "receive"
    pub action: String,
    /// Team name (required for create, destroy, status, message, receive)
    #[serde(default)]
    pub name: Option<String>,
    // ... existing fields ...
    /// Message recipient (for message action: agent name or "*" for broadcast)
    #[serde(default)]
    pub to: Option<String>,
    /// Message content (for message action)
    #[serde(default)]
    pub content: Option<String>,
    /// Message type hint (for message action, default "text")
    #[serde(default)]
    pub message_type: Option<String>,
    /// Agent name to receive messages for (for receive action)
    #[serde(default)]
    pub agent: Option<String>,
    /// Max messages to receive (for receive action, default 10)
    #[serde(default)]
    pub limit: Option<u32>,
}
```

**Step 3: Implement message dispatch**

In the `handle_team()` method (in team_tools.rs), add handling for the new actions:

```rust
        "message" => {
            let name = params.name.as_deref().ok_or_else(|| /* error */)?;
            let to = params.to.as_deref().ok_or_else(|| /* error: 'to' required */)?;
            let content = params.content.as_deref().ok_or_else(|| /* error: 'content' required */)?;
            let message_type = params.message_type.as_deref().unwrap_or("text");

            // Rate limit check
            if let Err(e) = self.rate_limiter.check(rate_limit::CATEGORY_TEAM_MESSAGE) {
                return Err(McpError::invalid_request(e, None));
            }

            // We need to know which agent is sending. Use the `agent` param
            // or infer from workspace context.
            let from = params.agent.as_deref().ok_or_else(|| /* error: 'agent' required for sender */)?;

            match self.message_relay.send(from, to, content, message_type).await {
                Ok(message_id) => {
                    let result = serde_json::json!({
                        "message_id": message_id,
                        "status": "delivered",
                    });
                    Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string_pretty(&result).unwrap(),
                    )]))
                }
                Err(e) => Err(McpError::invalid_request(e, None)),
            }
        }

        "receive" => {
            let agent = params.agent.as_deref().ok_or_else(|| /* error */)?;
            let limit = params.limit.unwrap_or(10) as usize;

            match self.message_relay.receive(agent, limit).await {
                Ok(messages) => {
                    let result = serde_json::json!({
                        "messages": messages,
                        "count": messages.len(),
                    });
                    Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string_pretty(&result).unwrap(),
                    )]))
                }
                Err(e) => Err(McpError::invalid_request(e, None)),
            }
        }
```

**Step 4: Update the tool description**

Update the `#[tool(description = "...")]` for the team tool to include the message/receive actions.

**Step 5: Write tests**

Add tests for the new rate limit category:

```rust
    #[test]
    fn team_message_rate_limit() {
        let config = RateLimitConfig {
            enabled: true,
            team_message_per_minute: 300,
            ..Default::default()
        };
        let limiter = RateLimiter::new(&config);

        // Burst of 50
        for _ in 0..50 {
            assert!(limiter.check(CATEGORY_TEAM_MESSAGE).is_ok());
        }
        assert!(limiter.check(CATEGORY_TEAM_MESSAGE).is_err());
    }
```

**Step 6: Run tests**

Run: `cargo test -p agentiso`
Expected: All tests pass.

**Step 7: Commit**

```bash
git add agentiso/src/mcp/team_tools.rs agentiso/src/mcp/rate_limit.rs agentiso/src/config.rs
git commit -m "feat(mcp): add team message/receive actions with rate limiting"
```

---

### Task 8: Integration Tests

**Owner:** `mcp-server`
**Files:**
- Modify: `scripts/test-mcp-integration.sh`

**Step 1: Add team messaging test steps**

After the existing team lifecycle tests (steps ~40-45), add:

```bash
# --- Team messaging tests ---
# Step N: Send message between team members
echo "Step N: team_message send"
RESULT=$(echo '{"jsonrpc":"2.0","id":N,"method":"tools/call","params":{"name":"team","arguments":{"action":"message","name":"test-team","agent":"worker-1","to":"worker-2","content":"hello from worker-1","message_type":"text"}}}' | timeout 30 "$BINARY" serve --config "$CONFIG" 2>/dev/null)
echo "$RESULT" | jq -e '.result.content[0].text | fromjson | .message_id' || fail "team message send"

# Step N+1: Receive messages
echo "Step N+1: team_message receive"
RESULT=$(echo '{"jsonrpc":"2.0","id":N+1,"method":"tools/call","params":{"name":"team","arguments":{"action":"receive","name":"test-team","agent":"worker-2","limit":10}}}' | timeout 30 "$BINARY" serve --config "$CONFIG" 2>/dev/null)
echo "$RESULT" | jq -e '.result.content[0].text | fromjson | .count' || fail "team message receive"

# Step N+2: Broadcast message
echo "Step N+2: team broadcast"
RESULT=$(echo '{"jsonrpc":"2.0","id":N+2,"method":"tools/call","params":{"name":"team","arguments":{"action":"message","name":"test-team","agent":"worker-1","to":"*","content":"broadcast to all","message_type":"text"}}}' | timeout 30 "$BINARY" serve --config "$CONFIG" 2>/dev/null)
echo "$RESULT" | jq -e '.result.content[0].text | fromjson | .status' || fail "team broadcast"
```

**Step 2: Run the integration test**

Run: `sudo ./scripts/test-mcp-integration.sh`
Expected: All existing steps pass + 3 new messaging steps pass.

**Step 3: Update docs**

Update `docs/tools.md` to document the new `message` and `receive` actions for the team tool.

Update `CLAUDE.md` test counts and Phase 4 status.

Update `AGENTS.md` to reflect Phase 4 completion.

**Step 4: Commit**

```bash
git add scripts/test-mcp-integration.sh docs/tools.md CLAUDE.md AGENTS.md
git commit -m "test: add team messaging e2e tests + update docs for Phase 4"
```

---

## Build & Verification Sequence

After all tasks are complete:

```bash
# 1. Build everything
cargo build --release
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# 2. Run unit tests
cargo test

# 3. Rebuild guest agent into Alpine image
sudo ./scripts/setup-e2e.sh

# 4. Run e2e tests
sudo ./scripts/e2e-test.sh

# 5. Run MCP integration tests
sudo ./scripts/test-mcp-integration.sh
```
