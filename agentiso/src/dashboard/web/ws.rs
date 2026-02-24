use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::{
    extract::{ws::{Message, WebSocket}, Query, State, WebSocketUpgrade},
    response::Response,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use super::DashboardState;

/// Query parameters for WebSocket upgrade (authentication).
#[derive(Deserialize)]
struct WsQuery {
    token: Option<String>,
}

pub fn routes() -> Router<Arc<DashboardState>> {
    Router::new().route("/ws", get(ws_upgrade))
}

/// WebSocket message envelope from server to client.
#[derive(Debug, Clone, Serialize)]
pub struct WsEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub topic: String,
    pub data: serde_json::Value,
    pub timestamp: String,
}

/// WebSocket message from client to server.
#[derive(Debug, Deserialize)]
struct WsClientMessage {
    #[serde(rename = "type")]
    msg_type: String,
    topic: Option<String>,
    #[allow(dead_code)]
    data: Option<serde_json::Value>,
}

/// Broadcast hub that distributes events to connected WebSocket clients.
pub struct BroadcastHub {
    sender: broadcast::Sender<WsEvent>,
    /// Tracks last known workspace states for diffing.
    last_workspace_states: RwLock<HashMap<String, String>>,
    /// Tracks last known team states for diffing.
    last_team_states: RwLock<HashMap<String, String>>,
}

impl BroadcastHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            sender,
            last_workspace_states: RwLock::new(HashMap::new()),
            last_team_states: RwLock::new(HashMap::new()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WsEvent> {
        self.sender.subscribe()
    }

    pub fn broadcast(&self, event: WsEvent) {
        // Ignore errors (no subscribers)
        let _ = self.sender.send(event);
    }

    pub fn broadcast_topic(&self, topic: &str, event_type: &str, data: serde_json::Value) {
        self.broadcast(WsEvent {
            event_type: event_type.to_string(),
            topic: topic.to_string(),
            data,
            timestamp: chrono::Utc::now().to_rfc3339(),
        });
    }

    /// Emit an event for a specific workspace (both per-workspace and aggregate topics).
    pub fn emit_workspace_event(&self, ws_id: &str, ws_name: &str, event_type: &str, data: serde_json::Value) {
        // Per-workspace topic
        self.broadcast_topic(
            &format!("workspace:{}", ws_id),
            event_type,
            data.clone(),
        );
        // Aggregate topic for list views
        self.broadcast_topic(
            "workspaces",
            event_type,
            serde_json::json!({
                "event": event_type,
                "workspace_id": ws_id,
                "workspace_name": ws_name,
                "detail": data,
            }),
        );
    }

    /// Emit an event for a specific team (both per-team and aggregate topics).
    #[allow(dead_code)]
    pub fn emit_team_event(&self, team_name: &str, event_type: &str, data: serde_json::Value) {
        self.broadcast_topic(
            &format!("team:{}", team_name),
            event_type,
            data.clone(),
        );
        self.broadcast_topic(
            "teams",
            event_type,
            serde_json::json!({
                "event": event_type,
                "team_name": team_name,
                "detail": data,
            }),
        );
    }
}

