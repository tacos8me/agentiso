import { create } from 'zustand';
import {
  fetchTeams,
  fetchTeamStatus,
  fetchTeamTasks,
  fetchTeamMessages,
  claimTask as apiClaimTask,
  createTask as apiCreateTask,
  startTask as apiStartTask,
  completeTask as apiCompleteTask,
  failTask as apiFailTask,
  releaseTask as apiReleaseTask,
} from '../api/teams';
import type { BoardTask } from '../types/workspace';
import { useNotificationStore } from './notifications';

const USE_MOCKS = import.meta.env.VITE_USE_MOCKS === 'true';

// Local team types for the store
export interface TeamMember {
  name: string;
  role: string;
  skills: string[];
  status: 'idle' | 'busy' | 'offline';
  workspace_id: string;
  ip: string;
}

export interface Team {
  name: string;
  state: 'Creating' | 'Ready' | 'Working' | 'Completing' | 'Destroyed';
  members: TeamMember[];
  max_vms: number;
  parent_team: string | null;
  created_at: string;
}

export interface TeamMessage {
  id: string;
  from: string;
  to: string | null;
  content: string;
  type: 'message' | 'broadcast';
  timestamp: string;
}

interface TeamStore {
  teams: Team[];
  selectedTeam: string | null;
  messages: TeamMessage[];
  tasks: BoardTask[];
  loading: boolean;
  error: string | null;

  // Data fetching
  fetchAll: () => Promise<void>;
  fetchTeamDetail: (name: string) => Promise<void>;
  fetchTasks: (teamName: string) => Promise<void>;
  fetchMessages: (teamName: string) => Promise<void>;
  startPolling: () => () => void;

  // Task actions
  createTask: (teamName: string, data: { title: string; description?: string; priority?: string; depends_on?: string[] }) => Promise<BoardTask | null>;
  claimTask: (teamName: string, taskId: string, agent: string | null) => Promise<void>;
  startTask: (teamName: string, taskId: string) => Promise<void>;
  completeTask: (teamName: string, taskId: string, result?: string) => Promise<void>;
  failTask: (teamName: string, taskId: string, reason?: string) => Promise<void>;
  releaseTask: (teamName: string, taskId: string) => Promise<void>;

  // Local state
  setTeams: (teams: Team[]) => void;
  addTeam: (team: Team) => void;
  removeTeam: (name: string) => void;
  selectTeam: (name: string | null) => void;
  addMessage: (message: TeamMessage) => void;
  setMessages: (messages: TeamMessage[]) => void;
  setTasks: (tasks: BoardTask[]) => void;
}

// Mock data for offline dev
const mockTeams: Team[] = [
  {
    name: 'analysis',
    state: 'Working',
    members: [
      { name: 'researcher', role: 'Lead analyst', skills: ['data-analysis', 'python'], status: 'busy', workspace_id: 'b2c3d4e5-f6a7-8901-bcde-f12345678901', ip: '10.99.0.3' },
      { name: 'coder', role: 'Implementation', skills: ['rust', 'typescript'], status: 'busy', workspace_id: 'c3d4e5f6-a7b8-9012-cdef-123456789012', ip: '10.99.0.5' },
      { name: 'reviewer', role: 'Code review', skills: ['rust', 'testing'], status: 'busy', workspace_id: 'e5f6a7b8-c9d0-1234-efab-345678901234', ip: '10.99.0.7' },
    ],
    max_vms: 5,
    parent_team: null,
    created_at: '2026-02-21T09:00:00Z',
  },
  {
    name: 'build',
    state: 'Creating',
    members: [
      { name: 'ci-runner', role: 'CI/CD', skills: ['docker', 'github-actions'], status: 'busy', workspace_id: 'a7b8c9d0-e1f2-3456-abcd-567890123456', ip: '10.99.0.9' },
      { name: 'deploy-staging', role: 'Deployment', skills: ['ansible', 'terraform'], status: 'idle', workspace_id: 'd0e1f2a3-b4c5-6789-defa-890123456789', ip: '10.99.0.11' },
    ],
    max_vms: 4,
    parent_team: null,
    created_at: '2026-02-21T07:00:00Z',
  },
];

const mockMessages: TeamMessage[] = [
  { id: 'msg-001', from: 'researcher', to: null, content: 'Schema analysis complete. Found 12 columns, 3 need type coercion.', type: 'broadcast', timestamp: '2026-02-21T10:15:00Z' },
  { id: 'msg-002', from: 'coder', to: 'researcher', content: 'Can you clarify the date format in column 7? Seeing mixed ISO and US formats.', type: 'message', timestamp: '2026-02-21T10:18:00Z' },
  { id: 'msg-003', from: 'researcher', to: 'coder', content: 'Column 7 uses ISO 8601. The US-format rows are data errors -- skip them.', type: 'message', timestamp: '2026-02-21T10:19:00Z' },
];

function addToast(type: 'team' | 'system', title: string, message: string) {
  useNotificationStore.getState().addNotification({ type, title, message });
}

// Debounced error toast to suppress repeated identical errors on poll cycles
let lastErrorKey = '';
let lastErrorTime = 0;

function addToastDebounced(type: 'team' | 'system', title: string, message: string) {
  const key = `${title}:${message}`;
  const now = Date.now();
  if (key === lastErrorKey && now - lastErrorTime < 30000) return;
  lastErrorKey = key;
  lastErrorTime = now;
  useNotificationStore.getState().addNotification({ type, title, message });
}

