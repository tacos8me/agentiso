import { api, ApiRequestError } from './client';
import type { Workspace } from '../types/workspace';

// ---------------------------------------------------------------------------
// Backend response shapes (match Rust WorkspaceInfo struct)
// ---------------------------------------------------------------------------

export interface WorkspaceApiInfo {
  id: string;
  name: string;
  state: string;
  base_image: string;
  ip: string;
  vsock_cid: number;
  vcpus: number;
  memory_mb: number;
  disk_gb: number;
  allow_internet: boolean;
  allow_inter_vm: boolean;
  qemu_pid: number | null;
  created_at: string;
  team_id: string | null;
  forked_from: string | null;
  snapshot_count: number;
}

export interface ExecResponse {
  exit_code: number;
  stdout: string;
  stderr: string;
}

export interface LogsResponse {
  console: string;
  stderr: string;
}

export interface SnapshotApiInfo {
  name: string;
  created_at: string;
  parent: string | null;
}

// ---------------------------------------------------------------------------
// Conversion: API shape -> frontend Workspace type
// ---------------------------------------------------------------------------

function mapState(s: string): Workspace['state'] {
  switch (s.toLowerCase()) {
    case 'running':
      return 'running';
    case 'stopped':
      return 'stopped';
    case 'suspended':
    case 'idle':
      return 'idle';
    case 'creating':
      return 'creating';
    default:
      return 'stopped';
  }
}

export function apiInfoToWorkspace(info: WorkspaceApiInfo): Workspace {
  return {
    id: info.id,
    name: info.name,
    state: mapState(info.state),
    network: {
      ip: info.ip,
      internet_enabled: info.allow_internet,
    },
    resources: {
      memory_mb: info.memory_mb,
      vcpus: info.vcpus,
      disk_mb: info.disk_gb * 1024,
      disk_used_mb: 0, // Not available from list endpoint
    },
    team_id: info.team_id,
    team_name: null, // Not returned by API; resolved client-side if needed
    forked_from: info.forked_from,
    created_at: info.created_at,
    uptime_seconds: 0, // Computed client-side from created_at for running VMs
    last_exec: null, // Not returned by list endpoint
    snapshots: [],
    cpu_history: [],
    qemu_pid: info.qemu_pid ?? null,
    vsock_cid: info.vsock_cid ?? null,
  };
}

// ---------------------------------------------------------------------------
// API functions
// ---------------------------------------------------------------------------

export async function fetchWorkspaces(): Promise<Workspace[]> {
  const infos = await api.get<WorkspaceApiInfo[]>('/workspaces');
  return infos.map(apiInfoToWorkspace);
}

export async function fetchWorkspace(id: string): Promise<Workspace> {
  const info = await api.get<WorkspaceApiInfo>(`/workspaces/${encodeURIComponent(id)}`);
  return apiInfoToWorkspace(info);
}

export async function createWorkspace(params: {
  name?: string;
  base_image?: string;
  vcpus?: number;
  memory_mb?: number;
  disk_gb?: number;
  allow_internet?: boolean;
}): Promise<Workspace> {
  const info = await api.post<WorkspaceApiInfo>('/workspaces', params);
  return apiInfoToWorkspace(info);
}

export async function destroyWorkspace(id: string): Promise<void> {
  await api.delete(`/workspaces/${encodeURIComponent(id)}`);
}

export async function startWorkspace(id: string): Promise<void> {
  await api.post(`/workspaces/${encodeURIComponent(id)}/start`);
}

export async function stopWorkspace(id: string): Promise<void> {
  await api.post(`/workspaces/${encodeURIComponent(id)}/stop`);
}

export async function execCommand(
  id: string,
  command: string,
  opts?: { timeout_secs?: number; workdir?: string },
): Promise<ExecResponse> {
  return api.post<ExecResponse>(`/workspaces/${encodeURIComponent(id)}/exec`, {
    command,
    ...opts,
  });
}

