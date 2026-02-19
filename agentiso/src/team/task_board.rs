use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::mcp::vault::{VaultManager, WriteMode};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Status of a board task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Claimed,
    InProgress,
    Completed,
    Failed,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Must match serde rename_all = "lowercase"
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::Claimed => write!(f, "claimed"),
            TaskStatus::InProgress => write!(f, "inprogress"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
        }
    }
}

/// Priority of a board task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskPriority::Low => write!(f, "low"),
            TaskPriority::Medium => write!(f, "medium"),
            TaskPriority::High => write!(f, "high"),
            TaskPriority::Critical => write!(f, "critical"),
        }
    }
}

/// In-memory representation of a vault-backed task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardTask {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    pub owner: Option<String>,
    pub priority: TaskPriority,
    pub depends_on: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub body: String,
}

/// Updates to apply to a task.
#[derive(Debug, Clone, Default)]
pub struct TaskUpdate {
    pub status: Option<TaskStatus>,
    /// `Some(None)` clears the owner, `Some(Some(x))` sets it.
    pub owner: Option<Option<String>>,
    pub priority: Option<TaskPriority>,
    pub body_append: Option<String>,
}

// ---------------------------------------------------------------------------
// TaskBoard
// ---------------------------------------------------------------------------

/// Vault-backed task board for a team.
pub struct TaskBoard {
    vault: Arc<VaultManager>,
    #[allow(dead_code)]
    team_name: String,
}

impl TaskBoard {
    pub fn new(vault: Arc<VaultManager>, team_name: String) -> Self {
        Self { vault, team_name }
    }

    /// Create a new task. Returns the created task.
    pub async fn create_task(
        &self,
        title: &str,
        description: &str,
        priority: TaskPriority,
        depends_on: Vec<String>,
    ) -> Result<BoardTask> {
        let id = self.next_task_id().await?;
        let now = Utc::now();
        let task = BoardTask {
            id: id.clone(),
            title: title.to_string(),
            status: TaskStatus::Pending,
            owner: None,
            priority,
            depends_on,
            created_at: now,
            updated_at: now,
            body: description.to_string(),
        };
        let markdown = self.render_task_markdown(&task);
        self.vault
            .write_note(&format!("tasks/{}.md", id), &markdown, WriteMode::Overwrite)
            .await?;
        Ok(task)
    }

    /// Read a task by ID.
    pub async fn get_task(&self, id: &str) -> Result<BoardTask> {
        let note = self
            .vault
            .read_note(&format!("tasks/{}.md", id))
            .await
            .with_context(|| format!("reading task '{}'", id))?;
        self.parse_task_markdown(&note.content, id)
    }

    /// List all tasks, optionally filtered by status.
    pub async fn list_tasks(&self, filter: Option<TaskStatus>) -> Result<Vec<BoardTask>> {
        let entries = self.vault.list_notes(Some("tasks"), false).await?;
        let mut tasks = Vec::new();
        for entry in entries {
            if entry.is_dir {
                continue;
            }
            if !entry.path.ends_with(".md") {
                continue;
            }
            // Extract task ID from filename: "task-001.md" -> "task-001"
            let id = match entry
                .path
                .strip_suffix(".md")
                .and_then(|p| p.rsplit('/').next())
            {
                Some(id) if id.starts_with("task-") => id,
                _ => continue,
            };
            if let Ok(task) = self.get_task(id).await {
                if filter.is_none() || filter.as_ref() == Some(&task.status) {
                    tasks.push(task);
                }
            }
        }
        Ok(tasks)
    }

    /// Update a task's fields.
    pub async fn update_task(&self, id: &str, updates: TaskUpdate) -> Result<BoardTask> {
        let mut task = self.get_task(id).await?;
        if let Some(status) = updates.status {
            task.status = status;
        }
        if let Some(owner) = updates.owner {
            task.owner = owner;
        }
        if let Some(priority) = updates.priority {
            task.priority = priority;
        }
        if let Some(ref append) = updates.body_append {
            task.body.push('\n');
            task.body.push_str(append);
        }
        task.updated_at = Utc::now();
        let markdown = self.render_task_markdown(&task);
        self.vault
            .write_note(&format!("tasks/{}.md", id), &markdown, WriteMode::Overwrite)
            .await?;
        Ok(task)
    }

