import { create } from 'zustand';
import type { Workspace, WorkspaceState } from '../types/workspace';
import {
  fetchWorkspaces,
  startWorkspace,
  stopWorkspace,
  destroyWorkspace,
  createWorkspace as apiCreateWorkspace,
  createSnapshot as apiCreateSnapshot,
  forkWorkspace as apiForkWorkspace,
  updateNetworkPolicy,
} from '../api/workspaces';
import { mockWorkspaces } from '../mocks';
import { useNotificationStore } from './notifications';

const USE_MOCKS = import.meta.env.VITE_USE_MOCKS === 'true';

interface WorkspaceStore {
  workspaces: Workspace[];
  selectedId: string | null;
  filter: WorkspaceState | 'all';
  loading: boolean;
  error: string | null;
  actionLoading: Record<string, boolean>;

  // Data fetching
  fetchAll: () => Promise<void>;
  startPolling: () => () => void;

  // CRUD via API
  apiStartWorkspace: (id: string) => Promise<void>;
  apiStopWorkspace: (id: string) => Promise<void>;
  apiDestroyWorkspace: (id: string) => Promise<void>;
  apiCreateWorkspace: (params: {
    name?: string;
    base_image?: string;
    vcpus?: number;
    memory_mb?: number;
    disk_gb?: number;
    allow_internet?: boolean;
  }) => Promise<Workspace | null>;
  apiCreateSnapshot: (id: string, name: string) => Promise<void>;
  apiForkWorkspace: (id: string, snapshot?: string, name?: string) => Promise<Workspace | null>;
  apiUpdateNetworkPolicy: (id: string, allow_internet: boolean) => Promise<void>;

  // Local state
  setWorkspaces: (workspaces: Workspace[]) => void;
  updateWorkspace: (id: string, partial: Partial<Workspace>) => void;
  addWorkspace: (workspace: Workspace) => void;
  removeWorkspace: (id: string) => void;
  selectWorkspace: (id: string | null) => void;
  setFilter: (filter: WorkspaceState | 'all') => void;
  getFiltered: () => Workspace[];
  getByState: (state: WorkspaceState) => Workspace[];
}

function addToast(type: 'workspace' | 'system', title: string, message: string) {
  useNotificationStore.getState().addNotification({ type, title, message });
}

