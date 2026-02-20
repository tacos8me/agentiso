use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Persisted session ownership mapping (serialized to state.json).
///
/// Only captures the session-to-workspace ownership. Quotas and usage are
/// recomputed from the workspace list on restore to avoid stale counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    pub session_id: String,
    pub workspace_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Resource limits enforced per session.
#[derive(Debug, Clone)]
pub struct SessionQuota {
    pub max_workspaces: usize,
    pub max_memory_mb: u64,
    pub max_disk_gb: u64,
}

impl Default for SessionQuota {
    fn default() -> Self {
        Self {
            max_workspaces: 10,
            max_memory_mb: 16384,
            max_disk_gb: 100,
        }
    }
}

/// Current resource usage for a session.
#[derive(Debug, Clone, Default)]
pub struct SessionUsage {
    pub workspace_count: usize,
    pub memory_mb: u64,
    pub disk_gb: u64,
}

/// A tracked MCP session with ownership and quota info.
#[derive(Debug, Clone)]
pub struct Session {
    #[allow(dead_code)] // Used by future session-info tool
    pub id: String,
    #[allow(dead_code)] // Used by future session-info tool
    pub created_at: DateTime<Utc>,
    pub workspaces: HashSet<Uuid>,
    pub quota: SessionQuota,
    pub usage: SessionUsage,
}

impl Session {
    fn new(id: String, quota: SessionQuota) -> Self {
        Self {
            id,
            created_at: Utc::now(),
            workspaces: HashSet::new(),
            quota,
            usage: SessionUsage::default(),
        }
    }
}

/// Errors from auth enforcement.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("unknown session: {0}")]
    UnknownSession(String),

    #[error("session {session_id} does not own workspace {workspace_id}")]
    NotOwner {
        session_id: String,
        workspace_id: Uuid,
    },

    #[error("quota exceeded: {resource} (limit: {limit}, current: {current}, requested: {requested})")]
    QuotaExceeded {
        resource: String,
        limit: u64,
        current: u64,
        requested: u64,
    },

    #[error("workspace {workspace_id} is owned by session {owner_session_id}. Use workspace_adopt(workspace_id=..., force=true) to claim it from the other session.")]
    AlreadyOwned {
        workspace_id: Uuid,
        owner_session_id: String,
    },
}

/// Thread-safe session and ownership tracker.
///
/// Every MCP connection gets a session. Workspaces created within a session
/// are owned by that session. Operations on a workspace require ownership.
/// Resource quotas are enforced per session.
#[derive(Debug, Clone)]
pub struct AuthManager {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    default_quota: SessionQuota,
}

