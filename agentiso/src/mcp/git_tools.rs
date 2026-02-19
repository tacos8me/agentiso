//! Git tool handlers extracted from tools.rs.
//!
//! Contains parameter structs, helper functions, and implementation logic
//! for the 5 git MCP tools: git_clone, git_status, git_commit, git_push, git_diff.

use rmcp::ErrorData as McpError;
use rmcp::model::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::info;

use super::tools::{shell_escape, AgentisoServer};

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct GitCloneParams {
    /// UUID or name of the workspace
    pub workspace_id: String,
    /// Git repository URL (https:// or git://)
    pub url: String,
    /// Destination path inside the VM (default: /workspace)
    pub path: Option<String>,
    /// Branch to checkout after cloning
    pub branch: Option<String>,
    /// Shallow clone depth (e.g. 1 for latest commit only)
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct GitStatusParams {
    /// UUID or name of the workspace
    pub workspace_id: String,
    /// Path to the git repository inside the VM (default: /workspace)
    pub path: Option<String>,
}

/// Structured git status output parsed from `git status --porcelain=v2 --branch`.
#[derive(Debug, Serialize)]
pub(crate) struct GitStatusInfo {
    pub branch: String,
    pub ahead: i64,
    pub behind: i64,
    pub staged: Vec<String>,
    pub modified: Vec<String>,
    pub untracked: Vec<String>,
    pub conflicted: Vec<String>,
    pub dirty: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct GitCommitParams {
    /// UUID or name of the workspace
    pub workspace_id: String,
    /// Path to git repository (default: /workspace)
    #[serde(default)]
    pub path: Option<String>,
    /// Commit message
    pub message: String,
    /// Stage all changes before committing (git add -A)
    #[serde(default)]
    pub add_all: Option<bool>,
    /// Author name (optional, uses git config default)
    #[serde(default)]
    pub author_name: Option<String>,
    /// Author email (optional, uses git config default)
    #[serde(default)]
    pub author_email: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct GitPushParams {
    /// UUID or name of the workspace
    pub workspace_id: String,
    /// Path to git repository (default: /workspace)
    #[serde(default)]
    pub path: Option<String>,
    /// Remote name (default: origin)
    #[serde(default)]
    pub remote: Option<String>,
    /// Branch to push (default: current branch)
    #[serde(default)]
    pub branch: Option<String>,
    /// Force push
    #[serde(default)]
    pub force: Option<bool>,
    /// Set upstream tracking
    #[serde(default)]
    pub set_upstream: Option<bool>,
    /// Timeout in seconds (default: 120)
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct GitDiffParams {
    /// UUID or name of the workspace
    pub workspace_id: String,
    /// Path to git repository (default: /workspace)
    #[serde(default)]
    pub path: Option<String>,
    /// Show staged changes (git diff --staged)
    #[serde(default)]
    pub staged: Option<bool>,
    /// Show stat summary instead of full diff
    #[serde(default)]
    pub stat: Option<bool>,
    /// Specific file to diff
    #[serde(default)]
    pub file_path: Option<String>,
    /// Maximum output bytes (default: 65536)
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

// ---------------------------------------------------------------------------
// Free helper functions
// ---------------------------------------------------------------------------

pub(crate) fn validate_git_url(url: &str) -> Result<(), McpError> {
    let valid = url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("git://")
        || url.starts_with("ssh://")
        || (url.contains('@') && url.contains(':'));

    if !valid {
        return Err(McpError::invalid_params(
            format!(
                "invalid git URL '{}'. Expected https://, http://, git://, ssh://, \
                 or SCP-style (git@host:path) URL.",
                url
            ),
            None,
        ));
    }

    // Reject URLs with shell metacharacters to prevent command injection
    if url.chars().any(|c| matches!(c, ';' | '|' | '&' | '$' | '`' | '\'' | '"' | '\\' | '\n' | '\r')) {
        return Err(McpError::invalid_params(
            "git URL contains invalid characters".to_string(),
            None,
        ));
    }

    Ok(())
}

/// Redact common credential patterns from git command output.
///
/// Strips GitHub PATs (`ghp_...`, `github_pat_...`), GitLab PATs (`glpat-...`),
/// and lines containing `Authorization:` or `Bearer ` headers.
pub(crate) fn redact_credentials(s: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    static TOKEN_RE: OnceLock<Regex> = OnceLock::new();
    static LINE_RE: OnceLock<Regex> = OnceLock::new();

    let token_re = TOKEN_RE.get_or_init(|| {
        Regex::new(r"ghp_[a-zA-Z0-9]{36,}|github_pat_[a-zA-Z0-9_]{22,}|glpat-[a-zA-Z0-9\-]{20,}")
            .expect("invalid token regex")
    });

    let line_re = LINE_RE.get_or_init(|| {
        Regex::new(r"(?m)^.*(?:Authorization:|Bearer ).*$").expect("invalid line regex")
    });

    let result = token_re.replace_all(s, "[REDACTED]");
    let result = line_re.replace_all(&result, "[REDACTED]");
    result.into_owned()
}

/// Parse `git status --porcelain=v2 --branch` output into structured data.
///
/// Porcelain v2 format:
/// - `# branch.head <name>` -- current branch
/// - `# branch.ab +<ahead> -<behind>` -- ahead/behind counts
/// - `1 <XY> ...` -- tracked file, ordinary change
/// - `2 <XY> ...` -- tracked file, rename/copy
/// - `u <XY> ...` -- unmerged (conflicted) file
/// - `? <path>` -- untracked file
pub(crate) fn parse_git_porcelain_v2(output: &str) -> GitStatusInfo {
    let mut branch = String::from("(unknown)");
    let mut ahead: i64 = 0;
    let mut behind: i64 = 0;
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicted = Vec::new();

    for line in output.lines() {
        if line.starts_with("# branch.head ") {
            branch = line.trim_start_matches("# branch.head ").to_string();
        } else if line.starts_with("# branch.ab ") {
            // Format: "# branch.ab +N -M"
            let ab = line.trim_start_matches("# branch.ab ");
            for token in ab.split_whitespace() {
                if let Some(val) = token.strip_prefix('+') {
                    ahead = val.parse().unwrap_or(0);
                } else if let Some(val) = token.strip_prefix('-') {
                    behind = val.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") {
            // Ordinary change.
            // Format: "1 XY <sub> <mH> <mI> <mW> <hH> <hI> <path>"
            let parts: Vec<&str> = line.splitn(9, ' ').collect();
            if parts.len() >= 9 {
                let xy = parts[1];
                let xy_bytes = xy.as_bytes();
                let file_path = parts[8];

                // X (index/staged status): not '.' means staged
                if !xy_bytes.is_empty() && xy_bytes[0] != b'.' {
                    staged.push(file_path.to_string());
                }
                // Y (worktree status): not '.' means modified in worktree
                if xy_bytes.len() >= 2 && xy_bytes[1] != b'.' {
                    modified.push(file_path.to_string());
                }
            }
        } else if line.starts_with("2 ") {
            // Rename/copy change.
            // Format: "2 XY <sub> <mH> <mI> <mW> <hH> <hI> <X_score> <path>\t<origPath>"
            let parts: Vec<&str> = line.splitn(10, ' ').collect();
            if parts.len() >= 10 {
                let xy = parts[1];
                let xy_bytes = xy.as_bytes();
                // The 10th field is "path\torigPath"
                let file_path = parts[9].split('\t').next().unwrap_or(parts[9]);

                // X (index/staged status): not '.' means staged
                if !xy_bytes.is_empty() && xy_bytes[0] != b'.' {
                    staged.push(file_path.to_string());
                }
                // Y (worktree status): not '.' means modified in worktree
                if xy_bytes.len() >= 2 && xy_bytes[1] != b'.' {
                    modified.push(file_path.to_string());
                }
            }
        } else if line.starts_with("u ") {
            // Unmerged (conflicted) file.
            // Format: "u XY <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>"
            let parts: Vec<&str> = line.splitn(11, ' ').collect();
            if let Some(path) = parts.last() {
                conflicted.push(path.to_string());
            }
        } else if line.starts_with("? ") {
            // Untracked file
            let path = line.trim_start_matches("? ");
            untracked.push(path.to_string());
        }
    }

    let dirty = !staged.is_empty() || !modified.is_empty() || !untracked.is_empty() || !conflicted.is_empty();

    GitStatusInfo {
        branch,
        ahead,
        behind,
        staged,
        modified,
        untracked,
        conflicted,
        dirty,
    }
}

// ---------------------------------------------------------------------------
// Handler implementations
// ---------------------------------------------------------------------------

impl AgentisoServer {
    pub(crate) async fn handle_git_clone(
        &self,
        params: GitCloneParams,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        info!(workspace_id = %ws_id, tool = "git_clone", url = %params.url, "tool call");
        self.check_ownership(ws_id).await?;

        validate_git_url(&params.url)?;

        let dest = params.path.as_deref().unwrap_or("/workspace");

        // Build the git clone command
        let mut cmd = format!("git clone");
        if let Some(depth) = params.depth {
            cmd.push_str(&format!(" --depth {}", depth));
        }
        if let Some(ref branch) = params.branch {
            cmd.push_str(&format!(" --branch {}", shell_escape(branch)));
        }
        cmd.push_str(&format!(" {} {}", shell_escape(&params.url), shell_escape(dest)));

        // Execute git clone with a generous timeout (clones can be slow).
        let result = self
            .workspace_manager
            .exec(ws_id, &cmd, None, None, Some(300))
            .await
            .map_err(|e| {
                let msg = format!("{:#}", e);
                if msg.contains("timed out") || msg.contains("Timeout") {
                    McpError::invalid_request(
                        "git clone timed out after 300s. The repository may be very large; \
                         try using depth=1 for a shallow clone, or use exec_background for manual control."
                            .to_string(),
                        None,
                    )
                } else {
                    McpError::invalid_request(msg, None)
                }
            })?;

        if result.exit_code != 0 {
            let error_detail = if !result.stderr.is_empty() {
                &result.stderr
            } else {
                &result.stdout
            };
            return Err(McpError::invalid_request(
                format!(
                    "git clone failed (exit code {}): {}",
                    result.exit_code,
                    error_detail.trim()
                ),
                None,
            ));
        }

        // Get the HEAD commit SHA
        let sha_result = self
            .workspace_manager
            .exec(
                ws_id,
                &format!("git -C {} rev-parse HEAD", shell_escape(dest)),
                None,
                None,
                Some(10),
            )
            .await
            .ok();

        let commit_sha = sha_result
            .as_ref()
            .filter(|r| r.exit_code == 0)
            .map(|r| r.stdout.trim().to_string())
            .unwrap_or_default();

        let info = serde_json::json!({
            "success": true,
            "path": dest,
            "commit_sha": commit_sha,
            "url": params.url,
            "branch": params.branch,
            "depth": params.depth,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    pub(crate) async fn handle_git_status(
        &self,
        params: GitStatusParams,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        let path = params.path.as_deref().unwrap_or("/workspace");
        info!(workspace_id = %ws_id, tool = "git_status", path = %path, "tool call");
        self.check_ownership(ws_id).await?;

        let cmd = format!(
            "git -C {} status --porcelain=v2 --branch 2>&1",
            shell_escape(path)
        );

        let result = self
            .workspace_manager
            .exec(ws_id, &cmd, None, None, Some(30))
            .await
            .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

        if result.exit_code != 0 {
            // Check if git is not initialized
            let output = if !result.stderr.is_empty() {
                &result.stderr
            } else {
                &result.stdout
            };
            if output.contains("not a git repository") {
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "error": "not_a_git_repository",
                        "message": format!("'{}' is not a git repository. Run 'git init' or 'git clone' first.", path),
                    }))
                    .unwrap(),
                )]));
            }
            return Err(McpError::invalid_request(
                format!("git status failed (exit code {}): {}", result.exit_code, output.trim()),
                None,
            ));
        }

        let status = parse_git_porcelain_v2(&result.stdout);

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&status).unwrap(),
        )]))
    }

    pub(crate) async fn handle_git_commit(
        &self,
        params: GitCommitParams,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        let path = params.path.as_deref().unwrap_or("/workspace");
        info!(workspace_id = %ws_id, tool = "git_commit", path = %path, "tool call");
        self.check_ownership(ws_id).await?;

        // Stage all changes if requested (default: true)
        let add_all = params.add_all.unwrap_or(true);
        if add_all {
            let add_cmd = format!("git -C {} add -A", shell_escape(path));
            let add_result = self
                .workspace_manager
                .exec(ws_id, &add_cmd, None, None, Some(30))
                .await
                .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

            if add_result.exit_code != 0 {
                let error_detail = if !add_result.stderr.is_empty() {
                    &add_result.stderr
                } else {
                    &add_result.stdout
                };
                return Err(McpError::invalid_request(
                    format!(
                        "git add -A failed (exit code {}): {}",
                        add_result.exit_code,
                        error_detail.trim()
                    ),
                    None,
                ));
            }
        }

        // Build the git commit command
        let mut cmd = format!("git -C {}", shell_escape(path));
        if let (Some(ref name), Some(ref email)) = (&params.author_name, &params.author_email) {
            cmd.push_str(&format!(
                " -c user.name={} -c user.email={}",
                shell_escape(name),
                shell_escape(email)
            ));
        }
        cmd.push_str(&format!(" commit -m {}", shell_escape(&params.message)));
        if let (Some(ref name), Some(ref email)) = (&params.author_name, &params.author_email) {
            cmd.push_str(&format!(
                " --author={}",
                shell_escape(&format!("{} <{}>", name, email))
            ));
        }

        let commit_result = self
            .workspace_manager
            .exec(ws_id, &cmd, None, None, Some(30))
            .await
            .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

        // Handle "nothing to commit" as a non-error info response
        if commit_result.exit_code != 0 {
            let output = format!("{}\n{}", commit_result.stdout, commit_result.stderr);
            if output.contains("nothing to commit") {
                let info = serde_json::json!({
                    "status": "nothing_to_commit",
                    "message": "Nothing to commit, working tree clean.",
                });
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&info).unwrap(),
                )]));
            }
            let error_detail = if !commit_result.stderr.is_empty() {
                &commit_result.stderr
            } else {
                &commit_result.stdout
            };
            return Err(McpError::invalid_request(
                format!(
                    "git commit failed (exit code {}): {}",
                    commit_result.exit_code,
                    error_detail.trim()
                ),
                None,
            ));
        }

        // Get the full commit SHA
        let sha_result = self
            .workspace_manager
            .exec(
                ws_id,
                &format!("git -C {} rev-parse HEAD", shell_escape(path)),
                None,
                None,
                Some(10),
            )
            .await
            .ok();

        let commit_sha = sha_result
            .as_ref()
            .filter(|r| r.exit_code == 0)
            .map(|r| r.stdout.trim().to_string())
            .unwrap_or_default();

        let short_sha = if commit_sha.len() >= 7 {
            commit_sha[..7].to_string()
        } else {
            commit_sha.clone()
        };

        // Get the summary line
        let summary_result = self
            .workspace_manager
            .exec(
                ws_id,
                &format!(
                    "git -C {} log -1 --format='%h %s'",
                    shell_escape(path)
                ),
                None,
                None,
                Some(10),
            )
            .await
            .ok();

        let summary = summary_result
            .as_ref()
            .filter(|r| r.exit_code == 0)
            .map(|r| r.stdout.trim().to_string())
            .unwrap_or_default();

        // Get current branch
        let branch_result = self
            .workspace_manager
            .exec(
                ws_id,
                &format!("git -C {} branch --show-current", shell_escape(path)),
                None,
                None,
                Some(10),
            )
            .await
            .ok();

        let branch = branch_result
            .as_ref()
            .filter(|r| r.exit_code == 0)
            .map(|r| {
                let b = r.stdout.trim().to_string();
                if b.is_empty() {
                    "(detached HEAD)".to_string()
                } else {
                    b
                }
            })
            .unwrap_or_else(|| "(unknown)".to_string());

        let info = serde_json::json!({
            "commit_sha": commit_sha,
            "short_sha": short_sha,
            "branch": branch,
            "summary": summary,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    pub(crate) async fn handle_git_push(
        &self,
        params: GitPushParams,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        let path = params.path.as_deref().unwrap_or("/workspace");
        let remote = params.remote.as_deref().unwrap_or("origin");
        let timeout = params.timeout_secs.unwrap_or(120);
        info!(workspace_id = %ws_id, tool = "git_push", path = %path, remote = %remote, "tool call");
        self.check_ownership(ws_id).await?;

        // Detect current branch if not specified
        let branch = if let Some(ref b) = params.branch {
            b.clone()
        } else {
            let branch_result = self
                .workspace_manager
                .exec(
                    ws_id,
                    &format!("git -C {} branch --show-current", shell_escape(path)),
                    None,
                    None,
                    Some(10),
                )
                .await
                .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

            if branch_result.exit_code != 0 || branch_result.stdout.trim().is_empty() {
                return Err(McpError::invalid_request(
                    "Could not detect current branch. You may be in a detached HEAD state. \
                     Specify the branch explicitly with the 'branch' parameter."
                        .to_string(),
                    None,
                ));
            }
            branch_result.stdout.trim().to_string()
        };

        // Build the git push command
        let mut cmd = format!("git -C {}", shell_escape(path));
        cmd.push_str(" push");
        if params.force.unwrap_or(false) {
            cmd.push_str(" --force");
        }
        if params.set_upstream.unwrap_or(false) {
            cmd.push_str(" -u");
        }
        cmd.push_str(&format!(" {} {}", shell_escape(remote), shell_escape(&branch)));

        let push_result = self
            .workspace_manager
            .exec(ws_id, &cmd, None, None, Some(timeout))
            .await
            .map_err(|e| {
                let msg = format!("{:#}", e);
                if msg.contains("timed out") || msg.contains("Timeout") {
                    McpError::invalid_request(
                        format!(
                            "git push timed out after {}s. Try increasing timeout_secs.",
                            timeout
                        ),
                        None,
                    )
                } else {
                    McpError::invalid_request(msg, None)
                }
            })?;

        let output = redact_credentials(&format!("{}\n{}", push_result.stdout, push_result.stderr));

        if push_result.exit_code != 0 {
            let error_detail = output.trim();
            // Provide helpful suggestions based on the error
            let suggestion = if error_detail.contains("Authentication")
                || error_detail.contains("authentication")
                || error_detail.contains("could not read Username")
                || error_detail.contains("terminal prompts disabled")
            {
                " Suggestion: use set_env to configure git credentials \
                 (e.g. GIT_ASKPASS, GITHUB_TOKEN) or inject an SSH key."
            } else if error_detail.contains("No configured push destination")
                || error_detail.contains("No such remote")
            {
                " Suggestion: configure a remote first with \
                 exec(command=\"git -C /workspace remote add origin <url>\")."
            } else if error_detail.contains("non-fast-forward")
                || error_detail.contains("rejected")
                || error_detail.contains("fetch first")
            {
                " Suggestion: pull/merge first, or use force=true to force push."
            } else {
                ""
            };

            return Err(McpError::invalid_request(
                format!(
                    "git push failed (exit code {}): {}.{}",
                    push_result.exit_code,
                    error_detail,
                    suggestion,
                ),
                None,
            ));
        }

        let info = serde_json::json!({
            "remote": remote,
            "branch": branch,
            "success": true,
            "output": output.trim(),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }

    pub(crate) async fn handle_git_diff(
        &self,
        params: GitDiffParams,
    ) -> Result<CallToolResult, McpError> {
        let ws_id = self.resolve_workspace_id(&params.workspace_id).await?;
        let path = params.path.as_deref().unwrap_or("/workspace");
        let max_bytes = params.max_bytes.unwrap_or(65536);
        info!(workspace_id = %ws_id, tool = "git_diff", path = %path, "tool call");
        self.check_ownership(ws_id).await?;

        // Build the main git diff command
        let mut cmd = format!("git -C {} diff", shell_escape(path));
        if params.staged.unwrap_or(false) {
            cmd.push_str(" --staged");
        }
        // If stat-only mode, append --stat to the main command
        let stat_only = params.stat.unwrap_or(false);
        if stat_only {
            cmd.push_str(" --stat");
        }
        if let Some(ref fp) = params.file_path {
            cmd.push_str(&format!(" -- {}", shell_escape(fp)));
        }

        let diff_result = self
            .workspace_manager
            .exec(ws_id, &cmd, None, None, Some(30))
            .await
            .map_err(|e| McpError::invalid_request(format!("{:#}", e), None))?;

        if diff_result.exit_code != 0 {
            let error_detail = if !diff_result.stderr.is_empty() {
                &diff_result.stderr
            } else {
                &diff_result.stdout
            };
            return Err(McpError::invalid_request(
                format!(
                    "git diff failed (exit code {}): {}",
                    diff_result.exit_code,
                    error_detail.trim()
                ),
                None,
            ));
        }

        // Truncate if needed
        let diff_output = &diff_result.stdout;
        let truncated = diff_output.len() > max_bytes;
        let diff_text = if truncated {
            format!("{}[TRUNCATED]", &diff_output[..max_bytes])
        } else {
            diff_output.to_string()
        };

        // If not stat-only, optionally also fetch the stat summary
        let stat_summary = if !stat_only {
            let mut stat_cmd = format!("git -C {} diff", shell_escape(path));
            if params.staged.unwrap_or(false) {
                stat_cmd.push_str(" --staged");
            }
            stat_cmd.push_str(" --stat");
            if let Some(ref fp) = params.file_path {
                stat_cmd.push_str(&format!(" -- {}", shell_escape(fp)));
            }

            let stat_result = self
                .workspace_manager
                .exec(ws_id, &stat_cmd, None, None, Some(30))
                .await
                .ok();

            stat_result
                .as_ref()
                .filter(|r| r.exit_code == 0)
                .map(|r| r.stdout.trim().to_string())
        } else {
            None
        };

        let mut info = serde_json::json!({
            "diff": diff_text,
            "truncated": truncated,
        });

        if let Some(ref stat) = stat_summary {
            info["stat"] = serde_json::Value::String(stat.clone());
        } else if stat_only {
            // In stat-only mode the main diff output IS the stat
            info["stat"] = serde_json::Value::String(diff_text.clone());
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&info).unwrap(),
        )]))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Git URL validation tests ---

    #[test]
    fn test_validate_git_url_https() {
        assert!(validate_git_url("https://github.com/user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_http() {
        assert!(validate_git_url("http://github.com/user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_ssh() {
        assert!(validate_git_url("ssh://git@github.com/user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_scp_style() {
        assert!(validate_git_url("git@github.com:user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_git_protocol() {
        assert!(validate_git_url("git://github.com/user/repo.git").is_ok());
    }

    #[test]
    fn test_validate_git_url_invalid() {
        assert!(validate_git_url("not-a-url").is_err());
        assert!(validate_git_url("/local/path").is_err());
        assert!(validate_git_url("ftp://host/repo").is_err());
    }

    #[test]
    fn test_validate_git_url_rejects_injection() {
        assert!(validate_git_url("https://github.com/repo; rm -rf /").is_err());
        assert!(validate_git_url("https://github.com/repo|cat /etc/passwd").is_err());
        assert!(validate_git_url("https://github.com/repo$(whoami)").is_err());
    }

    // --- Git clone params tests ---

    #[test]
    fn test_git_clone_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "url": "https://github.com/user/repo.git",
            "path": "/home/user/project",
            "branch": "main",
            "depth": 1
        });
        let params: GitCloneParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.url, "https://github.com/user/repo.git");
        assert_eq!(params.path.as_deref(), Some("/home/user/project"));
        assert_eq!(params.branch.as_deref(), Some("main"));
        assert_eq!(params.depth, Some(1));
    }

    #[test]
    fn test_git_clone_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "url": "https://github.com/user/repo.git"
        });
        let params: GitCloneParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.url, "https://github.com/user/repo.git");
        assert!(params.path.is_none());
        assert!(params.branch.is_none());
        assert!(params.depth.is_none());
    }

    #[test]
    fn test_git_clone_params_missing_required() {
        // Missing url
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<GitCloneParams>(json).is_err());

        // Missing workspace_id
        let json = serde_json::json!({
            "url": "https://github.com/user/repo.git"
        });
        assert!(serde_json::from_value::<GitCloneParams>(json).is_err());
    }

    // --- Git status params tests ---

    #[test]
    fn test_git_status_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/home/user/project"
        });
        let params: GitStatusParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.path.as_deref(), Some("/home/user/project"));
    }

    #[test]
    fn test_git_status_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: GitStatusParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert!(params.path.is_none());
    }

    #[test]
    fn test_git_status_params_missing_required() {
        let json = serde_json::json!({});
        assert!(serde_json::from_value::<GitStatusParams>(json).is_err());
    }

    // --- Git porcelain v2 parser tests ---

    #[test]
    fn test_parse_git_porcelain_v2_clean() {
        let output = "# branch.oid abc123def456\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.branch, "main");
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
        assert!(status.staged.is_empty());
        assert!(status.modified.is_empty());
        assert!(status.untracked.is_empty());
        assert!(status.conflicted.is_empty());
        assert!(!status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_with_changes() {
        let output = "\
# branch.oid abc123def456
# branch.head feature-branch
# branch.upstream origin/feature-branch
# branch.ab +3 -1
1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 abc123def456abc123def456abc123def456abc12345 README.md
1 .M N... 100644 100644 100644 abc123def456abc123def456abc123def456abc12345 abc123def456abc123def456abc123def456abc12345 src/main.rs
? test.log
";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.branch, "feature-branch");
        assert_eq!(status.ahead, 3);
        assert_eq!(status.behind, 1);
        assert_eq!(status.staged, vec!["README.md"]);
        assert_eq!(status.modified, vec!["src/main.rs"]);
        assert_eq!(status.untracked, vec!["test.log"]);
        assert!(status.conflicted.is_empty());
        assert!(status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_untracked_only() {
        let output = "# branch.head main\n? newfile.txt\n? another.txt\n";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.branch, "main");
        assert_eq!(status.untracked, vec!["newfile.txt", "another.txt"]);
        assert!(status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_conflicted() {
        let output = "\
# branch.head main
u UU N... 100644 100644 100644 100644 abc123 def456 789abc conflicted-file.rs
";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.conflicted, vec!["conflicted-file.rs"]);
        assert!(status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_rename() {
        let output = "\
# branch.head main
2 R. N... 100644 100644 100644 abc123 def456 R100 new-name.rs\told-name.rs
";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.staged, vec!["new-name.rs"]);
        assert!(status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_detached_head() {
        let output = "# branch.oid abc123\n# branch.head (detached)\n";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.branch, "(detached)");
        assert!(!status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_empty_output() {
        let status = parse_git_porcelain_v2("");
        assert_eq!(status.branch, "(unknown)");
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
        assert!(!status.dirty);
    }

    #[test]
    fn test_parse_git_porcelain_v2_staged_and_modified() {
        // A file that is both staged and has worktree modifications
        let output = "\
# branch.head main
1 MM N... 100644 100644 100644 abc123 def456 both-changed.rs
";
        let status = parse_git_porcelain_v2(output);
        assert_eq!(status.staged, vec!["both-changed.rs"]);
        assert_eq!(status.modified, vec!["both-changed.rs"]);
        assert!(status.dirty);
    }

    // --- git_commit param tests ---

    #[test]
    fn test_git_commit_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/home/user/project",
            "message": "feat: add new feature",
            "add_all": false,
            "author_name": "Test User",
            "author_email": "test@example.com"
        });
        let params: GitCommitParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.path.as_deref(), Some("/home/user/project"));
        assert_eq!(params.message, "feat: add new feature");
        assert_eq!(params.add_all, Some(false));
        assert_eq!(params.author_name.as_deref(), Some("Test User"));
        assert_eq!(params.author_email.as_deref(), Some("test@example.com"));
    }

    #[test]
    fn test_git_commit_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "message": "fix: resolve bug"
        });
        let params: GitCommitParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.message, "fix: resolve bug");
        assert!(params.path.is_none());
        assert!(params.add_all.is_none());
        assert!(params.author_name.is_none());
        assert!(params.author_email.is_none());
    }

    #[test]
    fn test_git_commit_params_missing_required() {
        // Missing message
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<GitCommitParams>(json).is_err());

        // Missing workspace_id
        let json = serde_json::json!({
            "message": "some commit"
        });
        assert!(serde_json::from_value::<GitCommitParams>(json).is_err());
    }

    // --- git_push param tests ---

    #[test]
    fn test_git_push_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/home/user/project",
            "remote": "upstream",
            "branch": "feature-branch",
            "force": true,
            "set_upstream": true,
            "timeout_secs": 300
        });
        let params: GitPushParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.path.as_deref(), Some("/home/user/project"));
        assert_eq!(params.remote.as_deref(), Some("upstream"));
        assert_eq!(params.branch.as_deref(), Some("feature-branch"));
        assert_eq!(params.force, Some(true));
        assert_eq!(params.set_upstream, Some(true));
        assert_eq!(params.timeout_secs, Some(300));
    }

    #[test]
    fn test_git_push_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: GitPushParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert!(params.path.is_none());
        assert!(params.remote.is_none());
        assert!(params.branch.is_none());
        assert!(params.force.is_none());
        assert!(params.set_upstream.is_none());
        assert!(params.timeout_secs.is_none());
    }

    #[test]
    fn test_git_push_params_missing_required() {
        // Missing workspace_id
        let json = serde_json::json!({
            "remote": "origin",
            "branch": "main"
        });
        assert!(serde_json::from_value::<GitPushParams>(json).is_err());
    }

    // --- git_diff param tests ---

    #[test]
    fn test_git_diff_params_full() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000",
            "path": "/home/user/project",
            "staged": true,
            "stat": true,
            "file_path": "src/main.rs",
            "max_bytes": 32768
        });
        let params: GitDiffParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(params.path.as_deref(), Some("/home/user/project"));
        assert_eq!(params.staged, Some(true));
        assert_eq!(params.stat, Some(true));
        assert_eq!(params.file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(params.max_bytes, Some(32768));
    }

    #[test]
    fn test_git_diff_params_minimal() {
        let json = serde_json::json!({
            "workspace_id": "550e8400-e29b-41d4-a716-446655440000"
        });
        let params: GitDiffParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.workspace_id, "550e8400-e29b-41d4-a716-446655440000");
        assert!(params.path.is_none());
        assert!(params.staged.is_none());
        assert!(params.stat.is_none());
        assert!(params.file_path.is_none());
        assert!(params.max_bytes.is_none());
    }

    #[test]
    fn test_git_diff_params_missing_required() {
        // Missing workspace_id
        let json = serde_json::json!({
            "staged": true,
            "stat": false
        });
        assert!(serde_json::from_value::<GitDiffParams>(json).is_err());
    }
}
