import { api } from './client';
import type { Team, TeamMessage } from '../stores/teams';
import type { BoardTask } from '../types/workspace';

// ---------------------------------------------------------------------------
// Backend response shapes
// ---------------------------------------------------------------------------

interface TeamListItem {
  name: string;
  state: string;
  member_count: number;
  created_at: string;
  parent_team: string | null;
  max_vms: number;
}

// team_status returns a rich report; we'll use the parts we need.
interface TeamStatusReport {
  name: string;
  state: string;
  members: TeamStatusMember[];
  max_vms: number;
  parent_team: string | null;
  created_at: string;
}

interface TeamStatusMember {
  name: string;
  role: string;
  skills: string[];
  workspace_id: string;
  ip: string;
  status: string;
}

// Backend BoardTask from vault-backed task board
interface TaskApiItem {
  id: string;
  title: string;
  description: string;
  status: string;
  owner: string | null;
  priority: string;
  depends_on: string[];
  created_at: string;
  completed_at: string | null;
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

function mapTeamState(s: string): Team['state'] {
  // Backend uses Debug-format: "Creating", "Ready", etc.
  switch (s) {
    case 'Creating':
      return 'Creating';
    case 'Ready':
      return 'Ready';
    case 'Working':
      return 'Working';
    case 'Completing':
      return 'Completing';
    case 'Destroyed':
      return 'Destroyed';
    default:
      return 'Ready';
  }
}

function mapMemberStatus(s: string): 'idle' | 'busy' | 'offline' {
  switch (s.toLowerCase()) {
    case 'idle':
      return 'idle';
    case 'busy':
      return 'busy';
    case 'offline':
      return 'offline';
    default:
      return 'idle';
  }
}

function teamListItemToTeam(t: TeamListItem): Team {
  return {
    name: t.name,
    state: mapTeamState(t.state),
    members: [], // Members only available from team status endpoint
    max_vms: t.max_vms,
    parent_team: t.parent_team,
    created_at: t.created_at,
  };
}

function teamStatusToTeam(s: TeamStatusReport): Team {
  return {
    name: s.name,
    state: mapTeamState(s.state),
    members: s.members.map((m) => ({
      name: m.name,
      role: m.role,
      skills: m.skills,
      status: mapMemberStatus(m.status),
      workspace_id: m.workspace_id,
      ip: m.ip,
    })),
    max_vms: s.max_vms,
    parent_team: s.parent_team,
    created_at: s.created_at,
  };
}

function mapTaskStatus(s: string): BoardTask['status'] {
  switch (s) {
    case 'pending':
      return 'pending';
    case 'claimed':
      return 'claimed';
    case 'in_progress':
      return 'in_progress';
    case 'completed':
      return 'completed';
    case 'failed':
      return 'failed';
    default:
      return 'pending';
  }
}

function mapTaskPriority(s: string): BoardTask['priority'] {
  switch (s) {
    case 'low':
      return 'low';
    case 'high':
      return 'high';
    case 'critical':
      return 'critical';
    default:
      return 'medium';
  }
}

function taskApiToBoard(t: TaskApiItem, teamName: string): BoardTask {
  return {
    id: t.id,
    title: t.title,
    description: t.description,
    status: mapTaskStatus(t.status),
    owner: t.owner,
    priority: mapTaskPriority(t.priority),
    depends_on: t.depends_on,
    team_name: teamName,
    created_at: t.created_at,
    completed_at: t.completed_at,
  };
}

// ---------------------------------------------------------------------------
// API functions
// ---------------------------------------------------------------------------

export async function fetchTeams(): Promise<Team[]> {
  const items = await api.get<TeamListItem[]>('/teams');
  return items.map(teamListItemToTeam);
}

export async function fetchTeamStatus(name: string): Promise<Team> {
  const report = await api.get<TeamStatusReport>(
    `/teams/${encodeURIComponent(name)}`,
  );
  return teamStatusToTeam(report);
}

export async function fetchTeamTasks(teamName: string): Promise<BoardTask[]> {
  const items = await api.get<TaskApiItem[]>(
    `/teams/${encodeURIComponent(teamName)}/tasks`,
  );
  return items.map((t) => taskApiToBoard(t, teamName));
}

export async function fetchTeamMessages(
  teamName: string,
): Promise<TeamMessage[]> {
  return api.get<TeamMessage[]>(
    `/teams/${encodeURIComponent(teamName)}/messages`,
  );
}

export async function claimTask(
  teamName: string,
  taskId: string,
  agent: string | null,
): Promise<void> {
  await api.post(
    `/teams/${encodeURIComponent(teamName)}/tasks/${encodeURIComponent(taskId)}/claim`,
    { agent },
  );
}

export async function createTask(
  teamName: string,
  data: { title: string; description?: string; priority?: string; depends_on?: string[] },
): Promise<BoardTask> {
  const item = await api.post<TaskApiItem>(
    `/teams/${encodeURIComponent(teamName)}/tasks`,
    data,
  );
  return taskApiToBoard(item, teamName);
}

export async function startTask(
  teamName: string,
  taskId: string,
): Promise<void> {
  await api.post(
    `/teams/${encodeURIComponent(teamName)}/tasks/${encodeURIComponent(taskId)}/start`,
  );
}

export async function completeTask(
  teamName: string,
  taskId: string,
  result?: string,
): Promise<void> {
  await api.post(
    `/teams/${encodeURIComponent(teamName)}/tasks/${encodeURIComponent(taskId)}/complete`,
    result !== undefined ? { result } : {},
  );
}

export async function failTask(
  teamName: string,
  taskId: string,
  reason?: string,
): Promise<void> {
  await api.post(
    `/teams/${encodeURIComponent(teamName)}/tasks/${encodeURIComponent(taskId)}/fail`,
    reason !== undefined ? { reason } : {},
  );
}

export async function releaseTask(
  teamName: string,
  taskId: string,
): Promise<void> {
  await api.post(
    `/teams/${encodeURIComponent(teamName)}/tasks/${encodeURIComponent(taskId)}/release`,
  );
}

export async function sendTeamMessage(
  teamName: string,
  from: string,
  content: string,
  opts?: { to?: string; broadcast?: boolean },
): Promise<void> {
  await api.post(`/teams/${encodeURIComponent(teamName)}/messages`, {
    from,
    content,
    to: opts?.to,
    broadcast: opts?.broadcast ?? false,
  });
}