impl AuthManager {
    pub fn new(default_quota: SessionQuota) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            default_quota,
        }
    }

    /// Register a new session. Returns the session ID.
    /// If a session with this ID already exists, it is a no-op.
    pub async fn register_session(&self, session_id: String) -> String {
        let mut sessions = self.sessions.write().await;
        sessions
            .entry(session_id.clone())
            .or_insert_with(|| Session::new(session_id.clone(), self.default_quota.clone()));
        session_id
    }

    /// Remove a session and disassociate all its workspaces.
    /// Returns the set of workspace IDs that were owned by this session.
    #[allow(dead_code)] // Called from session cleanup and used in tests
    pub async fn remove_session(&self, session_id: &str) -> HashSet<Uuid> {
        let mut sessions = self.sessions.write().await;
        sessions
            .remove(session_id)
            .map(|s| s.workspaces)
            .unwrap_or_default()
    }

    /// Record that a workspace was created by a session.
    /// Also updates usage counters. Fails if quota would be exceeded.
    pub async fn register_workspace(
        &self,
        session_id: &str,
        workspace_id: Uuid,
        memory_mb: u64,
        disk_gb: u64,
    ) -> Result<(), AuthError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| AuthError::UnknownSession(session_id.to_string()))?;

        // Check workspace count quota.
        if session.usage.workspace_count >= session.quota.max_workspaces {
            return Err(AuthError::QuotaExceeded {
                resource: "workspaces".into(),
                limit: session.quota.max_workspaces as u64,
                current: session.usage.workspace_count as u64,
                requested: 1,
            });
        }

        // Check memory quota.
        if session.usage.memory_mb + memory_mb > session.quota.max_memory_mb {
            return Err(AuthError::QuotaExceeded {
                resource: "memory_mb".into(),
                limit: session.quota.max_memory_mb,
                current: session.usage.memory_mb,
                requested: memory_mb,
            });
        }

        // Check disk quota.
        if session.usage.disk_gb + disk_gb > session.quota.max_disk_gb {
            return Err(AuthError::QuotaExceeded {
                resource: "disk_gb".into(),
                limit: session.quota.max_disk_gb,
                current: session.usage.disk_gb,
                requested: disk_gb,
            });
        }

        session.workspaces.insert(workspace_id);
        session.usage.workspace_count += 1;
        session.usage.memory_mb += memory_mb;
        session.usage.disk_gb += disk_gb;
        Ok(())
    }

    /// Remove a workspace from a session's ownership and release quota.
    pub async fn unregister_workspace(
        &self,
        session_id: &str,
        workspace_id: Uuid,
        memory_mb: u64,
        disk_gb: u64,
    ) -> Result<(), AuthError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| AuthError::UnknownSession(session_id.to_string()))?;

        if session.workspaces.remove(&workspace_id) {
            session.usage.workspace_count = session.usage.workspace_count.saturating_sub(1);
            session.usage.memory_mb = session.usage.memory_mb.saturating_sub(memory_mb);
            session.usage.disk_gb = session.usage.disk_gb.saturating_sub(disk_gb);
        }
        Ok(())
    }

    /// Check whether adding a workspace with the given resources would exceed quota.
    /// Does NOT modify state -- purely a read-only check.
    pub async fn check_quota(
        &self,
        session_id: &str,
        memory_mb: u64,
        disk_gb: u64,
    ) -> Result<(), AuthError> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| AuthError::UnknownSession(session_id.to_string()))?;

        if session.usage.workspace_count >= session.quota.max_workspaces {
            return Err(AuthError::QuotaExceeded {
                resource: "workspaces".into(),
                limit: session.quota.max_workspaces as u64,
                current: session.usage.workspace_count as u64,
                requested: 1,
            });
        }
        if session.usage.memory_mb + memory_mb > session.quota.max_memory_mb {
            return Err(AuthError::QuotaExceeded {
                resource: "memory_mb".into(),
                limit: session.quota.max_memory_mb,
                current: session.usage.memory_mb,
                requested: memory_mb,
            });
        }
        if session.usage.disk_gb + disk_gb > session.quota.max_disk_gb {
            return Err(AuthError::QuotaExceeded {
                resource: "disk_gb".into(),
                limit: session.quota.max_disk_gb,
                current: session.usage.disk_gb,
                requested: disk_gb,
            });
        }
        Ok(())
    }

    /// Verify that the given session owns the given workspace.
    pub async fn check_ownership(
        &self,
        session_id: &str,
        workspace_id: Uuid,
    ) -> Result<(), AuthError> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| AuthError::UnknownSession(session_id.to_string()))?;

        if !session.workspaces.contains(&workspace_id) {
            return Err(AuthError::NotOwner {
                session_id: session_id.to_string(),
                workspace_id,
            });
        }
        Ok(())
    }

    /// Get the current usage for a session.
    #[allow(dead_code)] // Public API for future usage-info tool
    pub async fn get_usage(&self, session_id: &str) -> Result<SessionUsage, AuthError> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| AuthError::UnknownSession(session_id.to_string()))?;
        Ok(session.usage.clone())
    }

    /// Get the quota for a session.
    #[allow(dead_code)] // Public API for future quota-info tool
    pub async fn get_quota(&self, session_id: &str) -> Result<SessionQuota, AuthError> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| AuthError::UnknownSession(session_id.to_string()))?;
        Ok(session.quota.clone())
    }

    /// List all workspace IDs owned by a session.
    pub async fn list_workspaces(&self, session_id: &str) -> Result<Vec<Uuid>, AuthError> {
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| AuthError::UnknownSession(session_id.to_string()))?;
        Ok(session.workspaces.iter().copied().collect())
    }

    /// Check whether a workspace is owned by any active session.
    /// Returns true if no session owns this workspace (i.e. it is orphaned).
    #[allow(dead_code)] // Public API, used in tests and available for future tools
    pub async fn is_workspace_orphaned(&self, workspace_id: &Uuid) -> bool {
        let sessions = self.sessions.read().await;
        !sessions
            .values()
            .any(|s| s.workspaces.contains(workspace_id))
    }

    /// Remove all sessions except the given one, releasing their workspace ownership.
    ///
    /// After a server restart, `restore_sessions` recreates ghost sessions that
    /// no MCP client is connected to. This method purges them so their workspaces
    /// become orphaned and can be adopted by the current session.
    /// Returns the number of stale sessions removed.
    pub async fn purge_stale_sessions(&self, keep_session_id: &str) -> usize {
        let mut sessions = self.sessions.write().await;
        let stale_ids: Vec<String> = sessions
            .keys()
            .filter(|id| id.as_str() != keep_session_id)
            .cloned()
            .collect();
        let count = stale_ids.len();
        for id in stale_ids {
            sessions.remove(&id);
        }
        count
    }

    /// Return all workspace IDs from `candidates` that are not owned by any session.
    pub async fn list_orphaned_workspaces(&self, candidates: &[Uuid]) -> Vec<Uuid> {
        let sessions = self.sessions.read().await;
        let all_owned: HashSet<Uuid> = sessions
            .values()
            .flat_map(|s| s.workspaces.iter().copied())
            .collect();
        candidates
            .iter()
            .filter(|id| !all_owned.contains(id))
            .copied()
            .collect()
    }

    /// Adopt an existing workspace into a session.
    ///
    /// Similar to `register_workspace` but specifically for reclaiming
    /// workspaces that already exist (e.g. after a daemon restart).
    /// Fails if the workspace is already owned by another session unless
    /// `force` is true, in which case ownership is transferred.
    pub async fn adopt_workspace(
        &self,
        session_id: &str,
        workspace_id: &Uuid,
        memory_mb: u64,
        disk_gb: u64,
        force: bool,
    ) -> Result<(), AuthError> {
        let mut sessions = self.sessions.write().await;
        let _session = sessions
            .get(session_id)
            .ok_or_else(|| AuthError::UnknownSession(session_id.to_string()))?;

        // Check if another session already owns this workspace.
        let mut previous_owner: Option<String> = None;
        for (sid, s) in sessions.iter() {
            if sid != session_id && s.workspaces.contains(workspace_id) {
                if force {
                    previous_owner = Some(sid.clone());
                } else {
                    return Err(AuthError::AlreadyOwned {
                        workspace_id: *workspace_id,
                        owner_session_id: sid.clone(),
                    });
                }
            }
        }

        // Read the adopting session to check ownership and quotas BEFORE
        // removing from the previous owner — otherwise a quota failure would
        // orphan the workspace.
        let session = sessions.get(session_id).unwrap();

        // If this session already owns it, no-op.
        if session.workspaces.contains(workspace_id) {
            return Ok(());
        }

        // Check workspace count quota.
        if session.usage.workspace_count >= session.quota.max_workspaces {
            return Err(AuthError::QuotaExceeded {
                resource: "workspaces".into(),
                limit: session.quota.max_workspaces as u64,
                current: session.usage.workspace_count as u64,
                requested: 1,
            });
        }

        // Check memory quota.
        if session.usage.memory_mb + memory_mb > session.quota.max_memory_mb {
            return Err(AuthError::QuotaExceeded {
                resource: "memory_mb".into(),
                limit: session.quota.max_memory_mb,
                current: session.usage.memory_mb,
                requested: memory_mb,
            });
        }

        // Check disk quota.
        if session.usage.disk_gb + disk_gb > session.quota.max_disk_gb {
            return Err(AuthError::QuotaExceeded {
                resource: "disk_gb".into(),
                limit: session.quota.max_disk_gb,
                current: session.usage.disk_gb,
                requested: disk_gb,
            });
        }

        // Quotas passed — now safe to remove from previous owner.
        if let Some(ref prev_sid) = previous_owner {
            if let Some(prev_session) = sessions.get_mut(prev_sid) {
                prev_session.workspaces.remove(workspace_id);
                prev_session.usage.workspace_count =
                    prev_session.usage.workspace_count.saturating_sub(1);
                prev_session.usage.memory_mb =
                    prev_session.usage.memory_mb.saturating_sub(memory_mb);
                prev_session.usage.disk_gb =
                    prev_session.usage.disk_gb.saturating_sub(disk_gb);
            }
        }

        // Borrow mutably to update the new owner.
        let session = sessions.get_mut(session_id).unwrap();
        session.workspaces.insert(*workspace_id);
        session.usage.workspace_count += 1;
        session.usage.memory_mb += memory_mb;
        session.usage.disk_gb += disk_gb;
        Ok(())
    }

    /// Export all sessions as persistable ownership records.
    ///
    /// Used by `save_state()` to include session→workspace mappings in the
    /// state file. Only sessions with at least one workspace are exported.
    pub async fn export_sessions(&self) -> Vec<PersistedSession> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .filter(|s| !s.workspaces.is_empty())
            .map(|s| PersistedSession {
                session_id: s.id.clone(),
                workspace_ids: s.workspaces.iter().copied().collect(),
                created_at: s.created_at,
            })
            .collect()
    }

    /// Restore session ownership from persisted records.
    ///
    /// Creates sessions and associates them with their workspaces. Usage
    /// counters are rebuilt from the provided workspace resource map
    /// (`workspace_id -> (memory_mb, disk_gb)`) so they stay consistent
    /// even if workspace resource limits changed while the server was down.
    ///
    /// Workspaces not present in `workspace_resources` (e.g. destroyed
    /// while offline) are silently skipped.
    pub async fn restore_sessions(
        &self,
        persisted: &[PersistedSession],
        workspace_resources: &HashMap<Uuid, (u64, u64)>,
    ) {
        let mut sessions = self.sessions.write().await;

        for ps in persisted {
            let mut ws_set = HashSet::new();
            let mut usage = SessionUsage::default();

            for ws_id in &ps.workspace_ids {
                // Only restore ownership for workspaces that still exist
                if let Some(&(mem, disk)) = workspace_resources.get(ws_id) {
                    ws_set.insert(*ws_id);
                    usage.workspace_count += 1;
                    usage.memory_mb += mem;
                    usage.disk_gb += disk;
                }
            }

            // Skip sessions with no valid workspaces remaining
            if ws_set.is_empty() {
                tracing::debug!(
                    session_id = %ps.session_id,
                    original_count = ps.workspace_ids.len(),
                    "skipping stale session (all workspaces gone)"
                );
                continue;
            }

            let stale_count = ps.workspace_ids.len() - ws_set.len();
            if stale_count > 0 {
                tracing::warn!(
                    session_id = %ps.session_id,
                    stale_count,
                    remaining = ws_set.len(),
                    "restored session with some workspaces missing"
                );
            }

            // Don't overwrite a session that was already registered by a
            // reconnecting client between load_state and now.
            if sessions.contains_key(&ps.session_id) {
                tracing::debug!(
                    session_id = %ps.session_id,
                    "session already exists, skipping restore"
                );
                continue;
            }

            let restored_count = usage.workspace_count;
            sessions.insert(
                ps.session_id.clone(),
                Session {
                    id: ps.session_id.clone(),
                    created_at: ps.created_at,
                    workspaces: ws_set,
                    quota: self.default_quota.clone(),
                    usage,
                },
            );

            tracing::info!(
                session_id = %ps.session_id,
                workspaces = restored_count,
                "restored session ownership from persisted state"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_lifecycle() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("test-session".into()).await;
        assert_eq!(sid, "test-session");

        let ws_id = Uuid::new_v4();
        auth.register_workspace(&sid, ws_id, 1024, 10)
            .await
            .unwrap();

        auth.check_ownership(&sid, ws_id).await.unwrap();

        let other_ws = Uuid::new_v4();
        assert!(auth.check_ownership(&sid, other_ws).await.is_err());

        let usage = auth.get_usage(&sid).await.unwrap();
        assert_eq!(usage.workspace_count, 1);
        assert_eq!(usage.memory_mb, 1024);
        assert_eq!(usage.disk_gb, 10);

        auth.unregister_workspace(&sid, ws_id, 1024, 10)
            .await
            .unwrap();
        let usage = auth.get_usage(&sid).await.unwrap();
        assert_eq!(usage.workspace_count, 0);

        let removed = auth.remove_session(&sid).await;
        assert!(removed.is_empty());
    }

    #[tokio::test]
    async fn test_quota_enforcement() {
        let quota = SessionQuota {
            max_workspaces: 2,
            max_memory_mb: 2048,
            max_disk_gb: 20,
        };
        let auth = AuthManager::new(quota);
        let sid = auth.register_session("quota-test".into()).await;

        auth.register_workspace(&sid, Uuid::new_v4(), 1024, 10)
            .await
            .unwrap();
        auth.register_workspace(&sid, Uuid::new_v4(), 1024, 10)
            .await
            .unwrap();

        // Third workspace should fail (max_workspaces = 2).
        let result = auth
            .register_workspace(&sid, Uuid::new_v4(), 512, 5)
            .await;
        assert!(matches!(result, Err(AuthError::QuotaExceeded { .. })));
    }

    #[tokio::test]
    async fn test_memory_quota_exceeded() {
        let quota = SessionQuota {
            max_workspaces: 10,
            max_memory_mb: 2048,
            max_disk_gb: 100,
        };
        let auth = AuthManager::new(quota);
        let sid = auth.register_session("mem-test".into()).await;

        auth.register_workspace(&sid, Uuid::new_v4(), 1500, 10)
            .await
            .unwrap();

        // This should exceed memory quota.
        let result = auth
            .register_workspace(&sid, Uuid::new_v4(), 600, 10)
            .await;
        assert!(matches!(result, Err(AuthError::QuotaExceeded { .. })));
    }

    #[tokio::test]
    async fn test_unknown_session() {
        let auth = AuthManager::new(SessionQuota::default());
        let result = auth.check_ownership("nonexistent", Uuid::new_v4()).await;
        assert!(matches!(result, Err(AuthError::UnknownSession(_))));
    }

    #[tokio::test]
    async fn test_check_quota_readonly() {
        let quota = SessionQuota {
            max_workspaces: 1,
            max_memory_mb: 1024,
            max_disk_gb: 10,
        };
        let auth = AuthManager::new(quota);
        let sid = auth.register_session("quota-check".into()).await;

        // check_quota should pass when there's room.
        auth.check_quota(&sid, 512, 5).await.unwrap();

        // Usage should still be zero -- check_quota is read-only.
        let usage = auth.get_usage(&sid).await.unwrap();
        assert_eq!(usage.workspace_count, 0);

        // Fill the quota.
        auth.register_workspace(&sid, Uuid::new_v4(), 512, 5)
            .await
            .unwrap();

        // check_quota should now fail.
        let result = auth.check_quota(&sid, 512, 5).await;
        assert!(matches!(result, Err(AuthError::QuotaExceeded { .. })));
    }

    #[tokio::test]
    async fn test_session_removal_returns_workspaces() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("remove-test".into()).await;

        let ws1 = Uuid::new_v4();
        let ws2 = Uuid::new_v4();
        auth.register_workspace(&sid, ws1, 512, 5).await.unwrap();
        auth.register_workspace(&sid, ws2, 512, 5).await.unwrap();

        let removed = auth.remove_session(&sid).await;
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&ws1));
        assert!(removed.contains(&ws2));

        // Session should no longer exist.
        let result = auth.check_ownership(&sid, ws1).await;
        assert!(matches!(result, Err(AuthError::UnknownSession(_))));
    }

    #[tokio::test]
    async fn test_cross_session_ownership() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid_a = auth.register_session("session-a".into()).await;
        let sid_b = auth.register_session("session-b".into()).await;

        let ws_a = Uuid::new_v4();
        auth.register_workspace(&sid_a, ws_a, 512, 5)
            .await
            .unwrap();

        // Session A owns ws_a.
        auth.check_ownership(&sid_a, ws_a).await.unwrap();

        // Session B does NOT own ws_a.
        let result = auth.check_ownership(&sid_b, ws_a).await;
        assert!(matches!(result, Err(AuthError::NotOwner { .. })));
    }

    #[tokio::test]
    async fn test_disk_quota_exceeded() {
        let quota = SessionQuota {
            max_workspaces: 10,
            max_memory_mb: 16384,
            max_disk_gb: 20,
        };
        let auth = AuthManager::new(quota);
        let sid = auth.register_session("disk-test".into()).await;

        auth.register_workspace(&sid, Uuid::new_v4(), 512, 15)
            .await
            .unwrap();

        // This should exceed disk quota (15 + 10 > 20).
        let result = auth
            .register_workspace(&sid, Uuid::new_v4(), 512, 10)
            .await;
        assert!(matches!(result, Err(AuthError::QuotaExceeded { .. })));
    }

    #[tokio::test]
    async fn test_double_register_session_is_noop() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("double-reg".into()).await;

        let ws = Uuid::new_v4();
        auth.register_workspace(&sid, ws, 512, 5).await.unwrap();

        // Re-register same session ID -- should be a no-op, not reset state.
        auth.register_session("double-reg".into()).await;

        // Workspace should still be owned.
        auth.check_ownership(&sid, ws).await.unwrap();

        // Usage should still reflect the workspace.
        let usage = auth.get_usage(&sid).await.unwrap();
        assert_eq!(usage.workspace_count, 1);
    }

    #[tokio::test]
    async fn test_get_quota_returns_defaults() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("quota-info".into()).await;

        let quota = auth.get_quota(&sid).await.unwrap();
        assert_eq!(quota.max_workspaces, 10);
        assert_eq!(quota.max_memory_mb, 16384);
        assert_eq!(quota.max_disk_gb, 100);
    }

    #[tokio::test]
    async fn test_list_workspaces_empty_and_populated() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("list-test".into()).await;

        // Empty session has no workspaces.
        let list = auth.list_workspaces(&sid).await.unwrap();
        assert!(list.is_empty());

        let ws = Uuid::new_v4();
        auth.register_workspace(&sid, ws, 256, 5).await.unwrap();

        let list = auth.list_workspaces(&sid).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list.contains(&ws));
    }

    #[tokio::test]
    async fn test_is_workspace_orphaned() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("orphan-test".into()).await;

        let owned_ws = Uuid::new_v4();
        let orphaned_ws = Uuid::new_v4();
        auth.register_workspace(&sid, owned_ws, 512, 5)
            .await
            .unwrap();

        // Owned workspace is not orphaned.
        assert!(!auth.is_workspace_orphaned(&owned_ws).await);

        // Unregistered workspace is orphaned.
        assert!(auth.is_workspace_orphaned(&orphaned_ws).await);
    }

    #[tokio::test]
    async fn test_list_orphaned_workspaces() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("orphan-list".into()).await;

        let ws1 = Uuid::new_v4();
        let ws2 = Uuid::new_v4();
        let ws3 = Uuid::new_v4();
        auth.register_workspace(&sid, ws1, 256, 5).await.unwrap();

        // ws1 is owned; ws2 and ws3 are orphaned.
        let orphans = auth.list_orphaned_workspaces(&[ws1, ws2, ws3]).await;
        assert_eq!(orphans.len(), 2);
        assert!(orphans.contains(&ws2));
        assert!(orphans.contains(&ws3));
        assert!(!orphans.contains(&ws1));
    }

    #[tokio::test]
    async fn test_adopt_workspace_basic() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("adopt-test".into()).await;

        let ws = Uuid::new_v4();

        // Workspace is orphaned before adoption.
        assert!(auth.is_workspace_orphaned(&ws).await);

        // Adopt it.
        auth.adopt_workspace(&sid, &ws, 1024, 10, false).await.unwrap();

        // Now it's owned.
        assert!(!auth.is_workspace_orphaned(&ws).await);
        auth.check_ownership(&sid, ws).await.unwrap();

        // Usage should reflect the adopted workspace.
        let usage = auth.get_usage(&sid).await.unwrap();
        assert_eq!(usage.workspace_count, 1);
        assert_eq!(usage.memory_mb, 1024);
        assert_eq!(usage.disk_gb, 10);
    }

    #[tokio::test]
    async fn test_adopt_workspace_already_owned_by_other() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid_a = auth.register_session("session-a".into()).await;
        let sid_b = auth.register_session("session-b".into()).await;

        let ws = Uuid::new_v4();
        auth.register_workspace(&sid_a, ws, 512, 5).await.unwrap();

        // Session B should not be able to adopt a workspace owned by session A (without force).
        let result = auth.adopt_workspace(&sid_b, &ws, 512, 5, false).await;
        assert!(matches!(result, Err(AuthError::AlreadyOwned { .. })));
    }

    #[tokio::test]
    async fn test_adopt_workspace_force_from_other_session() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid_a = auth.register_session("session-a".into()).await;
        let sid_b = auth.register_session("session-b".into()).await;

        let ws = Uuid::new_v4();
        auth.register_workspace(&sid_a, ws, 512, 5).await.unwrap();

        // Session A owns it.
        auth.check_ownership(&sid_a, ws).await.unwrap();

        // Session B force-adopts it.
        auth.adopt_workspace(&sid_b, &ws, 512, 5, true)
            .await
            .unwrap();

        // Session B now owns it.
        auth.check_ownership(&sid_b, ws).await.unwrap();

        // Session A no longer owns it.
        let result = auth.check_ownership(&sid_a, ws).await;
        assert!(matches!(result, Err(AuthError::NotOwner { .. })));

        // Session A usage should be decremented.
        let usage_a = auth.get_usage(&sid_a).await.unwrap();
        assert_eq!(usage_a.workspace_count, 0);
        assert_eq!(usage_a.memory_mb, 0);
        assert_eq!(usage_a.disk_gb, 0);

        // Session B usage should reflect the workspace.
        let usage_b = auth.get_usage(&sid_b).await.unwrap();
        assert_eq!(usage_b.workspace_count, 1);
        assert_eq!(usage_b.memory_mb, 512);
        assert_eq!(usage_b.disk_gb, 5);
    }

    #[tokio::test]
    async fn test_adopt_workspace_idempotent() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("adopt-idem".into()).await;

        let ws = Uuid::new_v4();
        auth.adopt_workspace(&sid, &ws, 512, 5, false).await.unwrap();

        // Adopting again should be a no-op.
        auth.adopt_workspace(&sid, &ws, 512, 5, false).await.unwrap();

        // Usage should still reflect only one workspace.
        let usage = auth.get_usage(&sid).await.unwrap();
        assert_eq!(usage.workspace_count, 1);
        assert_eq!(usage.memory_mb, 512);
        assert_eq!(usage.disk_gb, 5);
    }

    #[tokio::test]
    async fn test_adopt_workspace_quota_exceeded() {
        let quota = SessionQuota {
            max_workspaces: 1,
            max_memory_mb: 1024,
            max_disk_gb: 10,
        };
        let auth = AuthManager::new(quota);
        let sid = auth.register_session("adopt-quota".into()).await;

        let ws1 = Uuid::new_v4();
        auth.adopt_workspace(&sid, &ws1, 512, 5, false).await.unwrap();

        // Second adoption should fail (max_workspaces = 1).
        let ws2 = Uuid::new_v4();
        let result = auth.adopt_workspace(&sid, &ws2, 512, 5, false).await;
        assert!(matches!(result, Err(AuthError::QuotaExceeded { .. })));
    }

    #[tokio::test]
    async fn test_adopt_workspace_unknown_session() {
        let auth = AuthManager::new(SessionQuota::default());
        let ws = Uuid::new_v4();
        let result = auth.adopt_workspace("nonexistent", &ws, 512, 5, false).await;
        assert!(matches!(result, Err(AuthError::UnknownSession(_))));
    }

    #[tokio::test]
    async fn test_export_sessions_empty() {
        let auth = AuthManager::new(SessionQuota::default());
        let _sid = auth.register_session("empty-session".into()).await;

        // Sessions with no workspaces are not exported
        let exported = auth.export_sessions().await;
        assert!(exported.is_empty());
    }

    #[tokio::test]
    async fn test_export_sessions_with_workspaces() {
        let auth = AuthManager::new(SessionQuota::default());
        let sid = auth.register_session("export-test".into()).await;

        let ws1 = Uuid::new_v4();
        let ws2 = Uuid::new_v4();
        auth.register_workspace(&sid, ws1, 512, 5).await.unwrap();
        auth.register_workspace(&sid, ws2, 1024, 10).await.unwrap();

        let exported = auth.export_sessions().await;
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].session_id, "export-test");
        assert_eq!(exported[0].workspace_ids.len(), 2);
    }

    #[tokio::test]
    async fn test_restore_sessions_basic() {
        let auth = AuthManager::new(SessionQuota::default());
        let ws1 = Uuid::new_v4();
        let ws2 = Uuid::new_v4();

        let persisted = vec![PersistedSession {
            session_id: "restored-session".to_string(),
            workspace_ids: vec![ws1, ws2],
            created_at: Utc::now(),
        }];

        let mut resources = HashMap::new();
        resources.insert(ws1, (512u64, 5u64));
        resources.insert(ws2, (1024u64, 10u64));

        auth.restore_sessions(&persisted, &resources).await;

        // Session should now exist with ownership of both workspaces
        auth.check_ownership("restored-session", ws1).await.unwrap();
        auth.check_ownership("restored-session", ws2).await.unwrap();

        // Usage should be correct
        let usage = auth.get_usage("restored-session").await.unwrap();
        assert_eq!(usage.workspace_count, 2);
        assert_eq!(usage.memory_mb, 1536); // 512 + 1024
        assert_eq!(usage.disk_gb, 15); // 5 + 10
    }

    #[tokio::test]
    async fn test_restore_sessions_skips_missing_workspaces() {
        let auth = AuthManager::new(SessionQuota::default());
        let ws1 = Uuid::new_v4();
        let ws_gone = Uuid::new_v4();

        let persisted = vec![PersistedSession {
            session_id: "partial-restore".to_string(),
            workspace_ids: vec![ws1, ws_gone],
            created_at: Utc::now(),
        }];

        // Only ws1 exists; ws_gone was destroyed while server was down
        let mut resources = HashMap::new();
        resources.insert(ws1, (512u64, 5u64));

        auth.restore_sessions(&persisted, &resources).await;

        // Session should own only ws1
        auth.check_ownership("partial-restore", ws1).await.unwrap();
        assert!(auth.check_ownership("partial-restore", ws_gone).await.is_err());

        let usage = auth.get_usage("partial-restore").await.unwrap();
        assert_eq!(usage.workspace_count, 1);
    }

    #[tokio::test]
    async fn test_restore_sessions_all_workspaces_gone() {
        let auth = AuthManager::new(SessionQuota::default());
        let ws_gone = Uuid::new_v4();

        let persisted = vec![PersistedSession {
            session_id: "stale-session".to_string(),
            workspace_ids: vec![ws_gone],
            created_at: Utc::now(),
        }];

        // No workspaces exist anymore
        let resources = HashMap::new();

        auth.restore_sessions(&persisted, &resources).await;

        // Session should not be created (all workspaces gone)
        let result = auth.check_ownership("stale-session", ws_gone).await;
        assert!(matches!(result, Err(AuthError::UnknownSession(_))));
    }

    #[tokio::test]
    async fn test_export_restore_roundtrip() {
        // Create sessions with workspaces, export, then restore to a new AuthManager
        let auth1 = AuthManager::new(SessionQuota::default());
        let sid = auth1.register_session("roundtrip".into()).await;

        let ws1 = Uuid::new_v4();
        let ws2 = Uuid::new_v4();
        auth1.register_workspace(&sid, ws1, 512, 5).await.unwrap();
        auth1.register_workspace(&sid, ws2, 1024, 10).await.unwrap();

        let exported = auth1.export_sessions().await;

        // Restore into a fresh AuthManager
        let auth2 = AuthManager::new(SessionQuota::default());
        let mut resources = HashMap::new();
        resources.insert(ws1, (512u64, 5u64));
        resources.insert(ws2, (1024u64, 10u64));

        auth2.restore_sessions(&exported, &resources).await;

        // Verify ownership transferred
        auth2.check_ownership("roundtrip", ws1).await.unwrap();
        auth2.check_ownership("roundtrip", ws2).await.unwrap();

        let usage = auth2.get_usage("roundtrip").await.unwrap();
        assert_eq!(usage.workspace_count, 2);
        assert_eq!(usage.memory_mb, 1536);
        assert_eq!(usage.disk_gb, 15);
    }

    #[tokio::test]
    async fn test_force_adopt_quota_exceeded_preserves_previous_owner() {
        // Session B has a tight quota (max 1 workspace, already full).
        let quota = SessionQuota {
            max_workspaces: 1,
            max_memory_mb: 1024,
            max_disk_gb: 10,
        };
        let auth = AuthManager::new(quota);
        let sid_a = auth.register_session("session-a".into()).await;
        let sid_b = auth.register_session("session-b".into()).await;

        let ws_owned_by_a = Uuid::new_v4();
        auth.register_workspace(&sid_a, ws_owned_by_a, 512, 5).await.unwrap();

        // Fill session B to its quota.
        let ws_owned_by_b = Uuid::new_v4();
        auth.register_workspace(&sid_b, ws_owned_by_b, 512, 5).await.unwrap();

        // Force-adopt should fail because session B is at quota.
        let result = auth
            .adopt_workspace(&sid_b, &ws_owned_by_a, 512, 5, true)
            .await;
        assert!(matches!(result, Err(AuthError::QuotaExceeded { .. })));

        // CRITICAL: Session A must still own the workspace (not orphaned).
        auth.check_ownership(&sid_a, ws_owned_by_a).await.unwrap();

        // Session A usage should be unchanged.
        let usage_a = auth.get_usage(&sid_a).await.unwrap();
        assert_eq!(usage_a.workspace_count, 1);
        assert_eq!(usage_a.memory_mb, 512);
        assert_eq!(usage_a.disk_gb, 5);
    }

    #[tokio::test]
    async fn test_persisted_session_serde() {
        let ps = PersistedSession {
            session_id: "test-session".to_string(),
            workspace_ids: vec![Uuid::new_v4(), Uuid::new_v4()],
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&ps).unwrap();
        let deserialized: PersistedSession = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.session_id, ps.session_id);
        assert_eq!(deserialized.workspace_ids.len(), 2);
    }
}
