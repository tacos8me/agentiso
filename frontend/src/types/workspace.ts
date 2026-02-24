// Canonical workspace + task types used by kanban and other components.
// These match the shapes in mocks/workspaces.ts and the backend API.

export type WorkspaceState = 'creating' | 'running' | 'idle' | 'stopped';
export type TaskStatus = 'pending' | 'claimed' | 'in_progress' | 'completed' | 'failed';
export type TaskPriority = 'low' | 'medium' | 'high' | 'critical';
export type BoardType = 'workspaces' | 'tasks';

export interface WorkspaceNetwork {
  ip: string;
  internet_enabled: boolean;
}

export interface WorkspaceResources {
  memory_mb: number;
  vcpus: number;
  disk_mb: number;
  disk_used_mb: number;
}

export interface WorkspaceSnapshot {
  name: string;
  created_at: string;
  size_mb: number;
}

export interface ExecResult {
  command: string;
  exit_code: number;
  duration_ms: number;
  timestamp: string;
}

export interface Workspace {
  id: string;
  name: string;
  state: WorkspaceState;
  network: WorkspaceNetwork;
  resources: WorkspaceResources;
  team_id: string | null;
  team_name: string | null;
  forked_from: string | null;
  created_at: string;
  uptime_seconds: number;
  last_exec: ExecResult | null;
  snapshots: WorkspaceSnapshot[];
  cpu_history: number[];
  qemu_pid: number | null;
  vsock_cid: number | null;
}

export interface BoardTask {
  id: string;
  title: string;
  description: string;
  status: TaskStatus;
  owner: string | null;
  priority: TaskPriority;
  depends_on: string[];
  team_name: string;
  created_at: string;
  completed_at: string | null;
}