export const useTeamStore = create<TeamStore>((set, get) => ({
  teams: USE_MOCKS ? mockTeams : [],
  selectedTeam: null,
  messages: USE_MOCKS ? mockMessages : [],
  tasks: [],
  loading: !USE_MOCKS,
  error: null,

  // --- Data fetching ---

  fetchAll: async () => {
    if (USE_MOCKS) return;
    try {
      const teams = await fetchTeams();
      set({ teams, loading: false, error: null });
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to fetch teams';
      set({ error: msg, loading: false });
    }
  },

  fetchTeamDetail: async (name) => {
    if (USE_MOCKS) return;
    try {
      const team = await fetchTeamStatus(name);
      set((s) => ({
        teams: s.teams.map((t) => (t.name === name ? team : t)),
      }));
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to fetch team status';
      addToastDebounced('system', 'Team fetch failed', msg);
    }
  },

  fetchTasks: async (teamName) => {
    if (USE_MOCKS) return;
    try {
      const tasks = await fetchTeamTasks(teamName);
      set({ tasks });
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to fetch tasks';
      addToastDebounced('system', 'Tasks fetch failed', msg);
    }
  },

  fetchMessages: async (teamName) => {
    if (USE_MOCKS) return;
    try {
      const messages = await fetchTeamMessages(teamName);
      set({ messages });
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to fetch messages';
      addToastDebounced('system', 'Messages fetch failed', msg);
    }
  },

  startPolling: () => {
    if (USE_MOCKS) return () => {};
    get().fetchAll();
    const interval = setInterval(() => {
      get().fetchAll();
      const selected = get().selectedTeam;
      if (selected) {
        get().fetchTeamDetail(selected);
        get().fetchTasks(selected);
      }
    }, 5000);
    return () => clearInterval(interval);
  },

  // --- Task actions ---

  createTask: async (teamName, data) => {
    if (USE_MOCKS) {
      const task: BoardTask = {
        id: `task-${Date.now()}`,
        title: data.title,
        description: data.description ?? '',
        status: 'pending',
        owner: null,
        priority: (data.priority as BoardTask['priority']) ?? 'medium',
        depends_on: data.depends_on ?? [],
        team_name: teamName,
        created_at: new Date().toISOString(),
        completed_at: null,
      };
      set((s) => ({ tasks: [...s.tasks, task] }));
      return task;
    }
    try {
      const task = await apiCreateTask(teamName, data);
      get().fetchTasks(teamName);
      return task;
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to create task';
      addToast('system', 'Task creation failed', msg);
      return null;
    }
  },

  claimTask: async (teamName, taskId, agent) => {
    if (USE_MOCKS) {
      set((s) => ({
        tasks: s.tasks.map((t) =>
          t.id === taskId ? { ...t, owner: agent, status: agent ? 'claimed' : 'pending' } : t
        ),
      }));
      return;
    }
    try {
      await apiClaimTask(teamName, taskId, agent);
      get().fetchTasks(teamName);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to assign task';
      addToast('system', 'Task assignment failed', msg);
    }
  },

  startTask: async (teamName, taskId) => {
    if (USE_MOCKS) {
      set((s) => ({
        tasks: s.tasks.map((t) =>
          t.id === taskId ? { ...t, status: 'in_progress' as const } : t
        ),
      }));
      return;
    }
    try {
      await apiStartTask(teamName, taskId);
      get().fetchTasks(teamName);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to start task';
      addToast('system', 'Task start failed', msg);
    }
  },

  completeTask: async (teamName, taskId, result?) => {
    if (USE_MOCKS) {
      set((s) => ({
        tasks: s.tasks.map((t) =>
          t.id === taskId ? { ...t, status: 'completed' as const, completed_at: new Date().toISOString() } : t
        ),
      }));
      return;
    }
    try {
      await apiCompleteTask(teamName, taskId, result);
      get().fetchTasks(teamName);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to complete task';
      addToast('system', 'Task completion failed', msg);
    }
  },

  failTask: async (teamName, taskId, reason?) => {
    if (USE_MOCKS) {
      set((s) => ({
        tasks: s.tasks.map((t) =>
          t.id === taskId ? { ...t, status: 'failed' as const, completed_at: new Date().toISOString() } : t
        ),
      }));
      return;
    }
    try {
      await apiFailTask(teamName, taskId, reason);
      get().fetchTasks(teamName);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to fail task';
      addToast('system', 'Task failure update failed', msg);
    }
  },

  releaseTask: async (teamName, taskId) => {
    if (USE_MOCKS) {
      set((s) => ({
        tasks: s.tasks.map((t) =>
          t.id === taskId ? { ...t, status: 'pending' as const, owner: null } : t
        ),
      }));
      return;
    }
    try {
      await apiReleaseTask(teamName, taskId);
      get().fetchTasks(teamName);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to release task';
      addToast('system', 'Task release failed', msg);
    }
  },

  // --- Local state ---

  setTeams: (teams) => set({ teams }),
  addTeam: (team) => set((s) => ({ teams: [...s.teams, team] })),
  removeTeam: (name) => set((s) => ({
    teams: s.teams.filter((t) => t.name !== name),
    selectedTeam: s.selectedTeam === name ? null : s.selectedTeam,
  })),
  selectTeam: (name) => set({ selectedTeam: name }),
  addMessage: (message) => set((s) => ({ messages: [...s.messages, message] })),
  setMessages: (messages) => set({ messages }),
  setTasks: (tasks) => set({ tasks }),
}));
