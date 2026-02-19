use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::config::VaultConfig;

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
        }))
    }

    /// Return the vault root path.
    pub fn root(&self) -> &PathBuf {
        &self.config.path
    }

    // -- path helpers -------------------------------------------------------

    /// Resolve a user-supplied path to an absolute path within the vault root.
    ///
    /// Rejects paths containing `..` components and ensures the result is
    /// inside the vault root after canonicalization.
    pub fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let path = Path::new(path);

        // Reject `..` components before any filesystem access.
        for component in path.components() {
            if let std::path::Component::ParentDir = component {
                bail!("path traversal (.. component) is not allowed");
            }
        }

        let joined = self.config.path.join(path);

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
                let canon_root = self.config.path.canonicalize()?;
                let relative = joined
                    .strip_prefix(&self.config.path)
                    .unwrap_or(joined.as_path());
                canon_root.join(relative)
            }
        };

        let canon_root = self.config.path.canonicalize()?;
        if !resolved.starts_with(&canon_root) {
            bail!(
                "resolved path {} is outside vault root {}",
                resolved.display(),
                canon_root.display()
            );
        }

        Ok(resolved)
    }

    /// Convert an absolute path back to a vault-relative string.
    fn relative_path(&self, abs: &Path) -> String {
        let canon_root = self
            .config
            .path
            .canonicalize()
            .unwrap_or_else(|_| self.config.path.clone());
        abs.strip_prefix(&canon_root)
            .unwrap_or(abs)
            .to_string_lossy()
            .to_string()
    }

    // -- read / write -------------------------------------------------------

    /// Read a note and parse its frontmatter.
    pub async fn read_note(&self, path: &str) -> Result<VaultNote> {
        let abs = self.resolve_path(path)?;
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
        let abs = self.resolve_path(path)?;

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
        fs::remove_file(&abs)
            .await
            .with_context(|| format!("deleting {}", abs.display()))?;
        Ok(())
    }

    // -- frontmatter --------------------------------------------------------

    /// Get the parsed YAML frontmatter of a note.
    pub async fn get_frontmatter(&self, path: &str) -> Result<Option<serde_yaml::Value>> {
        let abs = self.resolve_path(path)?;
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
        let content = fs::read_to_string(&abs).await?;
        Ok(extract_tags(&content))
    }

    /// Add a tag to the frontmatter `tags` array.
    pub async fn add_tag(&self, path: &str, tag: &str) -> Result<()> {
        let abs = self.resolve_path(path)?;
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
            self.config.path.canonicalize()?
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
            self.config.path.canonicalize()?
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
}
