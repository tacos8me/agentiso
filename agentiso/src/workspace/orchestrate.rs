//! Orchestration engine for batch AI coding tasks across parallel VMs.
//!
//! Reads a TOML task file, forks worker VMs from a golden snapshot,
//! injects API keys via SetEnv, runs `opencode run` in each, and
//! collects results.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

use crate::mcp::vault::VaultManager;
use crate::vm::opencode::OpenCodeResult;
use crate::workspace::WorkspaceManager;

// ---------------------------------------------------------------------------
// Plan types (parsed from TOML)
// ---------------------------------------------------------------------------

/// An orchestration plan parsed from a TOML task file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationPlan {
    /// Name or UUID of the golden workspace to fork from.
    pub golden_workspace: String,
    /// Snapshot name to fork from (default: "golden").
    #[serde(default = "default_snapshot_name")]
    pub snapshot_name: String,
    /// List of tasks to execute in parallel workers.
    pub tasks: Vec<TaskDef>,
}

/// A single task definition within an orchestration plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDef {
    /// Human-readable task name (used for worker naming and result files).
    pub name: String,
    /// The prompt to pass to `opencode run`.
    pub prompt: String,
    /// Working directory inside the VM (default: /workspace).
    pub workdir: Option<String>,
    /// Optional vault queries to resolve and inject as context into the prompt.
    pub vault_context: Option<Vec<VaultQuery>>,
}

/// A vault query to resolve before dispatching a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultQuery {
    /// Search query string or note path.
    pub query: String,
    /// "search" (full-text search) or "read" (read specific note).
    #[serde(default = "default_vault_query_kind")]
    pub kind: String,
}

fn default_vault_query_kind() -> String {
    "search".to_string()
}

fn default_snapshot_name() -> String {
    "golden".to_string()
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Aggregated results from an orchestration run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationResult {
    /// Per-task results.
    pub tasks: Vec<TaskResult>,
    /// Number of tasks that succeeded.
    pub success_count: usize,
    /// Number of tasks that failed.
    pub failure_count: usize,
}

/// Result of a single task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// Task name from the plan.
    pub name: String,
    /// Whether the task completed successfully.
    pub success: bool,
    /// Process exit code (or -1 if the task never ran).
    pub exit_code: i32,
    /// Standard output from opencode.
    pub stdout: String,
    /// Standard error output.
    pub stderr: String,
    /// Workspace ID of the worker VM (for debugging).
    pub workspace_id: String,
}

// ---------------------------------------------------------------------------
// Plan loading
// ---------------------------------------------------------------------------

/// Load and validate an orchestration plan from a TOML file.
pub fn load_plan(path: &Path) -> Result<OrchestrationPlan> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read task file: {}", path.display()))?;

    let plan: OrchestrationPlan = toml::from_str(&content)
        .with_context(|| format!("failed to parse task file: {}", path.display()))?;

    validate_plan(&plan)?;
    Ok(plan)
}

