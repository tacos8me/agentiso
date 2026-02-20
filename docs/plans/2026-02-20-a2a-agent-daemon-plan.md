# A2A Agent Daemon Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build an autonomous agent daemon inside guest VMs that watches for task assignments, executes them (via OpenCode or shell), and reports results back — completing the A2A team coordination loop.

**Architecture:** The daemon is a tokio task spawned at guest-agent startup. It reads pushed messages from the existing MESSAGE_INBOX (populated by host relay on vsock port 5001), executes tasks, and stores results in a new RESULT_OUTBOX. The host collects results by sending a new `PollDaemonResults` request to the guest on vsock port 5000. No new vsock ports needed.

**Tech Stack:** Rust, tokio, serde_json, agentiso-protocol crate, existing guest-agent exec infrastructure

---

## Current State (from 6-agent research audit)

### What Already Works
- `MESSAGE_INBOX` global static in guest-agent (line 19) — stores pushed messages
- Relay vsock (port 5001) push handler (`handle_relay_connection`, line 1153) — host can push TeamMessage to guest
- HTTP API (port 8080) with `/health` and `/messages` endpoints — peer agents can query
- `TeamMessageEnvelope` type with `message_type` field (supports "task_assignment" hint)
- Host `MessageRelay` — stores messages in per-agent inboxes
- Host `TaskBoard` — vault-backed with Kahn's dependency ordering, claim/release/complete lifecycle
- Host `TeamManager` — creates teams, writes AgentCards, connects relay vsock, applies nftables
- All A2A protocol types exist in `protocol/src/lib.rs` (8 request/response pairs, tested)

### What's Broken / Missing
1. **Host relay does NOT push messages to guest** — `relay.send()` stores in host inbox only; relay vsock is connected but never used for delivery post-creation
2. **All A2A handlers rejected in guest** — VaultRead/Write, TaskClaim, TeamMessage, TeamReceive all return errors (lines 998-1021)
3. **No daemon loop** — nothing inside the guest reads MESSAGE_INBOX and acts on it
4. **No result collection** — no protocol message for host to poll daemon results
5. **No heartbeat** — no way to know if a daemon is alive/working/stuck

### Architecture Diagram

```
MCP Client (Claude Code / OpenCode)
         │
         ├── team(action="message", to="worker-1", type="task_assignment")
         │     └→ Host relay.send() stores in host inbox
         │     └→ [NEW] Host pushes to guest via relay vsock (port 5001)
         │     └→ Guest MESSAGE_INBOX receives message
         │     └→ [NEW] Daemon reads inbox, executes task
         │     └→ [NEW] Daemon stores result in RESULT_OUTBOX
         │
         └── team(action="receive", agent="worker-1")
               └→ Host drains host relay inbox (existing)
               └→ [NEW] Host sends PollDaemonResults to guest (port 5000)
               └→ [NEW] Guest returns RESULT_OUTBOX contents
               └→ Host combines and returns to MCP client
```

---

## Task 1: Add PollDaemonResults Protocol Type

**Files:**
- Modify: `protocol/src/lib.rs`
- Test: `protocol/src/lib.rs` (inline tests)

This adds the wire types for the host to query daemon results from the guest.

**Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` section in `protocol/src/lib.rs`:

```rust
#[test]
fn test_poll_daemon_results_request_roundtrip() {
    let req = GuestRequest::PollDaemonResults(PollDaemonResultsRequest {
        limit: 10,
    });
    let bytes = encode_message(&req).unwrap();
    let decoded: GuestRequest = serde_json::from_slice(&bytes[4..]).unwrap();
    match decoded {
        GuestRequest::PollDaemonResults(r) => assert_eq!(r.limit, 10),
        _ => panic!("expected PollDaemonResults"),
    }
}

#[test]
fn test_daemon_results_response_roundtrip() {
    let resp = GuestResponse::DaemonResults(DaemonResultsResponse {
        results: vec![DaemonTaskResult {
            task_id: "task-001".to_string(),
            success: true,
            exit_code: 0,
            stdout: "all tests pass".to_string(),
            stderr: String::new(),
            elapsed_secs: 45,
            source_message_id: "msg-abc".to_string(),
        }],
        pending_tasks: 0,
    });
    let bytes = encode_message(&resp).unwrap();
    let decoded: GuestResponse = serde_json::from_slice(&bytes[4..]).unwrap();
    match decoded {
        GuestResponse::DaemonResults(r) => {
            assert_eq!(r.results.len(), 1);
            assert_eq!(r.results[0].task_id, "task-001");
            assert!(r.results[0].success);
            assert_eq!(r.pending_tasks, 0);
        }
        _ => panic!("expected DaemonResults"),
    }
}

