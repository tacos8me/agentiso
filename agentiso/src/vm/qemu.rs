use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{debug, trace, warn};

/// Default timeout for individual QMP commands. If QEMU does not respond
/// within this duration the command is considered failed.
pub const DEFAULT_QMP_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum delay between connection retry attempts in `connect_with_retry`.
const MAX_BACKOFF: Duration = Duration::from_secs(2);

/// Validate a tag name used in HMP commands (savevm, loadvm, delvm).
///
/// HMP commands are parsed as free-form text, so special characters could
/// inject additional commands. This function restricts tags to safe characters.
fn validate_hmp_tag(tag: &str) -> Result<()> {
    if tag.is_empty() {
        bail!("HMP tag must not be empty");
    }
    if !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        bail!("HMP tag '{}' contains invalid characters (allowed: alphanumeric, -, _, .)", tag);
    }
    if tag.len() > 128 {
        bail!("HMP tag '{}' too long (max 128 chars)", tag);
    }
    Ok(())
}

/// QMP (QEMU Machine Protocol) client for controlling a running QEMU instance.
///
/// QMP is a JSON-based protocol over a Unix domain socket. The client must
/// first negotiate capabilities before sending commands.
pub struct QmpClient {
    reader: BufReader<tokio::io::ReadHalf<UnixStream>>,
    writer: tokio::io::WriteHalf<UnixStream>,
    socket_path: PathBuf,
    /// Timeout applied to each individual QMP command (write + read loop).
    command_timeout: Duration,
}

/// A QMP command to send to QEMU.
#[derive(Debug, Serialize)]
struct QmpCommand {
    execute: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<serde_json::Value>,
}

/// The QMP greeting message sent by QEMU on connection.
#[derive(Debug, Deserialize)]
struct QmpGreeting {
    #[allow(dead_code)]
    #[serde(rename = "QMP")]
    qmp: QmpGreetingInfo,
}

#[derive(Debug, Deserialize)]
struct QmpGreetingInfo {
    #[allow(dead_code)]
    version: serde_json::Value,
    #[allow(dead_code)]
    capabilities: Vec<serde_json::Value>,
}

/// A QMP response from QEMU.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum QmpResponse {
    Return {
        #[serde(rename = "return")]
        ret: serde_json::Value,
    },
    Error {
        error: QmpError,
    },
    Event {
        event: String,
        #[allow(dead_code)]
        data: Option<serde_json::Value>,
        #[allow(dead_code)]
        timestamp: Option<serde_json::Value>,
    },
}

#[derive(Debug, Deserialize)]
struct QmpError {
    class: String,
    desc: String,
}

/// Status of the QEMU VM.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Public API returned by QmpClient::query_status
pub enum VmStatus {
    Running,
    Paused,
    Shutdown,
    /// Any other status reported by QMP.
    Other(String),
}

#[allow(dead_code)] // Public API consumed by VmManager
impl QmpClient {
    /// Connect to a QMP socket and perform the capability negotiation handshake.
    ///
    /// This will:
    /// 1. Connect to the Unix socket
    /// 2. Read the QMP greeting
    /// 3. Send `qmp_capabilities` to enter command mode
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .with_context(|| format!("failed to connect to QMP socket: {}", socket_path.display()))?;