/// WebSocket upgrade handler with token authentication.
///
/// When `admin_token` is configured, the client must pass `?token=<admin_token>`
/// as a query parameter. Returns 401 if the token is missing or wrong.
async fn ws_upgrade(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let admin_token = &state.config.dashboard.admin_token;

    // If admin_token is set, require matching token query parameter
    if !admin_token.is_empty() {
        let provided = query.token.as_deref().unwrap_or("");
        // Use constant-time comparison (same as auth middleware)
        if provided.len() != admin_token.len()
            || provided
                .as_bytes()
                .iter()
                .zip(admin_token.as_bytes().iter())
                .fold(0u8, |acc, (x, y)| acc | (x ^ y))
                != 0
        {
            return super::error_response(
                axum::http::StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "invalid or missing WebSocket auth token",
            );
        }
    }

    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

/// Handle a single WebSocket connection.
async fn handle_ws(mut socket: WebSocket, state: Arc<DashboardState>) {
    let mut rx = state.ws_hub.subscribe();
    let mut subscriptions: HashSet<String> = HashSet::new();

    loop {
        tokio::select! {
            // Forward broadcast events to client (filtered by subscriptions)
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        // Send if client subscribed to this topic or has no subscriptions (all)
                        if subscriptions.is_empty() || subscriptions.contains(&event.topic) {
                            if let Ok(json) = serde_json::to_string(&event) {
                                if socket.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(n, "WebSocket client lagged, skipping messages");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            // Handle client messages
            result = socket.recv() => {
                match result {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(msg) = serde_json::from_str::<WsClientMessage>(&text) {
                            match msg.msg_type.as_str() {
                                "subscribe" => {
                                    if let Some(topic) = msg.topic {
                                        subscriptions.insert(topic);
                                    }
                                }
                                "unsubscribe" => {
                                    if let Some(topic) = msg.topic {
                                        subscriptions.remove(&topic);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

/// Start a background task that polls workspace/team state and broadcasts diffs.
/// This acts as a fallback reconciliation loop — primary updates come from
/// event-driven `emit_workspace_event` / `emit_team_event` calls.
pub fn start_polling_task(state: Arc<DashboardState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            poll_and_broadcast(&state).await;
        }
    });
}

async fn poll_and_broadcast(state: &DashboardState) {
    // Poll workspace states
    if let Ok(workspaces) = state.workspace_manager.list().await {
        let mut last_states = state.ws_hub.last_workspace_states.write().await;
        let mut changed = false;

        for ws in &workspaces {
            let id = ws.id.to_string();
            let current_state = ws.state.to_string();
            let prev = last_states.get(&id);

            if prev.is_none_or(|p| p != &current_state) {
                changed = true;
                last_states.insert(id.clone(), current_state.clone());

                // Broadcast per-workspace change
                state.ws_hub.broadcast_topic(
                    &format!("workspace:{}", id),
                    "state_change",
                    serde_json::json!({
                        "id": id,
                        "name": ws.name,
                        "state": current_state,
                        "qemu_pid": ws.qemu_pid,
                    }),
                );
            }
        }

        // Check for destroyed workspaces
        let current_ids: HashSet<String> = workspaces.iter().map(|ws| ws.id.to_string()).collect();
        let removed: Vec<String> = last_states
            .keys()
            .filter(|k| !current_ids.contains(*k))
            .cloned()
            .collect();
        for id in &removed {
            last_states.remove(id);
            changed = true;
            state.ws_hub.broadcast_topic(
                &format!("workspace:{}", id),
                "destroyed",
                serde_json::json!({"id": id}),
            );
        }

        if changed {
            // Broadcast aggregate workspaces update
            let summary: Vec<serde_json::Value> = workspaces
                .iter()
                .map(|ws| {
                    serde_json::json!({
                        "id": ws.id.to_string(),
                        "name": ws.name,
                        "state": ws.state.to_string(),
                    })
                })
                .collect();
            state.ws_hub.broadcast_topic("workspaces", "update", serde_json::json!(summary));
        }
    }

    // Poll team states
    let teams = state.workspace_manager.list_teams().await;
    let mut last_team = state.ws_hub.last_team_states.write().await;
    let mut team_changed = false;

    for t in &teams {
        let current_state = format!("{:?}", t.state);
        let prev = last_team.get(&t.name);
        if prev.is_none_or(|p| p != &current_state) {
            team_changed = true;
            last_team.insert(t.name.clone(), current_state.clone());

            state.ws_hub.broadcast_topic(
                &format!("team:{}", t.name),
                "state_change",
                serde_json::json!({
                    "name": t.name,
                    "state": current_state,
                    "member_count": t.member_workspace_ids.len(),
                }),
            );
        }
    }

    // Check for destroyed teams
    let current_names: HashSet<String> = teams.iter().map(|t| t.name.clone()).collect();
    let removed_teams: Vec<String> = last_team
        .keys()
        .filter(|k| !current_names.contains(*k))
        .cloned()
        .collect();
    for name in &removed_teams {
        last_team.remove(name);
        team_changed = true;
        state.ws_hub.broadcast_topic(
            &format!("team:{}", name),
            "destroyed",
            serde_json::json!({"name": name}),
        );
    }

    if team_changed {
        let summary: Vec<serde_json::Value> = teams
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "state": format!("{:?}", t.state),
                    "member_count": t.member_workspace_ids.len(),
                })
            })
            .collect();
        state.ws_hub.broadcast_topic("teams", "update", serde_json::json!(summary));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_hub_no_subscribers_is_ok() {
        let hub = BroadcastHub::new();
        // Should not panic even with no subscribers
        hub.broadcast_topic("test", "event", serde_json::json!({"hello": "world"}));
    }

    #[test]
    fn ws_event_serialization() {
        let event = WsEvent {
            event_type: "state_change".to_string(),
            topic: "workspaces".to_string(),
            data: serde_json::json!({"count": 5}),
            timestamp: "2026-02-21T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"state_change\""));
        assert!(json.contains("\"topic\":\"workspaces\""));
    }

    #[test]
    fn emit_workspace_event_sends_to_both_topics() {
        let hub = BroadcastHub::new();
        let mut rx = hub.subscribe();

        hub.emit_workspace_event(
            "abc-123",
            "test-ws",
            "workspace.started",
            serde_json::json!({"state": "running"}),
        );

        // Should receive per-workspace event
        let evt1 = rx.try_recv().unwrap();
        assert_eq!(evt1.topic, "workspace:abc-123");
        assert_eq!(evt1.event_type, "workspace.started");

        // Should receive aggregate event
        let evt2 = rx.try_recv().unwrap();
        assert_eq!(evt2.topic, "workspaces");
        assert_eq!(evt2.event_type, "workspace.started");
        let data = evt2.data;
        assert_eq!(data["workspace_id"], "abc-123");
        assert_eq!(data["workspace_name"], "test-ws");
    }

    #[test]
    fn emit_team_event_sends_to_both_topics() {
        let hub = BroadcastHub::new();
        let mut rx = hub.subscribe();

        hub.emit_team_event(
            "my-team",
            "team.created",
            serde_json::json!({"member_count": 3}),
        );

        let evt1 = rx.try_recv().unwrap();
        assert_eq!(evt1.topic, "team:my-team");
        assert_eq!(evt1.event_type, "team.created");

        let evt2 = rx.try_recv().unwrap();
        assert_eq!(evt2.topic, "teams");
        assert_eq!(evt2.event_type, "team.created");
        assert_eq!(evt2.data["team_name"], "my-team");
    }
}
