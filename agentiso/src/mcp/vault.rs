use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::{Mutex as TokioMutex, RwLock};

use crate::config::VaultConfig;

/// Maximum write size for vault notes (10 MiB).
const MAX_WRITE_BYTES: usize = 10 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A parsed vault note with optional YAML frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultNote {
    /// Path relative to vault root.
    pub path: String,
    /// Full file content (including frontmatter markers).
    pub content: String,
    /// Parsed YAML frontmatter, if present.
    pub frontmatter: Option<serde_yaml::Value>,
}

/// A single search hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Path relative to vault root.
    pub path: String,
    /// 1-based line number of the match.
    pub line_number: usize,
    /// The matching line.
    pub line: String,
    /// A few surrounding lines for context.
    pub context: String,
}

/// A directory entry returned by `list_notes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteEntry {
    /// Path relative to vault root.
    pub path: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// File size in bytes (None for directories).
    pub size: Option<u64>,
}

/// How `write_note` should apply content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteMode {
    Overwrite,
    Append,
    Prepend,
}

// ---------------------------------------------------------------------------
// VaultManager
// ---------------------------------------------------------------------------

/// Manages read/write/search operations on an Obsidian-style markdown vault.
pub struct VaultManager {
    config: VaultConfig,
    /// Per-path RwLock for concurrent access safety.
    locks: TokioMutex<HashMap<PathBuf, Arc<RwLock<()>>>>,
    /// Optional scope prefix for team path isolation.
    scope: Option<PathBuf>,
}

impl VaultManager {
    /// Create a new VaultManager from config.
    ///
    /// Returns `None` if vault is disabled or the path does not exist.
    pub fn new(config: &VaultConfig) -> Option<Arc<Self>> {
        if !config.enabled {
            return None;
        }
        if !config.path.exists() || !config.path.is_dir() {
            return None;
        }
        Some(Arc::new(Self {
            config: config.clone(),
            locks: TokioMutex::new(HashMap::new()),
            scope: None,
        }))
    }

    /// Create a scoped VaultManager that restricts all operations to a
    /// subdirectory of the vault root. Creates the scope directory if needed.
    pub fn with_scope(config: &VaultConfig, scope: &str) -> Option<Arc<Self>> {
        if !config.enabled {
            return None;
        }
        let scoped_path = config.path.join(scope);
        if !scoped_path.exists() {
            std::fs::create_dir_all(&scoped_path).ok()?;
        }
        Some(Arc::new(Self {
            config: config.clone(),
            locks: TokioMutex::new(HashMap::new()),
            scope: Some(PathBuf::from(scope)),
        }))
    }

    /// Return the vault root path.
    #[allow(dead_code)] // public API for future use
    pub fn root(&self) -> &PathBuf {
        &self.config.path
    }

    /// Return the effective base path (vault root + scope prefix).
    fn base_path(&self) -> PathBuf {
        if let Some(ref scope) = self.scope {
            self.config.path.join(scope)
        } else {
            self.config.path.clone()
        }
    }

    /// Get or create an RwLock for a specific file path.
    async fn path_lock(&self, path: &PathBuf) -> Arc<RwLock<()>> {
        let mut map = self.locks.lock().await;
        map.entry(path.clone())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }

    // -- path helpers -------------------------------------------------------

    /// Resolve a user-supplied path to an absolute path within the vault root
    /// (or within the scope subdirectory, if a scope is set).
    ///
    /// Rejects paths containing `..` components and ensures the result is
    /// inside the effective base path after canonicalization.
    pub fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let path = Path::new(path);

        // Reject `..` components before any filesystem access.
        for component in path.components() {
            if let std::path::Component::ParentDir = component {
                bail!("path traversal (.. component) is not allowed");
            }
        }

        let base = self.base_path();
        let joined = base.join(path);

        // If the target already exists we can canonicalize; otherwise we
        // canonicalize the parent and append the file name.
        let resolved = if joined.exists() {
            joined.canonicalize()?
        } else {
            let parent = joined.parent().context("path has no parent")?;
            if parent.exists() {
                parent.canonicalize()?.join(
                    joined.file_name().context("path has no file name")?,
                )
            } else {
                // Parent doesn't exist yet — build canonical path from root.
                let canon_base = base.canonicalize()?;
                let relative = joined
                    .strip_prefix(&base)
                    .unwrap_or(joined.as_path());
                canon_base.join(relative)
            }
        };

        let canon_base = base.canonicalize()?;
        if !resolved.starts_with(&canon_base) {
            bail!(
                "resolved path {} is outside vault base {}",
                resolved.display(),
                canon_base.display()
            );
        }

