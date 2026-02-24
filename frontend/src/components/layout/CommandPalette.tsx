import { useState, useCallback, useEffect, useRef } from 'react';
import { Search, Box, BookOpen, Users, Terminal, Plus, Columns2, LayoutDashboard, ListPlus } from 'lucide-react';
import { useUiStore } from '../../stores/ui';
import { useWorkspaceStore } from '../../stores/workspaces';
import { useVaultStore } from '../../stores/vault';
import { useTeamStore } from '../../stores/teams';

interface CommandItem {
  id: string;
  label: string;
  sublabel?: string;
  icon: typeof Box;
  category: string;
  shortcut?: string;
  action: () => void;
}

function fuzzyMatch(text: string, query: string): boolean {
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  let qi = 0;
  for (let i = 0; i < lower.length && qi < q.length; i++) {
    if (lower[i] === q[qi]) qi++;
  }
  return qi === q.length;
}

export function CommandPalette() {
  const open = useUiStore((s) => s.commandPaletteOpen);
  const setOpen = useUiStore((s) => s.setCommandPaletteOpen);
  const openInPane = useUiStore((s) => s.openInPane);
  const toggleSidebar = useUiStore((s) => s.toggleSidebar);
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const notes = useVaultStore((s) => s.notes);
  const teams = useTeamStore((s) => s.teams);

  const [query, setQuery] = useState('');
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const items: CommandItem[] = [
    {
      id: 'action-new-workspace',
      label: 'Create Workspace',
      category: 'Actions',
      icon: Plus,
      action: () => {
        useUiStore.getState().setCreateWorkspaceDialogOpen(true);
        setOpen(false);
      },
    },
    ...(teams.length > 0
      ? teams.map((t) => ({
          id: `action-new-task-${t.name}`,
          label: teams.length === 1 ? 'Create Task' : `Create Task: ${t.name}`,
          sublabel: teams.length === 1 ? `in team: ${t.name}` : `${t.members.length} members`,
          category: 'Actions',
          icon: ListPlus,
          action: () => {
            useUiStore.getState().openCreateTaskDialog(t.name);
            setOpen(false);
          },
        }))
      : []),
    {
      id: 'action-open-terminal',
      label: 'Open Terminal',
      category: 'Actions',
      icon: Terminal,
      shortcut: 'Ctrl+`',
      action: () => { openInPane('terminal', 'Terminal'); setOpen(false); },
    },
    {
      id: 'action-search-vault',
      label: 'Search Vault',
      category: 'Actions',
      icon: Search,
      action: () => {
        useUiStore.getState().setSidebarSection('vault');
        setOpen(false);
      },
    },
    {
      id: 'action-toggle-sidebar',
      label: 'Toggle Sidebar',
      category: 'Actions',
      icon: Columns2,
      action: () => { toggleSidebar(); setOpen(false); },
    },
    {
      id: 'action-open-kanban',
      label: 'Open Workspaces Board',
      category: 'Actions',
      icon: LayoutDashboard,
      action: () => { openInPane('kanban', 'Workspaces'); setOpen(false); },
    },
    ...workspaces.map((w) => ({
      id: `ws-${w.id}`,
      label: w.name,
      sublabel: `${w.state} - ${w.network.ip}`,
      category: 'Workspaces',
      icon: Box,
      action: () => { openInPane('workspace-detail', w.name, { workspaceId: w.id }); setOpen(false); },
    })),
    ...notes.map((n) => ({
      id: `note-${n.path}`,
      label: n.name,
      sublabel: n.path,
      category: 'Vault',
      icon: BookOpen,
      action: () => { openInPane('vault-note', n.name, { path: n.path }); setOpen(false); },
    })),
    ...teams.map((t) => ({
      id: `team-${t.name}`,
      label: t.name,
      sublabel: `${t.members.length} members - ${t.state}`,
      category: 'Teams',
      icon: Users,
      action: () => { openInPane('team-detail', t.name, { teamName: t.name }); setOpen(false); },
    })),
    ...workspaces
      .filter((w) => w.state === 'running')
      .map((w) => ({
        id: `term-${w.id}`,
        label: `Open Terminal: ${w.name}`,
        sublabel: w.network.ip,
        category: 'Actions',
        icon: Terminal,
        action: () => { openInPane('terminal', `Terminal: ${w.name}`, { workspaceId: w.id }); setOpen(false); },
      })),
  ];

  const filtered = query
    ? items.filter(
        (item) =>
          fuzzyMatch(item.label, query) ||
          (item.sublabel && fuzzyMatch(item.sublabel, query)) ||
          fuzzyMatch(item.category, query)
      )
    : items;

  // Flatten items to get the ID at selectedIndex
  const flatItems: CommandItem[] = [];
  const grouped = new Map<string, CommandItem[]>();
  for (const item of filtered) {
    const list = grouped.get(item.category) ?? [];
    list.push(item);
    grouped.set(item.category, list);
    flatItems.push(item);
  }

  const activeDescendantId = flatItems[selectedIndex]?.id ?? undefined;

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault();
          setSelectedIndex((i) => Math.min(i + 1, filtered.length - 1));
          break;
        case 'ArrowUp':
          e.preventDefault();
          setSelectedIndex((i) => Math.max(i - 1, 0));
          break;
        case 'Enter':
          e.preventDefault();
          if (filtered[selectedIndex]) filtered[selectedIndex].action();
          break;
        case 'Escape':
          setOpen(false);
          break;
      }
    },
    [filtered, selectedIndex, setOpen]
  );

  useEffect(() => {
    if (open) {
      setQuery('');
      setSelectedIndex(0);
      setTimeout(() => inputRef.current?.focus(), 0);
    }
  }, [open]);

  useEffect(() => { setSelectedIndex(0); }, [query]);

  // Scroll selected item into view
  useEffect(() => {
    if (!listRef.current || !activeDescendantId) return;
    const el = listRef.current.querySelector(`[data-item-id="${activeDescendantId}"]`);
    el?.scrollIntoView({ block: 'nearest' });
  }, [activeDescendantId]);

  useEffect(() => {
    function handleGlobalKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        setOpen(!open);
      }
    }
    window.addEventListener('keydown', handleGlobalKey);
    return () => window.removeEventListener('keydown', handleGlobalKey);
  }, [open, setOpen]);

  if (!open) return null;

  let flatIndex = 0;

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]"
      onClick={() => setOpen(false)}
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
    >
      <div className="absolute inset-0 bg-black/60 animate-fade-in" />
      <div
        className="relative w-full max-w-[520px] bg-[var(--surface)] border border-[var(--border)] rounded-xl shadow-2xl overflow-hidden animate-fade-in"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        <div className="flex items-center gap-2 px-4 h-12 border-b border-[var(--border)]">
          <Search size={16} className="text-[var(--text-muted)] shrink-0" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search workspaces, vault, teams, actions..."
            className="flex-1 bg-transparent text-sm text-[var(--text)] placeholder:text-[var(--text-dim)] outline-none"
            role="combobox"
            aria-expanded="true"
            aria-controls="command-palette-list"
            aria-activedescendant={activeDescendantId}
            aria-autocomplete="list"
            aria-label="Search commands"
          />
        </div>

        <div
          ref={listRef}
          className="max-h-[360px] overflow-y-auto py-2"
          id="command-palette-list"
          role="listbox"
          aria-label="Search results"
        >
          {filtered.length === 0 && (
            <div className="px-4 py-6 text-center text-sm text-[var(--text-muted)]">
              No results found
            </div>
          )}
          {Array.from(grouped.entries()).map(([category, categoryItems]) => (
            <div key={category} role="group" aria-label={category}>
              <div className="px-4 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-dim)]" role="presentation">
                {category}
              </div>
              {categoryItems.map((item) => {
                const currentIndex = flatIndex++;
                const isSelected = currentIndex === selectedIndex;
                return (
                  <button
                    key={item.id}
                    id={item.id}
                    data-item-id={item.id}
                    role="option"
                    aria-selected={isSelected}
                    onClick={item.action}
                    className={`flex items-center gap-3 w-full px-4 py-2 text-left transition-colors ${
                      isSelected
                        ? 'bg-[#5C4033]/10 border-l-2 border-[#5C4033] text-[var(--text)]'
                        : 'border-l-2 border-transparent text-[var(--text)] hover:bg-[var(--surface-2)]'
                    }`}
                  >
                    <item.icon size={14} className={isSelected ? 'text-[var(--text)]' : 'text-[var(--text-muted)]'} />
                    <div className="flex-1 min-w-0">
                      <div className="text-sm truncate flex items-center gap-2">
                        {item.label}
                        <span className="text-[10px] font-medium text-[var(--text-dim)] bg-[var(--surface-2)] px-1.5 py-0.5 rounded-full lowercase shrink-0">
                          {item.category === 'Workspaces' ? 'workspace' : item.category === 'Vault' ? 'vault' : item.category === 'Teams' ? 'team' : 'action'}
                        </span>
                      </div>
                      {item.sublabel && (
                        <div className="text-xs text-[var(--text-muted)] truncate">{item.sublabel}</div>
                      )}
                    </div>
                    {item.shortcut && (
                      <kbd className="text-[10px] font-mono bg-[var(--surface-2)] text-[var(--text-muted)] px-1.5 py-0.5 rounded border border-[var(--border)] shrink-0">
                        {item.shortcut}
                      </kbd>
                    )}
                  </button>
                );
              })}
            </div>
          ))}
        </div>

        <div className="flex items-center gap-4 px-4 h-8 border-t border-[var(--border)] text-xs text-[#4A4238]">
          <span><kbd className="font-mono">&#8593;&#8595;</kbd> Navigate</span>
          <span><kbd className="font-mono">&#8629;</kbd> Open</span>
          <span><kbd className="font-mono">esc</kbd> Close</span>
        </div>
      </div>
    </div>
  );
}