export async function getWorkspaceLogs(
  id: string,
  lines?: number,
): Promise<LogsResponse> {
  const q = lines ? `?lines=${lines}` : '';
  return api.get<LogsResponse>(`/workspaces/${encodeURIComponent(id)}/logs${q}`);
}

export async function listSnapshots(id: string): Promise<SnapshotApiInfo[]> {
  return api.get<SnapshotApiInfo[]>(`/workspaces/${encodeURIComponent(id)}/snapshots`);
}

export async function createSnapshot(
  id: string,
  name: string,
): Promise<void> {
  await api.post(`/workspaces/${encodeURIComponent(id)}/snapshots`, { name });
}

export async function forkWorkspace(
  id: string,
  snapshot?: string,
  name?: string,
): Promise<Workspace> {
  const info = await api.post<WorkspaceApiInfo>(
    `/workspaces/${encodeURIComponent(id)}/fork`,
    { snapshot, name },
  );
  return apiInfoToWorkspace(info);
}

export async function updateNetworkPolicy(
  id: string,
  allow_internet?: boolean,
): Promise<void> {
  await api.post(`/workspaces/${encodeURIComponent(id)}/network-policy`, {
    allow_internet,
  });
}

// ---------------------------------------------------------------------------
// Exec streaming (SSE)
// ---------------------------------------------------------------------------

export interface ExecStreamEvent {
  type: 'started' | 'stdout' | 'stderr' | 'exit' | 'error';
  data: string;
  job_id?: string;
  exit_code?: number;
}

/**
 * Stream exec output from a workspace using Server-Sent Events.
 * Returns an abort controller so the caller can cancel the stream.
 */
export function execStream(
  id: string,
  command: string,
  onEvent: (event: ExecStreamEvent) => void,
  opts?: { timeout_secs?: number; workdir?: string },
): { abort: () => void } {
  const controller = new AbortController();

  const token = (api as unknown as { token: string | null }).token;
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (token) headers['Authorization'] = `Bearer ${token}`;

  fetch(`/api/workspaces/${encodeURIComponent(id)}/exec/stream`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ command, ...opts }),
    signal: controller.signal,
  })
    .then(async (response) => {
      if (!response.ok) {
        const text = await response.text();
        onEvent({ type: 'error', data: text });
        return;
      }

      const reader = response.body?.getReader();
      if (!reader) {
        onEvent({ type: 'error', data: 'No response body' });
        return;
      }

      const decoder = new TextDecoder();
      let buffer = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });

        // Parse SSE format: "event: <type>\ndata: <json>\n\n"
        const messages = buffer.split('\n\n');
        buffer = messages.pop() ?? '';

        for (const msg of messages) {
          if (!msg.trim()) continue;

          let eventType = 'message';
          let eventData = '';

          for (const line of msg.split('\n')) {
            if (line.startsWith('event:')) {
              eventType = line.slice(6).trim();
            } else if (line.startsWith('data:')) {
              eventData = line.slice(5).trim();
            }
          }

          if (!eventData) continue;

          try {
            const parsed = JSON.parse(eventData);
            const streamEvent: ExecStreamEvent = { type: eventType as ExecStreamEvent['type'], data: '' };

            if (eventType === 'started') {
              streamEvent.job_id = parsed.job_id?.toString();
              streamEvent.data = '';
            } else if (eventType === 'stdout' || eventType === 'stderr') {
              streamEvent.data = parsed.data ?? '';
            } else if (eventType === 'exit') {
              streamEvent.exit_code = parsed.exit_code;
              streamEvent.data = '';
            } else if (eventType === 'error') {
              streamEvent.data = parsed.message ?? '';
            }

            onEvent(streamEvent);
          } catch {
            // Ignore malformed events
          }
        }
      }
    })
    .catch((err: unknown) => {
      if (controller.signal.aborted) return;
      const msg = err instanceof Error ? err.message : String(err);
      onEvent({ type: 'error', data: msg });
    });

  return { abort: () => controller.abort() };
}

export { ApiRequestError };
