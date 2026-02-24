/**
 * KanbanDemo -- renders the full kanban board wired to Zustand stores.
 * Stores fetch from the live REST API (or mocks when VITE_USE_MOCKS=true).
 *
 * Usage:
 *   import { KanbanDemo } from "./components/kanban/KanbanDemo";
 *   <KanbanDemo />
 */
import { useEffect } from "react";
import { Board } from "./Board";
import { useWorkspaceStore } from "../../stores/workspaces";
import { useTeamStore } from "../../stores/teams";
import type { WorkspaceState, TaskStatus } from "../../types/workspace";
import "./kanban.css";

export function KanbanDemo() {
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const loading = useWorkspaceStore((s) => s.loading);
  const startWorkspacePolling = useWorkspaceStore((s) => s.startPolling);

  const tasks = useTeamStore((s) => s.tasks);
  const teams = useTeamStore((s) => s.teams);
  const startTeamPolling = useTeamStore((s) => s.startPolling);

  const apiStartWorkspace = useWorkspaceStore((s) => s.apiStartWorkspace);
  const apiStopWorkspace = useWorkspaceStore((s) => s.apiStopWorkspace);
  const apiDestroyWorkspace = useWorkspaceStore((s) => s.apiDestroyWorkspace);
  const apiCreateSnapshot = useWorkspaceStore((s) => s.apiCreateSnapshot);
  const apiForkWorkspace = useWorkspaceStore((s) => s.apiForkWorkspace);
  const apiUpdateNetworkPolicy = useWorkspaceStore((s) => s.apiUpdateNetworkPolicy);

  // Start polling on mount
  useEffect(() => {
    const stopWs = startWorkspacePolling();
    const stopTeams = startTeamPolling();
    return () => {
      stopWs();
      stopTeams();
    };
  }, [startWorkspacePolling, startTeamPolling]);

  // Fetch tasks for all teams when teams change
  useEffect(() => {
    if (teams.length > 0) {
      // Fetch tasks for the first team (or selected team)
      useTeamStore.getState().fetchTasks(teams[0].name);
    }
  }, [teams]);

  const handleWorkspaceStateChange = (workspaceId: string, newState: WorkspaceState) => {
    switch (newState) {
      case "running":
        apiStartWorkspace(workspaceId);
        break;
      case "stopped":
        apiStopWorkspace(workspaceId);
        break;
      // Other state changes (creating, idle) can't be triggered via DnD
    }
  };

  const handleTaskStatusChange = (_taskId: string, _newStatus: TaskStatus) => {
    // Task board drag-and-drop state changes are not wired to API in this phase.
    // The backend task board uses individual claim/start/complete/fail endpoints,
    // not a generic "set status" endpoint.
  };

  const handleWorkspaceAction = (workspaceId: string, action: string) => {
    switch (action) {
      case "start":
        apiStartWorkspace(workspaceId);
        break;
      case "stop":
        apiStopWorkspace(workspaceId);
        break;
      case "destroy":
        apiDestroyWorkspace(workspaceId);
        break;
      case "snapshot":
        apiCreateSnapshot(workspaceId, `snap-${Date.now()}`);
        break;
      case "fork":
        apiForkWorkspace(workspaceId);
        break;
      case "enable_internet":
        apiUpdateNetworkPolicy(workspaceId, true);
        break;
      case "disable_internet":
        apiUpdateNetworkPolicy(workspaceId, false);
        break;
      // "terminal", "logs" -- handled by pane system, not here
    }
  };

  return (
    <div className="h-screen w-screen bg-[#0A0A0A] text-[#DCD5CC]">
      <Board
        workspaces={workspaces}
        tasks={tasks}
        loading={loading}
        onWorkspaceStateChange={handleWorkspaceStateChange}
        onTaskStatusChange={handleTaskStatusChange}
        onWorkspaceAction={handleWorkspaceAction}
      />
    </div>
  );
}
