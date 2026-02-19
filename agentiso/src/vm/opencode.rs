//! OpenCode execution wrapper for running AI coding tasks inside VMs.
//!
//! This module provides a thin wrapper around the guest agent's `Exec` RPC
//! to invoke `opencode run` with structured JSON output. It handles argument
//! construction, timeout management, and output parsing.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::vsock::VsockClient;

/// Default timeout for `opencode run` in seconds (5 minutes).
/// LLM API calls can take 30s-300s depending on task complexity.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Result of an `opencode run` invocation inside a VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeResult {
    /// Whether the command completed successfully (exit code 0).
    pub success: bool,
    /// Process exit code.
    pub exit_code: i32,
    /// Standard output (contains JSON when `--format json` is used).
    pub stdout: String,
    /// Standard error output.
    pub stderr: String,
    /// Parsed structured output from opencode's JSON format, if available.
    pub output: Option<OpenCodeOutput>,
}

/// Structured output parsed from opencode's `--format json` output.
///
/// This captures the fields that opencode emits when run with `--format json`.
/// Unknown fields are ignored for forward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeOutput {
    /// Summary or final message from the AI agent.
    #[serde(default)]
    pub message: String,
    /// Files that were changed during the task.
    #[serde(default)]
    pub files_changed: Vec<String>,
    /// Total tokens used (if reported).
    #[serde(default)]
    pub tokens_used: Option<u64>,
}

/// Execute `opencode run` inside a VM via the guest agent.
///
/// Sends an `Exec` request to run `opencode run "<prompt>" --format json`
/// in the guest. The working directory defaults to `/workspace`.
///
/// # Arguments
/// * `vsock` - Connected vsock client for the target VM
/// * `prompt` - The task prompt to pass to opencode
/// * `timeout_secs` - Maximum execution time in seconds (0 = use default)
/// * `workdir` - Optional working directory (defaults to `/workspace`)
pub async fn run_opencode(
    vsock: &mut VsockClient,
    prompt: &str,
    timeout_secs: u64,
    workdir: Option<&str>,
) -> Result<OpenCodeResult> {
    let timeout = if timeout_secs == 0 {
        DEFAULT_TIMEOUT_SECS
    } else {
        timeout_secs
    };

    let work_dir = workdir.unwrap_or("/workspace");

    debug!(
        prompt_len = prompt.len(),
        timeout_secs = timeout,
        workdir = work_dir,
        "executing opencode run"
    );

    let exec_result = vsock
        .exec(
            "opencode",
            vec![
                "run".to_string(),
                prompt.to_string(),
                "--format".to_string(),
                "json".to_string(),
            ],
            Some(work_dir.to_string()),
            HashMap::new(),
            timeout,
        )
        .await
        .context("failed to execute opencode in guest VM")?;

    let success = exec_result.exit_code == 0;

    // Try to parse structured JSON output from stdout
    let output = if success {
        parse_opencode_output(&exec_result.stdout)
    } else {
        warn!(
            exit_code = exec_result.exit_code,
            stderr_len = exec_result.stderr.len(),
            "opencode run failed"
        );
        None
    };

    Ok(OpenCodeResult {
        success,
        exit_code: exec_result.exit_code,
        stdout: exec_result.stdout,
        stderr: exec_result.stderr,
        output,
    })
}

