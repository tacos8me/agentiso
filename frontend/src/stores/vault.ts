import { create } from 'zustand';
import type { VaultNote, VaultTreeNode, VaultFolder, VaultGraphData, VaultSearchResult } from '../types/vault';
import {
  fetchNotes,
  readNote,
  writeNote,
  deleteNote,
  searchVault,
  getVaultGraph,
  type NoteEntry,
  type NoteContent,
} from '../api/vault';
import { useNotificationStore } from './notifications';

// ---------------------------------------------------------------------------
// Helper: build tree from flat NoteEntry list
// ---------------------------------------------------------------------------

function buildTreeFromEntries(entries: NoteEntry[]): VaultTreeNode[] {
  const folderMap = new Map<string, VaultFolder>();
  const rootChildren: VaultTreeNode[] = [];

  // First pass: create all folders
  for (const entry of entries) {
    if (entry.is_dir) {
      const name = entry.path.split('/').pop() || entry.path;
      folderMap.set(entry.path, { name, path: entry.path, children: [] });
    }
  }

  // Ensure parent folders exist for files in nested paths
  for (const entry of entries) {
    if (!entry.is_dir) {
      const parts = entry.path.split('/');
      for (let i = 1; i < parts.length; i++) {
        const folderPath = parts.slice(0, i).join('/');
        if (!folderMap.has(folderPath)) {
          folderMap.set(folderPath, {
            name: parts[i - 1],
            path: folderPath,
            children: [],
          });
        }
      }
    }
  }

  // Second pass: assign files to their parent folders
  for (const entry of entries) {
    if (entry.is_dir) continue;
    const parts = entry.path.split('/');
    const name = parts.pop() || entry.path;
    const parentPath = parts.join('/');

    const noteNode: VaultTreeNode = {
      type: 'note',
      note: {
        path: entry.path,
        name: name.replace(/\.md$/, ''),
        content: '',
        frontmatter: {},
        backlinks: [],
        outlinks: [],
        tags: [],
        modified_at: '',
      },
    };

    if (parentPath && folderMap.has(parentPath)) {
      folderMap.get(parentPath)!.children.push(noteNode);
    } else {
      rootChildren.push(noteNode);
    }
  }

  // Assign subfolders to their parent folders
  for (const [path, folder] of folderMap) {
    const parts = path.split('/');
    if (parts.length > 1) {
      const parentPath = parts.slice(0, -1).join('/');
      const parent = folderMap.get(parentPath);
      if (parent) {
        // Only add if not already added
        if (!parent.children.some(c => c.type === 'folder' && c.folder.path === path)) {
          parent.children.push({ type: 'folder', folder });
        }
        continue;
      }
    }
    // Top-level folder
    rootChildren.push({ type: 'folder', folder });
  }

  // Sort children: folders first, then notes, alphabetically
  function sortChildren(nodes: VaultTreeNode[]) {
    nodes.sort((a, b) => {
      if (a.type !== b.type) return a.type === 'folder' ? -1 : 1;
      const nameA = a.type === 'folder' ? a.folder.name : a.note.name;
      const nameB = b.type === 'folder' ? b.folder.name : b.note.name;
      return nameA.localeCompare(nameB);
    });
    for (const node of nodes) {
      if (node.type === 'folder') sortChildren(node.folder.children);
    }
  }

  sortChildren(rootChildren);
  return rootChildren;
}

// ---------------------------------------------------------------------------
// Helper: convert backend NoteContent to frontend VaultNote
// ---------------------------------------------------------------------------

function noteContentToVaultNote(nc: NoteContent): VaultNote {
  const name = nc.path.split('/').pop()?.replace(/\.md$/, '') || nc.path;
  const fm = (nc.frontmatter ?? {}) as Record<string, unknown>;
  const tags = Array.isArray(fm.tags) ? (fm.tags as string[]) : [];

  // Extract wikilinks from content for outlinks
  const outlinks: string[] = [];
  const wikiRe = /\[\[([^\]]+)\]\]/g;
  let match: RegExpExecArray | null;
  while ((match = wikiRe.exec(nc.content)) !== null) {
    outlinks.push(match[1]);
  }

  return {
    path: nc.path,
    name,
    content: nc.content,
    frontmatter: fm as VaultNote['frontmatter'],
    backlinks: [],
    outlinks,
    tags,
    modified_at: (fm.updated as string) || (fm.created as string) || '',
  };
}

// ---------------------------------------------------------------------------
// Debounce helper
// ---------------------------------------------------------------------------

let saveTimer: ReturnType<typeof setTimeout> | null = null;

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

interface VaultStore {
  notes: VaultNote[];
  tree: VaultTreeNode[];
  currentPath: string | null;
  currentNote: VaultNote | null;
  searchQuery: string;
  searchResults: VaultSearchResult[];
  graphData: VaultGraphData | null;
  loading: boolean;
  saving: boolean;
  treeLoading: boolean;

  // Actions
  loadTree: () => Promise<void>;
  openNote: (path: string) => Promise<void>;
  closeNote: () => void;
  saveNote: (path: string, content: string) => void;
  saveNoteImmediate: (path: string, content: string) => Promise<void>;
  createNote: (path: string, content?: string) => Promise<void>;
  deleteNoteAction: (path: string) => Promise<void>;
  renameNote: (from: string, to: string) => Promise<void>;
  search: (query: string) => Promise<void>;
  setSearchQuery: (query: string) => void;
  loadGraph: () => Promise<void>;
  getCurrentNote: () => VaultNote | null;

  // Legacy compat
  setNotes: (notes: VaultNote[]) => void;
  setTree: (tree: VaultTreeNode[]) => void;
  setSearchResults: (results: VaultSearchResult[]) => void;
}

