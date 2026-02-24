import { wsManager } from './websocket';
import { useWorkspaceStore } from '../stores/workspaces';
import { useTeamStore } from '../stores/teams';
import { useMetricsStore } from '../stores/metrics';
import { useNotificationStore } from '../stores/notifications';
import { apiInfoToWorkspace } from './workspaces';
import type { WorkspaceApiInfo } from './workspaces';

// ---------------------------------------------------------------------------
// Connection state (reactive via zustand-like pattern)
// ---------------------------------------------------------------------------

type ConnectionStatus = 'connected' | 'disconnected' | 'reconnecting';
type ConnectionListener = (status: ConnectionStatus) => void;

let currentStatus: ConnectionStatus = 'disconnected';
const statusListeners = new Set<ConnectionListener>();

export function getConnectionStatus(): ConnectionStatus {
  return currentStatus;
}

export function onConnectionStatusChange(listener: ConnectionListener): () => void {
  statusListeners.add(listener);
  return () => { statusListeners.delete(listener); };
}

function setConnectionStatus(status: ConnectionStatus) {
  if (status === currentStatus) return;
  currentStatus = status;
  for (const listener of statusListeners) {
    listener(status);
  }
}

// ---------------------------------------------------------------------------
// WebSocket event data shapes
// ---------------------------------------------------------------------------

interface WorkspaceEvent {
  event: 'state_changed' | 'created' | 'destroyed' | 'workspace.created' | 'workspace.started' | 'workspace.stopped' | 'workspace.destroyed';
  workspace?: WorkspaceApiInfo;
  workspace_id?: string;
  workspace_name?: string;
  state?: string;
  detail?: Record<string, unknown>;
}

interface TeamEvent {
  event: 'created' | 'destroyed' | 'member_changed';
  team_name?: string;
  team?: {
    name: string;
    state: string;
    members: Array<{
      name: string;
      role: string;
      skills: string[];
      status: string;
      workspace_id: string;
      ip: string;
    }>;
    max_vms: number;
    parent_team: string | null;
    created_at: string;
  };
}

interface TeamMessageEvent {
  from: string;
  to: string | null;
  content: string;
  type: 'message' | 'broadcast';
  timestamp: string;
}

interface MetricsEvent {
  workspace_counts?: { creating: number; running: number; idle: number; stopped: number };
  team_count?: number;
  exec_total?: number;
  exec_errors?: number;
  vm_boot_avg_ms?: number;
  cpu_percent?: number;
  memory_used_mb?: number;
  memory_total_mb?: number;
  zfs_used_gb?: number;
  zfs_total_gb?: number;
  warm_pool_size?: number;
}

// ---------------------------------------------------------------------------
// Per-workspace subscription tracking
// ---------------------------------------------------------------------------

const perWorkspaceUnsubs = new Map<string, () => void>();

export function subscribeToWorkspace(workspaceId: string) {
  if (perWorkspaceUnsubs.has(workspaceId)) return;
  const unsub = wsManager.subscribe(`workspace:${workspaceId}`, (_msg) => {
    // Per-workspace events (exec output streaming, etc.) handled in future phases
  });
  perWorkspaceUnsubs.set(workspaceId, unsub);
}

export function unsubscribeFromWorkspace(workspaceId: string) {
  const unsub = perWorkspaceUnsubs.get(workspaceId);
  if (unsub) {
    unsub();
    perWorkspaceUnsubs.delete(workspaceId);
  }
}

// ---------------------------------------------------------------------------
// Main sync init — call once on app mount
// ---------------------------------------------------------------------------

let initialized = false;
const unsubscribers: Array<() => void> = [];

