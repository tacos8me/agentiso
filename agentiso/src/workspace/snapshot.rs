use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A named snapshot of a workspace (disk state and optionally VM memory).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: Uuid,
    pub name: String,
    pub workspace_id: Uuid,
    /// Full ZFS snapshot path, e.g. "tank/agentiso/workspaces/ws-{id}@{name}".
    pub zfs_snapshot: String,
    /// Path to saved QEMU memory state file, if this is a live snapshot.
    pub qemu_state: Option<PathBuf>,
    /// Parent snapshot ID (forms a tree).
    pub parent: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Manages the snapshot tree for a single workspace.
///
/// Snapshots form a tree where each snapshot can have a parent. The tree is
/// stored as a flat map keyed by snapshot ID, with parent pointers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnapshotTree {
    snapshots: HashMap<Uuid, Snapshot>,
}

#[allow(dead_code)]
impl SnapshotTree {
    pub fn new() -> Self {
        Self {
            snapshots: HashMap::new(),
        }
    }

    /// Add a snapshot to the tree.
    pub fn add(&mut self, snapshot: Snapshot) {
        self.snapshots.insert(snapshot.id, snapshot);
    }

    /// Remove a snapshot by ID. Returns the removed snapshot if found.
    ///
    /// This does NOT remove children -- callers must decide how to handle
    /// orphaned children (re-parent or reject deletion).
    pub fn remove(&mut self, id: &Uuid) -> Option<Snapshot> {
        self.snapshots.remove(id)
    }

    /// Look up a snapshot by ID.
    pub fn get(&self, id: &Uuid) -> Option<&Snapshot> {
        self.snapshots.get(id)
    }

    /// Look up a snapshot by name.
    pub fn get_by_name(&self, name: &str) -> Option<&Snapshot> {
        self.snapshots.values().find(|s| s.name == name)
    }

    /// Return all snapshots as a sorted list (by creation time, ascending).
    pub fn list(&self) -> Vec<&Snapshot> {
        let mut snaps: Vec<&Snapshot> = self.snapshots.values().collect();
        snaps.sort_by_key(|s| s.created_at);
        snaps
    }

    /// Return IDs of direct children of a given snapshot.
    pub fn children_of(&self, parent_id: &Uuid) -> Vec<Uuid> {
        self.snapshots
            .values()
            .filter(|s| s.parent.as_ref() == Some(parent_id))
            .map(|s| s.id)
            .collect()
    }

    /// Check if any children reference this snapshot as parent.
    pub fn has_children(&self, id: &Uuid) -> bool {
        self.snapshots
            .values()
            .any(|s| s.parent.as_ref() == Some(id))
    }

