import { useEffect, lazy, Suspense } from 'react';
import { Group, Panel, Separator } from 'react-resizable-panels';
import { X } from 'lucide-react';
import { useUiStore } from '../../stores/ui';
import { useWorkspaceStore } from '../../stores/workspaces';
import { useTeamStore } from '../../stores/teams';
import { useVaultStore } from '../../stores/vault';
import { Board } from '../kanban/Board';
import { LoadingSkeleton } from '../common/LoadingSkeleton';
import type { PaneConfig, PaneType } from '../../types';

const TerminalTabs = lazy(() => import('../terminal/TerminalTabs').then(m => ({ default: m.TerminalTabs })));
const NoteEditor = lazy(() => import('../vault/NoteEditor').then(m => ({ default: m.NoteEditor })));
const BacklinkPanel = lazy(() => import('../vault/BacklinkPanel').then(m => ({ default: m.BacklinkPanel })));
const GraphView = lazy(() => import('../vault/GraphView').then(m => ({ default: m.GraphView })));
const TeamDetailPane = lazy(() => import('../teams/TeamDetailPane').then(m => ({ default: m.TeamDetailPane })));
const TeamChat = lazy(() => import('../teams/TeamChat').then(m => ({ default: m.TeamChat })));
const WorkspaceDetailPane = lazy(() => import('../workspace/WorkspaceDetailPane').then(m => ({ default: m.WorkspaceDetailPane })));

// Empty state component with CSS-only decorative shapes
function EmptyState({ type, title, description }: {
  type: 'workspaces' | 'vault' | 'teams' | 'terminal' | 'generic';
  title: string;
  description: string;
}) {
  const shapeClass = {
    workspaces: 'empty-state-shape',
    vault: 'empty-state-shape-circle',
    teams: 'empty-state-shape-lines',
    terminal: 'empty-state-shape-lines',
    generic: 'empty-state-shape',
  }[type];

  return (
    <div className="flex flex-col items-center justify-center h-full gap-4 px-6">
      <div className={shapeClass} aria-hidden="true" />
      <div className="text-center">
        <p className="text-sm text-[var(--text-muted)]">{title}</p>
        <p className="text-xs text-[var(--text-dim)] mt-1">{description}</p>
      </div>
    </div>
  );
}

function KanbanPane() {
  const openInPane = useUiStore((s) => s.openInPane);
  const sidebarSection = useUiStore((s) => s.sidebarSection);
  const activeCustomBoardId = useUiStore((s) => s.activeCustomBoardId);
  const customBoards = useUiStore((s) => s.customBoards);
  const setActiveCustomBoard = useUiStore((s) => s.setActiveCustomBoard);
  const activeCustomBoard = activeCustomBoardId
    ? customBoards.find((b) => b.id === activeCustomBoardId) ?? null
    : null;
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const loading = useWorkspaceStore((s) => s.loading);
  const tasks = useTeamStore((s) => s.tasks);

  const apiStartWorkspace = useWorkspaceStore((s) => s.apiStartWorkspace);
  const apiStopWorkspace = useWorkspaceStore((s) => s.apiStopWorkspace);
  const apiDestroyWorkspace = useWorkspaceStore((s) => s.apiDestroyWorkspace);
  const apiCreateSnapshot = useWorkspaceStore((s) => s.apiCreateSnapshot);
  const apiForkWorkspace = useWorkspaceStore((s) => s.apiForkWorkspace);
  const apiUpdateNetworkPolicy = useWorkspaceStore((s) => s.apiUpdateNetworkPolicy);

  const startWorkspacePolling = useWorkspaceStore((s) => s.startPolling);
  const startTeamPolling = useTeamStore((s) => s.startPolling);

  useEffect(() => {
    const stopWs = startWorkspacePolling();
    const stopTeams = startTeamPolling();
    return () => { stopWs(); stopTeams(); };
  }, [startWorkspacePolling, startTeamPolling]);

  // Sidebar section drives the board type
  const boardType = sidebarSection === 'tasks' ? 'tasks' : 'workspaces';

  return (
    <Board
      workspaces={workspaces}
      tasks={tasks}
      boardType={boardType as 'workspaces' | 'tasks'}
      loading={loading}
      activeCustomBoard={activeCustomBoard}
      onActiveCustomBoardChange={setActiveCustomBoard}
      onWorkspaceStateChange={(id, newState) => {
        if (newState === 'running') apiStartWorkspace(id);
        else if (newState === 'stopped') apiStopWorkspace(id);
      }}
      onWorkspaceAction={(workspaceId, action) => {
        switch (action) {
          case 'start': apiStartWorkspace(workspaceId); break;
          case 'stop': apiStopWorkspace(workspaceId); break;
          case 'destroy': apiDestroyWorkspace(workspaceId); break;
          case 'snapshot': apiCreateSnapshot(workspaceId, `snap-${Date.now()}`); break;
          case 'fork': apiForkWorkspace(workspaceId); break;
          case 'enable_internet': apiUpdateNetworkPolicy(workspaceId, true); break;
          case 'disable_internet': apiUpdateNetworkPolicy(workspaceId, false); break;
          case 'terminal': openInPane('terminal', 'Terminal', { workspaceId }); break;
          case 'logs': openInPane('terminal', 'Logs', { workspaceId }); break;
        }
      }}
    />
  );
}