        let (read_half, write_half) = tokio::io::split(stream);
        let mut client = Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            socket_path: socket_path.to_path_buf(),
            command_timeout: DEFAULT_QMP_COMMAND_TIMEOUT,
        };

        // Read the QMP greeting
        let greeting_line = client.read_line().await?;
        let _greeting: QmpGreeting = serde_json::from_str(&greeting_line)
            .with_context(|| format!("failed to parse QMP greeting: {}", greeting_line))?;
        debug!(socket = %socket_path.display(), "QMP greeting received");

        // Negotiate capabilities (we accept defaults)
        client
            .execute_raw("qmp_capabilities", None)
            .await
            .context("QMP capability negotiation failed")?;
        debug!(socket = %socket_path.display(), "QMP capabilities negotiated");

        Ok(client)
    }

    /// Connect to a QMP socket with retries, waiting for QEMU to create it.
    ///
    /// Uses exponential backoff starting at `retry_delay`, doubling each
    /// attempt, and capping at 2 seconds.
    pub async fn connect_with_retry(
        socket_path: &Path,
        max_retries: u32,
        retry_delay: std::time::Duration,
    ) -> Result<Self> {
        let mut last_error = None;

        for attempt in 1..=max_retries {
            match Self::connect(socket_path).await {
                Ok(client) => return Ok(client),
                Err(e) => {
                    // attempt is 1-indexed, backoff_delay expects 0-indexed
                    let delay = backoff_delay(retry_delay, attempt - 1);
                    trace!(
                        attempt,
                        max_retries,
                        delay_ms = delay.as_millis(),
                        error = %e,
                        "QMP connect attempt failed, retrying"
                    );
                    last_error = Some(e);
                    tokio::time::sleep(delay).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("QMP connect failed with no attempts")))
    }

    /// Query the VM status.
    pub async fn query_status(&mut self) -> Result<VmStatus> {
        let result = self.execute("query-status", None).await?;
        let status_str = result["status"]
            .as_str()
            .context("query-status response missing 'status' field")?;

        Ok(match status_str {
            "running" => VmStatus::Running,
            "paused" => VmStatus::Paused,
            "shutdown" => VmStatus::Shutdown,
            other => VmStatus::Other(other.to_string()),
        })
    }

    /// Stop (pause) the VM.
    pub async fn stop(&mut self) -> Result<()> {
        self.execute("stop", None).await?;
        Ok(())
    }

    /// Continue (resume) a paused VM.
    pub async fn cont(&mut self) -> Result<()> {
        self.execute("cont", None).await?;
        Ok(())
    }

    /// Initiate a clean shutdown via ACPI power button.
    pub async fn system_powerdown(&mut self) -> Result<()> {
        self.execute("system_powerdown", None).await?;
        Ok(())
    }

    /// Immediately terminate the VM (like pulling the power cord).
    pub async fn quit(&mut self) -> Result<()> {
        self.execute("quit", None).await?;
        Ok(())
    }

    /// Save VM state to a named tag (for live snapshots).
    /// The tag name is used with loadvm to restore.
    pub async fn savevm(&mut self, tag: &str) -> Result<()> {
        validate_hmp_tag(tag)?;
        self.execute("human-monitor-command", Some(serde_json::json!({
            "command-line": format!("savevm {}", tag)
        })))
        .await?;
        Ok(())
    }

    /// Restore VM state from a named tag.
    pub async fn loadvm(&mut self, tag: &str) -> Result<()> {
        validate_hmp_tag(tag)?;
        self.execute("human-monitor-command", Some(serde_json::json!({
            "command-line": format!("loadvm {}", tag)
        })))
        .await?;
        Ok(())
    }

    /// Delete a saved VM state by tag name.
    pub async fn delvm(&mut self, tag: &str) -> Result<()> {
        validate_hmp_tag(tag)?;
        self.execute("human-monitor-command", Some(serde_json::json!({
            "command-line": format!("delvm {}", tag)
        })))
        .await?;
        Ok(())
    }

    /// Execute a QMP command and return the result value.
    ///
    /// This skips any async events that arrive before the command response.
    pub async fn execute(
        &mut self,
        command: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        self.execute_raw(command, arguments).await
    }

    /// Send a raw QMP command and read the response.
    ///
    /// The entire write + read loop is wrapped in `command_timeout`. If QEMU
    /// does not respond within the timeout, an error is returned rather than
    /// blocking indefinitely.
    async fn execute_raw(
        &mut self,
        command: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let timeout = self.command_timeout;
        let cmd_name = command.to_string();

        tokio::time::timeout(timeout, self.execute_raw_inner(command, arguments))
            .await
            .map_err(|_| {
                anyhow::anyhow!("QMP command '{}' timed out after {:?}", cmd_name, timeout)
            })?
    }

    /// Inner implementation of execute_raw, called within a timeout wrapper.
    async fn execute_raw_inner(
        &mut self,
        command: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let cmd = QmpCommand {
            execute: command.to_string(),
            arguments,
        };

        let mut json = serde_json::to_string(&cmd)
            .context("failed to serialize QMP command")?;
        json.push('\n');

        trace!(command, json = %json.trim(), "sending QMP command");

        self.writer
            .write_all(json.as_bytes())
            .await
            .context("failed to write to QMP socket")?;
        self.writer
            .flush()
            .await
            .context("failed to flush QMP socket")?;

        // Read responses, skipping async events until we get a return or error
        loop {
            let line = self.read_line().await?;
            trace!(response = %line, "QMP response line");

            let response: QmpResponse = serde_json::from_str(&line)
                .with_context(|| format!("failed to parse QMP response: {}", line))?;

            match response {
                QmpResponse::Return { ret } => {
                    debug!(command, "QMP command succeeded");
                    return Ok(ret);
                }
                QmpResponse::Error { error } => {
                    bail!(
                        "QMP command '{}' failed: {} ({})",
                        command,
                        error.desc,
                        error.class
                    );
                }
                QmpResponse::Event { event, .. } => {
                    debug!(event, "QMP async event received (skipping)");
                    continue;
                }
            }
        }
    }

    /// Read a single line from the QMP socket.
    async fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let bytes_read = self
            .reader
            .read_line(&mut line)
            .await
            .context("failed to read from QMP socket")?;

        if bytes_read == 0 {
            bail!(
                "QMP socket closed unexpectedly: {}",
                self.socket_path.display()
            );
        }

        Ok(line.trim().to_string())
    }

    /// Return the path of the connected QMP socket.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

