import { useEffect } from 'react';
import { TopBar } from './components/layout/TopBar';
import { Sidebar, CreateWorkspaceDialog } from './components/layout/Sidebar';
import { CreateTaskDialog } from './components/teams/CreateTaskDialog';
import { PaneManager } from './components/layout/PaneManager';
import { CommandPalette } from './components/layout/CommandPalette';
import { ToastContainer } from './components/common/Toast';
import { useKeyboard } from './hooks/useKeyboard';
import { initRealtimeSync, teardownRealtimeSync } from './api/realtimeSync';
import { useResponsive } from './hooks/useResponsive';
import { useUiStore } from './stores/ui';
import { useTeamStore } from './stores/teams';
import { useWorkspaceStore } from './stores/workspaces';

export default function App() {
  useKeyboard();
  useResponsive();
  const createWorkspaceDialogOpen = useUiStore((s) => s.createWorkspaceDialogOpen);
  const setCreateWorkspaceDialogOpen = useUiStore((s) => s.setCreateWorkspaceDialogOpen);
  const createTaskDialogOpen = useUiStore((s) => s.createTaskDialogOpen);
  const createTaskDialogTeam = useUiStore((s) => s.createTaskDialogTeam);
  const setCreateTaskDialogOpen = useUiStore((s) => s.setCreateTaskDialogOpen);
  const tasks = useTeamStore((s) => s.tasks);
  const workspaces = useWorkspaceStore((s) => s.workspaces);

  useEffect(() => {
    initRealtimeSync();
    return () => teardownRealtimeSync();
  }, []);

  // Dynamic page title reflecting workspace counts
  useEffect(() => {
    const running = workspaces.filter((w) => w.state === 'running').length;
    const stopped = workspaces.filter((w) => w.state === 'stopped').length;
    if (workspaces.length === 0) {
      document.title = 'agentiso';
    } else {
      document.title = `agentiso — ${running} running, ${stopped} stopped`;
    }
  }, [workspaces]);

  return (
    <div className="flex flex-col h-screen w-screen overflow-hidden bg-[var(--bg)]">
      <a href="#main-content" className="skip-to-content">
        Skip to content
      </a>
      <TopBar />
      <div className="flex flex-1 overflow-hidden">
        <Sidebar />
        <main id="main-content" className="flex-1 overflow-hidden" role="main" aria-label="Main content">
          <PaneManager />
        </main>
      </div>
      <CommandPalette />
      {createWorkspaceDialogOpen && (
        <CreateWorkspaceDialog onClose={() => setCreateWorkspaceDialogOpen(false)} />
      )}
      {createTaskDialogOpen && createTaskDialogTeam && (
        <CreateTaskDialog
          teamName={createTaskDialogTeam}
          existingTasks={tasks.filter((t) => t.team_name === createTaskDialogTeam)}
          onClose={() => setCreateTaskDialogOpen(false)}
        />
      )}
      <ToastContainer />
      {/* Live region for screen reader announcements (DnD etc.) */}
      <div aria-live="polite" aria-atomic="true" className="sr-only" id="a11y-announcer" />
    </div>
  );
}