function VaultNotePane({ path }: { path?: string }) {
  const openNote = useVaultStore((s) => s.openNote);
  const currentNote = useVaultStore((s) => s.currentNote);
  const currentPath = useVaultStore((s) => s.currentPath);
  const loading = useVaultStore((s) => s.loading);
  const loadGraph = useVaultStore((s) => s.loadGraph);
  const graphData = useVaultStore((s) => s.graphData);
  const openInPane = useUiStore((s) => s.openInPane);

  useEffect(() => {
    if (path && path !== currentPath) {
      openNote(path);
    }
    if (!graphData) loadGraph();
  }, [path, currentPath, openNote, graphData, loadGraph]);

  if (loading) {
    return <LoadingSkeleton variant="editor" />;
  }

  if (!currentNote) {
    return (
      <EmptyState
        type="vault"
        title="No note selected"
        description="Select a note from the sidebar to start reading."
      />
    );
  }

  return (
    <Suspense fallback={<LoadingSkeleton variant="editor" />}>
      <div className="flex flex-col h-full">
        <div className="flex-1 overflow-hidden">
          <NoteEditor
            note={currentNote}
            onNavigate={(name) => {
              openInPane('vault-note', name, { path: name });
            }}
            onTagClick={() => {}}
          />
        </div>
        <BacklinkPanel
          notePath={currentNote.path}
          onNavigate={(navPath) => {
            openInPane('vault-note', navPath.split('/').pop() || navPath, { path: navPath });
          }}
        />
      </div>
    </Suspense>
  );
}

function GraphPane() {
  const openInPane = useUiStore((s) => s.openInPane);

  return (
    <Suspense fallback={<LoadingSkeleton variant="card" />}>
      <GraphView
        onNoteSelect={(notePath) => {
          const name = notePath.split('/').pop()?.replace(/\.md$/, '') || notePath;
          openInPane('vault-note', name, { path: notePath });
        }}
      />
    </Suspense>
  );
}