        Ok(resolved)
    }

    /// Convert an absolute path back to a base-relative string.
    fn relative_path(&self, abs: &Path) -> String {
        let base = self.base_path();
        let canon_base = base
            .canonicalize()
            .unwrap_or(base);
        abs.strip_prefix(&canon_base)
            .unwrap_or(abs)
            .to_string_lossy()
            .to_string()
    }

    // -- read / write -------------------------------------------------------

    /// Read a note and parse its frontmatter.
    pub async fn read_note(&self, path: &str) -> Result<VaultNote> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.read().await;
        let content = fs::read_to_string(&abs)
            .await
            .with_context(|| format!("reading {}", abs.display()))?;
        let frontmatter = parse_frontmatter(&content);
        Ok(VaultNote {
            path: self.relative_path(&abs),
            content,
            frontmatter,
        })
    }

    /// Write (or append/prepend to) a note.
    pub async fn write_note(&self, path: &str, content: &str, mode: WriteMode) -> Result<()> {
        if content.len() > MAX_WRITE_BYTES {
            bail!(
                "content size {} bytes exceeds maximum write size of {} bytes (10 MiB)",
                content.len(),
                MAX_WRITE_BYTES
            );
        }

        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.write().await;

        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).await?;
        }

        match mode {
            WriteMode::Overwrite => {
                fs::write(&abs, content).await?;
            }
            WriteMode::Append => {
                let existing = if abs.exists() {
                    fs::read_to_string(&abs).await?
                } else {
                    String::new()
                };
                let mut combined = existing;
                combined.push_str(content);
                fs::write(&abs, combined).await?;
            }
            WriteMode::Prepend => {
                let existing = if abs.exists() {
                    fs::read_to_string(&abs).await?
                } else {
                    String::new()
                };
                let mut combined = content.to_string();
                combined.push_str(&existing);
                fs::write(&abs, combined).await?;
            }
        }

        Ok(())
    }

    /// Delete a note.
    pub async fn delete_note(&self, path: &str) -> Result<()> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.write().await;
        fs::remove_file(&abs)
            .await
            .with_context(|| format!("deleting {}", abs.display()))?;
        Ok(())
    }

    // -- frontmatter --------------------------------------------------------

    /// Get the parsed YAML frontmatter of a note.
    pub async fn get_frontmatter(&self, path: &str) -> Result<Option<serde_yaml::Value>> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.read().await;
        let content = fs::read_to_string(&abs).await?;
        Ok(parse_frontmatter(&content))
    }

    /// Set a single key in the frontmatter, creating frontmatter if absent.
    pub async fn set_frontmatter(
        &self,
        path: &str,
        key: &str,
        value: serde_yaml::Value,
    ) -> Result<()> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.write().await;
        let content = fs::read_to_string(&abs).await?;
        let (mut fm, body) = split_frontmatter(&content);

        let mapping = fm
            .get_or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

        if let serde_yaml::Value::Mapping(ref mut map) = mapping {
            map.insert(serde_yaml::Value::String(key.to_string()), value);
        } else {
            bail!("frontmatter is not a YAML mapping");
        }

        let new_content = render_with_frontmatter(Some(mapping), &body);
        fs::write(&abs, new_content).await?;
        Ok(())
    }

    /// Delete a key from the frontmatter.
    pub async fn delete_frontmatter(&self, path: &str, key: &str) -> Result<()> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.write().await;
        let content = fs::read_to_string(&abs).await?;
        let (fm, body) = split_frontmatter(&content);

        if let Some(mut fm_val) = fm {
            if let serde_yaml::Value::Mapping(ref mut map) = fm_val {
                map.remove(&serde_yaml::Value::String(key.to_string()));
            }
            let fm_ref = if let serde_yaml::Value::Mapping(ref map) = fm_val {
                if map.is_empty() {
                    None
                } else {
                    Some(&fm_val)
                }
            } else {
                Some(&fm_val)
            };
            let new_content = render_with_frontmatter(fm_ref, &body);
            fs::write(&abs, new_content).await?;
        }

        Ok(())
    }

    // -- tags ---------------------------------------------------------------

    /// Get all tags from a note (frontmatter `tags` array + inline `#tag`).
    pub async fn get_tags(&self, path: &str) -> Result<Vec<String>> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.read().await;
        let content = fs::read_to_string(&abs).await?;
        Ok(extract_tags(&content))
    }

    /// Add a tag to the frontmatter `tags` array.
    pub async fn add_tag(&self, path: &str, tag: &str) -> Result<()> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.write().await;
        let content = fs::read_to_string(&abs).await?;
        let (mut fm, body) = split_frontmatter(&content);

        let mapping = fm
            .get_or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

        if let serde_yaml::Value::Mapping(ref mut map) = mapping {
            let tags_key = serde_yaml::Value::String("tags".to_string());
            let tags_entry = map
                .entry(tags_key)
                .or_insert_with(|| serde_yaml::Value::Sequence(vec![]));

            if let serde_yaml::Value::Sequence(ref mut seq) = tags_entry {
                let tag_val = serde_yaml::Value::String(tag.to_string());
                if !seq.contains(&tag_val) {
                    seq.push(tag_val);
                }
            }
        }

        let new_content = render_with_frontmatter(Some(mapping), &body);
        fs::write(&abs, new_content).await?;
        Ok(())
    }

    /// Remove a tag from the frontmatter `tags` array.
    pub async fn remove_tag(&self, path: &str, tag: &str) -> Result<()> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.write().await;
        let content = fs::read_to_string(&abs).await?;
        let (fm, body) = split_frontmatter(&content);

        if let Some(mut fm_val) = fm {
            if let serde_yaml::Value::Mapping(ref mut map) = fm_val {
                let tags_key = serde_yaml::Value::String("tags".to_string());
                if let Some(serde_yaml::Value::Sequence(ref mut seq)) = map.get_mut(&tags_key) {
                    seq.retain(|v| v != &serde_yaml::Value::String(tag.to_string()));
                }
            }
            let new_content = render_with_frontmatter(Some(&fm_val), &body);
            fs::write(&abs, new_content).await?;
        }

        Ok(())
    }

    // -- search / list ------------------------------------------------------

    /// Search vault notes for a query string or regex pattern.
    pub async fn search(
        &self,
        query: &str,
        is_regex: bool,
        path_prefix: Option<&str>,
        tag: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<SearchResult>> {
        let compiled_regex = if is_regex {
            Some(Regex::new(query).context("invalid regex pattern")?)
        } else {
            None
        };

        let search_root = if let Some(prefix) = path_prefix {
            self.resolve_path(prefix)?
        } else {
            self.base_path().canonicalize()?
        };

        let mut results = Vec::new();
        let max = if max_results == 0 { usize::MAX } else { max_results };

        let walker = self.build_walker(&search_root);

        for entry in walker {
            if results.len() >= max {
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }
            let entry_path = entry.path();
            if !self.has_matching_extension(entry_path) {
                continue;
            }

            let content = match fs::read_to_string(entry_path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Optional tag filter.
            if let Some(required_tag) = tag {
                let tags = extract_tags(&content);
                if !tags.iter().any(|t| t == required_tag) {
                    continue;
                }
            }

            let lines: Vec<&str> = content.lines().collect();

            for (i, line) in lines.iter().enumerate() {
                if results.len() >= max {
                    break;
                }
                let matched = if let Some(ref re) = compiled_regex {
                    re.is_match(line)
                } else {
                    line.contains(query)
                };

                if matched {
                    let context_start = i.saturating_sub(2);
                    let context_end = (i + 3).min(lines.len());
                    let context = lines[context_start..context_end].join("\n");

                    results.push(SearchResult {
                        path: self.relative_path(entry_path),
                        line_number: i + 1,
                        line: line.to_string(),
                        context,
                    });
                }
            }
        }

        Ok(results)
    }

    /// List notes and directories under a vault path.
    pub async fn list_notes(
        &self,
        path: Option<&str>,
        recursive: bool,
    ) -> Result<Vec<NoteEntry>> {
        let list_root = if let Some(p) = path {
            self.resolve_path(p)?
        } else {
            self.base_path().canonicalize()?
        };

        if !list_root.is_dir() {
            bail!("{} is not a directory", list_root.display());
        }

        let mut entries = Vec::new();

        if recursive {
            let walker = self.build_walker(&list_root);
            for entry in walker {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let p = entry.path();
                if p == list_root {
                    continue;
                }
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

                if !is_dir && !self.has_matching_extension(p) {
                    continue;
                }

                let size = if is_dir {
                    None
                } else {
                    fs::metadata(p).await.ok().map(|m| m.len())
                };

                entries.push(NoteEntry {
                    path: self.relative_path(p),
                    is_dir,
                    size,
                });
            }
        } else {
            let mut read_dir = fs::read_dir(&list_root).await?;
            while let Some(entry) = read_dir.next_entry().await? {
                let p = entry.path();
                let meta = entry.metadata().await?;
                let is_dir = meta.is_dir();

                if is_dir {
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        if self.config.exclude_dirs.iter().any(|e| e == name) {
                            continue;
                        }
                    }
                }

                if !is_dir && !self.has_matching_extension(&p) {
                    continue;
                }

                let size = if is_dir { None } else { Some(meta.len()) };

                entries.push(NoteEntry {
                    path: self.relative_path(&p),
                    is_dir,
                    size,
                });
            }
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    // -- move ---------------------------------------------------------------

    /// Move (rename) a note within the vault.
    pub async fn move_note(
        &self,
        path: &str,
        new_path: &str,
        overwrite: bool,
    ) -> Result<(String, String)> {
        let abs_from = self.resolve_path(path)?;
        let abs_to = self.resolve_path(new_path)?;

        // Lock both paths in deterministic order to prevent deadlocks.
        let (lock_a, lock_b) = if abs_from <= abs_to {
            (self.path_lock(&abs_from).await, self.path_lock(&abs_to).await)
        } else {
            (self.path_lock(&abs_to).await, self.path_lock(&abs_from).await)
        };
        let _guard_a = lock_a.write().await;
        let _guard_b = lock_b.write().await;

        if !abs_from.is_file() {
            bail!("source '{}' does not exist or is not a file", abs_from.display());
        }

        if !overwrite && abs_to.exists() {
            bail!(
                "destination '{}' already exists (set overwrite=true to replace)",
                abs_to.display()
            );
        }

        if let Some(parent) = abs_to.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::rename(&abs_from, &abs_to)
            .await
            .with_context(|| format!("moving {} -> {}", abs_from.display(), abs_to.display()))?;

        Ok((self.relative_path(&abs_from), self.relative_path(&abs_to)))
    }

    // -- batch read ---------------------------------------------------------

    /// Read multiple notes in a single call. Partial failures are captured
    /// per-path rather than aborting the entire batch.
    pub async fn batch_read(
        &self,
        paths: &[String],
        include_content: bool,
        include_frontmatter: bool,
    ) -> Result<Vec<serde_json::Value>> {
        if paths.len() > 10 {
            bail!("batch_read accepts at most 10 paths, got {}", paths.len());
        }

        let mut results = Vec::with_capacity(paths.len());

        for path in paths {
            match self.read_note(path).await {
                Ok(note) => {
                    let content = if include_content {
                        serde_json::Value::String(note.content)
                    } else {
                        serde_json::Value::Null
                    };
                    let frontmatter = if include_frontmatter {
                        match note.frontmatter {
                            Some(fm) => serde_json::to_value(&fm).unwrap_or(serde_json::Value::Null),
                            None => serde_json::Value::Null,
                        }
                    } else {
                        serde_json::Value::Null
                    };
                    results.push(serde_json::json!({
                        "path": note.path,
                        "content": content,
                        "frontmatter": frontmatter,
                        "error": null,
                    }));
                }
                Err(e) => {
                    results.push(serde_json::json!({
                        "path": path,
                        "content": null,
                        "frontmatter": null,
                        "error": format!("{:#}", e),
                    }));
                }
            }
        }

        Ok(results)
    }

    // -- stats --------------------------------------------------------------

    /// Compute aggregate statistics about the vault.
    pub async fn stats(&self, recent_count: usize) -> Result<serde_json::Value> {
        let canon_root = self.base_path().canonicalize()?;
        let walker = self.build_walker(&canon_root);

        let mut total_notes: u64 = 0;
        let mut total_folders: u64 = 0;
        let mut total_size_bytes: u64 = 0;

        // (relative_path, size_bytes, mtime)
        let mut recent: Vec<(String, u64, std::time::SystemTime)> = Vec::new();

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let p = entry.path();
            if p == canon_root {
                continue;
            }

            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

            if is_dir {
                total_folders += 1;
                continue;
            }

            if !self.has_matching_extension(p) {
                continue;
            }

            let meta = match fs::metadata(p).await {
                Ok(m) => m,
                Err(_) => continue,
            };

            let size = meta.len();
            total_notes += 1;
            total_size_bytes += size;

            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            recent.push((self.relative_path(p), size, mtime));
        }

        // Sort by most recently modified first.
        recent.sort_by(|a, b| b.2.cmp(&a.2));
        recent.truncate(recent_count);

        let recent_files: Vec<serde_json::Value> = recent
            .into_iter()
            .map(|(path, size, mtime)| {
                let dt: chrono::DateTime<chrono::Utc> = mtime.into();
                serde_json::json!({
                    "path": path,
                    "size_bytes": size,
                    "modified": dt.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "total_notes": total_notes,
            "total_folders": total_folders,
            "total_size_bytes": total_size_bytes,
            "recent_files": recent_files,
        }))
    }

    // -- search and replace -------------------------------------------------

    /// Perform a search-and-replace within a single note. Returns the number
    /// of replacements made.
    pub async fn search_replace(
        &self,
        path: &str,
        search: &str,
        replace: &str,
        is_regex: bool,
    ) -> Result<usize> {
        let abs = self.resolve_path(path)?;
        let lock = self.path_lock(&abs).await;
        let _guard = lock.write().await;
        let content = fs::read_to_string(&abs).await?;

        let (new_content, count) = if is_regex {
            let re = Regex::new(search).context("invalid regex pattern")?;
            let mut count = 0usize;
            let replaced = re.replace_all(&content, |caps: &regex::Captures| {
                count += 1;
                let mut dst = String::new();
                caps.expand(replace, &mut dst);
                dst
            });
            (replaced.into_owned(), count)
        } else {
            let count = content.matches(search).count();
            (content.replace(search, replace), count)
        };

        if count > 0 {
            fs::write(&abs, new_content).await?;
        }

        Ok(count)
    }

    // -- internal helpers ---------------------------------------------------

    fn build_walker(&self, root: &Path) -> ignore::Walk {
        let mut builder = ignore::WalkBuilder::new(root);
        builder.hidden(false);
        builder.git_ignore(false);
        builder.git_global(false);
        builder.git_exclude(false);

        let exclude_dirs = self.config.exclude_dirs.clone();
        builder.filter_entry(move |entry| {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                if let Some(name) = entry.file_name().to_str() {
                    if exclude_dirs.iter().any(|e| e == name) {
                        return false;
                    }
                }
            }
            true
        });

        builder.build()
    }

    fn has_matching_extension(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| self.config.extensions.iter().any(|e| e == ext))
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Frontmatter helpers (pure functions)
// ---------------------------------------------------------------------------

/// Parse YAML frontmatter from the beginning of a markdown file.
fn parse_frontmatter(content: &str) -> Option<serde_yaml::Value> {
    let (fm, _) = split_frontmatter(content);
    fm
}

/// Split content into (optional frontmatter Value, body text).
fn split_frontmatter(content: &str) -> (Option<serde_yaml::Value>, String) {
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
            let yaml_str = &rest[..yaml_end];
            let body = &rest[body_start..];
            let fm: Option<serde_yaml::Value> = serde_yaml::from_str(yaml_str).ok();
            (fm, body.to_string())
        }
        None => (None, content.to_string()),
    }
}