/// Validate an orchestration plan.
fn validate_plan(plan: &OrchestrationPlan) -> Result<()> {
    if plan.golden_workspace.is_empty() {
        bail!("golden_workspace must not be empty");
    }
    if plan.tasks.is_empty() {
        bail!("tasks list must not be empty");
    }
    if plan.tasks.len() > 20 {
        bail!(
            "too many tasks: {} (maximum 20, limited by batch_fork)",
            plan.tasks.len()
        );
    }

    // Check for duplicate task names
    let mut seen = std::collections::HashSet::new();
    for task in &plan.tasks {
        if task.name.is_empty() {
            bail!("task name must not be empty");
        }
        if task.prompt.is_empty() {
            bail!("task '{}' has an empty prompt", task.name);
        }
        if !seen.insert(&task.name) {
            bail!("duplicate task name: '{}'", task.name);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Vault context resolution
// ---------------------------------------------------------------------------

/// Resolve vault queries into a formatted context string.
///
/// For "search" queries, runs a full-text search and formats the top results.
/// For "read" queries, reads the full note content.
/// Individual query failures are logged and skipped.
async fn resolve_vault_context(vault: &VaultManager, queries: &[VaultQuery]) -> String {
    let mut sections = Vec::new();

    for q in queries {
        match q.kind.as_str() {
            "search" => {
                match vault.search(&q.query, false, None, None, 5).await {
                    Ok(results) if !results.is_empty() => {
                        let mut section =
                            format!("### Search: \"{}\"\n", q.query);
                        for hit in &results {
                            section.push_str(&format!(
                                "\n**{}** (line {}):\n```\n{}\n```\n",
                                hit.path, hit.line_number, hit.context
                            ));
                        }
                        sections.push(section);
                    }
                    Ok(_) => {
                        warn!(query = %q.query, "vault search returned no results");
                    }
                    Err(e) => {
                        warn!(query = %q.query, error = %e, "vault search failed, skipping");
                    }
                }
            }
            "read" => {
                match vault.read_note(&q.query).await {
                    Ok(note) => {
                        sections.push(format!(
                            "### {}\n\n{}\n",
                            note.path, note.content
                        ));
                    }
                    Err(e) => {
                        warn!(path = %q.query, error = %e, "vault read failed, skipping");
                    }
                }
            }
            other => {
                warn!(kind = %other, query = %q.query, "unknown vault_context kind, skipping");
            }
        }
    }

    if sections.is_empty() {
        return String::new();
    }

    format!("## Project Knowledge Base\n\n{}", sections.join("\n"))
}

// ---------------------------------------------------------------------------
// Execution engine
// ---------------------------------------------------------------------------

/// Aggregated results from an orchestration run, including worker IDs for cleanup.
#[derive(Debug)]
pub struct ExecuteOutcome {
    /// The orchestration results.
    pub result: OrchestrationResult,
    /// Worker workspace IDs created during execution (for external cleanup if needed).
    #[allow(dead_code)]
    pub worker_ids: Vec<Uuid>,
}

/// Execute an orchestration plan.
///
/// Forks worker VMs from the golden snapshot, injects API keys,
/// runs `opencode run` in each, and collects results.
///
/// If a `vault` manager is provided, `vault_context` queries in each task
/// will be resolved and appended to the worker prompt.
///
/// Returns an `ExecuteOutcome` containing the results and worker IDs.
/// Worker VMs are destroyed before returning; the worker IDs are provided
/// so callers (e.g. SIGINT handlers) can also attempt cleanup.
pub async fn execute(
    manager: Arc<WorkspaceManager>,
    plan: &OrchestrationPlan,
    api_key: &str,
    max_parallel: usize,
    vault: Option<Arc<VaultManager>>,
) -> Result<ExecuteOutcome> {
    let task_count = plan.tasks.len();
    info!(
        golden = %plan.golden_workspace,
        snapshot = %plan.snapshot_name,
        tasks = task_count,
        max_parallel,
        "starting orchestration"
    );

    // Resolve the golden workspace ID
    let golden_id = manager
        .find_by_name(&plan.golden_workspace)
        .await
        .or_else(|| Uuid::parse_str(&plan.golden_workspace).ok())
        .with_context(|| {
            format!(
                "golden workspace '{}' not found (not a valid UUID or name)",
                plan.golden_workspace
            )
        })?;

    // Verify the snapshot exists
    let golden_ws = manager.get(golden_id).await?;
    golden_ws
        .snapshots
        .get_by_name(&plan.snapshot_name)
        .with_context(|| {
            format!(
                "snapshot '{}' not found on workspace '{}'",
                plan.snapshot_name, plan.golden_workspace
            )
        })?;

    // Create the semaphore once, used for both fork and execution phases
    let semaphore = Arc::new(Semaphore::new(max_parallel));

    // Fork worker VMs (rate-limited by semaphore)
    info!(count = task_count, "forking worker VMs");
    let mut workers: Vec<(usize, String, Uuid)> = Vec::with_capacity(task_count);
    let mut fork_errors: HashMap<usize, String> = HashMap::new();

    let mut join_set = tokio::task::JoinSet::new();
    for (i, task) in plan.tasks.iter().enumerate() {
        let mgr = manager.clone();
        let snap = plan.snapshot_name.clone();
        let name = format!("orch-{}", task.name);
        let sem = semaphore.clone();
        join_set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = mgr.fork(golden_id, &snap, Some(name.clone())).await;
            (i, name, result)
        });
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok((idx, name, Ok(ws))) => {
                workers.push((idx, name, ws.id));
            }
            Ok((idx, name, Err(e))) => {
                let msg = format!("fork '{}' failed: {:#}", name, e);
                fork_errors.insert(idx, msg);
            }
            Err(e) => {
                fork_errors.insert(usize::MAX, format!("fork task panicked: {}", e));
            }
        }
    }

    if !fork_errors.is_empty() {
        warn!(
            errors = fork_errors.len(),
            "some forks failed: {:?}", fork_errors.values().collect::<Vec<_>>()
        );
    }

    // Build task -> workspace mapping using the task index carried through the JoinSet
    let task_workspace_map: Vec<(usize, Uuid)> = workers
        .iter()
        .map(|(idx, _name, ws_id)| (*idx, *ws_id))
        .collect();

    // Resolve vault context for all tasks concurrently (before spawning workers)
    let mut resolved_prompts: HashMap<usize, String> = HashMap::new();
    if let Some(ref v) = vault {
        resolved_prompts = resolve_all_vault_contexts(v.clone(), &plan.tasks).await;
    }

    // Execute tasks in parallel with semaphore
    let mut exec_set = tokio::task::JoinSet::new();

    for (task_idx, ws_id) in task_workspace_map {
        let mut task_def = plan.tasks[task_idx].clone();
        // Replace prompt with vault-enriched version if available
        if let Some(enriched) = resolved_prompts.remove(&task_idx) {
            task_def.prompt = enriched;
        }
        let mgr = manager.clone();
        let sem = semaphore.clone();
        let key = api_key.to_string();

        exec_set.spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = execute_task(&mgr, ws_id, &task_def, &key).await;
            (task_def.name.clone(), ws_id, result)
        });
    }

    let mut results: Vec<TaskResult> = Vec::new();

    while let Some(result) = exec_set.join_next().await {
        match result {
            Ok((name, ws_id, Ok(oc_result))) => {
                results.push(TaskResult {
                    name,
                    success: oc_result.success,
                    exit_code: oc_result.exit_code,
                    stdout: oc_result.stdout,
                    stderr: oc_result.stderr,
                    workspace_id: ws_id.to_string(),
                });
            }
            Ok((name, ws_id, Err(e))) => {
                results.push(TaskResult {
                    name,
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("{:#}", e),
                    workspace_id: ws_id.to_string(),
                });
            }
            Err(e) => {
                results.push(TaskResult {
                    name: "unknown".to_string(),
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("task panicked: {}", e),
                    workspace_id: String::new(),
                });
            }
        }
    }

    // Add results for tasks that never got a worker (fork failures), with specific errors
    for (i, task) in plan.tasks.iter().enumerate() {
        if !workers.iter().any(|(idx, _, _)| *idx == i) {
            let stderr = fork_errors
                .get(&i)
                .map(|e| format!("worker VM fork failed: {}", e))
                .unwrap_or_else(|| "worker VM fork failed".to_string());
            results.push(TaskResult {
                name: task.name.clone(),
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr,
                workspace_id: String::new(),
            });
        }
    }

    // Sort results by task name for stable output
    results.sort_by(|a, b| a.name.cmp(&b.name));

    // Collect worker IDs before destroying (for the return value)
    let worker_ids: Vec<Uuid> = workers.iter().map(|(_, _, ws_id)| *ws_id).collect();

    // Destroy all worker VMs
    info!(count = workers.len(), "destroying worker VMs");
    for (_, _, ws_id) in &workers {
        if let Err(e) = manager.destroy(*ws_id).await {
            warn!(workspace_id = %ws_id, error = %e, "failed to destroy worker VM");
        }
    }

    let success_count = results.iter().filter(|r| r.success).count();
    let failure_count = results.len() - success_count;

    info!(success = success_count, failed = failure_count, "orchestration complete");

    Ok(ExecuteOutcome {
        result: OrchestrationResult {
            tasks: results,
            success_count,
            failure_count,
        },
        worker_ids,
    })
}