    /// Generate next task ID (`task-NNN` format).
    async fn next_task_id(&self) -> Result<String> {
        let entries = self
            .vault
            .list_notes(Some("tasks"), false)
            .await
            .unwrap_or_default();
        let max_num = entries
            .iter()
            .filter(|e| !e.is_dir)
            .filter_map(|e| e.path.strip_suffix(".md"))
            .filter_map(|p| p.rsplit('/').next())
            .filter_map(|name| name.strip_prefix("task-"))
            .filter_map(|num| num.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        Ok(format!("task-{:03}", max_num + 1))
    }

    /// Parse task markdown with YAML frontmatter into BoardTask.
    pub fn parse_task_markdown(&self, content: &str, id: &str) -> Result<BoardTask> {
        // Split frontmatter from body
        let (fm_str, body_after_fm) = split_frontmatter_raw(content);

        // Parse frontmatter YAML
        let fm: serde_yaml::Value = if let Some(yaml_str) = fm_str {
            serde_yaml::from_str(&yaml_str)
                .context("parsing task frontmatter YAML")?
        } else {
            bail!("task '{}' has no YAML frontmatter", id);
        };

        let mapping = fm
            .as_mapping()
            .context("frontmatter is not a YAML mapping")?;

        let status_str = mapping
            .get(&serde_yaml::Value::String("status".into()))
            .and_then(|v| v.as_str())
            .unwrap_or("pending");

        let status: TaskStatus = serde_yaml::from_str(&format!("\"{}\"", status_str))
            .context("parsing task status")?;

        let priority_str = mapping
            .get(&serde_yaml::Value::String("priority".into()))
            .and_then(|v| v.as_str())
            .unwrap_or("medium");

        let priority: TaskPriority = serde_yaml::from_str(&format!("\"{}\"", priority_str))
            .context("parsing task priority")?;

        let owner = mapping
            .get(&serde_yaml::Value::String("owner".into()))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let depends_on = mapping
            .get(&serde_yaml::Value::String("depends_on".into()))
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let created_at = mapping
            .get(&serde_yaml::Value::String("created_at".into()))
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let updated_at = mapping
            .get(&serde_yaml::Value::String("updated_at".into()))
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        // Extract title from first `# ` heading in body
        let mut title = String::new();
        let mut body_lines = Vec::new();
        let mut found_title = false;
        for line in body_after_fm.lines() {
            if !found_title {
                if let Some(heading) = line.strip_prefix("# ") {
                    title = heading.trim().to_string();
                    found_title = true;
                    continue;
                }
                // Skip blank lines before the title
                if line.trim().is_empty() {
                    continue;
                }
            }
            body_lines.push(line);
        }

        // Trim leading/trailing blank lines from body
        while body_lines.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
            body_lines.remove(0);
        }
        while body_lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
            body_lines.pop();
        }

        let body = body_lines.join("\n");

        if title.is_empty() {
            bail!("task '{}' has no title (expected `# Title` heading)", id);
        }

