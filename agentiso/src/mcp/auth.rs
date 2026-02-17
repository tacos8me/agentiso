use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

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
}