#[test]
fn test_task_assignment_message_roundtrip() {
    let msg = TaskAssignmentPayload {
        task_id: "task-001".to_string(),
        command: "opencode run 'implement auth'".to_string(),
        workdir: Some("/workspace".to_string()),
        env: std::collections::HashMap::from([
            ("ANTHROPIC_API_KEY".to_string(), "sk-test".to_string()),
        ]),
        timeout_secs: 300,
    };
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: TaskAssignmentPayload = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.task_id, "task-001");
    assert_eq!(parsed.timeout_secs, 300);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p agentiso-protocol -- test_poll_daemon_results`
Expected: FAIL — `PollDaemonResults` not found

**Step 3: Write minimal implementation**

Add these types to `protocol/src/lib.rs`:

```rust
// --- Daemon protocol types ---

/// Payload for task_assignment messages (sent as TeamMessage.content JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssignmentPayload {
    pub task_id: String,
    pub command: String,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default = "default_task_timeout")]
    pub timeout_secs: u64,
}

fn default_task_timeout() -> u64 {
    300
}

/// Host polls guest daemon for completed task results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollDaemonResultsRequest {
    #[serde(default = "default_poll_limit")]
    pub limit: u32,
}

fn default_poll_limit() -> u32 {
    20
}

/// Result of a single daemon task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonTaskResult {
    pub task_id: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub elapsed_secs: u64,
    /// The message_id of the TeamMessage that assigned this task.
    pub source_message_id: String,
}

/// Response containing daemon task results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResultsResponse {
    pub results: Vec<DaemonTaskResult>,
    /// Number of tasks still executing in the daemon.
    pub pending_tasks: u32,
}
```

Add to `GuestRequest` enum:
```rust
PollDaemonResults(PollDaemonResultsRequest),
```

Add to `GuestResponse` enum:
```rust
DaemonResults(DaemonResultsResponse),
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p agentiso-protocol`
Expected: ALL PASS (existing 70 tests + 3 new)

**Step 5: Commit**

```bash
git add protocol/src/lib.rs
git commit -m "feat(protocol): add PollDaemonResults + TaskAssignmentPayload types"
```

---

## Task 2: Complete Host Relay Push Path

**Files:**
- Modify: `agentiso/src/team/message_relay.rs` — Add push callback
- Modify: `agentiso/src/team/mod.rs` — Wire push callback during team creation
- Modify: `agentiso/src/mcp/tools.rs` — Wire push into team message action
- Test: `agentiso/src/team/message_relay.rs` (inline tests)

Currently `relay.send()` stores messages in the host-side inbox but never pushes to the guest. This task completes the push path.

**Step 1: Write the failing test**

Add to `message_relay.rs` tests:

```rust
#[tokio::test]
async fn push_callback_invoked_on_send() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let relay = MessageRelay::new();
    relay.register("alice", "team1", Uuid::new_v4()).await;
    relay.register("bob", "team1", Uuid::new_v4()).await;

    let push_count = Arc::new(AtomicU32::new(0));
    let count_clone = push_count.clone();
    relay.set_push_callback(Box::new(move |_team, _agent, _envelope| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    })).await;

    relay.send("team1", "alice", "bob", "hello", "text").await.unwrap();
    assert_eq!(push_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn push_callback_invoked_per_recipient_on_broadcast() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let relay = MessageRelay::new();
    relay.register("lead", "team1", Uuid::new_v4()).await;
    relay.register("coder", "team1", Uuid::new_v4()).await;
    relay.register("tester", "team1", Uuid::new_v4()).await;

    let push_count = Arc::new(AtomicU32::new(0));
    let count_clone = push_count.clone();
    relay.set_push_callback(Box::new(move |_team, _agent, _envelope| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    })).await;

    relay.send("team1", "lead", "*", "start", "text").await.unwrap();
    // Should push to coder and tester (not lead)
    assert_eq!(push_count.load(Ordering::SeqCst), 2);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p agentiso -- message_relay::tests::push_callback`
Expected: FAIL — `set_push_callback` not found

**Step 3: Write minimal implementation**

In `message_relay.rs`, add a push callback type and field:

```rust
/// Callback invoked after storing a message, for push delivery to guest.
/// Args: (team, recipient_agent_name, envelope)
pub type PushCallback = Box<dyn Fn(&str, &str, &TeamMessageEnvelope) + Send + Sync>;