export function initRealtimeSync() {
  if (initialized) return;
  initialized = true;

  // Monitor connection status
  const statusPoll = setInterval(() => {
    if (wsManager.connected) {
      setConnectionStatus('connected');
    } else if (currentStatus === 'connected') {
      setConnectionStatus('reconnecting');
    } else {
      setConnectionStatus('disconnected');
    }
  }, 1000);
  unsubscribers.push(() => clearInterval(statusPoll));

  // Connect
  wsManager.connect();

  // Subscribe to workspace events (handles both polling-based and event-driven formats)
  unsubscribers.push(
    wsManager.subscribe('workspaces', (msg) => {
      const data = msg.data as WorkspaceEvent;
      const addNotif = useNotificationStore.getState().addNotification;

      // Normalize event name: event-driven broadcasts use "workspace.started" etc.
      const event = data.event ?? msg.type;

      switch (event) {
        case 'state_changed': {
          if (data.workspace) {
            const ws = apiInfoToWorkspace(data.workspace);
            useWorkspaceStore.getState().updateWorkspace(ws.id, ws);
            addNotif({
              type: 'workspace',
              title: `${ws.name} is now ${ws.state}`,
              message: `Workspace ${ws.name} changed to ${ws.state}`,
            });
          } else if (data.workspace_id && data.state) {
            useWorkspaceStore.getState().updateWorkspace(data.workspace_id, {
              state: mapWsState(data.state),
            });
          }
          break;
        }
        case 'workspace.started': {
          if (data.workspace_id) {
            useWorkspaceStore.getState().updateWorkspace(data.workspace_id, {
              state: 'running',
            });
            addNotif({
              type: 'workspace',
              title: `${data.workspace_name ?? data.workspace_id.slice(0, 8)} started`,
              message: `Workspace is now running`,
            });
          }
          break;
        }
        case 'workspace.stopped': {
          if (data.workspace_id) {
            useWorkspaceStore.getState().updateWorkspace(data.workspace_id, {
              state: 'stopped',
            });
            addNotif({
              type: 'workspace',
              title: `${data.workspace_name ?? data.workspace_id.slice(0, 8)} stopped`,
              message: `Workspace has been stopped`,
            });
          }
          break;
        }
        case 'created':
        case 'workspace.created': {
          if (data.workspace) {
            const ws = apiInfoToWorkspace(data.workspace);
            useWorkspaceStore.getState().addWorkspace(ws);
            addNotif({
              type: 'workspace',
              title: `${ws.name} created`,
              message: `Workspace ${ws.name} is now available`,
            });
          } else if (data.workspace_id) {
            // Event-driven create: we only have the ID, trigger a refresh
            addNotif({
              type: 'workspace',
              title: `${data.workspace_name ?? 'Workspace'} created`,
              message: `New workspace is now available`,
            });
          }
          break;
        }
        case 'destroyed':
        case 'workspace.destroyed': {
          if (data.workspace_id) {
            useWorkspaceStore.getState().removeWorkspace(data.workspace_id);
            addNotif({
              type: 'workspace',
              title: 'Workspace destroyed',
              message: `Workspace ${data.workspace_name ?? data.workspace_id.slice(0, 8)} has been removed`,
            });
          }
          break;
        }
      }
    })
  );

  // Subscribe to team events
  unsubscribers.push(
    wsManager.subscribe('teams', (msg) => {
      const data = msg.data as TeamEvent;
      const addNotif = useNotificationStore.getState().addNotification;

      switch (data.event) {
        case 'created': {
          if (data.team) {
            useTeamStore.getState().addTeam({
              name: data.team.name,
              state: data.team.state as 'Creating' | 'Ready' | 'Working' | 'Completing' | 'Destroyed',
              members: data.team.members.map((m) => ({
                ...m,
                status: m.status as 'idle' | 'busy' | 'offline',
              })),
              max_vms: data.team.max_vms,
              parent_team: data.team.parent_team,
              created_at: data.team.created_at,
            });
            addNotif({
              type: 'team',
              title: `Team ${data.team.name} created`,
              message: `Team with ${data.team.members.length} members`,
            });
          }
          break;
        }
        case 'destroyed': {
          if (data.team_name) {
            useTeamStore.getState().removeTeam(data.team_name);
            addNotif({
              type: 'team',
              title: `Team ${data.team_name} destroyed`,
              message: `Team ${data.team_name} has been removed`,
            });
          }
          break;
        }
      }
    })
  );

  // Subscribe to team messages (wildcard — handles team:{name}:messages)
  unsubscribers.push(
    wsManager.subscribe('*', (msg) => {
      // Match team:{name}:messages topics
      const match = msg.topic.match(/^team:([^:]+):messages$/);
      if (!match) return;
      const data = msg.data as TeamMessageEvent;
      const addNotif = useNotificationStore.getState().addNotification;

      useTeamStore.getState().addMessage({
        id: `msg-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
        from: data.from,
        to: data.to,
        content: data.content,
        type: data.type,
        timestamp: data.timestamp,
      });

      const preview = data.content.length > 60
        ? data.content.slice(0, 60) + '...'
        : data.content;
      addNotif({
        type: 'team',
        title: `${data.from}: ${data.type === 'broadcast' ? '(broadcast)' : ''}`,
        message: preview,
      });
    })
  );

  // Subscribe to metrics
  unsubscribers.push(
    wsManager.subscribe('metrics', (msg) => {
      const data = msg.data as MetricsEvent;
      useMetricsStore.getState().update(data);
    })
  );
}

export function teardownRealtimeSync() {
  for (const unsub of unsubscribers) {
    unsub();
  }
  unsubscribers.length = 0;
  for (const unsub of perWorkspaceUnsubs.values()) {
    unsub();
  }
  perWorkspaceUnsubs.clear();
  wsManager.disconnect();
  initialized = false;
  setConnectionStatus('disconnected');
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function mapWsState(s: string): 'creating' | 'running' | 'idle' | 'stopped' {
  switch (s.toLowerCase()) {
    case 'running': return 'running';
    case 'creating': return 'creating';
    case 'idle':
    case 'suspended': return 'idle';
    default: return 'stopped';
  }
}