/// Attempt to parse opencode's JSON output from stdout.
///
/// Returns `None` if the output is not valid JSON or doesn't match
/// the expected schema. This is intentionally lenient -- we capture
/// the raw stdout regardless, so callers can always fall back to that.
fn parse_opencode_output(stdout: &str) -> Option<OpenCodeOutput> {
    // opencode may emit non-JSON lines before the JSON output.
    // Try to find and parse the last JSON object in the output.
    let trimmed = stdout.trim();

    // First, try parsing the entire output as JSON
    if let Ok(output) = serde_json::from_str::<OpenCodeOutput>(trimmed) {
        return Some(output);
    }

    // Fall back: look for the last line that starts with '{'
    for line in trimmed.lines().rev() {
        let line = line.trim();
        if line.starts_with('{') {
            if let Ok(output) = serde_json::from_str::<OpenCodeOutput>(line) {
                return Some(output);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_timeout() {
        assert_eq!(DEFAULT_TIMEOUT_SECS, 300);
    }

    // -----------------------------------------------------------------------
    // OpenCodeResult construction
    // -----------------------------------------------------------------------

    #[test]
    fn test_opencode_result_success() {
        let result = OpenCodeResult {
            success: true,
            exit_code: 0,
            stdout: "task completed".into(),
            stderr: String::new(),
            output: None,
        };
        assert!(result.success);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn test_opencode_result_failure() {
        let result = OpenCodeResult {
            success: false,
            exit_code: 1,
            stdout: String::new(),
            stderr: "opencode: error: API key not set".into(),
            output: None,
        };
        assert!(!result.success);
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("API key"));
    }

    #[test]
    fn test_opencode_result_with_output() {
        let output = OpenCodeOutput {
            message: "Created new file".into(),
            files_changed: vec!["src/main.rs".into(), "Cargo.toml".into()],
            tokens_used: Some(1500),
        };
        let result = OpenCodeResult {
            success: true,
            exit_code: 0,
            stdout: "{}".into(),
            stderr: String::new(),
            output: Some(output),
        };
        assert!(result.output.is_some());
        let out = result.output.unwrap();
        assert_eq!(out.files_changed.len(), 2);
        assert_eq!(out.tokens_used, Some(1500));
    }

    // -----------------------------------------------------------------------
    // OpenCodeResult serialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_opencode_result_serialization_roundtrip() {
        let result = OpenCodeResult {
            success: true,
            exit_code: 0,
            stdout: "hello".into(),
            stderr: String::new(),
            output: Some(OpenCodeOutput {
                message: "done".into(),
                files_changed: vec!["a.rs".into()],
                tokens_used: Some(100),
            }),
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: OpenCodeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.success, result.success);
        assert_eq!(decoded.exit_code, result.exit_code);
        assert_eq!(decoded.stdout, result.stdout);
        assert!(decoded.output.is_some());
        let out = decoded.output.unwrap();
        assert_eq!(out.message, "done");
        assert_eq!(out.files_changed, vec!["a.rs"]);
    }

    // -----------------------------------------------------------------------
    // parse_opencode_output
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_valid_json_output() {
        let json = r#"{"message": "Task completed", "files_changed": ["src/lib.rs"], "tokens_used": 2500}"#;
        let output = parse_opencode_output(json);
        assert!(output.is_some());
        let out = output.unwrap();
        assert_eq!(out.message, "Task completed");
        assert_eq!(out.files_changed, vec!["src/lib.rs"]);
        assert_eq!(out.tokens_used, Some(2500));
    }

    #[test]
    fn test_parse_minimal_json_output() {
        // Only message field, others use defaults
        let json = r#"{"message": "done"}"#;
        let output = parse_opencode_output(json);
        assert!(output.is_some());
        let out = output.unwrap();
        assert_eq!(out.message, "done");
        assert!(out.files_changed.is_empty());
        assert!(out.tokens_used.is_none());
    }

    #[test]
    fn test_parse_empty_json_object() {
        let output = parse_opencode_output("{}");
        assert!(output.is_some());
        let out = output.unwrap();
        assert!(out.message.is_empty());
        assert!(out.files_changed.is_empty());
    }

    #[test]
    fn test_parse_json_with_unknown_fields() {
        // Forward compatibility: unknown fields are ignored
        let json = r#"{"message": "ok", "files_changed": [], "unknown_field": 42, "nested": {"a": 1}}"#;
        let output = parse_opencode_output(json);
        assert!(output.is_some());
        assert_eq!(output.unwrap().message, "ok");
    }

    #[test]
    fn test_parse_json_with_prefix_lines() {
        // opencode may print status lines before the JSON output
        let stdout = "Starting task...\nProcessing...\n{\"message\": \"completed\", \"files_changed\": [\"main.py\"]}";
        let output = parse_opencode_output(stdout);
        assert!(output.is_some());
        let out = output.unwrap();
        assert_eq!(out.message, "completed");
        assert_eq!(out.files_changed, vec!["main.py"]);
    }

    #[test]
    fn test_parse_empty_stdout() {
        let output = parse_opencode_output("");
        assert!(output.is_none());
    }

    #[test]
    fn test_parse_non_json_stdout() {
        let output = parse_opencode_output("this is not json at all");
        assert!(output.is_none());
    }

    #[test]
    fn test_parse_whitespace_only() {
        let output = parse_opencode_output("   \n\n  ");
        assert!(output.is_none());
    }

    #[test]
    fn test_parse_json_with_trailing_whitespace() {
        let json = r#"  {"message": "trimmed"}  "#;
        let output = parse_opencode_output(json);
        assert!(output.is_some());
        assert_eq!(output.unwrap().message, "trimmed");
    }

    #[test]
    fn test_parse_invalid_json_object() {
        // Starts with { but is not valid JSON
        let output = parse_opencode_output("{not valid json}");
        assert!(output.is_none());
    }

    // -----------------------------------------------------------------------
    // OpenCodeOutput defaults
    // -----------------------------------------------------------------------

    #[test]
    fn test_opencode_output_defaults() {
        let out: OpenCodeOutput = serde_json::from_str("{}").unwrap();
        assert!(out.message.is_empty());
        assert!(out.files_changed.is_empty());
        assert!(out.tokens_used.is_none());
    }
}