function PaneContent({ type, data }: { type: PaneType; data?: Record<string, unknown> }) {
  switch (type) {
    case 'kanban':
      return <KanbanPane />;
    case 'workspace-detail':
      return (
        <Suspense fallback={<LoadingSkeleton variant="card" />}>
          <WorkspaceDetailPane workspaceId={data?.workspaceId as string ?? ''} />
        </Suspense>
      );
    case 'vault-note':
      return <VaultNotePane path={data?.path as string | undefined} />;
    case 'terminal':
      return (
        <Suspense fallback={<LoadingSkeleton variant="terminal" />}>
          <TerminalTabs workspaceId={data?.workspaceId as string | undefined} />
        </Suspense>
      );
    case 'team-chat':
      return (
        <Suspense fallback={<LoadingSkeleton variant="card" />}>
          <TeamChat teamName={data?.team as string ?? ''} />
        </Suspense>
      );
    case 'graph':
      return <GraphPane />;
    case 'team-detail':
      return (
        <Suspense fallback={<LoadingSkeleton variant="card" />}>
          <TeamDetailPane teamName={data?.teamName as string ?? ''} />
        </Suspense>
      );
    default:
      return (
        <EmptyState
          type="generic"
          title="Unknown pane type"
          description="This pane type is not recognized."
        />
      );
  }
}

function PaneTabBar({ pane }: { pane: PaneConfig }) {
  const setActiveTab = useUiStore((s) => s.setActiveTab);
  const removeTabFromPane = useUiStore((s) => s.removeTabFromPane);

  return (
    <div
      className="flex items-center h-9 bg-[var(--surface)] border-b border-[var(--border)] shrink-0 overflow-x-auto"
      role="tablist"
      aria-label="Pane tabs"
    >
      {pane.tabs.map((tab) => {
        const isActive = tab.id === pane.activeTabId;
        return (
          <div
            key={tab.id}
            role="tab"
            aria-selected={isActive}
            tabIndex={isActive ? 0 : -1}
            className={`flex items-center gap-1.5 h-full px-3 text-[13px] cursor-pointer border-r border-[var(--border)] transition-colors group ${
              isActive
                ? 'bg-[var(--bg)] text-[var(--text)] active-tab-indicator'
                : 'text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-[var(--surface-2)]'
            }`}
            onClick={() => setActiveTab(pane.id, tab.id)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                setActiveTab(pane.id, tab.id);
              }
            }}
          >
            <span className="truncate max-w-[120px]">{tab.title}</span>
            {pane.tabs.length > 1 && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  removeTabFromPane(pane.id, tab.id);
                }}
                className="w-5 h-5 flex items-center justify-center rounded opacity-0 group-hover:opacity-100 hover:bg-[var(--surface-3)] transition-opacity"
                aria-label={`Close ${tab.title} tab`}
              >
                <X size={10} />
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}

function SinglePane({ pane }: { pane: PaneConfig }) {
  const activeTab = pane.tabs.find((t) => t.id === pane.activeTabId) ?? pane.tabs[0];
  if (!activeTab) return null;

  return (
    <div className="flex flex-col h-full" role="tabpanel" aria-label={activeTab.title}>
      {pane.tabs.length > 1 && <PaneTabBar pane={pane} />}
      <div className="flex-1 overflow-hidden">
        <PaneContent type={activeTab.type} data={activeTab.data} />
      </div>
    </div>
  );
}

export function PaneManager() {
  const panes = useUiStore((s) => s.panes);

  if (panes.length === 0) {
    return (
      <EmptyState
        type="generic"
        title="No panes open"
        description="Use the sidebar or Ctrl+K to get started."
      />
    );
  }

  if (panes.length === 1) {
    return (
      <div className="h-full bg-[var(--bg)]">
        <SinglePane pane={panes[0]} />
      </div>
    );
  }

  // Multiple panes: split with resizable panels
  // On narrow screens (< 1024px), panels stack vertically via CSS
  const elements: React.ReactNode[] = [];
  panes.forEach((pane, i) => {
    if (i > 0) {
      elements.push(
        <Separator
          key={`sep-${pane.id}`}
          className="w-1 bg-[var(--border)] hover:bg-[var(--accent)] transition-colors cursor-col-resize"
        />
      );
    }
    elements.push(
      <Panel key={pane.id} id={pane.id} minSize={15}>
        <SinglePane pane={pane} />
      </Panel>
    );
  });

  return (
    <div className="h-full bg-[var(--bg)] pane-stack-vertical">
      <Group orientation="horizontal" className="h-full">
        {elements}
      </Group>
    </div>
  );
}
