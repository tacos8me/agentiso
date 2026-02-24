import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { PaneConfig, PaneTab, PaneType, WorkspaceState } from '../types';
import type { TaskStatus } from '../types/workspace';

// Custom board definitions
export interface CustomBoardColumn {
  id: string;
  label: string;
  color: string;
  values: string[]; // workspace states or task statuses to include
}

export interface CustomBoard {
  id: string;
  name: string;
  type: 'workspaces' | 'tasks';
  columns: CustomBoardColumn[];
}

export type ViewMode = 'board' | 'table';

interface UiStore {
  sidebarOpen: boolean;
  sidebarCollapsed: boolean; // icon-rail mode for medium screens
  sidebarWidth: number;
  sidebarSection: 'workspaces' | 'tasks' | 'vault' | 'teams';
  commandPaletteOpen: boolean;
  activePaneId: string;
  panes: PaneConfig[];
  mobileMenuOpen: boolean;
  createWorkspaceDialogOpen: boolean;
  createTaskDialogOpen: boolean;
  createTaskDialogTeam: string | null;

  // View mode (table vs kanban board)
  viewMode: ViewMode;
  setViewMode: (mode: ViewMode) => void;

  // Custom boards
  customBoards: CustomBoard[];
  activeCustomBoardId: string | null;
  addCustomBoard: (board: CustomBoard) => void;
  removeCustomBoard: (id: string) => void;
  setActiveCustomBoard: (id: string | null) => void;

  setCreateWorkspaceDialogOpen: (open: boolean) => void;
  openCreateTaskDialog: (teamName?: string) => void;
  setCreateTaskDialogOpen: (open: boolean) => void;
  toggleSidebar: () => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  setSidebarWidth: (width: number) => void;
  setSidebarSection: (section: 'workspaces' | 'tasks' | 'vault' | 'teams') => void;
  toggleCommandPalette: () => void;
  setCommandPaletteOpen: (open: boolean) => void;
  setActivePane: (id: string) => void;
  setMobileMenuOpen: (open: boolean) => void;
  addPane: (pane: PaneConfig) => void;
  removePane: (id: string) => void;
  addTabToPane: (paneId: string, tab: PaneTab) => void;
  removeTabFromPane: (paneId: string, tabId: string) => void;
  setActiveTab: (paneId: string, tabId: string) => void;
  openInPane: (type: PaneType, title: string, data?: Record<string, unknown>) => void;
}

const defaultPane: PaneConfig = {
  id: 'main',
  tabs: [
    { id: 'kanban-home', type: 'kanban', title: 'Workspaces' },
  ],
  activeTabId: 'kanban-home',
};

import { WORKSPACE_COLUMNS, TASK_COLUMNS } from '../constants/board';

// Default boards that ship with the app (derived from shared constants)
const DEFAULT_WORKSPACE_BOARD_COLUMNS: CustomBoardColumn[] = WORKSPACE_COLUMNS.map((c) => ({
  id: c.id, label: c.label, color: c.color, values: [c.id],
}));

const DEFAULT_TASK_BOARD_COLUMNS: CustomBoardColumn[] = TASK_COLUMNS.map((c) => ({
  id: c.id, label: c.label, color: c.color, values: [c.id],
}));

export const DEFAULT_BOARDS: CustomBoard[] = [
  { id: 'default-workspaces', name: 'Workspaces', type: 'workspaces', columns: DEFAULT_WORKSPACE_BOARD_COLUMNS },
  { id: 'default-tasks', name: 'Team Tasks', type: 'tasks', columns: DEFAULT_TASK_BOARD_COLUMNS },
];

// Persisted slice for custom boards + view mode
const usePersistedUiStore = create<{ customBoards: CustomBoard[]; viewMode: ViewMode }>()(
  persist(
    () => ({
      customBoards: [] as CustomBoard[],
      viewMode: 'table' as ViewMode,
    }),
    { name: 'agentiso-custom-boards' },
  ),
);