pub struct MessageRelay {
    agents: RwLock<HashMap<String, AgentMailbox>>,
    push_callback: RwLock<Option<PushCallback>>,
}
```

Update `new()`:
```rust
pub fn new() -> Self {
    Self {
        agents: RwLock::new(HashMap::new()),
        push_callback: RwLock::new(None),
    }
}
```

Add method:
```rust
pub async fn set_push_callback(&self, callback: PushCallback) {
    let mut cb = self.push_callback.write().await;
    *cb = Some(callback);
}
```

In `send()`, after storing the envelope in the recipient's inbox, invoke the callback:
```rust
// After pushing to inbox:
if let Some(ref cb) = *self.push_callback.read().await {
    cb(team, &recipient_agent_name, &envelope);
}
```

For broadcast, invoke the callback per recipient.

**Step 4: Run test to verify it passes**

Run: `cargo test -p agentiso -- message_relay::tests`
Expected: ALL PASS

**Step 5: Wire push callback to VmManager relay push**

In the MCP server initialization (or where TeamManager is created), set up the push callback that actually sends messages via the relay vsock:

In `agentiso/src/mcp/tools.rs`, in the `team(action="message")` handler section, after `relay.send()`, add relay push:

```rust
// After relay.send() succeeds:
// Push to guest via relay vsock if connected
if let Some(ws_id) = self.relay.workspace_id(&team_name, &to_agent).await {
    let vm_mgr = self.workspace_manager.vm_manager().await;
    if let Some(relay_client) = vm_mgr.get_relay_client(&ws_id) {
        let push_req = GuestRequest::TeamMessage(TeamMessageRequest {
            to: envelope.from.clone(), // "from" field carries sender name on push
            content: envelope.content.clone(),
            message_type: envelope.message_type.clone(),
        });
        // Fire-and-forget push — don't fail the send if push fails
        let mut client = relay_client.lock().await;
        if let Err(e) = client.request(&push_req).await {
            tracing::warn!(error = %e, "relay push to guest failed (message stored in host inbox)");
        }
    }
}
```

Note: This wiring depends on `VmManager` exposing the relay client. Check if `get_relay_client()` exists; if not, add a simple accessor.

**Step 6: Commit**

```bash
git add agentiso/src/team/message_relay.rs agentiso/src/team/mod.rs agentiso/src/mcp/tools.rs
git commit -m "feat: complete relay push path — messages now pushed to guest via vsock"
```

---

## Task 3: Create Guest Daemon Module

**Files:**
- Create: `guest-agent/src/daemon.rs`
- Test: `guest-agent/src/daemon.rs` (inline tests)

This is the core daemon: an async loop that reads MESSAGE_INBOX, executes task assignments, and stores results.

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agentiso_protocol::TaskAssignmentPayload;

    #[test]
    fn parse_task_assignment_from_message() {
        let payload = TaskAssignmentPayload {
            task_id: "task-001".to_string(),
            command: "echo hello".to_string(),
            workdir: Some("/tmp".to_string()),
            env: Default::default(),
            timeout_secs: 30,
        };
        let content = serde_json::to_string(&payload).unwrap();

        let envelope = TeamMessageEnvelope {
            message_id: "msg-1".to_string(),
            from: "lead".to_string(),
            to: "worker-1".to_string(),
            content,
            message_type: "task_assignment".to_string(),
            timestamp: "2026-02-20T12:00:00Z".to_string(),
        };

        let parsed = parse_task_assignment(&envelope);
        assert!(parsed.is_some());
        let task = parsed.unwrap();
        assert_eq!(task.task_id, "task-001");
        assert_eq!(task.command, "echo hello");
    }

    #[test]
    fn non_task_message_returns_none() {
        let envelope = TeamMessageEnvelope {
            message_id: "msg-2".to_string(),
            from: "lead".to_string(),
            to: "worker-1".to_string(),
            content: "just a chat message".to_string(),
            message_type: "text".to_string(),
            timestamp: "2026-02-20T12:00:00Z".to_string(),
        };
        assert!(parse_task_assignment(&envelope).is_none());
    }

    #[test]
    fn result_outbox_drain() {
        let outbox = ResultOutbox::new();
        assert!(outbox.drain(10).is_empty());

        outbox.push(DaemonTaskResult {
            task_id: "task-001".to_string(),
            success: true,
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
            elapsed_secs: 5,
            source_message_id: "msg-1".to_string(),
        });

        let results = outbox.drain(10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, "task-001");

        // Should be empty after drain
        assert!(outbox.drain(10).is_empty());
    }

    #[test]
    fn pending_task_count() {
        let state = DaemonState::new();
        assert_eq!(state.pending_count(), 0);
        state.increment_pending();
        assert_eq!(state.pending_count(), 1);
        state.decrement_pending();
        assert_eq!(state.pending_count(), 0);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p agentiso-guest -- daemon::tests`
Expected: FAIL — module `daemon` not found

**Step 3: Write minimal implementation**

Create `guest-agent/src/daemon.rs`:

```rust
//! Autonomous agent daemon that processes task assignments from MESSAGE_INBOX.
//!
//! Spawned as a tokio task at guest-agent startup. Watches for incoming
//! TeamMessage with type="task_assignment", parses the TaskAssignmentPayload,
//! executes the command, and stores results in RESULT_OUTBOX for host collection.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use agentiso_protocol::{DaemonTaskResult, TaskAssignmentPayload, TeamMessageEnvelope};
use tokio::process::Command;
use tracing::{error, info, warn};

/// Maximum concurrent tasks the daemon will execute.
const MAX_CONCURRENT_TASKS: usize = 4;

/// How often the daemon polls MESSAGE_INBOX for new messages (seconds).
const POLL_INTERVAL_SECS: u64 = 2;

/// Maximum output size per task (2 MiB, matches guest-agent limit).
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Result Outbox
// ---------------------------------------------------------------------------

/// Thread-safe outbox for completed task results.
pub struct ResultOutbox {
    results: Mutex<Vec<DaemonTaskResult>>,
}

impl ResultOutbox {
    pub fn new() -> Self {
        Self {
            results: Mutex::new(Vec::new()),
        }
    }

    pub fn push(&self, result: DaemonTaskResult) {
        let mut results = self.results.lock().unwrap();
        results.push(result);
    }

    /// Drain up to `limit` results from the outbox.
    pub fn drain(&self, limit: usize) -> Vec<DaemonTaskResult> {
        let mut results = self.results.lock().unwrap();
        let count = limit.min(results.len());
        results.drain(..count).collect()
    }

    pub fn len(&self) -> usize {
        self.results.lock().unwrap().len()
    }
}

// ---------------------------------------------------------------------------
// Daemon State
// ---------------------------------------------------------------------------

/// Tracks daemon execution state.
pub struct DaemonState {
    pending: AtomicU32,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            pending: AtomicU32::new(0),
        }
    }

    pub fn pending_count(&self) -> u32 {
        self.pending.load(Ordering::SeqCst)
    }

    pub fn increment_pending(&self) {
        self.pending.fetch_add(1, Ordering::SeqCst);
    }

    pub fn decrement_pending(&self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Message parsing
// ---------------------------------------------------------------------------

/// Parse a TaskAssignmentPayload from a TeamMessage envelope.
/// Returns None if message_type != "task_assignment" or content isn't valid JSON.
pub fn parse_task_assignment(envelope: &TeamMessageEnvelope) -> Option<TaskAssignmentPayload> {
    if envelope.message_type != "task_assignment" {
        return None;
    }
    serde_json::from_str(&envelope.content).ok()
}

// ---------------------------------------------------------------------------
// Task execution
// ---------------------------------------------------------------------------

/// Execute a single task assignment and return the result.
async fn execute_task(task: &TaskAssignmentPayload, source_message_id: &str) -> DaemonTaskResult {
    let start = Instant::now();

    info!(
        task_id = %task.task_id,
        command = %task.command,
        timeout = task.timeout_secs,
        "daemon: executing task"
    );

    let workdir = task.workdir.as_deref().unwrap_or("/workspace");

    let result = tokio::time::timeout(
        Duration::from_secs(task.timeout_secs),
        async {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(&task.command).current_dir(workdir);

            // Apply environment variables
            for (k, v) in &task.env {
                cmd.env(k, v);
            }

            // Kill on drop to prevent orphan processes
            cmd.kill_on_drop(true);

            cmd.output().await
        },
    )
    .await;

    let elapsed = start.elapsed().as_secs();

    match result {
        Ok(Ok(output)) => {
            let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // Truncate output
            if stdout.len() > MAX_OUTPUT_BYTES {
                stdout.truncate(MAX_OUTPUT_BYTES);
                stdout.push_str("\n... (truncated)");
            }
            if stderr.len() > MAX_OUTPUT_BYTES {
                stderr.truncate(MAX_OUTPUT_BYTES);
                stderr.push_str("\n... (truncated)");
            }

            let exit_code = output.status.code().unwrap_or(-1);
            let success = exit_code == 0;

            info!(
                task_id = %task.task_id,
                exit_code,
                elapsed_secs = elapsed,
                "daemon: task completed"
            );

            DaemonTaskResult {
                task_id: task.task_id.clone(),
                success,
                exit_code,
                stdout,
                stderr,
                elapsed_secs: elapsed,
                source_message_id: source_message_id.to_string(),
            }
        }
        Ok(Err(e)) => {
            error!(task_id = %task.task_id, error = %e, "daemon: task exec failed");
            DaemonTaskResult {
                task_id: task.task_id.clone(),
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("exec failed: {}", e),
                elapsed_secs: elapsed,
                source_message_id: source_message_id.to_string(),
            }
        }
        Err(_) => {
            warn!(task_id = %task.task_id, timeout = task.timeout_secs, "daemon: task timed out");
            DaemonTaskResult {
                task_id: task.task_id.clone(),
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("task timed out after {}s", task.timeout_secs),
                elapsed_secs: elapsed,
                source_message_id: source_message_id.to_string(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main daemon loop
// ---------------------------------------------------------------------------

/// Run the daemon loop. Call via `tokio::spawn(daemon::run(outbox, state, inbox))`.
///
/// The daemon:
/// 1. Polls MESSAGE_INBOX every POLL_INTERVAL_SECS
/// 2. Parses task_assignment messages
/// 3. Spawns task execution (up to MAX_CONCURRENT_TASKS)
/// 4. Stores results in RESULT_OUTBOX
/// 5. Non-task messages are left for HTTP /messages retrieval
pub async fn run(
    outbox: &'static ResultOutbox,
    state: &'static DaemonState,
    inbox: &'static tokio::sync::Mutex<Vec<TeamMessageEnvelope>>,
) {
    info!("daemon: starting (poll_interval={}s, max_concurrent={})", POLL_INTERVAL_SECS, MAX_CONCURRENT_TASKS);

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TASKS));

    loop {
        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;

        // Drain inbox
        let messages = {
            let mut inbox = inbox.lock().await;
            std::mem::take(&mut *inbox)
        };

        if messages.is_empty() {
            continue;
        }

        let mut non_task_messages = Vec::new();

        for msg in messages {
            if let Some(task) = parse_task_assignment(&msg) {
                // Check concurrency limit
                if state.pending_count() >= MAX_CONCURRENT_TASKS as u32 {
                    warn!(
                        task_id = %task.task_id,
                        "daemon: max concurrent tasks reached, requeueing"
                    );
                    // Put back in inbox
                    let mut inbox = inbox.lock().await;
                    inbox.push(msg);
                    continue;
                }

                // Spawn task execution
                let sem = semaphore.clone();
                let message_id = msg.message_id.clone();
                state.increment_pending();

                tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    let result = execute_task(&task, &message_id).await;
                    outbox.push(result);
                    state.decrement_pending();
                });
            } else {
                // Not a task assignment — put back for HTTP /messages retrieval
                non_task_messages.push(msg);
            }
        }

        // Re-insert non-task messages
        if !non_task_messages.is_empty() {
            let mut inbox = inbox.lock().await;
            // Prepend so they're first in queue
            non_task_messages.append(&mut *inbox);
            *inbox = non_task_messages;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_task_assignment_from_message() {
        let payload = TaskAssignmentPayload {
            task_id: "task-001".to_string(),
            command: "echo hello".to_string(),
            workdir: Some("/tmp".to_string()),
            env: Default::default(),
            timeout_secs: 30,
        };
        let content = serde_json::to_string(&payload).unwrap();

        let envelope = TeamMessageEnvelope {
            message_id: "msg-1".to_string(),
            from: "lead".to_string(),
            to: "worker-1".to_string(),
            content,
            message_type: "task_assignment".to_string(),
            timestamp: "2026-02-20T12:00:00Z".to_string(),
        };

        let parsed = parse_task_assignment(&envelope);
        assert!(parsed.is_some());
        let task = parsed.unwrap();
        assert_eq!(task.task_id, "task-001");
        assert_eq!(task.command, "echo hello");
    }

    #[test]
    fn non_task_message_returns_none() {
        let envelope = TeamMessageEnvelope {
            message_id: "msg-2".to_string(),
            from: "lead".to_string(),
            to: "worker-1".to_string(),
            content: "just a chat message".to_string(),
            message_type: "text".to_string(),
            timestamp: "2026-02-20T12:00:00Z".to_string(),
        };
        assert!(parse_task_assignment(&envelope).is_none());
    }

    #[test]
    fn malformed_task_content_returns_none() {
        let envelope = TeamMessageEnvelope {
            message_id: "msg-3".to_string(),
            from: "lead".to_string(),
            to: "worker-1".to_string(),
            content: "not valid json{{{".to_string(),
            message_type: "task_assignment".to_string(),
            timestamp: "2026-02-20T12:00:00Z".to_string(),
        };
        assert!(parse_task_assignment(&envelope).is_none());
    }

    #[test]
    fn result_outbox_drain() {
        let outbox = ResultOutbox::new();
        assert!(outbox.drain(10).is_empty());

        outbox.push(DaemonTaskResult {
            task_id: "task-001".to_string(),
            success: true,
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
            elapsed_secs: 5,
            source_message_id: "msg-1".to_string(),
        });

        let results = outbox.drain(10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, "task-001");

        // Should be empty after drain
        assert!(outbox.drain(10).is_empty());
    }

    #[test]
    fn result_outbox_respects_limit() {
        let outbox = ResultOutbox::new();
        for i in 0..5 {
            outbox.push(DaemonTaskResult {
                task_id: format!("task-{:03}", i),
                success: true,
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                elapsed_secs: 1,
                source_message_id: format!("msg-{}", i),
            });
        }

        let batch = outbox.drain(3);
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].task_id, "task-000");

        let remaining = outbox.drain(10);
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].task_id, "task-003");
    }

    #[test]
    fn pending_task_count() {
        let state = DaemonState::new();
        assert_eq!(state.pending_count(), 0);
        state.increment_pending();
        assert_eq!(state.pending_count(), 1);
        state.increment_pending();
        assert_eq!(state.pending_count(), 2);
        state.decrement_pending();
        assert_eq!(state.pending_count(), 1);
        state.decrement_pending();
        assert_eq!(state.pending_count(), 0);
    }
}
```