/// Resolve vault contexts for all tasks concurrently, maintaining order.
async fn resolve_all_vault_contexts(
    vault: Arc<VaultManager>,
    tasks: &[TaskDef],
) -> HashMap<usize, String> {
    let mut vault_set = tokio::task::JoinSet::new();

    for (i, task) in tasks.iter().enumerate() {
        if let Some(queries) = &task.vault_context {
            if !queries.is_empty() {
                let queries = queries.clone();
                let prompt = task.prompt.clone();
                let v = vault.clone();
                vault_set.spawn(async move {
                    let context = resolve_vault_context(&v, &queries).await;
                    if context.is_empty() {
                        None
                    } else {
                        Some((i, format!("{}\n\n{}", prompt, context)))
                    }
                });
            }
        }
    }

    let mut resolved = HashMap::new();
    while let Some(result) = vault_set.join_next().await {
        if let Ok(Some((idx, enriched))) = result {
            resolved.insert(idx, enriched);
        }
    }
    resolved
}

/// Execute a single task in a worker VM.
async fn execute_task(
    manager: &WorkspaceManager,
    workspace_id: Uuid,
    task: &TaskDef,
    api_key: &str,
) -> Result<OpenCodeResult> {
    info!(task = %task.name, workspace_id = %workspace_id, "executing task");

    // Inject API key via SetEnv
    let mut env_vars = HashMap::new();
    env_vars.insert("ANTHROPIC_API_KEY".to_string(), api_key.to_string());

    manager
        .set_env(workspace_id, env_vars)
        .await
        .context("failed to inject API key")?;

    // Run opencode
    manager
        .run_opencode(workspace_id, &task.prompt, 0, task.workdir.as_deref())
        .await
        .with_context(|| format!("opencode run failed for task '{}'", task.name))
}