export const useVaultStore = create<VaultStore>((set, get) => ({
  notes: [],
  tree: [],
  currentPath: null,
  currentNote: null,
  searchQuery: '',
  searchResults: [],
  graphData: null,
  loading: false,
  saving: false,
  treeLoading: false,

  setNotes: (notes) => set({ notes }),
  setTree: (tree) => set({ tree }),
  setSearchResults: (results) => set({ searchResults: results }),

  loadTree: async () => {
    set({ treeLoading: true });
    try {
      const entries = await fetchNotes(undefined, true);
      const tree = buildTreeFromEntries(entries);
      set({ tree, treeLoading: false });
    } catch (e) {
      set({ treeLoading: false });
      useNotificationStore.getState().addNotification({
        type: 'vault',
        title: 'Failed to load vault',
        message: e instanceof Error ? e.message : 'Unknown error',
      });
    }
  },

  openNote: async (path: string) => {
    set({ currentPath: path, loading: true });
    try {
      const nc = await readNote(path);
      const note = noteContentToVaultNote(nc);
      set((s) => ({
        currentNote: note,
        loading: false,
        notes: s.notes.some((n) => n.path === path)
          ? s.notes.map((n) => (n.path === path ? note : n))
          : [...s.notes, note],
      }));
    } catch (e) {
      set({ currentNote: null, loading: false });
      useNotificationStore.getState().addNotification({
        type: 'vault',
        title: 'Note not found',
        message: `Could not load ${path}`,
      });
    }
  },

  closeNote: () => set({ currentPath: null, currentNote: null }),

  saveNote: (path: string, content: string) => {
    // Debounced save (500ms)
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      get().saveNoteImmediate(path, content);
    }, 500);
  },

  saveNoteImmediate: async (path: string, content: string) => {
    set({ saving: true });
    try {
      await writeNote(path, content);
      set({ saving: false });
    } catch (e) {
      set({ saving: false });
      useNotificationStore.getState().addNotification({
        type: 'vault',
        title: 'Save failed',
        message: `Could not save ${path}: ${e instanceof Error ? e.message : 'Unknown error'}`,
      });
    }
  },

  createNote: async (path: string, content?: string) => {
    try {
      await writeNote(path, content || `# ${path.split('/').pop()?.replace(/\.md$/, '') || 'New Note'}\n`);
      await get().loadTree();
      await get().openNote(path);
    } catch (e) {
      useNotificationStore.getState().addNotification({
        type: 'vault',
        title: 'Create failed',
        message: e instanceof Error ? e.message : 'Unknown error',
      });
    }
  },

  deleteNoteAction: async (path: string) => {
    try {
      await deleteNote(path);
      set((s) => ({
        notes: s.notes.filter((n) => n.path !== path),
        currentPath: s.currentPath === path ? null : s.currentPath,
        currentNote: s.currentPath === path ? null : s.currentNote,
      }));
      await get().loadTree();
    } catch (e) {
      useNotificationStore.getState().addNotification({
        type: 'vault',
        title: 'Delete failed',
        message: e instanceof Error ? e.message : 'Unknown error',
      });
    }
  },

  renameNote: async (from: string, to: string) => {
    try {
      // Read old content, write to new path, delete old
      const nc = await readNote(from);
      await writeNote(to, nc.content);
      await deleteNote(from);
      set((s) => ({
        currentPath: s.currentPath === from ? to : s.currentPath,
      }));
      await get().loadTree();
      if (get().currentPath === to) {
        await get().openNote(to);
      }
    } catch (e) {
      useNotificationStore.getState().addNotification({
        type: 'vault',
        title: 'Rename failed',
        message: e instanceof Error ? e.message : 'Unknown error',
      });
    }
  },

  search: async (query: string) => {
    set({ searchQuery: query });
    if (!query.trim()) {
      set({ searchResults: [] });
      return;
    }
    try {
      const hits = await searchVault(query);
      const results: VaultSearchResult[] = hits.map((h) => ({
        path: h.path,
        name: h.path.split('/').pop()?.replace(/\.md$/, '') || h.path,
        line: h.line_number,
        context: h.line,
        matchStart: 0,
        matchEnd: 0,
      }));
      set({ searchResults: results });
    } catch {
      set({ searchResults: [] });
    }
  },

  setSearchQuery: (query: string) => set({ searchQuery: query }),

  loadGraph: async () => {
    try {
      const resp = await getVaultGraph();
      const graphData: VaultGraphData = {
        nodes: resp.nodes.map((n) => ({
          id: n.id,
          name: n.label.split('/').pop()?.replace(/\.md$/, '') || n.label,
          folder: n.id.includes('/') ? n.id.split('/')[0] : '',
          tags: [],
          val: 1,
        })),
        links: resp.edges.map((e) => ({
          source: e.source,
          target: e.target,
        })),
      };
      // Compute val (connectivity) per node
      const linkCounts = new Map<string, number>();
      for (const link of graphData.links) {
        linkCounts.set(link.source, (linkCounts.get(link.source) || 0) + 1);
        linkCounts.set(link.target, (linkCounts.get(link.target) || 0) + 1);
      }
      for (const node of graphData.nodes) {
        node.val = 1 + (linkCounts.get(node.id) || 0);
      }
      set({ graphData });
    } catch (e) {
      useNotificationStore.getState().addNotification({
        type: 'vault',
        title: 'Graph load failed',
        message: e instanceof Error ? e.message : 'Unknown error',
      });
    }
  },

  getCurrentNote: () => get().currentNote,
}));