/// Spawn a QEMU process and return its child handle.
///
/// The caller is responsible for:
/// - Creating the run directory beforehand
/// - Connecting to the QMP socket after QEMU starts
/// - Waiting for the guest agent to be ready
pub async fn spawn_qemu(config: &super::microvm::VmConfig) -> Result<tokio::process::Child> {
    let qemu_cmd = config.build_command();

    debug!(
        cmd = %qemu_cmd.command_line(),
        "spawning QEMU process"
    );

    // Ensure the run directory exists
    tokio::fs::create_dir_all(&config.run_dir)
        .await
        .with_context(|| {
            format!(
                "failed to create QEMU run directory: {}",
                config.run_dir.display()
            )
        })?;

    // Redirect QEMU stderr to a file in the run directory.
    // Using Stdio::piped() without reading would cause QEMU to hang once
    // the 64KB pipe buffer fills (QEMU's main loop is single-threaded).
    let stderr_path = config.run_dir.join("qemu-stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path)
        .context("failed to create qemu-stderr.log")?;

    let child = qemu_cmd
        .to_tokio_command()
        .stderr(std::process::Stdio::from(stderr_file))
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn QEMU process")?;

    let pid = child.id().unwrap_or(0);
    debug!(pid, "QEMU process spawned");

    // Write PID file so orphaned QEMU processes can be found after a daemon crash.
    if pid != 0 {
        let pid_path = config.run_dir.join("qemu.pid");
        if let Err(e) = tokio::fs::write(&pid_path, pid.to_string()).await {
            warn!(
                pid,
                path = %pid_path.display(),
                error = %e,
                "failed to write QEMU PID file"
            );
        }
    }

    Ok(child)
}

/// Kill a QEMU process by PID. Used as a last resort when QMP is unresponsive.
pub async fn kill_qemu(pid: u32) -> Result<()> {
    warn!(pid, "force-killing QEMU process");

    let status = tokio::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .await
        .context("failed to execute kill command")?;

    if status.success() {
        debug!(pid, "QEMU process killed");
        Ok(())
    } else {
        bail!("failed to kill QEMU process {}: exit code {:?}", pid, status.code())
    }
}

/// Wait for a QMP socket file to appear on disk, with timeout.
pub async fn wait_for_qmp_socket(
    socket_path: &Path,
    timeout: std::time::Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let poll_interval = std::time::Duration::from_millis(50);

    loop {
        if socket_path.exists() {
            return Ok(());
        }

        if tokio::time::Instant::now() >= deadline {
            bail!(
                "QMP socket did not appear within {:?}: {}",
                timeout,
                socket_path.display()
            );
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Read the last `n` lines of a file, returning them as a single string.
///
/// If the file does not exist or cannot be read, returns a descriptive
/// placeholder string rather than an error, since this is used for
/// best-effort diagnostics in error paths.
pub async fn read_tail(path: &Path, n: usize) -> String {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => {
            let lines: Vec<&str> = contents.lines().collect();
            let start = lines.len().saturating_sub(n);
            lines[start..].join("\n")
        }
        Err(e) => format!("[could not read {}: {}]", path.display(), e),
    }
}

/// Compute the exponential backoff delay for a given attempt.
///
/// `attempt` is 0-indexed. The delay starts at `base` and doubles each
/// attempt, capping at `MAX_BACKOFF`.
pub(crate) fn backoff_delay(base: Duration, attempt: u32) -> Duration {
    let multiplier = 2u32.saturating_pow(attempt);
    let delay = base.saturating_mul(multiplier);
    delay.min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // QmpCommand serialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_qmp_command_serializes_without_arguments() {
        let cmd = QmpCommand {
            execute: "query-status".to_string(),
            arguments: None,
        };
        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["execute"], "query-status");
        // arguments should be absent (skip_serializing_if = Option::is_none)
        assert!(json.get("arguments").is_none());
    }

    #[test]
    fn test_qmp_command_serializes_with_arguments() {
        let cmd = QmpCommand {
            execute: "human-monitor-command".to_string(),
            arguments: Some(serde_json::json!({"command-line": "savevm snap1"})),
        };
        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["execute"], "human-monitor-command");
        assert_eq!(json["arguments"]["command-line"], "savevm snap1");
    }

    #[test]
    fn test_qmp_command_capabilities_format() {
        let cmd = QmpCommand {
            execute: "qmp_capabilities".to_string(),
            arguments: None,
        };
        let json_str = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json_str, r#"{"execute":"qmp_capabilities"}"#);
    }

    // -----------------------------------------------------------------------
    // QmpResponse deserialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_qmp_response_return_empty() {
        let json = r#"{"return": {}}"#;
        let resp: QmpResponse = serde_json::from_str(json).unwrap();
        match resp {
            QmpResponse::Return { ret } => {
                assert!(ret.is_object());
            }
            _ => panic!("expected Return variant"),
        }
    }

    #[test]
    fn test_qmp_response_return_with_status() {
        let json = r#"{"return": {"status": "running", "singlestep": false, "running": true}}"#;
        let resp: QmpResponse = serde_json::from_str(json).unwrap();
        match resp {
            QmpResponse::Return { ret } => {
                assert_eq!(ret["status"], "running");
            }
            _ => panic!("expected Return variant"),
        }
    }

    #[test]
    fn test_qmp_response_error() {
        let json = r#"{"error": {"class": "GenericError", "desc": "Command not found"}}"#;
        let resp: QmpResponse = serde_json::from_str(json).unwrap();
        match resp {
            QmpResponse::Error { error } => {
                assert_eq!(error.class, "GenericError");
                assert_eq!(error.desc, "Command not found");
            }
            _ => panic!("expected Error variant"),
        }
    }

    #[test]
    fn test_qmp_response_event() {
        let json = r#"{"event": "POWERDOWN", "data": {}, "timestamp": {"seconds": 1234, "microseconds": 0}}"#;
        let resp: QmpResponse = serde_json::from_str(json).unwrap();
        match resp {
            QmpResponse::Event { event, .. } => {
                assert_eq!(event, "POWERDOWN");
            }
            _ => panic!("expected Event variant"),
        }
    }

    #[test]
    fn test_qmp_response_event_minimal() {
        // Events can come without data or timestamp
        let json = r#"{"event": "STOP"}"#;
        let resp: QmpResponse = serde_json::from_str(json).unwrap();
        match resp {
            QmpResponse::Event { event, data, timestamp } => {
                assert_eq!(event, "STOP");
                assert!(data.is_none());
                assert!(timestamp.is_none());
            }
            _ => panic!("expected Event variant"),
        }
    }

    // -----------------------------------------------------------------------
    // QmpGreeting parsing
    // -----------------------------------------------------------------------

    #[test]
    fn test_qmp_greeting_parses() {
        let json = r#"{"QMP": {"version": {"qemu": {"micro": 0, "minor": 2, "major": 9}, "package": "v9.2.0"}, "capabilities": ["oob"]}}"#;
        let greeting: QmpGreeting = serde_json::from_str(json).unwrap();
        assert_eq!(
            greeting.qmp.version["qemu"]["major"],
            serde_json::json!(9)
        );
        assert_eq!(greeting.qmp.capabilities.len(), 1);
    }

    #[test]
    fn test_qmp_greeting_empty_capabilities() {
        let json = r#"{"QMP": {"version": {"qemu": {"micro": 0, "minor": 0, "major": 8}}, "capabilities": []}}"#;
        let greeting: QmpGreeting = serde_json::from_str(json).unwrap();
        assert!(greeting.qmp.capabilities.is_empty());
    }

    // -----------------------------------------------------------------------
    // VmStatus
    // -----------------------------------------------------------------------

    #[test]
    fn test_vm_status_equality() {
        assert_eq!(VmStatus::Running, VmStatus::Running);
        assert_eq!(VmStatus::Paused, VmStatus::Paused);
        assert_eq!(VmStatus::Shutdown, VmStatus::Shutdown);
        assert_ne!(VmStatus::Running, VmStatus::Paused);
        assert_eq!(
            VmStatus::Other("inmigrate".into()),
            VmStatus::Other("inmigrate".into())
        );
        assert_ne!(
            VmStatus::Other("inmigrate".into()),
            VmStatus::Other("postmigrate".into())
        );
    }

    #[test]
    fn test_vm_status_clone() {
        let status = VmStatus::Other("prelaunch".into());
        let cloned = status.clone();
        assert_eq!(status, cloned);
    }

    // -----------------------------------------------------------------------
    // wait_for_qmp_socket (async test using tempfile)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_wait_for_qmp_socket_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("qmp.sock");
        // Create the file so it exists immediately
        std::fs::write(&sock_path, "").unwrap();

        let result =
            wait_for_qmp_socket(&sock_path, std::time::Duration::from_millis(100)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_for_qmp_socket_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("nonexistent.sock");

        let result =
            wait_for_qmp_socket(&sock_path, std::time::Duration::from_millis(100)).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("did not appear within"));
    }

    #[tokio::test]
    async fn test_wait_for_qmp_socket_appears_later() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("delayed.sock");
        let sock_path_clone = sock_path.clone();

        // Spawn a task that creates the file after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            std::fs::write(&sock_path_clone, "").unwrap();
        });

        let result =
            wait_for_qmp_socket(&sock_path, std::time::Duration::from_secs(2)).await;
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // QMP command timeout constant
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_qmp_command_timeout() {
        assert_eq!(DEFAULT_QMP_COMMAND_TIMEOUT, Duration::from_secs(10));
    }

    #[test]
    fn test_max_backoff_cap() {
        assert_eq!(MAX_BACKOFF, Duration::from_secs(2));
    }

    // -----------------------------------------------------------------------
    // Exponential backoff calculation
    // -----------------------------------------------------------------------

    #[test]
    fn test_backoff_delay_first_attempt() {
        let base = Duration::from_millis(200);
        // attempt 0 => 200ms * 2^0 = 200ms
        assert_eq!(backoff_delay(base, 0), Duration::from_millis(200));
    }

    #[test]
    fn test_backoff_delay_doubles() {
        let base = Duration::from_millis(200);
        // attempt 1 => 200ms * 2 = 400ms
        assert_eq!(backoff_delay(base, 1), Duration::from_millis(400));
        // attempt 2 => 200ms * 4 = 800ms
        assert_eq!(backoff_delay(base, 2), Duration::from_millis(800));
        // attempt 3 => 200ms * 8 = 1600ms
        assert_eq!(backoff_delay(base, 3), Duration::from_millis(1600));
    }

    #[test]
    fn test_backoff_delay_caps_at_max() {
        let base = Duration::from_millis(200);
        // attempt 4 => 200ms * 16 = 3200ms, but capped at 2000ms
        assert_eq!(backoff_delay(base, 4), MAX_BACKOFF);
        // attempt 10 => huge, still capped at 2000ms
        assert_eq!(backoff_delay(base, 10), MAX_BACKOFF);
    }

    #[test]
    fn test_backoff_delay_large_base() {
        let base = Duration::from_secs(1);
        // attempt 0 => 1s
        assert_eq!(backoff_delay(base, 0), Duration::from_secs(1));
        // attempt 1 => 2s, capped at 2s
        assert_eq!(backoff_delay(base, 1), MAX_BACKOFF);
    }

    // -----------------------------------------------------------------------
    // HMP tag validation
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_hmp_tag_valid() {
        assert!(validate_hmp_tag("snap1").is_ok());
        assert!(validate_hmp_tag("my-snapshot_v2.0").is_ok());
        assert!(validate_hmp_tag("a").is_ok());
    }

    #[test]
    fn test_validate_hmp_tag_empty() {
        assert!(validate_hmp_tag("").is_err());
    }

    #[test]
    fn test_validate_hmp_tag_special_chars() {
        assert!(validate_hmp_tag("snap;rm -rf /").is_err());
        assert!(validate_hmp_tag("snap name").is_err());
        assert!(validate_hmp_tag("snap\nname").is_err());
    }

    #[test]
    fn test_validate_hmp_tag_too_long() {
        let long_tag = "a".repeat(129);
        assert!(validate_hmp_tag(&long_tag).is_err());
        let ok_tag = "a".repeat(128);
        assert!(validate_hmp_tag(&ok_tag).is_ok());
    }

    // -----------------------------------------------------------------------
    // read_tail helper
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_read_tail_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.log");
        let content = (1..=50).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        std::fs::write(&file_path, &content).unwrap();

        let tail = read_tail(&file_path, 5).await;
        assert!(tail.contains("line 46"));
        assert!(tail.contains("line 50"));
        assert!(!tail.contains("line 45"));
    }

    #[tokio::test]
    async fn test_read_tail_fewer_lines_than_requested() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("short.log");
        std::fs::write(&file_path, "line 1\nline 2\nline 3").unwrap();

        let tail = read_tail(&file_path, 10).await;
        assert!(tail.contains("line 1"));
        assert!(tail.contains("line 3"));
    }

    #[tokio::test]
    async fn test_read_tail_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("empty.log");
        std::fs::write(&file_path, "").unwrap();

        let tail = read_tail(&file_path, 5).await;
        assert_eq!(tail, "");
    }

    #[tokio::test]
    async fn test_read_tail_missing_file() {
        let path = Path::new("/tmp/nonexistent-agentiso-test-file.log");
        let tail = read_tail(path, 5).await;
        assert!(tail.contains("could not read"));
        assert!(tail.contains("nonexistent-agentiso-test-file.log"));
    }
}