/// Reconstruct file content from frontmatter and body.
fn render_with_frontmatter(fm: Option<&serde_yaml::Value>, body: &str) -> String {
    match fm {
        Some(val) => {
            let yaml = serde_yaml::to_string(val).unwrap_or_default();
            let yaml = yaml.trim_end_matches('\n');
            format!("---\n{}\n---\n{}", yaml, body)
        }
        None => body.to_string(),
    }
}

/// Extract tags from frontmatter `tags` array and inline `#tag` patterns.
fn extract_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();

    // From frontmatter.
    if let Some(fm) = parse_frontmatter(content) {
        if let Some(serde_yaml::Value::Sequence(seq)) =
            fm.get(&serde_yaml::Value::String("tags".to_string()))
        {
            for val in seq {
                if let serde_yaml::Value::String(s) = val {
                    if !tags.contains(s) {
                        tags.push(s.clone());
                    }
                }
            }
        }
    }

    // From inline #tags in body text (skip fenced code blocks).
    let (_, body) = split_frontmatter(content);
    let tag_re = Regex::new(r"(?:^|[\s(])(#[a-zA-Z][a-zA-Z0-9_/-]*)").unwrap();
    let mut in_code_block = false;
    for line in body.lines() {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        for cap in tag_re.captures_iter(line) {
            let tag = cap[1].trim_start_matches('#').to_string();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }

    tags
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test VaultConfig pointing at the temp dir.
    fn test_config(root: &Path) -> VaultConfig {
        VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![
                ".obsidian".to_string(),
                ".trash".to_string(),
                ".git".to_string(),
            ],
        }
    }

    async fn setup_vault() -> (TempDir, Arc<VaultManager>) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();

        let notes_dir = root.join("notes");
        fs::create_dir_all(&notes_dir).await.unwrap();
        fs::create_dir_all(root.join(".obsidian")).await.unwrap();

        fs::write(
            root.join("hello.md"),
            "---\ntitle: Hello World\ntags:\n  - greeting\n  - example\n---\n# Hello\n\nThis is a #test note with #inline-tags.\n",
        )
        .await
        .unwrap();

        fs::write(
            root.join("notes/second.md"),
            "---\nauthor: alice\n---\nSome content here.\n",
        )
        .await
        .unwrap();

        fs::write(
            root.join("notes/plain.md"),
            "No frontmatter here.\nJust plain text.\n",
        )
        .await
        .unwrap();

        fs::write(root.join(".obsidian/config.json"), "{}").await.unwrap();

        let cfg = test_config(&root);
        let vm = VaultManager::new(&cfg).unwrap();
        (dir, vm)
    }

    #[tokio::test]
    async fn test_resolve_path_normal() {
        let (_dir, vm) = setup_vault().await;
        let p = vm.resolve_path("hello.md").unwrap();
        assert!(p.exists());
        assert!(p.ends_with("hello.md"));
    }

    #[tokio::test]
    async fn test_resolve_path_rejects_traversal() {
        let (_dir, vm) = setup_vault().await;
        let err = vm.resolve_path("../etc/passwd");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("path traversal"));
    }

    #[tokio::test]
    async fn test_resolve_path_subdirectory() {
        let (_dir, vm) = setup_vault().await;
        let p = vm.resolve_path("notes/second.md").unwrap();
        assert!(p.exists());
    }

    #[tokio::test]
    async fn test_read_note_with_frontmatter() {
        let (_dir, vm) = setup_vault().await;
        let note = vm.read_note("hello.md").await.unwrap();
        assert_eq!(note.path, "hello.md");
        assert!(note.frontmatter.is_some());
        let fm = note.frontmatter.unwrap();
        assert_eq!(fm.get("title").and_then(|v| v.as_str()), Some("Hello World"));
    }

    #[tokio::test]
    async fn test_read_note_without_frontmatter() {
        let (_dir, vm) = setup_vault().await;
        let note = vm.read_note("notes/plain.md").await.unwrap();
        assert!(note.frontmatter.is_none());
        assert!(note.content.contains("No frontmatter here."));
    }

    #[tokio::test]
    async fn test_write_note_overwrite() {
        let (_dir, vm) = setup_vault().await;
        vm.write_note("new.md", "# New Note\n", WriteMode::Overwrite)
            .await
            .unwrap();
        let note = vm.read_note("new.md").await.unwrap();
        assert_eq!(note.content, "# New Note\n");
    }

    #[tokio::test]
    async fn test_write_note_append() {
        let (_dir, vm) = setup_vault().await;
        vm.write_note("notes/plain.md", "\nAppended.\n", WriteMode::Append)
            .await
            .unwrap();
        let note = vm.read_note("notes/plain.md").await.unwrap();
        assert!(note.content.ends_with("Appended.\n"));
        assert!(note.content.starts_with("No frontmatter here."));
    }

    #[tokio::test]
    async fn test_write_note_prepend() {
        let (_dir, vm) = setup_vault().await;
        vm.write_note("notes/plain.md", "Prepended!\n", WriteMode::Prepend)
            .await
            .unwrap();
        let note = vm.read_note("notes/plain.md").await.unwrap();
        assert!(note.content.starts_with("Prepended!"));
    }

    #[tokio::test]
    async fn test_delete_note() {
        let (_dir, vm) = setup_vault().await;
        vm.write_note("to-delete.md", "bye", WriteMode::Overwrite)
            .await
            .unwrap();
        vm.delete_note("to-delete.md").await.unwrap();
        assert!(vm.read_note("to-delete.md").await.is_err());
    }

    #[tokio::test]
    async fn test_get_frontmatter() {
        let (_dir, vm) = setup_vault().await;
        let fm = vm.get_frontmatter("hello.md").await.unwrap();
        assert!(fm.is_some());
        let fm = fm.unwrap();
        assert_eq!(fm.get("title").and_then(|v| v.as_str()), Some("Hello World"));
    }

    #[tokio::test]
    async fn test_set_frontmatter() {
        let (_dir, vm) = setup_vault().await;
        vm.set_frontmatter(
            "notes/second.md",
            "status",
            serde_yaml::Value::String("draft".to_string()),
        )
        .await
        .unwrap();

        let fm = vm.get_frontmatter("notes/second.md").await.unwrap().unwrap();
        assert_eq!(fm.get("status").and_then(|v| v.as_str()), Some("draft"));
        assert_eq!(fm.get("author").and_then(|v| v.as_str()), Some("alice"));
    }

    #[tokio::test]
    async fn test_set_frontmatter_creates_if_absent() {
        let (_dir, vm) = setup_vault().await;
        vm.set_frontmatter(
            "notes/plain.md",
            "category",
            serde_yaml::Value::String("misc".to_string()),
        )
        .await
        .unwrap();

        let fm = vm.get_frontmatter("notes/plain.md").await.unwrap().unwrap();
        assert_eq!(fm.get("category").and_then(|v| v.as_str()), Some("misc"));

        let note = vm.read_note("notes/plain.md").await.unwrap();
        assert!(note.content.contains("No frontmatter here."));
    }

    #[tokio::test]
    async fn test_delete_frontmatter() {
        let (_dir, vm) = setup_vault().await;
        vm.delete_frontmatter("notes/second.md", "author").await.unwrap();

        let fm = vm.get_frontmatter("notes/second.md").await.unwrap();
        // Mapping was the only key, so frontmatter removed entirely.
        assert!(fm.is_none() || fm.as_ref().unwrap().as_mapping().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_tags() {
        let (_dir, vm) = setup_vault().await;
        let tags = vm.get_tags("hello.md").await.unwrap();
        assert!(tags.contains(&"greeting".to_string()));
        assert!(tags.contains(&"example".to_string()));
        assert!(tags.contains(&"test".to_string()));
        assert!(tags.contains(&"inline-tags".to_string()));
    }

    #[tokio::test]
    async fn test_add_tag() {
        let (_dir, vm) = setup_vault().await;
        vm.add_tag("hello.md", "new-tag").await.unwrap();
        let tags = vm.get_tags("hello.md").await.unwrap();
        assert!(tags.contains(&"new-tag".to_string()));
        assert!(tags.contains(&"greeting".to_string()));
    }

    #[tokio::test]
    async fn test_add_tag_idempotent() {
        let (_dir, vm) = setup_vault().await;
        vm.add_tag("hello.md", "greeting").await.unwrap();
        let fm = vm.get_frontmatter("hello.md").await.unwrap().unwrap();
        let tags = fm.get("tags").unwrap().as_sequence().unwrap();
        let count = tags.iter().filter(|v| v.as_str() == Some("greeting")).count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_remove_tag() {
        let (_dir, vm) = setup_vault().await;
        vm.remove_tag("hello.md", "example").await.unwrap();
        let fm = vm.get_frontmatter("hello.md").await.unwrap().unwrap();
        let tags = fm.get("tags").unwrap().as_sequence().unwrap();
        assert!(!tags.iter().any(|v| v.as_str() == Some("example")));
        assert!(tags.iter().any(|v| v.as_str() == Some("greeting")));
    }

    #[tokio::test]
    async fn test_search_plain_text() {
        let (_dir, vm) = setup_vault().await;
        let results = vm.search("plain text", false, None, None, 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].path, "notes/plain.md");
    }

    #[tokio::test]
    async fn test_search_regex() {
        let (_dir, vm) = setup_vault().await;
        let results = vm.search(r"#\s+Hello", true, None, None, 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].path, "hello.md");
    }

    #[tokio::test]
    async fn test_search_with_path_prefix() {
        let (_dir, vm) = setup_vault().await;
        let results = vm
            .search("content", false, Some("notes"), None, 10)
            .await
            .unwrap();
        for r in &results {
            assert!(r.path.starts_with("notes/"));
        }
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_search_with_tag_filter() {
        let (_dir, vm) = setup_vault().await;
        let results = vm
            .search("note", false, None, Some("greeting"), 10)
            .await
            .unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().all(|r| r.path == "hello.md"));
    }

    #[tokio::test]
    async fn test_search_max_results() {
        let (_dir, vm) = setup_vault().await;
        let results = vm.search("e", false, None, None, 1).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_search_excludes_obsidian_dir() {
        let (_dir, vm) = setup_vault().await;
        let results = vm.search("{}", false, None, None, 100).await.unwrap();
        assert!(results.iter().all(|r| !r.path.contains(".obsidian")));
    }

    #[tokio::test]
    async fn test_list_notes_root() {
        let (_dir, vm) = setup_vault().await;
        let entries = vm.list_notes(None, false).await.unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"hello.md"));
        assert!(paths.contains(&"notes"));
        assert!(!paths.iter().any(|p| p.contains(".obsidian")));
    }

    #[tokio::test]
    async fn test_list_notes_recursive() {
        let (_dir, vm) = setup_vault().await;
        let entries = vm.list_notes(None, true).await.unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"hello.md"));
        assert!(paths.iter().any(|p| p.contains("notes/second.md")));
        assert!(paths.iter().any(|p| p.contains("notes/plain.md")));
    }

    #[tokio::test]
    async fn test_list_notes_subdirectory() {
        let (_dir, vm) = setup_vault().await;
        let entries = vm.list_notes(Some("notes"), false).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.path.starts_with("notes/")));
    }

    #[tokio::test]
    async fn test_search_replace_plain() {
        let (_dir, vm) = setup_vault().await;
        let count = vm
            .search_replace("notes/plain.md", "plain text", "PLAIN TEXT", false)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let note = vm.read_note("notes/plain.md").await.unwrap();
        assert!(note.content.contains("PLAIN TEXT"));
        assert!(!note.content.contains("plain text"));
    }

    #[tokio::test]
    async fn test_search_replace_regex() {
        let (_dir, vm) = setup_vault().await;
        let count = vm
            .search_replace("notes/plain.md", r"(\w+) text", "replaced_$1", true)
            .await
            .unwrap();
        assert!(count >= 1);
        let note = vm.read_note("notes/plain.md").await.unwrap();
        assert!(note.content.contains("replaced_plain"));
    }

    #[tokio::test]
    async fn test_search_replace_no_match() {
        let (_dir, vm) = setup_vault().await;
        let count = vm
            .search_replace("notes/plain.md", "nonexistent", "replacement", false)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_write_creates_parent_dirs() {
        let (_dir, vm) = setup_vault().await;
        vm.write_note("deep/nested/dir/note.md", "deep note", WriteMode::Overwrite)
            .await
            .unwrap();
        let note = vm.read_note("deep/nested/dir/note.md").await.unwrap();
        assert_eq!(note.content, "deep note");
    }

    #[tokio::test]
    async fn test_frontmatter_roundtrip() {
        let (_dir, vm) = setup_vault().await;
        vm.write_note("roundtrip.md", "Body text here.\n", WriteMode::Overwrite)
            .await
            .unwrap();

        vm.set_frontmatter(
            "roundtrip.md",
            "title",
            serde_yaml::Value::String("Test".to_string()),
        )
        .await
        .unwrap();

        let note = vm.read_note("roundtrip.md").await.unwrap();
        assert!(note.content.contains("---\n"));
        assert!(note.content.contains("Body text here."));
        assert!(note.frontmatter.is_some());
    }

    #[tokio::test]
    async fn test_extract_tags_inline_only() {
        let content = "# My Note\n\nThis has #rust and #async tags.\n";
        let tags = extract_tags(content);
        assert!(tags.contains(&"rust".to_string()));
        assert!(tags.contains(&"async".to_string()));
    }

    #[tokio::test]
    async fn test_extract_tags_skips_code_blocks() {
        let content = "# My Note\n\n```\n#not-a-tag\n```\n\n#real-tag\n";
        let tags = extract_tags(content);
        assert!(!tags.contains(&"not-a-tag".to_string()));
        assert!(tags.contains(&"real-tag".to_string()));
    }

    #[tokio::test]
    async fn test_note_entry_size() {
        let (_dir, vm) = setup_vault().await;
        let entries = vm.list_notes(Some("notes"), false).await.unwrap();
        for entry in entries {
            assert!(!entry.is_dir);
            assert!(entry.size.is_some());
            assert!(entry.size.unwrap() > 0);
        }
    }

    #[tokio::test]
    async fn test_new_disabled_returns_none() {
        let cfg = VaultConfig {
            enabled: false,
            path: PathBuf::from("/tmp"),
            extensions: vec![],
            exclude_dirs: vec![],
        };
        assert!(VaultManager::new(&cfg).is_none());
    }

    #[tokio::test]
    async fn test_new_nonexistent_path_returns_none() {
        let cfg = VaultConfig {
            enabled: true,
            path: PathBuf::from("/nonexistent/vault/path"),
            extensions: vec![],
            exclude_dirs: vec![],
        };
        assert!(VaultManager::new(&cfg).is_none());
    }

    #[tokio::test]
    async fn test_write_note_at_size_limit() {
        let (_dir, vm) = setup_vault().await;
        // Exactly at the 10 MiB limit should succeed.
        let content = "x".repeat(MAX_WRITE_BYTES);
        vm.write_note("big.md", &content, WriteMode::Overwrite)
            .await
            .unwrap();
        let note = vm.read_note("big.md").await.unwrap();
        assert_eq!(note.content.len(), MAX_WRITE_BYTES);
    }

    #[tokio::test]
    async fn test_write_note_over_size_limit() {
        let (_dir, vm) = setup_vault().await;
        // One byte over the 10 MiB limit should fail.
        let content = "x".repeat(MAX_WRITE_BYTES + 1);
        let err = vm
            .write_note("toobig.md", &content, WriteMode::Overwrite)
            .await;
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("exceeds maximum write size"));
    }

    // --- move_note tests ---

    #[tokio::test]
    async fn test_move_note_basic() {
        let (_dir, vm) = setup_vault().await;
        vm.write_note("moveme.md", "move content", WriteMode::Overwrite)
            .await
            .unwrap();
        let (from, to) = vm.move_note("moveme.md", "moved.md", false).await.unwrap();
        assert_eq!(from, "moveme.md");
        assert_eq!(to, "moved.md");
        // Old path should not exist.
        assert!(vm.read_note("moveme.md").await.is_err());
        // New path should have the content.
        let note = vm.read_note("moved.md").await.unwrap();
        assert_eq!(note.content, "move content");
    }

    #[tokio::test]
    async fn test_move_note_creates_parent_dirs() {
        let (_dir, vm) = setup_vault().await;
        vm.write_note("flat.md", "data", WriteMode::Overwrite)
            .await
            .unwrap();
        let (_from, to) = vm
            .move_note("flat.md", "deep/sub/dir/flat.md", false)
            .await
            .unwrap();
        assert_eq!(to, "deep/sub/dir/flat.md");
        let note = vm.read_note("deep/sub/dir/flat.md").await.unwrap();
        assert_eq!(note.content, "data");
    }

    #[tokio::test]
    async fn test_move_note_rejects_nonexistent_source() {
        let (_dir, vm) = setup_vault().await;
        let err = vm.move_note("nonexistent.md", "dest.md", false).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_move_note_rejects_overwrite_without_flag() {
        let (_dir, vm) = setup_vault().await;
        // Both source and dest exist.
        let err = vm.move_note("hello.md", "notes/second.md", false).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_move_note_overwrite_allowed() {
        let (_dir, vm) = setup_vault().await;
        let (_, _) = vm.move_note("hello.md", "notes/second.md", true).await.unwrap();
        let note = vm.read_note("notes/second.md").await.unwrap();
        assert!(note.content.contains("Hello"));
    }

    // --- batch_read tests ---

    #[tokio::test]
    async fn test_batch_read_all_success() {
        let (_dir, vm) = setup_vault().await;
        let results = vm
            .batch_read(
                &["hello.md".to_string(), "notes/plain.md".to_string()],
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0]["error"].is_null());
        assert!(results[1]["error"].is_null());
        assert!(results[0]["content"].as_str().unwrap().contains("Hello"));
        assert!(results[1]["content"].as_str().unwrap().contains("No frontmatter"));
    }

    #[tokio::test]
    async fn test_batch_read_partial_failure() {
        let (_dir, vm) = setup_vault().await;
        let results = vm
            .batch_read(
                &["hello.md".to_string(), "nonexistent.md".to_string()],
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0]["error"].is_null());
        assert!(!results[1]["error"].is_null());
        assert_eq!(results[1]["path"].as_str(), Some("nonexistent.md"));
    }

    #[tokio::test]
    async fn test_batch_read_exceeds_max_paths() {
        let (_dir, vm) = setup_vault().await;
        let paths: Vec<String> = (0..11).map(|i| format!("note{}.md", i)).collect();
        let err = vm.batch_read(&paths, true, true).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("at most 10"));
    }

    #[tokio::test]
    async fn test_batch_read_exclude_content() {
        let (_dir, vm) = setup_vault().await;
        let results = vm
            .batch_read(&["hello.md".to_string()], false, true)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0]["content"].is_null());
        // Frontmatter should still be present.
        assert!(!results[0]["frontmatter"].is_null());
    }

    #[tokio::test]
    async fn test_batch_read_exclude_frontmatter() {
        let (_dir, vm) = setup_vault().await;
        let results = vm
            .batch_read(&["hello.md".to_string()], true, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(!results[0]["content"].is_null());
        assert!(results[0]["frontmatter"].is_null());
    }

    // --- stats tests ---

    #[tokio::test]
    async fn test_stats_basic() {
        let (_dir, vm) = setup_vault().await;
        let stats = vm.stats(10).await.unwrap();
        // We have 3 .md files: hello.md, notes/second.md, notes/plain.md
        assert_eq!(stats["total_notes"].as_u64(), Some(3));
        // 1 directory: notes (not .obsidian which is excluded)
        assert_eq!(stats["total_folders"].as_u64(), Some(1));
        assert!(stats["total_size_bytes"].as_u64().unwrap() > 0);
        let recent = stats["recent_files"].as_array().unwrap();
        assert_eq!(recent.len(), 3);
        // Each recent entry should have path, size_bytes, modified.
        for entry in recent {
            assert!(entry["path"].as_str().is_some());
            assert!(entry["size_bytes"].as_u64().is_some());
            assert!(entry["modified"].as_str().is_some());
        }
    }

    #[tokio::test]
    async fn test_stats_recent_count_limit() {
        let (_dir, vm) = setup_vault().await;
        let stats = vm.stats(1).await.unwrap();
        let recent = stats["recent_files"].as_array().unwrap();
        assert_eq!(recent.len(), 1);
    }

    #[tokio::test]
    async fn test_stats_empty_vault() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let cfg = test_config(&root);
        let vm = VaultManager::new(&cfg).unwrap();
        let stats = vm.stats(10).await.unwrap();
        assert_eq!(stats["total_notes"].as_u64(), Some(0));
        assert_eq!(stats["total_folders"].as_u64(), Some(0));
        assert_eq!(stats["total_size_bytes"].as_u64(), Some(0));
        assert_eq!(stats["recent_files"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_concurrent_writes_no_data_loss() {
        let dir = TempDir::new().unwrap();
        let vault_path = dir.path().to_path_buf();

        // Create initial file
        std::fs::write(vault_path.join("test.md"), "").unwrap();

        let config = VaultConfig {
            enabled: true,
            path: vault_path,
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::new(&config).unwrap();

        let mut handles = vec![];
        for i in 0..10 {
            let vm = vm.clone();
            handles.push(tokio::spawn(async move {
                vm.write_note(
                    &format!("test-{}.md", i),
                    &format!("content-{}", i),
                    WriteMode::Overwrite,
                )
                .await
                .unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // Verify all 10 files exist with correct content
        for i in 0..10 {
            let content = vm.read_note(&format!("test-{}.md", i)).await.unwrap();
            assert!(content.content.contains(&format!("content-{}", i)));
        }
    }

    // --- scope tests ---

    #[tokio::test]
    async fn test_scoped_vault_read_write() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();
        vm.write_note("status.md", "# Status\nAll good.", WriteMode::Overwrite)
            .await
            .unwrap();

        // File should exist at vault_root/teams/alpha/status.md
        assert!(dir.path().join("teams/alpha/status.md").exists());

        let note = vm.read_note("status.md").await.unwrap();
        assert!(note.content.contains("All good"));
    }

    #[test]
    fn test_scoped_resolve_path_blocks_escape() {
        let dir = TempDir::new().unwrap();
        // Create the scope directory so resolve_path can canonicalize it
        std::fs::create_dir_all(dir.path().join("teams/alpha")).unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();
        let result = vm.resolve_path("../../other-team/secret.md");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_scoped_vault_list_stays_in_scope() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };

        // Write files at root level
        let root_vm = VaultManager::new(&config).unwrap();
        root_vm
            .write_note("root.md", "root content", WriteMode::Overwrite)
            .await
            .unwrap();
        root_vm
            .write_note("teams/alpha/scoped.md", "scoped content", WriteMode::Overwrite)
            .await
            .unwrap();

        // Scoped manager should only see files in teams/alpha
        let scoped_vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();
        let entries = scoped_vm.list_notes(None, true).await.unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"scoped.md"));
        assert!(!paths.iter().any(|p| p.contains("root.md")));
    }

    #[tokio::test]
    async fn test_scoped_vault_search_stays_in_scope() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };

        let root_vm = VaultManager::new(&config).unwrap();
        root_vm
            .write_note("root.md", "findme", WriteMode::Overwrite)
            .await
            .unwrap();
        root_vm
            .write_note("teams/alpha/scoped.md", "findme", WriteMode::Overwrite)
            .await
            .unwrap();

        let scoped_vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();
        let results = scoped_vm.search("findme", false, None, None, 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "scoped.md");
    }

    // --- team isolation and concurrency tests ---

    #[tokio::test]
    async fn test_scoped_vaults_are_isolated() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };

        let alpha = VaultManager::with_scope(&config, "teams/alpha").unwrap();
        let beta = VaultManager::with_scope(&config, "teams/beta").unwrap();

        // Write a note in alpha scope
        alpha
            .write_note("secret.md", "Alpha's secret data", WriteMode::Overwrite)
            .await
            .unwrap();

        // Verify alpha can read it
        let note = alpha.read_note("secret.md").await.unwrap();
        assert!(note.content.contains("Alpha's secret"));

        // Verify beta CANNOT read it (file doesn't exist in beta scope)
        let result = beta.read_note("secret.md").await;
        assert!(result.is_err());

        // Verify beta's list doesn't show alpha's files
        let beta_list = beta.list_notes(None, true).await.unwrap();
        assert!(
            beta_list.is_empty()
                || !beta_list.iter().any(|e| e.path.contains("secret"))
        );
    }

    #[tokio::test]
    async fn test_scoped_vault_cannot_escape_via_traversal() {
        let dir = TempDir::new().unwrap();
        // Write a file at vault root
        std::fs::write(dir.path().join("root-secret.md"), "top secret").unwrap();

        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let scoped = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Try to escape scope with ../
        let result = scoped.read_note("../../root-secret.md").await;
        assert!(result.is_err());

        let result = scoped.read_note("../beta/secret.md").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_concurrent_team_writes_no_data_loss() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // 10 concurrent writers each writing their own file
        let mut handles = vec![];
        for i in 0..10 {
            let vm = vm.clone();
            handles.push(tokio::spawn(async move {
                vm.write_note(
                    &format!("task-{}.md", i),
                    &format!(
                        "---\nowner: agent-{}\nstatus: pending\n---\n# Task {}\n",
                        i, i
                    ),
                    WriteMode::Overwrite,
                )
                .await
                .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Verify all 10 files exist and have correct content
        for i in 0..10 {
            let note = vm.read_note(&format!("task-{}.md", i)).await.unwrap();
            assert!(note.content.contains(&format!("agent-{}", i)));
        }
    }

    #[tokio::test]
    async fn test_concurrent_cross_team_writes() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };

        let mut handles = vec![];
        for team_idx in 0..3u32 {
            let config = config.clone();
            let team_name = format!("teams/team-{}", team_idx);
            handles.push(tokio::spawn(async move {
                let vm = VaultManager::with_scope(&config, &team_name).unwrap();
                for i in 0..5u32 {
                    vm.write_note(
                        &format!("note-{}.md", i),
                        &format!("Team {} note {}", team_idx, i),
                        WriteMode::Overwrite,
                    )
                    .await
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Verify each team has exactly its own 5 notes
        for team_idx in 0..3u32 {
            let vm =
                VaultManager::with_scope(&config, &format!("teams/team-{}", team_idx))
                    .unwrap();
            let notes = vm.list_notes(None, true).await.unwrap();
            let md_notes: Vec<_> = notes.iter().filter(|n| n.path.ends_with(".md")).collect();
            assert_eq!(
                md_notes.len(),
                5,
                "team-{} should have 5 notes, got {}",
                team_idx,
                md_notes.len()
            );

            // Verify content is team-specific
            for i in 0..5u32 {
                let note = vm.read_note(&format!("note-{}.md", i)).await.unwrap();
                assert!(note.content.contains(&format!("Team {}", team_idx)));
            }
        }
    }

    #[tokio::test]
    async fn test_scoped_vault_search_isolation() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };

        let alpha = VaultManager::with_scope(&config, "teams/alpha").unwrap();
        let beta = VaultManager::with_scope(&config, "teams/beta").unwrap();

        alpha
            .write_note(
                "status.md",
                "ALPHA_MARKER: all systems go",
                WriteMode::Overwrite,
            )
            .await
            .unwrap();
        beta.write_note(
            "status.md",
            "BETA_MARKER: standing by",
            WriteMode::Overwrite,
        )
        .await
        .unwrap();

        // Search from alpha scope should only find ALPHA_MARKER
        let results = alpha.search("MARKER", false, None, None, 100).await.unwrap();
        assert!(results.iter().any(|r| r.line.contains("ALPHA_MARKER")));
        assert!(!results.iter().any(|r| r.line.contains("BETA_MARKER")));

        // Search from beta scope should only find BETA_MARKER
        let results = beta.search("MARKER", false, None, None, 100).await.unwrap();
        assert!(results.iter().any(|r| r.line.contains("BETA_MARKER")));
        assert!(!results.iter().any(|r| r.line.contains("ALPHA_MARKER")));
    }

    #[tokio::test]
    async fn test_scoped_vault_stats_isolation() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };

        let alpha = VaultManager::with_scope(&config, "teams/alpha").unwrap();
        let beta = VaultManager::with_scope(&config, "teams/beta").unwrap();

        // Write 3 notes in alpha, 1 note in beta
        for i in 0..3 {
            alpha
                .write_note(
                    &format!("doc-{}.md", i),
                    &format!("Alpha doc {}", i),
                    WriteMode::Overwrite,
                )
                .await
                .unwrap();
        }
        beta.write_note("single.md", "Beta doc", WriteMode::Overwrite)
            .await
            .unwrap();

        // Alpha stats should show 3 notes
        let alpha_stats = alpha.stats(10).await.unwrap();
        assert_eq!(alpha_stats["total_notes"].as_u64(), Some(3));

        // Beta stats should show 1 note
        let beta_stats = beta.stats(10).await.unwrap();
        assert_eq!(beta_stats["total_notes"].as_u64(), Some(1));

        // Alpha's recent files should not include beta's file
        let alpha_recent = alpha_stats["recent_files"].as_array().unwrap();
        assert!(!alpha_recent
            .iter()
            .any(|e| e["path"].as_str().unwrap().contains("single")));
    }

    // --- agent card discovery and lifecycle tests ---

    /// Helper: create a VaultConfig that includes .json files for agent cards.
    fn json_vault_config(root: &Path) -> VaultConfig {
        VaultConfig {
            enabled: true,
            path: root.to_path_buf(),
            extensions: vec!["md".to_string(), "json".to_string()],
            exclude_dirs: vec![],
        }
    }

    #[tokio::test]
    async fn test_agent_card_write_and_read() {
        let dir = TempDir::new().unwrap();
        let config = json_vault_config(dir.path());
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        let card = serde_json::json!({
            "name": "researcher",
            "role": "Research and code analysis",
            "skills": ["web_search", "code_analysis", "summarization"],
            "workspace_id": "abc-12345",
            "ip": "10.99.0.5",
            "status": "ready",
            "team": "alpha"
        });

        vm.write_note(
            "cards/researcher.json",
            &serde_json::to_string_pretty(&card).unwrap(),
            WriteMode::Overwrite,
        )
        .await
        .unwrap();

        let note = vm.read_note("cards/researcher.json").await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&note.content).unwrap();
        assert_eq!(parsed["name"], "researcher");
        assert_eq!(parsed["skills"].as_array().unwrap().len(), 3);
        assert_eq!(parsed["status"], "ready");
    }

    #[tokio::test]
    async fn test_agent_card_discovery_via_list() {
        let dir = TempDir::new().unwrap();
        let config = json_vault_config(dir.path());
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Register 3 agents
        for name in ["researcher", "coder", "reviewer"] {
            let card = serde_json::json!({
                "name": name,
                "role": format!("{} agent", name),
                "status": "ready",
                "team": "alpha"
            });
            vm.write_note(
                &format!("cards/{}.json", name),
                &serde_json::to_string(&card).unwrap(),
                WriteMode::Overwrite,
            )
            .await
            .unwrap();
        }

        // Discover all cards via list
        let entries = vm.list_notes(Some("cards"), false).await.unwrap();
        let json_files: Vec<_> = entries
            .iter()
            .filter(|e| e.path.ends_with(".json"))
            .collect();
        assert_eq!(json_files.len(), 3, "should discover 3 agent cards");

        // Read each card and verify
        for entry in &json_files {
            let note = vm.read_note(&entry.path).await.unwrap();
            let card: serde_json::Value = serde_json::from_str(&note.content).unwrap();
            assert_eq!(card["status"], "ready");
            assert_eq!(card["team"], "alpha");
        }
    }

    #[tokio::test]
    async fn test_agent_card_status_update_atomic() {
        let dir = TempDir::new().unwrap();
        let config = json_vault_config(dir.path());
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        let card = serde_json::json!({
            "name": "worker",
            "status": "ready",
            "current_task": null
        });
        vm.write_note(
            "cards/worker.json",
            &serde_json::to_string_pretty(&card).unwrap(),
            WriteMode::Overwrite,
        )
        .await
        .unwrap();

        // Update status atomically (read-modify-write)
        let note = vm.read_note("cards/worker.json").await.unwrap();
        let mut card: serde_json::Value = serde_json::from_str(&note.content).unwrap();
        card["status"] = serde_json::json!("working");
        card["current_task"] = serde_json::json!("task-42");
        vm.write_note(
            "cards/worker.json",
            &serde_json::to_string_pretty(&card).unwrap(),
            WriteMode::Overwrite,
        )
        .await
        .unwrap();

        // Verify update persisted
        let updated = vm.read_note("cards/worker.json").await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&updated.content).unwrap();
        assert_eq!(parsed["status"], "working");
        assert_eq!(parsed["current_task"], "task-42");
    }

    #[tokio::test]
    async fn test_agent_card_cross_team_invisible() {
        let dir = TempDir::new().unwrap();
        let config = json_vault_config(dir.path());

        let alpha = VaultManager::with_scope(&config, "teams/alpha").unwrap();
        let beta = VaultManager::with_scope(&config, "teams/beta").unwrap();

        // Alpha registers an agent
        let card = serde_json::json!({"name": "alpha-agent", "team": "alpha"});
        alpha
            .write_note(
                "cards/alpha-agent.json",
                &serde_json::to_string(&card).unwrap(),
                WriteMode::Overwrite,
            )
            .await
            .unwrap();

        // Beta cannot see alpha's cards via list (cards/ dir doesn't exist in beta scope)
        let beta_entries = beta.list_notes(Some("cards"), false).await;
        let beta_cards = beta_entries.unwrap_or_default();
        assert!(
            !beta_cards.iter().any(|c| c.path.contains("alpha-agent")),
            "beta should not see alpha's agent cards"
        );

        // Beta cannot read alpha's card directly (path is scoped)
        let result = beta.read_note("cards/alpha-agent.json").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_agent_card_search_by_skill() {
        let dir = TempDir::new().unwrap();
        let config = json_vault_config(dir.path());
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Register agents with different skills
        let cards = vec![
            ("coder", vec!["rust", "python"]),
            ("tester", vec!["testing", "python"]),
            ("reviewer", vec!["code_review", "rust"]),
        ];
        for (name, skills) in &cards {
            let card = serde_json::json!({"name": name, "skills": skills});
            vm.write_note(
                &format!("cards/{}.json", name),
                &serde_json::to_string(&card).unwrap(),
                WriteMode::Overwrite,
            )
            .await
            .unwrap();
        }

        // Search for agents with "rust" skill
        let results = vm
            .search("rust", false, Some("cards"), None, 100)
            .await
            .unwrap();
        let rust_agents: Vec<_> = results
            .iter()
            .filter(|r| r.path.contains("cards/") && r.line.contains("rust"))
            .collect();
        assert!(
            rust_agents.len() >= 2,
            "should find at least coder and reviewer, got {}",
            rust_agents.len()
        );
    }

    // --- task board tests ---

    #[tokio::test]
    async fn test_task_board_create_and_read_frontmatter() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        let task_content = "---\nstatus: pending\nowner: null\npriority: high\ndepends_on: []\ncreated_at: \"2026-02-19T22:00:00Z\"\n---\n# Implement feature X\n\nDetailed description here.\n";
        vm.write_note("tasks/task-001.md", task_content, WriteMode::Overwrite)
            .await
            .unwrap();

        let note = vm.read_note("tasks/task-001.md").await.unwrap();
        assert!(note.content.contains("Implement feature X"));

        // Verify frontmatter is parsed correctly
        let fm = note.frontmatter.as_ref().expect("frontmatter should be present");
        assert_eq!(fm.get("status").and_then(|v| v.as_str()), Some("pending"));
        assert_eq!(fm.get("priority").and_then(|v| v.as_str()), Some("high"));
        assert!(fm.get("owner").is_some(), "owner key should exist");
        // YAML null parses as Null variant
        assert!(fm.get("owner").unwrap().is_null(), "owner should be null");

        // Verify depends_on is an empty sequence
        let deps = fm.get("depends_on").and_then(|v| v.as_sequence());
        assert!(deps.is_some(), "depends_on should be a sequence");
        assert!(deps.unwrap().is_empty(), "depends_on should be empty");

        // Verify created_at timestamp
        assert_eq!(
            fm.get("created_at").and_then(|v| v.as_str()),
            Some("2026-02-19T22:00:00Z")
        );
    }

    #[tokio::test]
    async fn test_task_board_claim_via_frontmatter_update() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Create unclaimed task
        let task = "---\nstatus: pending\nowner: null\n---\n# Task 1\n";
        vm.write_note("tasks/task-001.md", task, WriteMode::Overwrite)
            .await
            .unwrap();

        // Claim it by updating frontmatter fields
        vm.set_frontmatter(
            "tasks/task-001.md",
            "owner",
            serde_yaml::Value::String("agent-coder".to_string()),
        )
        .await
        .unwrap();
        vm.set_frontmatter(
            "tasks/task-001.md",
            "status",
            serde_yaml::Value::String("in_progress".to_string()),
        )
        .await
        .unwrap();

        // Verify claim persisted
        let note = vm.read_note("tasks/task-001.md").await.unwrap();
        let fm = note.frontmatter.as_ref().expect("frontmatter should be present");
        assert_eq!(
            fm.get("owner").and_then(|v| v.as_str()),
            Some("agent-coder")
        );
        assert_eq!(
            fm.get("status").and_then(|v| v.as_str()),
            Some("in_progress")
        );
        // Body content must be preserved after frontmatter updates
        assert!(note.content.contains("# Task 1"));
    }

    #[tokio::test]
    async fn test_task_board_concurrent_claim_safety() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Create unclaimed task
        let task = "---\nstatus: pending\nowner: null\n---\n# Contested Task\n";
        vm.write_note("tasks/contested.md", task, WriteMode::Overwrite)
            .await
            .unwrap();

        // 5 agents try to claim simultaneously
        let mut handles = vec![];
        for i in 0..5u32 {
            let vm = vm.clone();
            handles.push(tokio::spawn(async move {
                // Each agent sets itself as owner — without an external locking
                // protocol multiple agents may read "null" and then overwrite.
                // This test verifies the file is not corrupted by concurrent writes.
                vm.set_frontmatter(
                    "tasks/contested.md",
                    "owner",
                    serde_yaml::Value::String(format!("agent-{}", i)),
                )
                .await
                .unwrap();
                i
            }));
        }

        let mut results = vec![];
        for h in handles {
            results.push(h.await.unwrap());
        }

        // Verify file is not corrupted — exactly one owner should be set (last writer wins)
        let final_note = vm.read_note("tasks/contested.md").await.unwrap();
        assert!(
            final_note.content.contains("# Contested Task"),
            "body should be preserved after concurrent frontmatter updates"
        );
        let fm = final_note
            .frontmatter
            .as_ref()
            .expect("frontmatter should be present");
        let owner = fm.get("owner").and_then(|v| v.as_str());
        assert!(owner.is_some(), "owner should be set");
        assert!(
            owner.unwrap().starts_with("agent-"),
            "owner should be one of the agents"
        );
    }

    #[tokio::test]
    async fn test_task_board_list_pending_tasks() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Create tasks with various statuses
        let tasks = vec![
            ("task-001.md", "pending"),
            ("task-002.md", "in_progress"),
            ("task-003.md", "pending"),
            ("task-004.md", "completed"),
            ("task-005.md", "pending"),
        ];
        for (name, status) in &tasks {
            let content = format!(
                "---\nstatus: {}\nowner: null\n---\n# {}\n",
                status, name
            );
            vm.write_note(&format!("tasks/{}", name), &content, WriteMode::Overwrite)
                .await
                .unwrap();
        }

        // Search for pending tasks using plain text search
        let results = vm
            .search("status: pending", false, Some("tasks"), None, 100)
            .await
            .unwrap();
        let pending_tasks: Vec<_> = results
            .iter()
            .filter(|r| r.path.contains("tasks/"))
            .collect();
        assert!(
            pending_tasks.len() >= 3,
            "should find at least 3 pending tasks, got {}",
            pending_tasks.len()
        );
    }

    #[tokio::test]
    async fn test_task_board_dependency_frontmatter() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Task A: no dependencies, completed
        vm.write_note(
            "tasks/task-a.md",
            "---\nstatus: completed\ndepends_on: []\n---\n# Task A\n",
            WriteMode::Overwrite,
        )
        .await
        .unwrap();
        // Task B: depends on A
        vm.write_note(
            "tasks/task-b.md",
            "---\nstatus: pending\ndepends_on:\n  - task-a\n---\n# Task B\n",
            WriteMode::Overwrite,
        )
        .await
        .unwrap();
        // Task C: depends on B
        vm.write_note(
            "tasks/task-c.md",
            "---\nstatus: pending\ndepends_on:\n  - task-b\n---\n# Task C\n",
            WriteMode::Overwrite,
        )
        .await
        .unwrap();

        // Read and verify dependency chain for Task B
        let note_b = vm.read_note("tasks/task-b.md").await.unwrap();
        let fm_b = note_b
            .frontmatter
            .as_ref()
            .expect("task-b should have frontmatter");
        let deps = fm_b
            .get("depends_on")
            .and_then(|v| v.as_sequence())
            .expect("depends_on should be a sequence");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].as_str(), Some("task-a"));

        // Verify Task C depends on B
        let note_c = vm.read_note("tasks/task-c.md").await.unwrap();
        let fm_c = note_c
            .frontmatter
            .as_ref()
            .expect("task-c should have frontmatter");
        let deps_c = fm_c
            .get("depends_on")
            .and_then(|v| v.as_sequence())
            .expect("depends_on should be a sequence");
        assert_eq!(deps_c.len(), 1);
        assert_eq!(deps_c[0].as_str(), Some("task-b"));

        // Verify Task A has empty depends_on
        let note_a = vm.read_note("tasks/task-a.md").await.unwrap();
        let fm_a = note_a
            .frontmatter
            .as_ref()
            .expect("task-a should have frontmatter");
        let deps_a = fm_a
            .get("depends_on")
            .and_then(|v| v.as_sequence())
            .expect("depends_on should be a sequence");
        assert!(deps_a.is_empty());
    }

    // --- message passing tests ---

    #[tokio::test]
    async fn test_message_write_and_read() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        let message = "---\nfrom: researcher\nto: coder\ntimestamp: \"2026-02-19T22:00:00Z\"\ntype: direct\n---\n# Message\n\nFound a bug in module X. Can you investigate?\n";
        vm.write_note("messages/001-researcher-to-coder.md", message, WriteMode::Overwrite)
            .await
            .unwrap();

        let note = vm
            .read_note("messages/001-researcher-to-coder.md")
            .await
            .unwrap();
        assert!(note.content.contains("Found a bug"));
        let fm = note
            .frontmatter
            .as_ref()
            .expect("message should have frontmatter");
        assert_eq!(fm.get("from").and_then(|v| v.as_str()), Some("researcher"));
        assert_eq!(fm.get("to").and_then(|v| v.as_str()), Some("coder"));
        assert_eq!(fm.get("type").and_then(|v| v.as_str()), Some("direct"));
        assert_eq!(
            fm.get("timestamp").and_then(|v| v.as_str()),
            Some("2026-02-19T22:00:00Z")
        );
    }

    #[tokio::test]
    async fn test_message_inbox_filtering() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Write messages to different recipients
        let messages = vec![
            (
                "messages/001-a-to-coder.md",
                "---\nfrom: a\nto: coder\n---\nMsg 1\n",
            ),
            (
                "messages/002-b-to-tester.md",
                "---\nfrom: b\nto: tester\n---\nMsg 2\n",
            ),
            (
                "messages/003-c-to-coder.md",
                "---\nfrom: c\nto: coder\n---\nMsg 3\n",
            ),
            (
                "messages/004-d-to-all.md",
                "---\nfrom: d\nto: all\n---\nBroadcast\n",
            ),
        ];
        for (path, content) in &messages {
            vm.write_note(path, content, WriteMode::Overwrite)
                .await
                .unwrap();
        }

        // Search for coder's messages via plain text search
        let results = vm
            .search("to: coder", false, Some("messages"), None, 100)
            .await
            .unwrap();
        let coder_msgs: Vec<_> = results
            .iter()
            .filter(|r| r.path.contains("messages/"))
            .collect();
        assert!(
            coder_msgs.len() >= 2,
            "coder should have at least 2 direct messages, got {}",
            coder_msgs.len()
        );

        // Verify none of the coder-targeted results are the tester-only message
        assert!(
            !coder_msgs
                .iter()
                .any(|r| r.path.contains("002-b-to-tester")),
            "tester-only message should not appear in coder search"
        );
    }

    #[tokio::test]
    async fn test_message_append_thread() {
        let dir = TempDir::new().unwrap();
        let config = VaultConfig {
            enabled: true,
            path: dir.path().to_path_buf(),
            extensions: vec!["md".to_string()],
            exclude_dirs: vec![],
        };
        let vm = VaultManager::with_scope(&config, "teams/alpha").unwrap();

        // Create a thread (quote topic value to prevent YAML treating #42 as comment)
        vm.write_note(
            "threads/bug-42.md",
            "---\ntopic: \"Bug #42\"\n---\n# Bug #42 Discussion\n",
            WriteMode::Overwrite,
        )
        .await
        .unwrap();

        // Agents append to the thread sequentially
        vm.write_note(
            "threads/bug-42.md",
            "\n## researcher (22:01)\nI found the root cause.\n",
            WriteMode::Append,
        )
        .await
        .unwrap();
        vm.write_note(
            "threads/bug-42.md",
            "\n## coder (22:05)\nFix pushed to branch.\n",
            WriteMode::Append,
        )
        .await
        .unwrap();
        vm.write_note(
            "threads/bug-42.md",
            "\n## tester (22:10)\nTests pass. LGTM.\n",
            WriteMode::Append,
        )
        .await
        .unwrap();

        // Read full thread and verify all messages are present in order
        let note = vm.read_note("threads/bug-42.md").await.unwrap();
        assert!(note.content.contains("root cause"));
        assert!(note.content.contains("Fix pushed"));
        assert!(note.content.contains("Tests pass"));

        // Verify original frontmatter is preserved
        let fm = note
            .frontmatter
            .as_ref()
            .expect("thread should have frontmatter");
        assert_eq!(fm.get("topic").and_then(|v| v.as_str()), Some("Bug #42"));

        // Verify order: researcher before coder before tester
        let root_cause_pos = note.content.find("root cause").unwrap();
        let fix_pushed_pos = note.content.find("Fix pushed").unwrap();
        let tests_pass_pos = note.content.find("Tests pass").unwrap();
        assert!(root_cause_pos < fix_pushed_pos);
        assert!(fix_pushed_pos < tests_pass_pos);
    }
}