**Step 4: Register module in guest-agent**

Add `pub mod daemon;` to `guest-agent/src/main.rs` (near the top, after imports).

**Step 5: Run tests to verify they pass**

Run: `cargo test -p agentiso-guest -- daemon::tests`
Expected: ALL PASS (6 tests)

**Step 6: Commit**

```bash
git add guest-agent/src/daemon.rs guest-agent/src/main.rs
git commit -m "feat(guest): add daemon module — task parsing, result outbox, execution loop"
```

---

## Task 4: Add PollDaemonResults Handler in Guest Agent

**Files:**
- Modify: `guest-agent/src/main.rs` — Add handler for PollDaemonResults, spawn daemon at startup

This wires the daemon into the guest-agent's request dispatch and makes it start automatically.

**Step 1: Add static globals for daemon state**

In `guest-agent/src/main.rs`, after the existing `MESSAGE_INBOX` static:

```rust
/// Daemon result outbox — completed task results awaiting host collection.
static RESULT_OUTBOX: std::sync::OnceLock<daemon::ResultOutbox> = std::sync::OnceLock::new();

fn result_outbox() -> &'static daemon::ResultOutbox {
    RESULT_OUTBOX.get_or_init(daemon::ResultOutbox::new)
}

/// Daemon execution state — tracks pending task count.
static DAEMON_STATE: std::sync::OnceLock<daemon::DaemonState> = std::sync::OnceLock::new();

fn daemon_state() -> &'static daemon::DaemonState {
    DAEMON_STATE.get_or_init(daemon::DaemonState::new)
}
```