        Ok(BoardTask {
            id: id.to_string(),
            title,
            status,
            owner,
            priority,
            depends_on,
            created_at,
            updated_at,
            body,
        })
    }

    /// Render a BoardTask as markdown with YAML frontmatter.
    pub fn render_task_markdown(&self, task: &BoardTask) -> String {
        let mut fm = serde_yaml::Mapping::new();

        fm.insert(
            serde_yaml::Value::String("status".into()),
            serde_yaml::Value::String(task.status.to_string()),
        );
        fm.insert(
            serde_yaml::Value::String("priority".into()),
            serde_yaml::Value::String(task.priority.to_string()),
        );

        if let Some(ref owner) = task.owner {
            fm.insert(
                serde_yaml::Value::String("owner".into()),
                serde_yaml::Value::String(owner.clone()),
            );
        }

        if !task.depends_on.is_empty() {
            let deps: Vec<serde_yaml::Value> = task
                .depends_on
                .iter()
                .map(|d| serde_yaml::Value::String(d.clone()))
                .collect();
            fm.insert(
                serde_yaml::Value::String("depends_on".into()),
                serde_yaml::Value::Sequence(deps),
            );
        }

        fm.insert(
            serde_yaml::Value::String("created_at".into()),
            serde_yaml::Value::String(task.created_at.to_rfc3339()),
        );
        fm.insert(
            serde_yaml::Value::String("updated_at".into()),
            serde_yaml::Value::String(task.updated_at.to_rfc3339()),
        );

        let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(fm)).unwrap_or_default();
        let yaml = yaml.trim_end_matches('\n');

        let mut out = format!("---\n{}\n---\n# {}\n", yaml, task.title);
        if !task.body.is_empty() {
            out.push('\n');
            out.push_str(&task.body);
            out.push('\n');
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Frontmatter helpers (local to this module)
// ---------------------------------------------------------------------------

/// Split content into (raw YAML string, body after closing ---).
fn split_frontmatter_raw(content: &str) -> (Option<String>, String) {
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return (None, content.to_string());
    }

    let rest = &content[4..]; // skip "---\n"
    let closing = rest
        .find("\n---\n")
        .map(|i| (i, i + 5))
        .or_else(|| rest.find("\n---\r\n").map(|i| (i, i + 6)))
        .or_else(|| {
            if rest.ends_with("\n---") {
                let i = rest.len() - 4;
                Some((i, rest.len()))
            } else {
                None
            }
        });

    match closing {
        Some((yaml_end, body_start)) => {
            let yaml_str = rest[..yaml_end].to_string();
            let body = rest[body_start..].to_string();
            (Some(yaml_str), body)
        }
        None => (None, content.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VaultConfig;

    /// Create a VaultManager backed by a temp directory.
    fn temp_vault() -> (tempfile::TempDir, Arc<VaultManager>) {
        let dir = tempfile::tempdir().unwrap();
        // Create "tasks" subdirectory
        std::fs::create_dir_all(dir.path().join("tasks")).unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vault = VaultManager::new(&config).unwrap();
        (dir, vault)
    }

    fn make_board(vault: Arc<VaultManager>) -> TaskBoard {
        TaskBoard::new(vault, "test-team".to_string())
    }

    #[test]
    fn test_render_and_parse_roundtrip() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let now = Utc::now();
        let task = BoardTask {
            id: "task-001".to_string(),
            title: "Implement feature X".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("alice".to_string()),
            priority: TaskPriority::High,
            depends_on: vec!["task-000".to_string()],
            created_at: now,
            updated_at: now,
            body: "This is the task description.\n\nWith multiple paragraphs.".to_string(),
        };

        let markdown = board.render_task_markdown(&task);
        let parsed = board.parse_task_markdown(&markdown, "task-001").unwrap();

        assert_eq!(parsed.id, task.id);
        assert_eq!(parsed.title, task.title);
        assert_eq!(parsed.status, task.status);
        assert_eq!(parsed.owner, task.owner);
        assert_eq!(parsed.priority, task.priority);
        assert_eq!(parsed.depends_on, task.depends_on);
        assert_eq!(parsed.body, task.body);
    }

    #[test]
    fn test_parse_frontmatter_fields() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let markdown = "---\nstatus: completed\npriority: critical\nowner: bob\ndepends_on:\n  - task-001\n  - task-002\ncreated_at: '2026-01-15T10:00:00+00:00'\nupdated_at: '2026-01-16T12:00:00+00:00'\n---\n# Fix the bug\n\nDetailed description here.\n";

        let task = board.parse_task_markdown(markdown, "task-003").unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(task.priority, TaskPriority::Critical);
        assert_eq!(task.owner, Some("bob".to_string()));
        assert_eq!(task.depends_on, vec!["task-001", "task-002"]);
        assert_eq!(task.title, "Fix the bug");
        assert_eq!(task.body, "Detailed description here.");
    }

    #[test]
    fn test_render_markdown_format() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let now = Utc::now();
        let task = BoardTask {
            id: "task-005".to_string(),
            title: "Write tests".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            priority: TaskPriority::Medium,
            depends_on: vec![],
            created_at: now,
            updated_at: now,
            body: "Add unit tests.".to_string(),
        };

        let md = board.render_task_markdown(&task);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("\n---\n"));
        assert!(md.contains("status: pending"));
        assert!(md.contains("priority: medium"));
        assert!(md.contains("# Write tests"));
        assert!(md.contains("Add unit tests."));
        // No owner should be present when None
        assert!(!md.contains("owner:"));
        // No depends_on when empty
        assert!(!md.contains("depends_on:"));
    }

    #[test]
    fn test_render_no_body() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let now = Utc::now();
        let task = BoardTask {
            id: "task-010".to_string(),
            title: "Empty body task".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            priority: TaskPriority::Low,
            depends_on: vec![],
            created_at: now,
            updated_at: now,
            body: String::new(),
        };

        let md = board.render_task_markdown(&task);
        assert!(md.contains("# Empty body task\n"));
        // Should end right after the heading, no extra blank lines
        assert!(md.ends_with("# Empty body task\n"));
    }

    #[test]
    fn test_parse_missing_frontmatter_fails() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let md = "# Just a heading\n\nNo frontmatter here.\n";
        assert!(board.parse_task_markdown(md, "task-bad").is_err());
    }

    #[test]
    fn test_parse_missing_title_fails() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let md = "---\nstatus: pending\npriority: low\n---\nNo heading here.\n";
        assert!(board.parse_task_markdown(md, "task-bad").is_err());
    }

    #[test]
    fn test_status_display() {
        assert_eq!(TaskStatus::Pending.to_string(), "pending");
        assert_eq!(TaskStatus::Claimed.to_string(), "claimed");
        assert_eq!(TaskStatus::InProgress.to_string(), "inprogress");
        assert_eq!(TaskStatus::Completed.to_string(), "completed");
        assert_eq!(TaskStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn test_priority_display() {
        assert_eq!(TaskPriority::Low.to_string(), "low");
        assert_eq!(TaskPriority::Medium.to_string(), "medium");
        assert_eq!(TaskPriority::High.to_string(), "high");
        assert_eq!(TaskPriority::Critical.to_string(), "critical");
    }

    #[test]
    fn test_status_serde_roundtrip() {
        for status in &[
            TaskStatus::Pending,
            TaskStatus::Claimed,
            TaskStatus::InProgress,
            TaskStatus::Completed,
            TaskStatus::Failed,
        ] {
            let json = serde_json::to_string(status).unwrap();
            let parsed: TaskStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, status);
        }
    }

    #[test]
    fn test_priority_serde_roundtrip() {
        for priority in &[
            TaskPriority::Low,
            TaskPriority::Medium,
            TaskPriority::High,
            TaskPriority::Critical,
        ] {
            let json = serde_json::to_string(priority).unwrap();
            let parsed: TaskPriority = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, priority);
        }
    }

    #[tokio::test]
    async fn test_create_and_get_task() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let task = board
            .create_task(
                "My first task",
                "Description of the task",
                TaskPriority::High,
                vec![],
            )
            .await
            .unwrap();

        assert_eq!(task.id, "task-001");
        assert_eq!(task.title, "My first task");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.priority, TaskPriority::High);
        assert!(task.owner.is_none());

        let fetched = board.get_task("task-001").await.unwrap();
        assert_eq!(fetched.title, "My first task");
        assert_eq!(fetched.body, "Description of the task");
        assert_eq!(fetched.status, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn test_list_tasks_with_filter() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        board
            .create_task("Task A", "Desc A", TaskPriority::Low, vec![])
            .await
            .unwrap();
        board
            .create_task("Task B", "Desc B", TaskPriority::Medium, vec![])
            .await
            .unwrap();
        board
            .create_task("Task C", "Desc C", TaskPriority::High, vec![])
            .await
            .unwrap();

        // Update task-002 to Completed
        board
            .update_task(
                "task-002",
                TaskUpdate {
                    status: Some(TaskStatus::Completed),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // List all
        let all = board.list_tasks(None).await.unwrap();
        assert_eq!(all.len(), 3);

        // Filter pending only
        let pending = board.list_tasks(Some(TaskStatus::Pending)).await.unwrap();
        assert_eq!(pending.len(), 2);

        // Filter completed only
        let completed = board
            .list_tasks(Some(TaskStatus::Completed))
            .await
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].title, "Task B");
    }

    #[tokio::test]
    async fn test_update_task() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        board
            .create_task("Update me", "Original body", TaskPriority::Low, vec![])
            .await
            .unwrap();

        let updated = board
            .update_task(
                "task-001",
                TaskUpdate {
                    status: Some(TaskStatus::InProgress),
                    owner: Some(Some("agent-1".to_string())),
                    priority: Some(TaskPriority::Critical),
                    body_append: Some("Appended note.".to_string()),
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.status, TaskStatus::InProgress);
        assert_eq!(updated.owner, Some("agent-1".to_string()));
        assert_eq!(updated.priority, TaskPriority::Critical);
        assert!(updated.body.contains("Original body"));
        assert!(updated.body.contains("Appended note."));

        // Verify persisted
        let fetched = board.get_task("task-001").await.unwrap();
        assert_eq!(fetched.status, TaskStatus::InProgress);
        assert_eq!(fetched.owner, Some("agent-1".to_string()));
    }

    #[tokio::test]
    async fn test_next_task_id_sequential() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let t1 = board
            .create_task("Task 1", "Desc", TaskPriority::Medium, vec![])
            .await
            .unwrap();
        let t2 = board
            .create_task("Task 2", "Desc", TaskPriority::Medium, vec![])
            .await
            .unwrap();
        let t3 = board
            .create_task("Task 3", "Desc", TaskPriority::Medium, vec![])
            .await
            .unwrap();

        assert_eq!(t1.id, "task-001");
        assert_eq!(t2.id, "task-002");
        assert_eq!(t3.id, "task-003");
    }

    #[tokio::test]
    async fn test_update_clear_owner() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        board
            .create_task("Owned task", "Desc", TaskPriority::Medium, vec![])
            .await
            .unwrap();

        // Set owner
        board
            .update_task(
                "task-001",
                TaskUpdate {
                    owner: Some(Some("alice".to_string())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let t = board.get_task("task-001").await.unwrap();
        assert_eq!(t.owner, Some("alice".to_string()));

        // Clear owner
        board
            .update_task(
                "task-001",
                TaskUpdate {
                    owner: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let t = board.get_task("task-001").await.unwrap();
        assert!(t.owner.is_none());
    }

    #[test]
    fn test_split_frontmatter_raw() {
        let content = "---\nfoo: bar\n---\n# Heading\n\nBody text.\n";
        let (fm, body) = split_frontmatter_raw(content);
        assert_eq!(fm.unwrap(), "foo: bar");
        assert_eq!(body, "# Heading\n\nBody text.\n");
    }

    #[test]
    fn test_split_frontmatter_raw_no_frontmatter() {
        let content = "# Just a heading\nBody.\n";
        let (fm, body) = split_frontmatter_raw(content);
        assert!(fm.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn test_roundtrip_with_depends_on() {
        let (_dir, vault) = temp_vault();
        let board = make_board(vault);

        let now = Utc::now();
        let task = BoardTask {
            id: "task-010".to_string(),
            title: "Depends on others".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            priority: TaskPriority::Medium,
            depends_on: vec!["task-001".to_string(), "task-002".to_string()],
            created_at: now,
            updated_at: now,
            body: "Blocked task.".to_string(),
        };

        let md = board.render_task_markdown(&task);
        let parsed = board.parse_task_markdown(&md, "task-010").unwrap();
        assert_eq!(parsed.depends_on, vec!["task-001", "task-002"]);
    }

    #[test]
    fn test_board_task_serde_json_roundtrip() {
        let task = BoardTask {
            id: "task-001".into(),
            title: "Test".into(),
            status: TaskStatus::Pending,
            owner: None,
            priority: TaskPriority::Medium,
            depends_on: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            body: "Body".into(),
        };
        let json = serde_json::to_string(&task).unwrap();
        let parsed: BoardTask = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "task-001");
        assert_eq!(parsed.status, TaskStatus::Pending);
    }
}