export const useUiStore = create<UiStore>((set, get) => ({
  sidebarOpen: true,
  sidebarCollapsed: false,
  sidebarWidth: 240,
  sidebarSection: 'workspaces',
  commandPaletteOpen: false,
  activePaneId: 'main',
  panes: [defaultPane],
  mobileMenuOpen: false,
  createWorkspaceDialogOpen: false,
  createTaskDialogOpen: false,
  createTaskDialogTeam: null,

  viewMode: usePersistedUiStore.getState().viewMode,
  setViewMode: (mode) => {
    set({ viewMode: mode });
    usePersistedUiStore.setState({ viewMode: mode });
  },

  customBoards: usePersistedUiStore.getState().customBoards,
  activeCustomBoardId: null,

  addCustomBoard: (board) => {
    set((s) => ({ customBoards: [...s.customBoards, board] }));
    usePersistedUiStore.setState((s) => ({ customBoards: [...s.customBoards, board] }));
  },
  removeCustomBoard: (id) => {
    set((s) => ({
      customBoards: s.customBoards.filter((b) => b.id !== id),
      activeCustomBoardId: s.activeCustomBoardId === id ? null : s.activeCustomBoardId,
    }));
    usePersistedUiStore.setState((s) => ({ customBoards: s.customBoards.filter((b) => b.id !== id) }));
  },
  setActiveCustomBoard: (id) => set({ activeCustomBoardId: id }),

  setCreateWorkspaceDialogOpen: (open) => set({ createWorkspaceDialogOpen: open }),
  openCreateTaskDialog: (teamName) => set({ createTaskDialogOpen: true, createTaskDialogTeam: teamName ?? null }),
  setCreateTaskDialogOpen: (open) => set({ createTaskDialogOpen: open, createTaskDialogTeam: open ? get().createTaskDialogTeam : null }),
  toggleSidebar: () => set((s) => ({ sidebarOpen: !s.sidebarOpen })),
  setSidebarCollapsed: (collapsed) => set({ sidebarCollapsed: collapsed }),
  setSidebarWidth: (width) => set({ sidebarWidth: width }),
  setSidebarSection: (section) => {
    set({ sidebarSection: section });
    // Update the main pane's first tab to match the sidebar section
    const { panes } = get();
    const mainPane = panes.find((p) => p.id === 'main');
    if (!mainPane) return;
    const sectionPaneType: Record<string, PaneType> = {
      workspaces: 'kanban',
      tasks: 'kanban',
      vault: 'vault-note',
      teams: 'team-detail',
    };
    const sectionTitle: Record<string, string> = {
      workspaces: 'Workspaces',
      tasks: 'Tasks',
      vault: 'Vault',
      teams: 'Teams',
    };
    const paneType = sectionPaneType[section] ?? 'kanban';
    const title = sectionTitle[section] ?? 'Workspaces';
    const homeTabId = 'kanban-home';
    set((s) => ({
      panes: s.panes.map((p) =>
        p.id === 'main'
          ? {
              ...p,
              tabs: p.tabs.map((t) =>
                t.id === homeTabId ? { ...t, type: paneType, title } : t
              ),
              activeTabId: homeTabId,
            }
          : p
      ),
    }));
  },
  toggleCommandPalette: () => set((s) => ({ commandPaletteOpen: !s.commandPaletteOpen })),
  setCommandPaletteOpen: (open) => set({ commandPaletteOpen: open }),
  setActivePane: (id) => set({ activePaneId: id }),
  setMobileMenuOpen: (open) => set({ mobileMenuOpen: open }),

  addPane: (pane) => set((s) => ({ panes: [...s.panes, pane] })),

  removePane: (id) =>
    set((s) => ({
      panes: s.panes.filter((p) => p.id !== id),
      activePaneId: s.activePaneId === id
        ? (s.panes[0]?.id ?? 'main')
        : s.activePaneId,
    })),

  addTabToPane: (paneId, tab) =>
    set((s) => ({
      panes: s.panes.map((p) =>
        p.id === paneId
          ? { ...p, tabs: [...p.tabs, tab], activeTabId: tab.id }
          : p
      ),
    })),

  removeTabFromPane: (paneId, tabId) =>
    set((s) => ({
      panes: s.panes.map((p) => {
        if (p.id !== paneId) return p;
        const tabs = p.tabs.filter((t) => t.id !== tabId);
        return {
          ...p,
          tabs,
          activeTabId: p.activeTabId === tabId
            ? (tabs[0]?.id ?? '')
            : p.activeTabId,
        };
      }),
    })),

  setActiveTab: (paneId, tabId) =>
    set((s) => ({
      panes: s.panes.map((p) =>
        p.id === paneId ? { ...p, activeTabId: tabId } : p
      ),
    })),

  openInPane: (type, title, data) => {
    const { panes, activePaneId } = get();
    const tabId = `${type}-${Date.now()}`;
    const tab: PaneTab = { id: tabId, type, title, data };
    const pane = panes.find((p) => p.id === activePaneId);
    if (pane) {
      const existing = pane.tabs.find(
        (t) => t.type === type && JSON.stringify(t.data) === JSON.stringify(data)
      );
      if (existing) {
        set((s) => ({
          panes: s.panes.map((p) =>
            p.id === activePaneId ? { ...p, activeTabId: existing.id } : p
          ),
        }));
      } else {
        // Limit tabs: if at max, remove oldest non-home tab
        const MAX_TABS = 8;
        let tabs = [...pane.tabs, tab];
        if (tabs.length > MAX_TABS) {
          const removableIdx = tabs.findIndex((t) => t.id !== 'kanban-home');
          if (removableIdx !== -1) {
            tabs = [...tabs.slice(0, removableIdx), ...tabs.slice(removableIdx + 1)];
          }
        }
        set((s) => ({
          panes: s.panes.map((p) =>
            p.id === activePaneId
              ? { ...p, tabs, activeTabId: tab.id }
              : p
          ),
        }));
      }
    }
  },
}));

// Re-export column types for convenience
export type { WorkspaceState, TaskStatus };
