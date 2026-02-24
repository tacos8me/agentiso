import { Search, PanelLeft, Menu, Box, ListTodo, BookOpen, Users } from 'lucide-react';
import { useUiStore } from '../../stores/ui';

import { NotificationCenter } from '../common/NotificationCenter';
import { ConnectionStatus } from '../common/ConnectionStatus';

export function TopBar() {
  const toggleSidebar = useUiStore((s) => s.toggleSidebar);
  const toggleCommandPalette = useUiStore((s) => s.toggleCommandPalette);
  const mobileMenuOpen = useUiStore((s) => s.mobileMenuOpen);
  const setMobileMenuOpen = useUiStore((s) => s.setMobileMenuOpen);
  const sidebarOpen = useUiStore((s) => s.sidebarOpen);
  const sidebarSection = useUiStore((s) => s.sidebarSection);
  const setSidebarSection = useUiStore((s) => s.setSidebarSection);

  return (
    <header
      className="flex items-center h-14 px-3 border-b border-[var(--border)] bg-[var(--surface)] shrink-0 select-none"
      role="banner"
    >
      {/* Left: sidebar toggle + logo + connection status */}
      <div className="flex items-center gap-2">
        {/* Hamburger for mobile (< 768px) */}
        <button
          onClick={() => setMobileMenuOpen(!mobileMenuOpen)}
          className="w-8 h-8 items-center justify-center rounded hover:bg-[var(--surface-2)] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors hidden max-[767px]:flex"
          aria-label="Toggle mobile menu"
          aria-expanded={mobileMenuOpen}
        >
          <Menu size={18} />
        </button>
        {/* Desktop sidebar toggle */}
        <button
          onClick={toggleSidebar}
          className="w-8 h-8 items-center justify-center rounded hover:bg-[var(--surface-2)] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors flex max-[767px]:hidden"
          aria-label="Toggle sidebar"
        >
          <PanelLeft size={18} />
        </button>
        <div className="flex items-center gap-2">
          <div
            className="w-6 h-6 rounded flex items-center justify-center"
            style={{ background: 'linear-gradient(135deg, #5C4033 0%, #7A5A4A 100%)' }}
          >
            <span className="text-[11px] font-bold text-[var(--text)]">a</span>
          </div>
          <span className="text-[15px] font-semibold text-[var(--text)] tracking-tight">agentiso</span>
          <ConnectionStatus />
        </div>
        {/* Section icons when sidebar is closed */}
        {!sidebarOpen && (
          <div className="flex items-center gap-0.5 ml-3">
            {[
              { id: 'workspaces' as const, icon: Box, label: 'Workspaces' },
              { id: 'tasks' as const, icon: ListTodo, label: 'Tasks' },
              { id: 'vault' as const, icon: BookOpen, label: 'Vault' },
              { id: 'teams' as const, icon: Users, label: 'Teams' },
            ].map(({ id, icon: Icon, label }) => (
              <button
                key={id}
                onClick={() => { setSidebarSection(id); }}
                className={`w-8 h-8 flex items-center justify-center rounded transition-colors ${
                  sidebarSection === id
                    ? 'bg-[#5C4033]/15 text-[var(--text)]'
                    : 'text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-[var(--surface-2)]'
                }`}
                title={label}
                aria-label={label}
              >
                <Icon size={16} />
              </button>
            ))}
          </div>
        )}
      </div>

      {/* Center: search trigger */}
      <div className="flex-1 flex justify-center">
        <button
          onClick={toggleCommandPalette}
          className="flex items-center gap-2 h-9 px-3 rounded-md border border-[var(--border)] bg-[var(--bg)] text-[var(--text-muted)] hover:border-[var(--border-hover)] hover:text-[var(--text)] transition-colors max-w-[320px] w-full shadow-[inset_0_1px_2px_rgba(0,0,0,0.3)] focus-within:ring-1 focus-within:ring-[#5C4033]/50"
          aria-label="Open command palette"
        >
          <Search size={15} />
          <span className="text-xs flex-1 text-left">Search...</span>
          <kbd className="text-[10px] font-mono bg-[var(--surface-2)] px-1.5 py-0.5 rounded border border-[var(--border)] max-[639px]:hidden">
            Ctrl+K
          </kbd>
        </button>
      </div>

      {/* Right: notifications */}
      <div className="flex items-center gap-1">
        <NotificationCenter />
      </div>
    </header>
  );
}
