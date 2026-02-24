import { useState, useCallback, useEffect, useRef, useMemo } from 'react';
import { ChevronRight, ChevronDown, Box, BookOpen, Users, ListTodo, Plus, X, Loader2, Package, FileText } from 'lucide-react';
import { StatusDot } from '../common/StatusDot';
import { useUiStore } from '../../stores/ui';
import { useWorkspaceStore } from '../../stores/workspaces';
import { useTeamStore } from '../../stores/teams';
import { useVaultStore } from '../../stores/vault';
import { useNotificationStore } from '../../stores/notifications';
import type { WorkspaceState } from '../../types/workspace';
import type { VaultTreeNode } from '../../types/vault';

type Section = 'workspaces' | 'tasks' | 'vault' | 'teams';

const sectionConfig: { id: Section; label: string; icon: typeof Box }[] = [
  { id: 'workspaces', label: 'Workspaces', icon: Box },
  { id: 'tasks', label: 'Tasks', icon: ListTodo },
  { id: 'vault', label: 'Vault', icon: BookOpen },
  { id: 'teams', label: 'Teams', icon: Users },
];

interface SidebarNode {
  id: string;
  label: string;
  kind: 'section' | 'workspace' | 'folder' | 'note' | 'team' | 'member' | 'task';
  state?: WorkspaceState | string;
  children?: SidebarNode[];
}

interface TreeItemProps {
  node: SidebarNode;
  depth: number;
  expandedIds: Set<string>;
  openItemIds: Set<string>;
  onToggle: (id: string) => void;
  onSelect: (node: SidebarNode) => void;
  collapsed?: boolean;
}