export const useWorkspaceStore = create<WorkspaceStore>((set, get) => ({
  workspaces: USE_MOCKS ? mockWorkspaces : [],
  selectedId: null,
  filter: 'all',
  loading: !USE_MOCKS,
  error: null,
  actionLoading: {},

  // --- Data fetching ---

  fetchAll: async () => {
    if (USE_MOCKS) return;
    try {
      const workspaces = await fetchWorkspaces();
      set({ workspaces, loading: false, error: null });
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to fetch workspaces';
      set({ error: msg, loading: false });
    }
  },

  startPolling: () => {
    if (USE_MOCKS) return () => {};
    // Fetch immediately on first call
    get().fetchAll();
    const interval = setInterval(() => {
      get().fetchAll();
    }, 5000);
    return () => clearInterval(interval);
  },

  // --- API actions ---

  apiStartWorkspace: async (id) => {
    set((s) => ({ actionLoading: { ...s.actionLoading, [id]: true } }));
    try {
      await startWorkspace(id);
      // Optimistically update state
      get().updateWorkspace(id, { state: 'running' });
      addToast('workspace', 'Workspace started', `${id} is now running`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to start workspace';
      addToast('system', 'Start failed', msg);
    } finally {
      set((s) => {
        const al = { ...s.actionLoading };
        delete al[id];
        return { actionLoading: al };
      });
    }
  },

  apiStopWorkspace: async (id) => {
    set((s) => ({ actionLoading: { ...s.actionLoading, [id]: true } }));
    try {
      await stopWorkspace(id);
      get().updateWorkspace(id, { state: 'stopped', uptime_seconds: 0 });
      addToast('workspace', 'Workspace stopped', `${id} has been stopped`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to stop workspace';
      addToast('system', 'Stop failed', msg);
    } finally {
      set((s) => {
        const al = { ...s.actionLoading };
        delete al[id];
        return { actionLoading: al };
      });
    }
  },

  apiDestroyWorkspace: async (id) => {
    set((s) => ({ actionLoading: { ...s.actionLoading, [id]: true } }));
    try {
      await destroyWorkspace(id);
      get().removeWorkspace(id);
      addToast('workspace', 'Workspace destroyed', `${id} has been destroyed`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to destroy workspace';
      if (msg.includes('not found') || (e instanceof Error && 'status' in e && (e as { status: number }).status === 404)) {
        get().removeWorkspace(id);
        addToast('workspace', 'Workspace not found', `${id} was already removed`);
      } else {
        addToast('system', 'Destroy failed', msg);
      }
    } finally {
      set((s) => {
        const al = { ...s.actionLoading };
        delete al[id];
        return { actionLoading: al };
      });
    }
  },

  apiCreateWorkspace: async (params) => {
    set({ loading: true });
    try {
      const ws = await apiCreateWorkspace(params);
      get().addWorkspace(ws);
      addToast('workspace', 'Workspace created', `${ws.name} is being created`);
      set({ loading: false });
      return ws;
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to create workspace';
      addToast('system', 'Create failed', msg);
      set({ loading: false });
      return null;
    }
  },

  apiCreateSnapshot: async (id, name) => {
    set((s) => ({ actionLoading: { ...s.actionLoading, [id]: true } }));
    try {
      await apiCreateSnapshot(id, name);
      addToast('workspace', 'Snapshot created', `Snapshot "${name}" created`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to create snapshot';
      addToast('system', 'Snapshot failed', msg);
    } finally {
      set((s) => {
        const al = { ...s.actionLoading };
        delete al[id];
        return { actionLoading: al };
      });
    }
  },

  apiForkWorkspace: async (id, snapshot, name) => {
    set((s) => ({ actionLoading: { ...s.actionLoading, [id]: true } }));
    try {
      const forked = await apiForkWorkspace(id, snapshot, name);
      get().addWorkspace(forked);
      addToast('workspace', 'Fork created', `Forked workspace ${forked.name}`);
      return forked;
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to fork workspace';
      addToast('system', 'Fork failed', msg);
      return null;
    } finally {
      set((s) => {
        const al = { ...s.actionLoading };
        delete al[id];
        return { actionLoading: al };
      });
    }
  },

  apiUpdateNetworkPolicy: async (id, allow_internet) => {
    set((s) => ({ actionLoading: { ...s.actionLoading, [id]: true } }));
    try {
      await updateNetworkPolicy(id, allow_internet);
      get().updateWorkspace(id, {
        network: {
          ...get().workspaces.find((w) => w.id === id)!.network,
          internet_enabled: allow_internet,
        },
      });
      addToast('workspace', 'Network updated', `Internet ${allow_internet ? 'enabled' : 'disabled'}`);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to update network policy';
      addToast('system', 'Network policy failed', msg);
    } finally {
      set((s) => {
        const al = { ...s.actionLoading };
        delete al[id];
        return { actionLoading: al };
      });
    }
  },

  // --- Local state ---

  setWorkspaces: (workspaces) => set({ workspaces }),

  updateWorkspace: (id, partial) =>
    set((state) => ({
      workspaces: state.workspaces.map((w) =>
        w.id === id ? { ...w, ...partial } : w
      ),
    })),

  addWorkspace: (workspace) =>
    set((state) => ({ workspaces: [...state.workspaces, workspace] })),

  removeWorkspace: (id) =>
    set((state) => ({
      workspaces: state.workspaces.filter((w) => w.id !== id),
      selectedId: state.selectedId === id ? null : state.selectedId,
    })),

  selectWorkspace: (id) => set({ selectedId: id }),

  setFilter: (filter) => set({ filter }),

  getFiltered: () => {
    const { workspaces, filter } = get();
    if (filter === 'all') return workspaces;
    return workspaces.filter((w) => w.state === filter);
  },

  getByState: (state) => {
    return get().workspaces.filter((w) => w.state === state);
  },
}));
