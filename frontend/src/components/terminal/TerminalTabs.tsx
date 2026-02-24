import { useState, useCallback } from 'react';
import { X, Plus, ChevronDown, LayoutGrid, Rows3 } from 'lucide-react';
import { TerminalPane } from './TerminalPane';
import { useTerminalStore } from '../../stores/terminals';
import { useWorkspaceStore } from '../../stores/workspaces';

type ViewMode = 'tabs' | 'grid';

interface TerminalTabsProps {
  workspaceId?: string;
}

export function TerminalTabs({ workspaceId }: TerminalTabsProps) {
  const sessions = useTerminalStore((s) => s.sessions);
  const createTerminal = useTerminalStore((s) => s.createTerminal);
  const destroyTerminal = useTerminalStore((s) => s.destroyTerminal);
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const runningWorkspaces = workspaces.filter((w) => w.state === 'running');

  const filteredSessions = workspaceId
    ? sessions.filter((s) => s.workspaceId === workspaceId)
    : sessions;

  const [activeSessionId, setActiveSessionId] = useState<string | null>(
    filteredSessions[0]?.id ?? null
  );
  const [showNewMenu, setShowNewMenu] = useState(false);
  const [viewMode, setViewMode] = useState<ViewMode>('tabs');
  // When a grid cell is double-clicked, maximize that session (solo view)
  const [maximizedSessionId, setMaximizedSessionId] = useState<string | null>(null);

  const activeSession = filteredSessions.find((s) => s.id === activeSessionId);
  const effectiveActiveId = activeSession?.id ?? filteredSessions[0]?.id ?? null;

  const handleNew = useCallback(
    (wsId: string) => {
      const ws = workspaces.find((w) => w.id === wsId);
      if (!ws) return;
      const id = createTerminal(wsId, ws.name);
      setActiveSessionId(id);
      setShowNewMenu(false);
    },
    [workspaces, createTerminal]
  );

  const handleClose = useCallback(
    (id: string) => {
      destroyTerminal(id);
      if (effectiveActiveId === id) {
        const remaining = filteredSessions.filter((s) => s.id !== id);
        setActiveSessionId(remaining[0]?.id ?? null);
      }
      if (maximizedSessionId === id) {
        setMaximizedSessionId(null);
      }
    },
    [destroyTerminal, effectiveActiveId, filteredSessions, maximizedSessionId]
  );

  const handleMaximize = useCallback(
    (id: string) => {
      setMaximizedSessionId((prev) => (prev === id ? null : id));
    },
    []
  );

  const getShellNum = useCallback(
    (s: (typeof filteredSessions)[0]) =>
      sessions
        .filter((t) => t.workspaceId === s.workspaceId)
        .findIndex((t) => t.id === s.id) + 1,
    [sessions]
  );

  if (filteredSessions.length === 0 && !workspaceId) {
    return (
      <div className="flex flex-col items-center justify-center h-full bg-[#0A0A0A] gap-4">
        <div className="empty-state-shape-lines" aria-hidden="true" />
        <div className="text-center">
          <p className="text-sm text-[var(--text-muted)] font-mono">No terminal sessions</p>
          {runningWorkspaces.length > 0 ? (
            <div className="flex flex-col items-center gap-2 mt-3">
              <span className="text-[var(--text-dim)] text-xs">Select a running workspace to open a terminal</span>
              {runningWorkspaces.slice(0, 5).map((ws) => (
                <button
                  key={ws.id}
                  onClick={() => handleNew(ws.id)}
                  className="px-3 py-1.5 rounded-md bg-[var(--surface-2)] text-[var(--text)] text-xs hover:bg-[var(--accent)] transition-colors font-mono"
                >
                  {ws.name}
                </button>
              ))}
            </div>
          ) : (
            <p className="text-[var(--text-dim)] text-xs mt-1">Start a workspace to use the terminal.</p>
          )}
        </div>
      </div>
    );
  }

  // If filtered to a workspace and no sessions, auto-create one
  if (filteredSessions.length === 0 && workspaceId) {
    const ws = workspaces.find((w) => w.id === workspaceId);
    if (ws) {
      const id = createTerminal(ws.id, ws.name);
      setActiveSessionId(id);
    }
    return (
      <div className="flex items-center justify-center h-full bg-[#0A0A0A] text-[var(--text-muted)] text-sm font-mono">
        Connecting to {workspaces.find((w) => w.id === workspaceId)?.name ?? 'workspace'}...
      </div>
    );
  }

  // Grid layout: determine CSS grid classes based on visible count
  const gridSessions = filteredSessions.slice(0, 4);
  const gridCols =
    gridSessions.length === 1
      ? 'grid-cols-1'
      : 'grid-cols-2';
  const gridRows =
    gridSessions.length <= 2
      ? 'grid-rows-1'
      : 'grid-rows-2';

  return (
    <div className="flex flex-col h-full bg-[#0A0A0A]" role="application" aria-label="Terminal">
      {/* Tab bar */}
      <div className="flex items-center h-8 bg-[#161210] border-b border-[#252018] shrink-0">
        <div className="flex items-center overflow-x-auto flex-1" role="tablist" aria-label="Terminal tabs">
          {filteredSessions.map((s) => {
            const isActive = s.id === effectiveActiveId;
            const shellNum = getShellNum(s);
            return (
              <div
                key={s.id}
                role="tab"
                aria-selected={isActive}
                tabIndex={isActive ? 0 : -1}
                className={`flex items-center gap-1.5 h-full px-3 text-xs cursor-pointer border-r border-[#252018] transition-colors group ${
                  isActive
                    ? 'bg-[#1E1A16] text-[#DCD5CC] active-tab-indicator'
                    : 'text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#1E1A16]'
                }`}
                onClick={() => setActiveSessionId(s.id)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    setActiveSessionId(s.id);
                  }
                }}
              >
                <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                  s.isExecuting ? 'bg-[#4A7C59] animate-pulse-dot' : 'bg-[#4A7C59]'
                }`} aria-label={s.isExecuting ? 'Executing' : 'Connected'} />
                <span className="font-mono truncate max-w-[140px]">
                  {s.workspaceName}:{shellNum}
                </span>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleClose(s.id);
                  }}
                  className="w-4 h-4 flex items-center justify-center rounded opacity-0 group-hover:opacity-100 hover:bg-[#262220] transition-opacity"
                  aria-label={`Close terminal ${s.workspaceName}:${shellNum}`}
                >
                  <X size={10} />
                </button>
              </div>
            );
          })}
        </div>

        {/* View mode toggle */}
        {filteredSessions.length > 1 && (
          <button
            onClick={() => {
              setViewMode((m) => (m === 'tabs' ? 'grid' : 'tabs'));
              setMaximizedSessionId(null);
            }}
            className="flex items-center justify-center w-8 h-8 text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#1E1A16] transition-colors"
            aria-label={viewMode === 'tabs' ? 'Switch to grid view' : 'Switch to tab view'}
            title={viewMode === 'tabs' ? 'Grid view' : 'Tab view'}
          >
            {viewMode === 'tabs' ? <LayoutGrid size={14} /> : <Rows3 size={14} />}
          </button>
        )}

        {/* New terminal button */}
        <div className="relative shrink-0">
          <button
            onClick={() => setShowNewMenu((v) => !v)}
            className="flex items-center gap-0.5 h-8 px-2 cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#1E1A16] transition-colors"
            aria-label="New terminal"
            aria-haspopup="true"
            aria-expanded={showNewMenu}
          >
            <Plus size={14} />
            <ChevronDown size={10} />
          </button>

          {showNewMenu && (
            <>
              <div className="fixed inset-0 z-40" onClick={() => setShowNewMenu(false)} />
              <div className="absolute right-0 top-8 z-50 min-w-[180px] bg-[#1E1A16] border border-[#252018] rounded-md shadow-xl overflow-hidden animate-fade-in">
                {runningWorkspaces.length === 0 ? (
                  <div className="px-3 py-2 text-xs text-[#6B6258]">No running workspaces</div>
                ) : (
                  runningWorkspaces.map((ws) => (
                    <button
                      key={ws.id}
                      onClick={() => handleNew(ws.id)}
                      className="w-full text-left px-3 py-2 text-xs text-[#DCD5CC] hover:bg-[#5C4033] transition-colors font-mono"
                    >
                      {ws.name}
                    </button>
                  ))
                )}
              </div>
            </>
          )}
        </div>
      </div>

      {/* Terminal content */}
      <div className="flex-1 overflow-hidden" role="tabpanel">
        {viewMode === 'tabs' || filteredSessions.length <= 1 ? (
          /* Tab mode: single terminal */
          effectiveActiveId ? (
            <TerminalPane
              sessionId={effectiveActiveId}
              onMaximize={undefined}
            />
          ) : (
            <div className="flex items-center justify-center h-full text-[var(--text-dim)] text-sm font-mono">
              No active terminal
            </div>
          )
        ) : maximizedSessionId && filteredSessions.some((s) => s.id === maximizedSessionId) ? (
          /* Grid mode, maximized: single terminal with restore on double-click */
          <div className="h-full flex flex-col">
            <GridCellHeader
              session={filteredSessions.find((s) => s.id === maximizedSessionId)!}
              shellNum={getShellNum(filteredSessions.find((s) => s.id === maximizedSessionId)!)}
              isActive={true}
              onClose={() => handleClose(maximizedSessionId)}
              onDoubleClick={() => setMaximizedSessionId(null)}
            />
            <div className="flex-1 overflow-hidden">
              <TerminalPane
                sessionId={maximizedSessionId}
                onMaximize={() => setMaximizedSessionId(null)}
              />
            </div>
          </div>
        ) : (
          /* Grid mode: 2x2 grid layout */
          <div className={`grid ${gridCols} ${gridRows} gap-1 h-full p-1`}>
            {gridSessions.map((s) => {
              const isFocused = s.id === effectiveActiveId;
              const shellNum = getShellNum(s);
              return (
                <div
                  key={s.id}
                  className={`flex flex-col overflow-hidden rounded-sm border ${
                    isFocused ? 'border-[#5C4033]' : 'border-[#252018]'
                  }`}
                  onClick={() => setActiveSessionId(s.id)}
                >
                  <GridCellHeader
                    session={s}
                    shellNum={shellNum}
                    isActive={isFocused}
                    onClose={() => handleClose(s.id)}
                    onDoubleClick={() => handleMaximize(s.id)}
                  />
                  <div className="flex-1 overflow-hidden">
                    <TerminalPane
                      sessionId={s.id}
                      onMaximize={() => handleMaximize(s.id)}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

/** Small header bar rendered above each grid cell */
function GridCellHeader({
  session,
  shellNum,
  isActive,
  onClose,
  onDoubleClick,
}: {
  session: { id: string; workspaceName: string; isExecuting: boolean };
  shellNum: number;
  isActive: boolean;
  onClose: () => void;
  onDoubleClick: () => void;
}) {
  return (
    <div
      className={`flex items-center justify-between h-6 px-2 bg-[#161210] shrink-0 select-none ${
        isActive ? 'text-[#DCD5CC]' : 'text-[#6B6258]'
      }`}
      onDoubleClick={onDoubleClick}
    >
      <span className="text-[11px] font-mono truncate">
        {session.workspaceName}:{shellNum}
      </span>
      <div className="flex items-center gap-1">
        {session.isExecuting && (
          <span className="w-1.5 h-1.5 rounded-full bg-[#4A7C59] animate-pulse-dot" />
        )}
        <button
          onClick={(e) => {
            e.stopPropagation();
            onClose();
          }}
          className="w-4 h-4 flex items-center justify-center rounded hover:bg-[#262220] transition-opacity opacity-60 hover:opacity-100"
          aria-label="Close"
        >
          <X size={9} />
        </button>
      </div>
    </div>
  );
}