    /// Total number of snapshots.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Get the chain of snapshot IDs from a given snapshot to the root.
    /// Returns in order [given_id, parent_id, grandparent_id, ...].
    pub fn ancestor_chain(&self, id: &Uuid) -> Vec<Uuid> {
        let mut chain = Vec::new();
        let mut current = Some(*id);
        while let Some(cid) = current {
            if chain.contains(&cid) {
                // Cycle detection -- should never happen, but be safe.
                break;
            }
            chain.push(cid);
            current = self.snapshots.get(&cid).and_then(|s| s.parent);
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(name: &str, workspace_id: Uuid, parent: Option<Uuid>) -> Snapshot {
        Snapshot {
            id: Uuid::new_v4(),
            name: name.to_string(),
            workspace_id,
            zfs_snapshot: format!("tank/agentiso/workspaces/ws-test@{}", name),
            qemu_state: None,
            parent,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn snapshot_tree_new_is_empty() {
        let tree = SnapshotTree::new();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn snapshot_tree_add_and_get() {
        let mut tree = SnapshotTree::new();
        let ws_id = Uuid::new_v4();
        let snap = make_snapshot("snap1", ws_id, None);
        let snap_id = snap.id;

        tree.add(snap);
        assert_eq!(tree.len(), 1);
        assert!(!tree.is_empty());

        let retrieved = tree.get(&snap_id).unwrap();
        assert_eq!(retrieved.name, "snap1");
        assert_eq!(retrieved.workspace_id, ws_id);
    }

    #[test]
    fn snapshot_tree_get_by_name() {
        let mut tree = SnapshotTree::new();
        let ws_id = Uuid::new_v4();
        tree.add(make_snapshot("alpha", ws_id, None));
        tree.add(make_snapshot("beta", ws_id, None));

        assert!(tree.get_by_name("alpha").is_some());
        assert!(tree.get_by_name("beta").is_some());
        assert!(tree.get_by_name("gamma").is_none());
    }

    #[test]
    fn snapshot_tree_remove() {
        let mut tree = SnapshotTree::new();
        let ws_id = Uuid::new_v4();
        let snap = make_snapshot("to_remove", ws_id, None);
        let snap_id = snap.id;

        tree.add(snap);
        assert_eq!(tree.len(), 1);

        let removed = tree.remove(&snap_id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().name, "to_remove");
        assert!(tree.is_empty());
    }

    #[test]
    fn snapshot_tree_remove_nonexistent() {
        let mut tree = SnapshotTree::new();
        let bogus_id = Uuid::new_v4();
        assert!(tree.remove(&bogus_id).is_none());
    }

    #[test]
    fn snapshot_tree_children_of() {
        let mut tree = SnapshotTree::new();
        let ws_id = Uuid::new_v4();

        let root = make_snapshot("root", ws_id, None);
        let root_id = root.id;
        tree.add(root);

        let child1 = make_snapshot("child1", ws_id, Some(root_id));
        let child1_id = child1.id;
        tree.add(child1);

        let child2 = make_snapshot("child2", ws_id, Some(root_id));
        let child2_id = child2.id;
        tree.add(child2);

        // Unrelated snapshot with no parent
        tree.add(make_snapshot("orphan", ws_id, None));

        let children = tree.children_of(&root_id);
        assert_eq!(children.len(), 2);
        assert!(children.contains(&child1_id));
        assert!(children.contains(&child2_id));

        assert!(tree.has_children(&root_id));
        assert!(!tree.has_children(&child1_id));
    }

    #[test]
    fn snapshot_tree_ancestor_chain() {
        let mut tree = SnapshotTree::new();
        let ws_id = Uuid::new_v4();

        let grandparent = make_snapshot("gp", ws_id, None);
        let gp_id = grandparent.id;
        tree.add(grandparent);

        let parent = make_snapshot("p", ws_id, Some(gp_id));
        let p_id = parent.id;
        tree.add(parent);

        let child = make_snapshot("c", ws_id, Some(p_id));
        let c_id = child.id;
        tree.add(child);

        let chain = tree.ancestor_chain(&c_id);
        assert_eq!(chain, vec![c_id, p_id, gp_id]);

        // Root's chain is just itself
        let root_chain = tree.ancestor_chain(&gp_id);
        assert_eq!(root_chain, vec![gp_id]);
    }

    #[test]
    fn snapshot_tree_ancestor_chain_nonexistent() {
        let tree = SnapshotTree::new();
        let bogus_id = Uuid::new_v4();
        // Non-existent ID returns just itself (it's pushed, then lookup fails, loop ends)
        let chain = tree.ancestor_chain(&bogus_id);
        assert_eq!(chain, vec![bogus_id]);
    }

    #[test]
    fn snapshot_tree_list_sorted_by_time() {
        let mut tree = SnapshotTree::new();
        let ws_id = Uuid::new_v4();

        // Add snapshots with slightly different timestamps
        for name in &["first", "second", "third"] {
            tree.add(make_snapshot(name, ws_id, None));
        }

        let listed = tree.list();
        assert_eq!(listed.len(), 3);
        // Since they are created nearly simultaneously with Utc::now(), just verify all present
        let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"first"));
        assert!(names.contains(&"second"));
        assert!(names.contains(&"third"));
    }

    #[test]
    fn snapshot_serde_roundtrip() {
        let mut tree = SnapshotTree::new();
        let ws_id = Uuid::new_v4();

        let root = make_snapshot("root", ws_id, None);
        let root_id = root.id;
        tree.add(root);
        tree.add(make_snapshot("child", ws_id, Some(root_id)));

        let json = serde_json::to_string(&tree).unwrap();
        let deserialized: SnapshotTree = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert!(deserialized.get_by_name("root").is_some());
        assert!(deserialized.get_by_name("child").is_some());
        assert_eq!(deserialized.children_of(&root_id).len(), 1);
    }
}