**Step 2: Add PollDaemonResults handler in handle_request()**

In the `handle_request()` match arm (around line 981), add before the catch-all A2A rejections:

```rust
GuestRequest::PollDaemonResults(req) => {
    let results = result_outbox().drain(req.limit as usize);
    let pending = daemon_state().pending_count();
    GuestResponse::DaemonResults(DaemonResultsResponse {
        results,
        pending_tasks: pending,
    })
}
```

**Step 3: Spawn daemon at startup**

In the `main()` function (around line 1555, after HTTP API spawn), add:

```rust
// Spawn the A2A agent daemon
let inbox_ref = message_inbox();
tokio::spawn(async move {
    daemon::run(result_outbox(), daemon_state(), inbox_ref).await;
});
info!("agent daemon started");
```

**Step 4: Build and verify**

Run: `cargo build -p agentiso-guest`
Expected: Compiles without errors

Run: `cargo test -p agentiso-guest`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add guest-agent/src/main.rs
git commit -m "feat(guest): wire daemon startup + PollDaemonResults handler"
```

---

## Task 5: Add VsockClient::poll_daemon_results() on Host Side

**Files:**
- Modify: `agentiso/src/vm/vsock.rs` — Add poll_daemon_results() method
- Modify: `agentiso/src/guest/protocol.rs` — Re-export new types

**Step 1: Add re-exports in protocol.rs**

In `agentiso/src/guest/protocol.rs`, add the new types to the `use` statement:

```rust
// Add to existing re-exports from agentiso_protocol:
pub use agentiso_protocol::{
    // ... existing ...
    DaemonResultsResponse, DaemonTaskResult, PollDaemonResultsRequest, TaskAssignmentPayload,
};
```

**Step 2: Add method to VsockClient**

In `agentiso/src/vm/vsock.rs`, add:

```rust
/// Poll the guest daemon for completed task results.
///
/// Returns results from the daemon's outbox and the count of still-pending tasks.
pub async fn poll_daemon_results(
    &mut self,
    limit: u32,
) -> Result<protocol::DaemonResultsResponse> {
    let req = GuestRequest::PollDaemonResults(protocol::PollDaemonResultsRequest { limit });

    let resp = self
        .request_with_retry_timeout(&req, Duration::from_secs(10))
        .await?;

    match Self::unwrap_response(resp, "poll_daemon_results")? {
        GuestResponse::DaemonResults(results) => Ok(results),
        other => bail!("unexpected response to PollDaemonResults: {:?}", other),
    }
}
```

**Step 3: Build and verify**

Run: `cargo build -p agentiso`
Expected: Compiles without errors

**Step 4: Commit**

```bash
git add agentiso/src/vm/vsock.rs agentiso/src/guest/protocol.rs
git commit -m "feat: add VsockClient::poll_daemon_results() host-side method"
```

---

## Task 6: Wire team(action="receive") to Poll Guest Daemon

**Files:**
- Modify: `agentiso/src/mcp/tools.rs` — Enhance team receive to also poll guest daemon

Currently `team(action="receive")` only drains from the host relay inbox. This task adds guest daemon polling.

**Step 1: Locate the receive handler**

Search `agentiso/src/mcp/tools.rs` for the `"receive"` action in the team tool handler. It calls `self.relay.receive(team, agent, limit)`.

**Step 2: Add daemon polling after relay drain**

After draining the host relay inbox, add:

```rust
// Also poll guest daemon for completed task results
let mut daemon_results_text = String::new();
if let Some(ws_id) = self.relay.workspace_id(&team_name, &agent_name).await {
    let vm_mgr = self.workspace_manager.vm_manager().await;
    if let Some(vsock) = vm_mgr.get_vsock_client(&ws_id) {
        let mut client = vsock.lock().await;
        match client.poll_daemon_results(limit as u32).await {
            Ok(daemon_resp) => {
                if !daemon_resp.results.is_empty() {
                    daemon_results_text = format!(
                        "\n\n## Daemon Task Results ({} completed, {} pending)\n",
                        daemon_resp.results.len(),
                        daemon_resp.pending_tasks
                    );
                    for r in &daemon_resp.results {
                        daemon_results_text.push_str(&format!(
                            "\n### Task: {} ({})\n- Exit code: {}\n- Elapsed: {}s\n- Stdout:\n```\n{}\n```\n",
                            r.task_id,
                            if r.success { "SUCCESS" } else { "FAILED" },
                            r.exit_code,
                            r.elapsed_secs,
                            if r.stdout.len() > 4096 {
                                format!("{}... (truncated)", &r.stdout[..4096])
                            } else {
                                r.stdout.clone()
                            },
                        ));
                        if !r.stderr.is_empty() {
                            daemon_results_text.push_str(&format!(
                                "- Stderr:\n```\n{}\n```\n",
                                if r.stderr.len() > 2048 {
                                    format!("{}... (truncated)", &r.stderr[..2048])
                                } else {
                                    r.stderr.clone()
                                },
                            ));
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to poll daemon results");
            }
        }
    }
}

// Append daemon results to the response
// (existing response text already has relay messages)
response_text.push_str(&daemon_results_text);
```

**Step 3: Build and verify**

Run: `cargo build -p agentiso`
Expected: Compiles

Run: `cargo test -p agentiso`
Expected: ALL PASS

**Step 4: Commit**

```bash
git add agentiso/src/mcp/tools.rs
git commit -m "feat: team receive now also polls guest daemon for task results"
```

---

## Task 7: Rebuild Guest Binary + Update Alpine Image

**Files:**
- Run: Build commands
- Run: `scripts/setup-e2e.sh`

After all guest-agent changes, the binary must be rebuilt and installed into the Alpine base image.

**Step 1: Build guest agent (musl static)**

```bash
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest
```

**Step 2: Build host binary**

```bash
cargo build --release
```

**Step 3: Re-run setup-e2e.sh (requires sudo)**

This installs the new guest binary into the Alpine image and recreates the base ZFS snapshot.

```bash
sudo ./scripts/setup-e2e.sh
```

**Step 4: Run e2e tests**

```bash
sudo ./scripts/e2e-test.sh
```
Expected: ALL PASS

**Step 5: Run MCP integration tests**

```bash
sudo ./scripts/test-mcp-integration.sh
```
Expected: ALL PASS

**Step 6: Commit (if any script changes needed)**

```bash
git add scripts/
git commit -m "test: update e2e for daemon protocol changes"
```

---

## Task 8: Add Daemon Integration Test to MCP Test Script

**Files:**
- Modify: `scripts/test-mcp-integration.sh`

Add a test step that exercises the full daemon loop: create team, send task assignment, poll for results.

**Step 1: Add test step**

```bash
# --- Daemon Task Execution ---
step "Create team for daemon test"
TEAM_RESULT=$(mcp_call "team" '{"action":"create","name":"daemon-test","roles":[{"name":"worker","role":"executor"}],"max_vms":1}')
check_success "$TEAM_RESULT"

step "Send task assignment to daemon"
TASK_PAYLOAD=$(cat <<'TASK'
{"task_id":"test-001","command":"echo daemon-works && date","workdir":"/tmp","env":{},"timeout_secs":30}
TASK
)
MSG_RESULT=$(mcp_call "team" "{\"action\":\"message\",\"team\":\"daemon-test\",\"from\":\"lead\",\"to\":\"worker\",\"content\":$(echo "$TASK_PAYLOAD" | jq -Rs .),\"message_type\":\"task_assignment\"}")
check_success "$MSG_RESULT"

step "Wait for daemon to execute task"
sleep 10  # Give daemon time to pick up and execute

step "Poll daemon results"
RECV_RESULT=$(mcp_call "team" '{"action":"receive","team":"daemon-test","agent":"worker","limit":10}')
check_success "$RECV_RESULT"
# Verify daemon results contain our task
echo "$RECV_RESULT" | grep -q "daemon-works" || echo "$RECV_RESULT" | grep -q "test-001" || fail "daemon result not found"

step "Destroy daemon test team"
mcp_call "team" '{"action":"destroy","name":"daemon-test"}' > /dev/null
```

**Step 2: Run integration test**

```bash
sudo ./scripts/test-mcp-integration.sh
```
Expected: ALL PASS including new daemon steps

**Step 3: Commit**

```bash
git add scripts/test-mcp-integration.sh
git commit -m "test: add daemon task execution integration test"
```

---

## Task 9: Update Documentation

**Files:**
- Modify: `docs/tools.md` — Document daemon behavior in team tool
- Modify: `docs/workflows.md` — Add daemon workflow example
- Modify: `CLAUDE.md` — Update status section

**Step 1: Update docs/tools.md**

In the `team` tool section, add under the `message` action:

```markdown
#### Daemon Task Assignment

When sending a message with `message_type: "task_assignment"`, the content must be a JSON-encoded `TaskAssignmentPayload`:

```json
{
  "task_id": "task-001",
  "command": "opencode run 'implement JWT auth in src/auth.rs'",
  "workdir": "/workspace",
  "env": {"ANTHROPIC_API_KEY": "sk-..."},
  "timeout_secs": 300
}
```

The guest daemon automatically:
1. Detects task_assignment messages in its inbox
2. Executes the command (up to 4 concurrent tasks)
3. Stores results for collection via `team(action="receive")`

Results include exit_code, stdout, stderr, elapsed_secs, and success flag.
```

**Step 2: Update docs/workflows.md**

Add a "Team Delegation with Daemon" workflow:

```markdown
### Team Delegation (Daemon-Driven)

1. Create team: `team(action="create", name="my-team", roles=[{name:"coder", role:"dev"}, {name:"tester", role:"qa"}])`
2. Inject API keys: `set_env(workspace="my-team-coder-xxxx", vars={"ANTHROPIC_API_KEY":"sk-..."})`
3. Assign task: `team(action="message", team="my-team", to="coder", content=<TaskAssignmentPayload JSON>, message_type="task_assignment")`
4. Poll results: `team(action="receive", team="my-team", agent="coder")` — includes daemon task results
5. Cleanup: `team(action="destroy", name="my-team")`
```

**Step 3: Update CLAUDE.md**

Add to the "Current Status" section:

```markdown
**A2A Agent Daemon (Phase 1, complete)**:
- Guest daemon auto-starts, watches MESSAGE_INBOX for task_assignment messages
- Executes up to 4 concurrent tasks with timeout and output truncation
- Results collected via `team(action="receive")` which polls both host relay and guest daemon
- Host relay push path completed — messages pushed to guest via vsock relay (port 5001)
- Protocol: TaskAssignmentPayload, PollDaemonResults, DaemonTaskResult types
```

**Step 4: Commit**

```bash
git add docs/tools.md docs/workflows.md CLAUDE.md
git commit -m "docs: document A2A agent daemon and team delegation workflow"
```

---

## Summary

| Task | LOC (est) | Files | Dependencies |
|------|-----------|-------|--------------|
| 1. Protocol types | ~70 | protocol/src/lib.rs | None |
| 2. Relay push path | ~80 | message_relay.rs, tools.rs | Task 1 |
| 3. Guest daemon module | ~250 | daemon.rs, main.rs | Task 1 |
| 4. PollDaemonResults handler | ~30 | main.rs | Tasks 1, 3 |
| 5. Host VsockClient method | ~25 | vsock.rs, protocol.rs | Task 1 |
| 6. Wire team receive | ~50 | tools.rs | Tasks 4, 5 |
| 7. Rebuild + e2e | ~0 | scripts/ | Tasks 1-6 |
| 8. Integration test | ~30 | test-mcp-integration.sh | Task 7 |
| 9. Documentation | ~40 | docs/, CLAUDE.md | All |
| **Total** | **~575** | | |

**Execution order:** Tasks 1 → (2, 3 in parallel) → (4, 5 in parallel) → 6 → 7 → 8 → 9

**Key design decisions:**
- No new vsock ports — reuses existing port 5000 (request/response) and 5001 (relay push)
- Push-based message delivery + poll-based result collection (natural MCP pattern)
- Daemon is a tokio task, not a separate process — shares guest-agent runtime
- Semaphore-gated concurrency (max 4 tasks) prevents resource exhaustion
- Non-task messages preserved in inbox for HTTP /messages retrieval
- Fire-and-forget relay push — message is always stored in host inbox as backup
