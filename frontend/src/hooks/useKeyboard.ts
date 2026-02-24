import { useEffect } from 'react';
import { useUiStore } from '../stores/ui';

/**
 * Global keyboard shortcut handler. Attach once in App.tsx.
 */
export function useKeyboard() {
  const toggleCommandPalette = useUiStore((s) => s.toggleCommandPalette);
  const setCommandPaletteOpen = useUiStore((s) => s.setCommandPaletteOpen);

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      const ctrl = e.ctrlKey || e.metaKey;

      // Ctrl+K — command palette (already handled in CommandPalette, but ensure global)
      if (ctrl && e.key === 'k') {
        e.preventDefault();
        toggleCommandPalette();
        return;
      }

      // Ctrl+\ — split pane vertical
      if (ctrl && e.key === '\\') {
        e.preventDefault();
        const state = useUiStore.getState();
        const newId = `pane-${Date.now()}`;
        state.addPane({
          id: newId,
          tabs: [{ id: `tab-${Date.now()}`, type: 'kanban', title: 'Workspaces' }],
          activeTabId: `tab-${Date.now()}`,
        });
        state.setActivePane(newId);
        return;
      }

      // Ctrl+- — split pane horizontal (placeholder: adds a new pane)
      if (ctrl && e.key === '-') {
        e.preventDefault();
        const state = useUiStore.getState();
        const newId = `pane-h-${Date.now()}`;
        state.addPane({
          id: newId,
          tabs: [{ id: `tab-h-${Date.now()}`, type: 'kanban', title: 'Workspaces' }],
          activeTabId: `tab-h-${Date.now()}`,
        });
        state.setActivePane(newId);
        return;
      }

      // Ctrl+W — close active pane tab
      if (ctrl && e.key === 'w') {
        // Only intercept if not in an input/textarea
        if (isEditing(e)) return;
        e.preventDefault();
        const state = useUiStore.getState();
        const pane = state.panes.find((p) => p.id === state.activePaneId);
        if (pane && pane.tabs.length > 1) {
          state.removeTabFromPane(pane.id, pane.activeTabId);
        }
        return;
      }

      // Ctrl+Tab / Ctrl+Shift+Tab — cycle pane tabs
      if (ctrl && e.key === 'Tab') {
        e.preventDefault();
        const state = useUiStore.getState();
        const pane = state.panes.find((p) => p.id === state.activePaneId);
        if (!pane || pane.tabs.length < 2) return;
        const idx = pane.tabs.findIndex((t) => t.id === pane.activeTabId);
        const next = e.shiftKey
          ? (idx - 1 + pane.tabs.length) % pane.tabs.length
          : (idx + 1) % pane.tabs.length;
        state.setActiveTab(pane.id, pane.tabs[next].id);
        return;
      }

      // Escape — close overlays
      if (e.key === 'Escape') {
        setCommandPaletteOpen(false);
        // NotificationCenter and drawers handle their own Escape via their components
        return;
      }

      // Ctrl+` — focus terminal pane
      if (ctrl && e.key === '`') {
        e.preventDefault();
        const state = useUiStore.getState();
        for (const pane of state.panes) {
          const termTab = pane.tabs.find((t) => t.type === 'terminal');
          if (termTab) {
            state.setActivePane(pane.id);
            state.setActiveTab(pane.id, termTab.id);
            return;
          }
        }
        return;
      }

      // Ctrl+1-9 — jump to pane by index
      if (ctrl && e.key >= '1' && e.key <= '9') {
        e.preventDefault();
        const state = useUiStore.getState();
        const idx = parseInt(e.key, 10) - 1;
        if (idx < state.panes.length) {
          state.setActivePane(state.panes[idx].id);
        }
        return;
      }
    }

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [toggleCommandPalette, setCommandPaletteOpen]);
}

function isEditing(e: KeyboardEvent): boolean {
  const target = e.target as HTMLElement;
  return (
    target.tagName === 'INPUT' ||
    target.tagName === 'TEXTAREA' ||
    target.isContentEditable
  );
}