function TreeItem({ node, depth, expandedIds, openItemIds, onToggle, onSelect, collapsed }: TreeItemProps) {
  const hasChildren = node.children && node.children.length > 0;
  const isFolder = node.kind === 'section' || node.kind === 'folder' || node.kind === 'team';
  const isExpanded = expandedIds.has(node.id);
  const isOpenInPane = openItemIds.has(node.id);

  return (
    <div role={hasChildren ? 'treeitem' : 'none'} aria-expanded={hasChildren ? isExpanded : undefined}>
      <div
        className={`flex items-center gap-1 h-8 px-2 cursor-pointer rounded group transition-colors ${
          isOpenInPane
            ? 'bg-[#5C4033]/10 text-[#DCD5CC] border-r-2 border-[#5C4033]'
            : 'hover:bg-[var(--surface-2)]'
        }`}
        style={{ paddingLeft: collapsed ? '8px' : `${8 + depth * 14}px` }}
        onClick={() => {
          if (hasChildren) onToggle(node.id);
          if (!isFolder || node.kind === 'team') onSelect(node);
        }}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            if (hasChildren) onToggle(node.id);
            if (!isFolder || node.kind === 'team') onSelect(node);
          }
        }}
        aria-label={node.label}
      >
        {!collapsed && hasChildren ? (
          <span className="w-4 h-4 flex items-center justify-center text-[var(--text-muted)] shrink-0">
            {isExpanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          </span>
        ) : !collapsed ? (
          <span className="w-4 shrink-0" />
        ) : null}

        {node.state && (
          <StatusDot status={node.state as WorkspaceState} size="sm" className="shrink-0" />
        )}

        {!collapsed && (
          <span
            className={`text-[13px] truncate ${
              isFolder ? 'text-[var(--text-muted)] font-medium' : 'text-[var(--text)]'
            }`}
          >
            {node.label}
          </span>
        )}
      </div>

      {hasChildren && isExpanded && !collapsed && (
        <div role="group">
          {node.children!.map((child) => (
            <TreeItem
              key={child.id}
              node={child}
              depth={depth + 1}
              expandedIds={expandedIds}
              openItemIds={openItemIds}
              onToggle={onToggle}
              onSelect={onSelect}
              collapsed={collapsed}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function vaultToSidebar(nodes: VaultTreeNode[]): SidebarNode[] {
  return nodes.map((n) => {
    if (n.type === 'folder') {
      return {
        id: `vault-${n.folder.path}`,
        label: n.folder.name,
        kind: 'folder' as const,
        children: vaultToSidebar(n.folder.children),
      };
    }
    return {
      id: `vault-${n.note.path}`,
      label: n.note.name,
      kind: 'note' as const,
    };
  });
}

export function CreateWorkspaceDialog({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => { inputRef.current?.focus(); }, []);

  const handleCreate = async () => {
    if (submitting) return;
    setSubmitting(true);
    try {
      await useWorkspaceStore.getState().apiCreateWorkspace({ name: name.trim() || undefined });
      onClose();
    } catch {
      setSubmitting(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-[#161210] border border-[#252018] border-t-2 border-t-[#5C4033] rounded-lg p-5 w-[380px] shadow-2xl animate-fade-in"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Create workspace"
      >
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-sm font-semibold text-[#DCD5CC]">Create Workspace</h3>
          <button onClick={onClose} className="text-[#6B6258] hover:text-[#DCD5CC]" aria-label="Close">
            <X size={14} />
          </button>
        </div>
        <div>
          <label htmlFor="ws-name" className="block text-xs text-[#6B6258] mb-1">Name (optional)</label>
          <input
            id="ws-name"
            ref={inputRef}
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            className="w-full bg-[#0A0A0A] text-sm text-[#DCD5CC] font-mono border border-[#252018] rounded px-3 py-1.5 outline-none focus:border-[#5C4033] transition-colors"
            placeholder="my-workspace"
            onKeyDown={(e) => { if (e.key === 'Enter') handleCreate(); }}
          />
        </div>
        <div className="flex justify-end gap-2 mt-4">
          <button onClick={onClose} className="px-3 py-1.5 text-xs rounded cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] transition-colors">
            Cancel
          </button>
          <button
            onClick={handleCreate}
            disabled={submitting}
            className="px-3 py-1.5 text-xs rounded cursor-pointer bg-[#5C4033] text-[#DCD5CC] hover:bg-[#7A5A4A] disabled:opacity-50 transition-colors flex items-center gap-1.5"
          >
            {submitting && <Loader2 size={12} className="animate-spin" />}
            {submitting ? 'Creating...' : 'Create'}
          </button>
        </div>
      </div>
    </div>
  );
}

function CreateNoteDialog({ onClose }: { onClose: () => void }) {
  const [path, setPath] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const openInPane = useUiStore((s) => s.openInPane);

  useEffect(() => { inputRef.current?.focus(); }, []);

  const handleCreate = async () => {
    const trimmed = path.trim();
    if (!trimmed || submitting) return;
    setSubmitting(true);
    const notePath = trimmed.endsWith('.md') ? trimmed : `${trimmed}.md`;
    try {
      await useVaultStore.getState().createNote(notePath);
      const name = notePath.split('/').pop()?.replace(/\.md$/, '') || notePath;
      openInPane('vault-note', name, { path: notePath });
      onClose();
    } catch {
      setSubmitting(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-[#161210] border border-[#252018] rounded-lg p-5 w-[380px] shadow-2xl animate-fade-in"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Create note"
      >
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-sm font-semibold text-[#DCD5CC]">New Note</h3>
          <button onClick={onClose} className="text-[#6B6258] hover:text-[#DCD5CC]" aria-label="Close">
            <X size={14} />
          </button>
        </div>
        <div>
          <label htmlFor="note-path" className="block text-xs text-[#6B6258] mb-1">Note path</label>
          <input
            id="note-path"
            ref={inputRef}
            type="text"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            className="w-full bg-[#0A0A0A] text-sm text-[#DCD5CC] border border-[#252018] rounded px-3 py-1.5 outline-none focus:border-[#5C4033] transition-colors"
            placeholder="folder/my-note"
            onKeyDown={(e) => { if (e.key === 'Enter') handleCreate(); }}
          />
        </div>
        <div className="flex justify-end gap-2 mt-4">
          <button onClick={onClose} className="px-3 py-1.5 text-xs rounded cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] transition-colors">
            Cancel
          </button>
          <button
            onClick={handleCreate}
            disabled={!path.trim() || submitting}
            className="px-3 py-1.5 text-xs rounded cursor-pointer bg-[#5C4033] text-[#DCD5CC] hover:bg-[#7A5A4A] disabled:opacity-50 transition-colors"
          >
            {submitting ? 'Creating...' : 'Create'}
          </button>
        </div>
      </div>
    </div>
  );
}

function SidebarContent({ collapsed }: { collapsed: boolean }) {
  const sidebarSection = useUiStore((s) => s.sidebarSection);
  const setSidebarSection = useUiStore((s) => s.setSidebarSection);
  const openInPane = useUiStore((s) => s.openInPane);
  const panes = useUiStore((s) => s.panes);
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const teams = useTeamStore((s) => s.teams);
  const tasks = useTeamStore((s) => s.tasks);
  const vaultTree = useVaultStore((s) => s.tree);

  // Compute which sidebar items are currently open in pane tabs
  const openItemIds = useMemo(() => {
    const ids = new Set<string>();
    for (const pane of panes) {
      for (const tab of pane.tabs) {
        if (tab.type === 'workspace-detail' && tab.data?.workspaceId) {
          ids.add(tab.data.workspaceId as string);
        } else if (tab.type === 'vault-note' && tab.data?.path) {
          ids.add(`vault-${tab.data.path as string}`);
        } else if (tab.type === 'team-detail' && tab.data?.teamName) {
          ids.add(`team-${tab.data.teamName as string}`);
        }
      }
    }
    return ids;
  }, [panes]);

  const [expandedIds, setExpandedIds] = useState<Set<string>>(
    new Set(['ws-running', 'ws-creating', 'ws-idle'])
  );

  const setCreateWorkspaceDialogOpen = useUiStore((s) => s.setCreateWorkspaceDialogOpen);
  const [showCreateNoteDialog, setShowCreateNoteDialog] = useState(false);

  const openCreateTaskDialog = useUiStore((s) => s.openCreateTaskDialog);

  const handlePlusClick = useCallback(() => {
    if (sidebarSection === 'teams') {
      useNotificationStore.getState().addNotification({
        type: 'system',
        title: 'Create teams via MCP tools',
        message: 'Team creation requires role definitions — use the team MCP tool instead.',
      });
    } else if (sidebarSection === 'workspaces') {
      setCreateWorkspaceDialogOpen(true);
    } else if (sidebarSection === 'tasks') {
      if (teams.length > 0) {
        openCreateTaskDialog(teams[0].name);
      } else {
        useNotificationStore.getState().addNotification({
          type: 'system',
          title: 'No teams available',
          message: 'Tasks belong to teams. Create a team via MCP tools first.',
        });
      }
    } else {
      setShowCreateNoteDialog(true);
    }
  }, [sidebarSection, setCreateWorkspaceDialogOpen, openCreateTaskDialog, teams]);

  const toggleExpanded = useCallback((id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handleSelect = useCallback((node: SidebarNode) => {
    if (node.kind === 'workspace' || node.kind === 'member') {
      openInPane('workspace-detail', node.label, { workspaceId: node.id });
    } else if (node.kind === 'note') {
      const path = node.id.startsWith('vault-') ? node.id.slice(6) : node.id;
      openInPane('vault-note', node.label, { path });
    } else if (node.kind === 'team') {
      const teamName = node.id.startsWith('team-') ? node.id.slice(5) : node.id;
      openInPane('team-detail', teamName, { teamName });
    }
  }, [openInPane]);

  const stateGroups: { state: WorkspaceState; label: string }[] = [
    { state: 'running', label: 'Running' },
    { state: 'creating', label: 'Creating' },
    { state: 'idle', label: 'Idle' },
    { state: 'stopped', label: 'Stopped' },
  ];

  const workspaceTree: SidebarNode[] = stateGroups
    .map(({ state, label }) => {
      const ws = workspaces.filter((w) => w.state === state);
      if (ws.length === 0) return null;
      return {
        id: `ws-${state}`,
        label: `${label} (${ws.length})`,
        kind: 'section' as const,
        children: ws.map((w) => ({
          id: w.id,
          label: w.name,
          kind: 'workspace' as const,
          state: w.state,
        })),
      };
    })
    .filter(Boolean) as SidebarNode[];

  const teamTree: SidebarNode[] = teams.map((t) => ({
    id: `team-${t.name}`,
    label: `${t.name} (${t.members.length})`,
    kind: 'team' as const,
    children: t.members.map((m) => ({
      id: m.workspace_id,
      label: m.name,
      kind: 'member' as const,
      state: m.status === 'busy' ? 'running' : m.status === 'idle' ? 'idle' : 'stopped',
    })),
  }));

  const taskStatusGroups: { status: string; label: string }[] = [
    { status: 'in_progress', label: 'In Progress' },
    { status: 'claimed', label: 'Claimed' },
    { status: 'pending', label: 'Pending' },
    { status: 'completed', label: 'Completed' },
    { status: 'failed', label: 'Failed' },
  ];

  const taskTree: SidebarNode[] = taskStatusGroups
    .map(({ status, label }) => {
      const matching = tasks.filter((t) => t.status === status);
      if (matching.length === 0) return null;
      return {
        id: `task-status-${status}`,
        label: `${label} (${matching.length})`,
        kind: 'section' as const,
        children: matching.map((t) => ({
          id: `task-${t.id}`,
          label: t.title,
          kind: 'task' as const,
          state: t.status,
        })),
      };
    })
    .filter(Boolean) as SidebarNode[];

  const vaultSidebar = vaultToSidebar(vaultTree);

  const treeData: Record<Section, SidebarNode[]> = {
    workspaces: workspaceTree,
    tasks: taskTree,
    vault: vaultSidebar,
    teams: teamTree,
  };

  return (
    <>
      {/* Section tabs */}
      <div className="flex items-center border-b border-[var(--border)]" role="tablist" aria-label="Sidebar sections">
        {sectionConfig.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            role="tab"
            aria-selected={sidebarSection === id}
            aria-controls={`sidebar-panel-${id}`}
            onClick={() => setSidebarSection(id)}
            className={`flex-1 flex items-center justify-center gap-0.5 h-9 text-[11px] transition-colors mx-0.5 ${
              sidebarSection === id
                ? 'text-[var(--text)] bg-[#5C4033]/15 rounded-md'
                : 'text-[var(--text-muted)] hover:text-[var(--text)]'
            }`}
            title={label}
            aria-label={label}
          >
            <Icon size={15} />
            {!collapsed && <span>{label}</span>}
          </button>
        ))}
        {/* Add button at the end of tab row */}
        <button
          onClick={handlePlusClick}
          className="mr-0.5 w-6 h-6 flex items-center justify-center rounded hover:bg-[var(--surface-2)] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors shrink-0"
          title={`New ${sidebarSection.slice(0, -1)}`}
          aria-label={`Create new ${sidebarSection.slice(0, -1)}`}
        >
          <Plus size={14} />
        </button>
      </div>

      {/* Tree content */}
      <div
        className="flex-1 overflow-y-auto overflow-x-hidden py-1"
        role="tree"
        aria-label={`${sidebarSection} tree`}
        id={`sidebar-panel-${sidebarSection}`}
      >
        {treeData[sidebarSection].map((node) => (
          <TreeItem
            key={node.id}
            node={node}
            depth={0}
            expandedIds={expandedIds}
            openItemIds={openItemIds}
            onToggle={toggleExpanded}
            onSelect={handleSelect}
            collapsed={collapsed}
          />
        ))}
        {treeData[sidebarSection].length === 0 && !collapsed && (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            {sidebarSection === 'workspaces' ? (
              <>
                <Package size={24} className="text-[#4A4238] mb-2" />
                <p className="text-sm text-[#6B6258]">No workspaces yet</p>
                <button
                  onClick={() => setCreateWorkspaceDialogOpen(true)}
                  className="mt-3 text-xs text-[#7A5A4A] hover:text-[#8B6B5A]"
                >
                  Create a workspace
                </button>
              </>
            ) : sidebarSection === 'tasks' ? (
              <>
                <ListTodo size={24} className="text-[#4A4238] mb-2" />
                <p className="text-sm text-[#6B6258]">No tasks yet</p>
              </>
            ) : sidebarSection === 'teams' ? (
              <>
                <Users size={24} className="text-[#4A4238] mb-2" />
                <p className="text-sm text-[#6B6258]">No teams yet</p>
              </>
            ) : (
              <>
                <FileText size={24} className="text-[#4A4238] mb-2" />
                <p className="text-sm text-[#6B6258]">No notes yet</p>
                <button
                  onClick={() => setShowCreateNoteDialog(true)}
                  className="mt-3 text-xs text-[#7A5A4A] hover:text-[#8B6B5A]"
                >
                  Create your first note
                </button>
              </>
            )}
          </div>
        )}
      </div>

      {showCreateNoteDialog && (
        <CreateNoteDialog onClose={() => setShowCreateNoteDialog(false)} />
      )}
    </>
  );
}

export function Sidebar() {
  const sidebarOpen = useUiStore((s) => s.sidebarOpen);
  const sidebarCollapsed = useUiStore((s) => s.sidebarCollapsed);
  const mobileMenuOpen = useUiStore((s) => s.mobileMenuOpen);
  const setMobileMenuOpen = useUiStore((s) => s.setMobileMenuOpen);

  // Close mobile menu on Escape
  useEffect(() => {
    if (!mobileMenuOpen) return;
    function handleKey(e: KeyboardEvent) {
      if (e.key === 'Escape') setMobileMenuOpen(false);
    }
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [mobileMenuOpen, setMobileMenuOpen]);

  // Mobile overlay sidebar (< 768px)
  const isMobile = typeof window !== 'undefined' && window.innerWidth < 768;

  if (isMobile) {
    if (!mobileMenuOpen) return null;
    return (
      <>
        {/* Backdrop */}
        <div
          className="fixed inset-0 z-40 bg-black/60 animate-fade-in"
          onClick={() => setMobileMenuOpen(false)}
          aria-hidden="true"
        />
        <aside
          className="fixed left-0 top-14 bottom-0 z-50 flex flex-col bg-[var(--surface)] border-r border-[var(--border)] select-none animate-slide-in"
          style={{ width: 280 }}
          role="navigation"
          aria-label="Main navigation"
        >
          <div className="flex items-center justify-between px-3 h-8 border-b border-[var(--border)]">
            <span className="text-xs font-semibold text-[var(--text-muted)]">Navigation</span>
            <button
              onClick={() => setMobileMenuOpen(false)}
              className="w-5 h-5 flex items-center justify-center rounded hover:bg-[var(--surface-2)] text-[var(--text-muted)]"
              aria-label="Close navigation"
            >
              <X size={12} />
            </button>
          </div>
          <SidebarContent collapsed={false} />
        </aside>
      </>
    );
  }

  // Desktop / tablet
  if (!sidebarOpen) return null;

  const isCollapsed = sidebarCollapsed;

  return (
    <aside
      className="flex flex-col h-full bg-[var(--surface)] border-r border-[var(--border)] select-none shrink-0 transition-[width] duration-200"
      style={{ width: isCollapsed ? 52 : 240 }}
      role="navigation"
      aria-label="Main navigation"
    >
      <SidebarContent collapsed={isCollapsed} />
    </aside>
  );
}
