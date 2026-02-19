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
// Execution engine
// ---------------------------------------------------------------------------

/// Execute an orchestration plan.
///
/// Forks worker VMs from the golden snapshot, injects API keys,
/// runs `opencode run` in each, and collects results.
pub async fn execute(
    manager: Arc<WorkspaceManager>,
    plan: &OrchestrationPlan,
    api_key: &str,
    max_parallel: usize,
) -> Result<OrchestrationResult> {
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

    // Fork worker VMs
    info!(count = task_count, "forking worker VMs");
    let mut workers: Vec<(String, Uuid)> = Vec::with_capacity(task_count);
    let mut fork_errors: Vec<String> = Vec::new();

    let mut join_set = tokio::task::JoinSet::new();
    for (i, task) in plan.tasks.iter().enumerate() {
        let mgr = manager.clone();
        let snap = plan.snapshot_name.clone();
        let name = format!("orch-{}", task.name);
        join_set.spawn(async move {
            let result = mgr.fork(golden_id, &snap, Some(name.clone())).await;
            (i, name, result)
        });
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok((_idx, name, Ok(ws))) => {
                workers.push((name, ws.id));
            }
            Ok((_idx, name, Err(e))) => {
                fork_errors.push(format!("fork '{}' failed: {:#}", name, e));
            }
            Err(e) => {
                fork_errors.push(format!("fork task panicked: {}", e));
            }
        }
    }

    if !fork_errors.is_empty() {
        warn!(
            errors = fork_errors.len(),
            "some forks failed: {:?}", fork_errors
        );
    }

    // Match workers to tasks by order of successful forks
    // Workers are pushed in completion order, so re-sort isn't needed;
    // we just pair the first N workers with the first N tasks.
    // But since JoinSet returns in arbitrary order, let's pair by index.
    // We need a different approach: fork with task index embedded.
    // Actually, let's re-fork with stable mapping.

    // Re-approach: fork sequentially to maintain stable task<->worker mapping,
    // but then execute tasks in parallel with semaphore.
    // The fork already happened above, so let's just build the mapping.
    // Workers were pushed in arbitrary JoinSet completion order, but we need
    // them mapped to tasks by name prefix. Let's sort workers by name.
    workers.sort_by(|a, b| a.0.cmp(&b.0));

    // Build task -> workspace mapping
    let mut task_workspace_map: Vec<(usize, Uuid)> = Vec::new();
    for (i, task) in plan.tasks.iter().enumerate() {
        let expected_name = format!("orch-{}", task.name);
        if let Some(pos) = workers.iter().position(|(name, _)| *name == expected_name) {
            task_workspace_map.push((i, workers[pos].1));
        }
    }

    // Execute tasks in parallel with semaphore
    let semaphore = Arc::new(Semaphore::new(max_parallel));
    let mut exec_set = tokio::task::JoinSet::new();

    for (task_idx, ws_id) in task_workspace_map {
        let task_def = plan.tasks[task_idx].clone();
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

    // Add results for tasks that never got a worker (fork failures)
    for task in &plan.tasks {
        let expected_name = format!("orch-{}", task.name);
        if !workers.iter().any(|(name, _)| *name == expected_name) {
            results.push(TaskResult {
                name: task.name.clone(),
                success: false,
                exit_code: -1,
                stdout: String::new(),
                stderr: "worker VM fork failed".to_string(),
                workspace_id: String::new(),
            });
        }
    }

    // Sort results by task name for stable output
    results.sort_by(|a, b| a.name.cmp(&b.name));

    // Destroy all worker VMs
    info!(count = workers.len(), "destroying worker VMs");
    for (_, ws_id) in &workers {
        if let Err(e) = manager.destroy(*ws_id).await {
            warn!(workspace_id = %ws_id, error = %e, "failed to destroy worker VM");
        }
    }

    let success_count = results.iter().filter(|r| r.success).count();
    let failure_count = results.len() - success_count;

    info!(success = success_count, failed = failure_count, "orchestration complete");

    Ok(OrchestrationResult {
        tasks: results,
        success_count,
        failure_count,
    })
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
                },
                TaskDef {
                    name: "task2".to_string(),
                    prompt: "do something else".to_string(),
                    workdir: Some("/workspace".to_string()),
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
                },
                TaskDef {
                    name: "same-name".to_string(),
                    prompt: "p2".to_string(),
                    workdir: None,
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
}