// ---------------------------------------------------------------------------
// Result saving
// ---------------------------------------------------------------------------

/// Save orchestration results to a directory.
pub async fn save_results(
    results: &OrchestrationResult,
    output_dir: &Path,
) -> Result<PathBuf> {
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = output_dir.join(&timestamp);

    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("failed to create results directory: {}", dir.display()))?;

    // Write summary JSON
    let summary_path = dir.join("summary.json");
    let summary_json = serde_json::to_string_pretty(results)
        .context("failed to serialize results")?;
    tokio::fs::write(&summary_path, &summary_json)
        .await
        .context("failed to write summary.json")?;

    // Write individual task results
    for task_result in &results.tasks {
        let task_dir = dir.join(&task_result.name);
        tokio::fs::create_dir_all(&task_dir).await.ok();

        if !task_result.stdout.is_empty() {
            tokio::fs::write(task_dir.join("stdout.txt"), &task_result.stdout)
                .await
                .ok();
        }
        if !task_result.stderr.is_empty() {
            tokio::fs::write(task_dir.join("stderr.txt"), &task_result.stderr)
                .await
                .ok();
        }

        let meta = serde_json::json!({
            "name": task_result.name,
            "success": task_result.success,
            "exit_code": task_result.exit_code,
            "workspace_id": task_result.workspace_id,
        });
        tokio::fs::write(
            task_dir.join("meta.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .await
        .ok();
    }

    info!(dir = %dir.display(), "results saved");
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Dry-run display
// ---------------------------------------------------------------------------

/// Print a dry-run summary of the orchestration plan.
pub fn print_dry_run(plan: &OrchestrationPlan) {
    println!("=== Orchestration Plan (dry run) ===");
    println!();
    println!("Golden workspace: {}", plan.golden_workspace);
    println!("Snapshot:         {}", plan.snapshot_name);
    println!("Tasks:            {}", plan.tasks.len());
    println!();
    for (i, task) in plan.tasks.iter().enumerate() {
        println!(
            "  {}. {} (workdir: {})",
            i + 1,
            task.name,
            task.workdir.as_deref().unwrap_or("/workspace")
        );
        // Truncate long prompts for display
        let prompt_display = if task.prompt.len() > 120 {
            format!("{}...", &task.prompt[..117])
        } else {
            task.prompt.clone()
        };
        println!("     prompt: {}", prompt_display);
    }
    println!();
    println!("No changes made (dry run).");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VaultConfig;
    use tempfile::TempDir;
    use tokio::fs;

    #[test]
    fn test_parse_plan_toml() {
        let toml = r#"
golden_workspace = "my-project"
snapshot_name = "golden"

[[tasks]]
name = "implement-auth"
prompt = "Implement user authentication in src/auth.rs"

[[tasks]]
name = "add-tests"
prompt = "Add unit tests for the auth module"
workdir = "/workspace/backend"
"#;

        let plan: OrchestrationPlan = toml::from_str(toml).unwrap();
        assert_eq!(plan.golden_workspace, "my-project");
        assert_eq!(plan.snapshot_name, "golden");
        assert_eq!(plan.tasks.len(), 2);
        assert_eq!(plan.tasks[0].name, "implement-auth");
        assert_eq!(
            plan.tasks[0].prompt,
            "Implement user authentication in src/auth.rs"
        );
        assert!(plan.tasks[0].workdir.is_none());
        assert!(plan.tasks[0].vault_context.is_none());
        assert_eq!(plan.tasks[1].name, "add-tests");
        assert_eq!(
            plan.tasks[1].workdir.as_deref(),
            Some("/workspace/backend")
        );
    }

    #[test]
    fn test_parse_plan_default_snapshot() {
        let toml = r#"
golden_workspace = "my-project"

[[tasks]]
name = "task1"
prompt = "do something"
"#;

        let plan: OrchestrationPlan = toml::from_str(toml).unwrap();
        assert_eq!(plan.snapshot_name, "golden");
    }

    #[test]
    fn test_validate_plan_valid() {
        let plan = OrchestrationPlan {
            golden_workspace: "my-project".to_string(),
            snapshot_name: "golden".to_string(),
            tasks: vec![
                TaskDef {
                    name: "task1".to_string(),
                    prompt: "do something".to_string(),
                    workdir: None,
                    vault_context: None,
                },
                TaskDef {
                    name: "task2".to_string(),
                    prompt: "do something else".to_string(),
                    workdir: Some("/workspace".to_string()),
                    vault_context: None,
                },
            ],
        };
        assert!(validate_plan(&plan).is_ok());
    }

    #[test]
    fn test_validate_plan_empty_workspace() {
        let plan = OrchestrationPlan {
            golden_workspace: String::new(),
            snapshot_name: "golden".to_string(),
            tasks: vec![TaskDef {
                name: "t".to_string(),
                prompt: "p".to_string(),
                workdir: None,
                vault_context: None,
            }],
        };
        assert!(validate_plan(&plan).is_err());
    }

    #[test]
    fn test_validate_plan_empty_tasks() {
        let plan = OrchestrationPlan {
            golden_workspace: "ws".to_string(),
            snapshot_name: "golden".to_string(),
            tasks: vec![],
        };
        let err = validate_plan(&plan).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn test_validate_plan_too_many_tasks() {
        let tasks: Vec<TaskDef> = (0..21)
            .map(|i| TaskDef {
                name: format!("task-{}", i),
                prompt: "do something".to_string(),
                workdir: None,
                vault_context: None,
            })
            .collect();
        let plan = OrchestrationPlan {
            golden_workspace: "ws".to_string(),
            snapshot_name: "golden".to_string(),
            tasks,
        };
        let err = validate_plan(&plan).unwrap_err();
        assert!(err.to_string().contains("maximum 20"));
    }

    #[test]
    fn test_validate_plan_duplicate_names() {
        let plan = OrchestrationPlan {
            golden_workspace: "ws".to_string(),
            snapshot_name: "golden".to_string(),
            tasks: vec![
                TaskDef {
                    name: "same-name".to_string(),
                    prompt: "p1".to_string(),
                    workdir: None,
                    vault_context: None,
                },
                TaskDef {
                    name: "same-name".to_string(),
                    prompt: "p2".to_string(),
                    workdir: None,
                    vault_context: None,
                },
            ],
        };
        let err = validate_plan(&plan).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn test_validate_plan_empty_task_name() {
        let plan = OrchestrationPlan {
            golden_workspace: "ws".to_string(),
            snapshot_name: "golden".to_string(),
            tasks: vec![TaskDef {
                name: String::new(),
                prompt: "p".to_string(),
                workdir: None,
                vault_context: None,
            }],
        };
        assert!(validate_plan(&plan).is_err());
    }

    #[test]
    fn test_validate_plan_empty_prompt() {
        let plan = OrchestrationPlan {
            golden_workspace: "ws".to_string(),
            snapshot_name: "golden".to_string(),
            tasks: vec![TaskDef {
                name: "t".to_string(),
                prompt: String::new(),
                workdir: None,
                vault_context: None,
            }],
        };
        let err = validate_plan(&plan).unwrap_err();
        assert!(err.to_string().contains("empty prompt"));
    }

    #[test]
    fn test_task_result_construction() {
        let result = TaskResult {
            name: "implement-auth".to_string(),
            success: true,
            exit_code: 0,
            stdout: "task completed".to_string(),
            stderr: String::new(),
            workspace_id: "abc-123".to_string(),
        };
        assert!(result.success);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn test_orchestration_result_serialization() {
        let result = OrchestrationResult {
            tasks: vec![
                TaskResult {
                    name: "t1".to_string(),
                    success: true,
                    exit_code: 0,
                    stdout: "ok".to_string(),
                    stderr: String::new(),
                    workspace_id: "ws-1".to_string(),
                },
                TaskResult {
                    name: "t2".to_string(),
                    success: false,
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: "error".to_string(),
                    workspace_id: "ws-2".to_string(),
                },
            ],
            success_count: 1,
            failure_count: 1,
        };

        let json = serde_json::to_string(&result).unwrap();
        let decoded: OrchestrationResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.tasks.len(), 2);
        assert_eq!(decoded.success_count, 1);
        assert_eq!(decoded.failure_count, 1);
    }

    // --- VaultQuery tests ---

    #[test]
    fn test_vault_query_search_deserialization() {
        let json = r#"{"query": "auth patterns", "kind": "search"}"#;
        let vq: VaultQuery = serde_json::from_str(json).unwrap();
        assert_eq!(vq.query, "auth patterns");
        assert_eq!(vq.kind, "search");
    }

    #[test]
    fn test_vault_query_read_deserialization() {
        let json = r#"{"query": "conventions/rust.md", "kind": "read"}"#;
        let vq: VaultQuery = serde_json::from_str(json).unwrap();
        assert_eq!(vq.query, "conventions/rust.md");
        assert_eq!(vq.kind, "read");
    }

    #[test]
    fn test_vault_query_default_kind() {
        let json = r#"{"query": "something"}"#;
        let vq: VaultQuery = serde_json::from_str(json).unwrap();
        assert_eq!(vq.kind, "search");
    }

    #[test]
    fn test_parse_plan_toml_with_vault_context() {
        let toml = r#"
golden_workspace = "my-project"

[[tasks]]
name = "implement-auth"
prompt = "Implement authentication"

[[tasks.vault_context]]
query = "auth patterns"
kind = "search"

[[tasks.vault_context]]
query = "conventions/rust-style.md"
kind = "read"
"#;

        let plan: OrchestrationPlan = toml::from_str(toml).unwrap();
        assert_eq!(plan.tasks.len(), 1);
        let vc = plan.tasks[0].vault_context.as_ref().unwrap();
        assert_eq!(vc.len(), 2);
        assert_eq!(vc[0].query, "auth patterns");
        assert_eq!(vc[0].kind, "search");
        assert_eq!(vc[1].query, "conventions/rust-style.md");
        assert_eq!(vc[1].kind, "read");
    }

    #[test]
    fn test_parse_plan_toml_vault_context_default_kind() {
        let toml = r#"
golden_workspace = "my-project"

[[tasks]]
name = "task1"
prompt = "do something"

[[tasks.vault_context]]
query = "search term"
"#;

        let plan: OrchestrationPlan = toml::from_str(toml).unwrap();
        let vc = plan.tasks[0].vault_context.as_ref().unwrap();
        assert_eq!(vc[0].kind, "search");
    }

    #[test]
    fn test_parse_plan_toml_no_vault_context() {
        let toml = r#"
golden_workspace = "my-project"

[[tasks]]
name = "task1"
prompt = "do something"
"#;

        let plan: OrchestrationPlan = toml::from_str(toml).unwrap();
        assert!(plan.tasks[0].vault_context.is_none());
    }

    #[tokio::test]
    async fn test_resolve_vault_context_search() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::write(
            root.join("auth-guide.md"),
            "# Auth Guide\n\nUse JWT tokens for auth patterns.\n",
        )
        .await
        .unwrap();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&cfg).unwrap();

        let queries = vec![VaultQuery {
            query: "auth patterns".to_string(),
            kind: "search".to_string(),
        }];

        let result = resolve_vault_context(&vm, &queries).await;
        assert!(result.contains("## Project Knowledge Base"));
        assert!(result.contains("auth-guide.md"));
        assert!(result.contains("auth patterns"));
    }

    #[tokio::test]
    async fn test_resolve_vault_context_read() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::write(
            root.join("conventions.md"),
            "# Rust Conventions\n\nUse snake_case.\n",
        )
        .await
        .unwrap();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&cfg).unwrap();

        let queries = vec![VaultQuery {
            query: "conventions.md".to_string(),
            kind: "read".to_string(),
        }];

        let result = resolve_vault_context(&vm, &queries).await;
        assert!(result.contains("## Project Knowledge Base"));
        assert!(result.contains("conventions.md"));
        assert!(result.contains("Use snake_case."));
    }

    #[tokio::test]
    async fn test_resolve_vault_context_empty_on_no_results() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&cfg).unwrap();

        let queries = vec![VaultQuery {
            query: "nonexistent-term-xyz".to_string(),
            kind: "search".to_string(),
        }];

        let result = resolve_vault_context(&vm, &queries).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_vault_context_skips_failed_read() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::write(root.join("good.md"), "Good content.\n")
            .await
            .unwrap();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&cfg).unwrap();

        // One read that fails (nonexistent file), one that succeeds
        let queries = vec![
            VaultQuery {
                query: "nonexistent.md".to_string(),
                kind: "read".to_string(),
            },
            VaultQuery {
                query: "good.md".to_string(),
                kind: "read".to_string(),
            },
        ];

        let result = resolve_vault_context(&vm, &queries).await;
        assert!(result.contains("Good content."));
        assert!(!result.contains("nonexistent.md"));
    }

    #[tokio::test]
    async fn test_resolve_vault_context_mixed_queries() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::write(
            root.join("patterns.md"),
            "# Patterns\n\nUse dependency injection for auth.\n",
        )
        .await
        .unwrap();

        fs::write(
            root.join("style.md"),
            "# Style Guide\n\nAll functions must have docs.\n",
        )
        .await
        .unwrap();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&cfg).unwrap();

        let queries = vec![
            VaultQuery {
                query: "auth".to_string(),
                kind: "search".to_string(),
            },
            VaultQuery {
                query: "style.md".to_string(),
                kind: "read".to_string(),
            },
        ];

        let result = resolve_vault_context(&vm, &queries).await;
        assert!(result.contains("## Project Knowledge Base"));
        assert!(result.contains("dependency injection"));
        assert!(result.contains("All functions must have docs."));
    }

    #[test]
    fn test_prompt_enrichment_format() {
        let prompt = "Implement authentication";
        let vault_context = "## Project Knowledge Base\n\n### auth-guide.md\n\nUse JWT tokens.\n";
        let enriched = format!("{}\n\n{}", prompt, vault_context);

        assert!(enriched.starts_with("Implement authentication"));
        assert!(enriched.contains("## Project Knowledge Base"));
        assert!(enriched.contains("Use JWT tokens."));
    }

    // --- Fix #1: Fork concurrency semaphore test ---

    #[tokio::test]
    async fn test_fork_semaphore_limits_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Semaphore;

        // Simulate the fork phase with a semaphore (max_parallel = 2)
        let max_parallel = 2;
        let semaphore = Arc::new(Semaphore::new(max_parallel));
        let concurrent_count = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let task_count = 6;
        let mut join_set = tokio::task::JoinSet::new();

        for i in 0..task_count {
            let sem = semaphore.clone();
            let cc = concurrent_count.clone();
            let mc = max_concurrent.clone();
            join_set.spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let current = cc.fetch_add(1, Ordering::SeqCst) + 1;
                // Update max observed concurrency
                mc.fetch_max(current, Ordering::SeqCst);
                // Simulate some work
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                cc.fetch_sub(1, Ordering::SeqCst);
                i
            });
        }

        let mut results = Vec::new();
        while let Some(result) = join_set.join_next().await {
            results.push(result.unwrap());
        }

        assert_eq!(results.len(), task_count);
        // The semaphore should have limited concurrency to max_parallel
        assert!(max_concurrent.load(Ordering::SeqCst) <= max_parallel);
    }

    // --- Fix #2: Fork error details in TaskResult ---

    #[test]
    fn test_fork_error_includes_specific_message() {
        // Simulate what happens when a fork fails: the error message
        // should include the specific error, not just "worker VM fork failed".
        let fork_errors: HashMap<usize, String> = [
            (0, "fork 'orch-task-a' failed: ZFS clone error: dataset not found".to_string()),
            (2, "fork 'orch-task-c' failed: network TAP create failed".to_string()),
        ]
        .into_iter()
        .collect();

        let tasks = vec![
            TaskDef {
                name: "task-a".to_string(),
                prompt: "p".to_string(),
                workdir: None,
                vault_context: None,
            },
            TaskDef {
                name: "task-b".to_string(),
                prompt: "p".to_string(),
                workdir: None,
                vault_context: None,
            },
            TaskDef {
                name: "task-c".to_string(),
                prompt: "p".to_string(),
                workdir: None,
                vault_context: None,
            },
        ];

        // Simulate workers: only task-b (index 1) succeeded
        let workers: Vec<(usize, String, Uuid)> = vec![
            (1, "orch-task-b".to_string(), Uuid::new_v4()),
        ];

        let mut results: Vec<TaskResult> = Vec::new();
        for (i, task) in tasks.iter().enumerate() {
            if !workers.iter().any(|(idx, _, _)| *idx == i) {
                let stderr = fork_errors
                    .get(&i)
                    .map(|e| format!("worker VM fork failed: {}", e))
                    .unwrap_or_else(|| "worker VM fork failed".to_string());
                results.push(TaskResult {
                    name: task.name.clone(),
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr,
                    workspace_id: String::new(),
                });
            }
        }

        assert_eq!(results.len(), 2);

        // task-a should have the ZFS-specific error
        let task_a = results.iter().find(|r| r.name == "task-a").unwrap();
        assert!(task_a.stderr.contains("worker VM fork failed:"));
        assert!(task_a.stderr.contains("ZFS clone error"));

        // task-c should have the network-specific error
        let task_c = results.iter().find(|r| r.name == "task-c").unwrap();
        assert!(task_c.stderr.contains("worker VM fork failed:"));
        assert!(task_c.stderr.contains("network TAP create failed"));
    }

    // --- Fix #4: Parallel vault context resolution ---

    #[tokio::test]
    async fn test_resolve_all_vault_contexts_parallel() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::write(
            root.join("auth.md"),
            "# Auth\n\nUse JWT for auth patterns.\n",
        )
        .await
        .unwrap();

        fs::write(
            root.join("style.md"),
            "# Style\n\nUse snake_case everywhere.\n",
        )
        .await
        .unwrap();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vault = VaultManager::new(&cfg).unwrap();

        let tasks = vec![
            TaskDef {
                name: "task-0".to_string(),
                prompt: "Implement auth".to_string(),
                workdir: None,
                vault_context: Some(vec![VaultQuery {
                    query: "auth patterns".to_string(),
                    kind: "search".to_string(),
                }]),
            },
            TaskDef {
                name: "task-1".to_string(),
                prompt: "No vault context here".to_string(),
                workdir: None,
                vault_context: None,
            },
            TaskDef {
                name: "task-2".to_string(),
                prompt: "Read style guide".to_string(),
                workdir: None,
                vault_context: Some(vec![VaultQuery {
                    query: "style.md".to_string(),
                    kind: "read".to_string(),
                }]),
            },
        ];

        let resolved = resolve_all_vault_contexts(vault, &tasks).await;

        // task-0 should have vault context appended
        assert!(resolved.contains_key(&0));
        let t0 = &resolved[&0];
        assert!(t0.starts_with("Implement auth"));
        assert!(t0.contains("## Project Knowledge Base"));
        assert!(t0.contains("JWT"));

        // task-1 has no vault_context, so it should not be in the map
        assert!(!resolved.contains_key(&1));

        // task-2 should have the style guide content
        assert!(resolved.contains_key(&2));
        let t2 = &resolved[&2];
        assert!(t2.starts_with("Read style guide"));
        assert!(t2.contains("snake_case"));
    }

    #[tokio::test]
    async fn test_resolve_all_vault_contexts_empty_queries_skipped() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        let cfg = VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vault = VaultManager::new(&cfg).unwrap();

        let tasks = vec![TaskDef {
            name: "task-0".to_string(),
            prompt: "Prompt".to_string(),
            workdir: None,
            vault_context: Some(vec![]), // empty queries list
        }];

        let resolved = resolve_all_vault_contexts(vault, &tasks).await;
        assert!(resolved.is_empty());
    }
}
