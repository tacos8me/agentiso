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
