import { create } from 'zustand';

export interface MetricsState {
  workspace_counts: {
    creating: number;
    running: number;
    idle: number;
    stopped: number;
  };
  team_count: number;
  exec_total: number;
  exec_errors: number;
  vm_boot_avg_ms: number;
  cpu_percent: number;
  memory_used_mb: number;
  memory_total_mb: number;
  zfs_used_gb: number;
  zfs_total_gb: number;
  warm_pool_size: number;
  last_updated: string | null;
}

interface MetricsStore extends MetricsState {
  update: (partial: Partial<MetricsState>) => void;
  totalWorkspaces: () => number;
  activeWorkspaces: () => number;
}

export const useMetricsStore = create<MetricsStore>((set, get) => ({
  workspace_counts: { creating: 0, running: 0, idle: 0, stopped: 0 },
  team_count: 0,
  exec_total: 0,
  exec_errors: 0,
  vm_boot_avg_ms: 0,
  cpu_percent: 0,
  memory_used_mb: 0,
  memory_total_mb: 0,
  zfs_used_gb: 0,
  zfs_total_gb: 0,
  warm_pool_size: 0,
  last_updated: null,

  update: (partial) => set((s) => ({ ...s, ...partial, last_updated: new Date().toISOString() })),

  totalWorkspaces: () => {
    const c = get().workspace_counts;
    return c.creating + c.running + c.idle + c.stopped;
  },

  activeWorkspaces: () => {
    const c = get().workspace_counts;
    return c.creating + c.running;
  },
}));
