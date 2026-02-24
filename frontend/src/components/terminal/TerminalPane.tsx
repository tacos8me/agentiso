import { useEffect, useRef, useCallback, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { SearchAddon } from '@xterm/addon-search';
import {
  Maximize2, Clipboard, Search, Trash2, ClipboardCopy,
  ChevronUp, ChevronDown, X, Copy, ClipboardPaste,
  Eraser, SquareDashedMousePointer,
} from 'lucide-react';
import '@xterm/xterm/css/xterm.css';
import { useTerminalStore } from '../../stores/terminals';

const XTERM_THEME = {
  background: '#0A0A0A',
  foreground: '#DCD5CC',
  cursor: '#5C4033',
  cursorAccent: '#0A0A0A',
  selectionBackground: '#5C403380',
  black: '#0A0A0A',
  red: '#8B4A4A',
  green: '#4A7C59',
  yellow: '#8B7B3A',
  blue: '#4A6B8B',
  magenta: '#7A5A6A',
  cyan: '#4A7B7B',
  white: '#DCD5CC',
  brightBlack: '#3E3830',
  brightRed: '#A85A5A',
  brightGreen: '#5A9C69',
  brightYellow: '#AB9B4A',
  brightBlue: '#5A8BAB',
  brightMagenta: '#9A7A8A',
  brightCyan: '#5A9B9B',
  brightWhite: '#EDE6DD',
};

const FONT_SIZE_KEY = 'agentiso-terminal-font-size';
const DEFAULT_FONT_SIZE = 13;
const MIN_FONT_SIZE = 10;
const MAX_FONT_SIZE = 20;

function loadFontSize(): number {
  try {
    const stored = localStorage.getItem(FONT_SIZE_KEY);
    if (stored) {
      const size = parseInt(stored, 10);
      if (size >= MIN_FONT_SIZE && size <= MAX_FONT_SIZE) return size;
    }
  } catch { /* ignore */ }
  return DEFAULT_FONT_SIZE;
}

function saveFontSize(size: number) {
  try {
    localStorage.setItem(FONT_SIZE_KEY, String(size));
  } catch { /* ignore */ }
}

interface TerminalPaneProps {
  sessionId: string;
  /** Callback to toggle maximize/restore in grid view. Undefined in tabs mode. */
  onMaximize?: (() => void) | undefined;
}

interface ContextMenuState {
  visible: boolean;
  x: number;
  y: number;
  hasSelection: boolean;
}

export function TerminalPane({ sessionId, onMaximize }: TerminalPaneProps) {
  const termRef = useRef<HTMLDivElement>(null);
  const xtermRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const searchAddonRef = useRef<SearchAddon | null>(null);
  const currentLineRef = useRef('');
  const lastOutputLenRef = useRef(0);
  const fontSizeRef = useRef(loadFontSize());
  const containerRef = useRef<HTMLDivElement>(null);

  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [pasteConfirm, setPasteConfirm] = useState<{ text: string; lineCount: number } | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState>({ visible: false, x: 0, y: 0, hasSelection: false });

  const session = useTerminalStore((s) => s.sessions.find((t) => t.id === sessionId));
  const addToHistory = useTerminalStore((s) => s.addToHistory);
  const setHistoryIndex = useTerminalStore((s) => s.setHistoryIndex);
  const appendOutput = useTerminalStore((s) => s.appendOutput);
  const executeCommand = useTerminalStore((s) => s.executeCommand);
  const executeBackground = useTerminalStore((s) => s.executeBackground);
  const cancelStream = useTerminalStore((s) => s.cancelStream);

  const searchInputRef = useRef<HTMLInputElement>(null);

  const writePrompt = useCallback(() => {
    const term = xtermRef.current;
    if (!term || !session) return;
    term.write(`\x1b[38;2;92;64;51m${session.workspaceName}:~$ \x1b[0m`);
  }, [session]);

  // Copy last command output (existing feature)
  const handleCopyLastOutput = useCallback(() => {
    if (!session) return;
    const buf = session.outputBuffer;
    const exitPattern = /\[exit \d+\]/g;
    let lastExitIdx = -1;
    let match: RegExpExecArray | null;
    while ((match = exitPattern.exec(buf)) !== null) {
      lastExitIdx = match.index;
    }
    if (lastExitIdx === -1) return;
    const exitEnd = buf.indexOf('\n', lastExitIdx);
    const endIdx = exitEnd === -1 ? buf.length : exitEnd;
    const promptMarker = ':~$ \x1b[0m';
    const beforeExit = buf.slice(0, lastExitIdx);
    const lastPromptIdx = beforeExit.lastIndexOf(promptMarker);
    if (lastPromptIdx === -1) return;
    const startIdx = lastPromptIdx + promptMarker.length;
    const firstNewline = buf.indexOf('\n', startIdx);
    if (firstNewline === -1 || firstNewline >= endIdx) return;
    const rawOutput = buf
      .slice(firstNewline + 1, endIdx)
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\[[0-9;]*m/g, '')
      .trimEnd();
    if (rawOutput) {
      navigator.clipboard.writeText(rawOutput).catch(() => {});
    }
  }, [session]);

  // Search controls
  const openSearch = useCallback(() => {
    setSearchOpen(true);
    setTimeout(() => searchInputRef.current?.focus(), 0);
  }, []);

  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    setSearchQuery('');
    searchAddonRef.current?.clearDecorations();
    xtermRef.current?.focus();
  }, []);

  const doSearchNext = useCallback(() => {
    if (searchQuery) searchAddonRef.current?.findNext(searchQuery);
  }, [searchQuery]);

  const doSearchPrev = useCallback(() => {
    if (searchQuery) searchAddonRef.current?.findPrevious(searchQuery);
  }, [searchQuery]);

  // Paste handler
  const handlePaste = useCallback(async () => {
    try {
      const text = await navigator.clipboard.readText();
      if (!text) return;
      const lineCount = text.split('\n').length;
      if (lineCount > 1) {
        setPasteConfirm({ text, lineCount });
      } else {
        const term = xtermRef.current;
        if (term) {
          currentLineRef.current += text;
          term.write(text);
        }
      }
    } catch {
      // Clipboard access denied
    }
  }, []);

  const confirmPaste = useCallback(() => {
    if (!pasteConfirm) return;
    const term = xtermRef.current;
    if (term) {
      currentLineRef.current += pasteConfirm.text;
      term.write(pasteConfirm.text);
    }
    setPasteConfirm(null);
    xtermRef.current?.focus();
  }, [pasteConfirm]);

  const cancelPaste = useCallback(() => {
    setPasteConfirm(null);
    xtermRef.current?.focus();
  }, []);

  // Font size controls
  const changeFontSize = useCallback((delta: number | null) => {
    const term = xtermRef.current;
    if (!term) return;
    let newSize: number;
    if (delta === null) {
      newSize = DEFAULT_FONT_SIZE;
    } else {
      newSize = Math.max(MIN_FONT_SIZE, Math.min(MAX_FONT_SIZE, fontSizeRef.current + delta));
    }
    if (newSize === fontSizeRef.current) return;
    fontSizeRef.current = newSize;
    saveFontSize(newSize);
    term.options.fontSize = newSize;
    fitAddonRef.current?.fit();
  }, []);

  // Context menu actions
  const handleCopy = useCallback(() => {
    const term = xtermRef.current;
    if (term) {
      const selection = term.getSelection();
      if (selection) navigator.clipboard.writeText(selection);
    }
    setContextMenu((s) => ({ ...s, visible: false }));
  }, []);

  const handleClear = useCallback(() => {
    xtermRef.current?.clear();
    setContextMenu((s) => ({ ...s, visible: false }));
  }, []);

  const handleSelectAll = useCallback(() => {
    xtermRef.current?.selectAll();
    setContextMenu((s) => ({ ...s, visible: false }));
  }, []);

  const handleCopyAllOutput = useCallback(() => {
    const term = xtermRef.current;
    if (!term) return;
    term.selectAll();
    const selection = term.getSelection();
    if (selection) navigator.clipboard.writeText(selection);
    term.clearSelection();
  }, []);

  // Initialize xterm
  useEffect(() => {
    if (!termRef.current || xtermRef.current) return;

    const term = new Terminal({
      theme: XTERM_THEME,
      fontFamily: "'JetBrains Mono', ui-monospace, monospace",
      fontSize: fontSizeRef.current,
      lineHeight: 1.4,
      cursorBlink: true,
      cursorStyle: 'block',
      scrollback: 5000,
      convertEol: true,
      allowProposedApi: true,
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    const searchAddon = new SearchAddon();
    term.loadAddon(searchAddon);
    searchAddonRef.current = searchAddon;

    term.open(termRef.current);

    // Try WebGL addon, fall back silently
    try {
      const webglAddon = new WebglAddon();
      webglAddon.onContextLoss(() => {
        webglAddon.dispose();
      });
      term.loadAddon(webglAddon);
    } catch {
      // WebGL not available, canvas renderer is fine
    }

    fitAddon.fit();

    xtermRef.current = term;
    fitAddonRef.current = fitAddon;

    // Write welcome message
    if (session) {
      term.writeln(`\x1b[38;2;90;82;74mConnected to ${session.workspaceName}\x1b[0m`);
      term.writeln('');
    }

    // Write any existing output buffer
    if (session && session.outputBuffer) {
      term.write(session.outputBuffer);
      lastOutputLenRef.current = session.outputBuffer.length;
    }

    writePrompt();

    return () => {
      term.dispose();
      xtermRef.current = null;
      fitAddonRef.current = null;
      searchAddonRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  // Watch for new output from store
  useEffect(() => {
    if (!session || !xtermRef.current) return;
    const newLen = session.outputBuffer.length;
    if (newLen > lastOutputLenRef.current) {
      const newText = session.outputBuffer.slice(lastOutputLenRef.current);
      xtermRef.current.write(newText);
      lastOutputLenRef.current = newLen;
      // Re-show prompt after output finishes
      if (!session.isExecuting) {
        writePrompt();
      }
    }
  }, [session, session?.outputBuffer, session?.isExecuting, writePrompt]);

  // ResizeObserver for auto-fit
  useEffect(() => {
    const container = termRef.current;
    if (!container || !fitAddonRef.current) return;

    const observer = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        try {
          fitAddonRef.current?.fit();
        } catch {
          // ignore fit errors during rapid resize
        }
      });
    });

    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  // Close context menu on click outside or Escape
  useEffect(() => {
    if (!contextMenu.visible) return;
    const handleClick = () => setContextMenu((s) => ({ ...s, visible: false }));
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setContextMenu((s) => ({ ...s, visible: false }));
    };
    document.addEventListener('click', handleClick);
    document.addEventListener('keydown', handleKey);
    return () => {
      document.removeEventListener('click', handleClick);
      document.removeEventListener('keydown', handleKey);
    };
  }, [contextMenu.visible]);

  // Keyboard input handler
  useEffect(() => {
    const term = xtermRef.current;
    if (!term || !session) return;

    const disposable = term.onData((data) => {
      const store = useTerminalStore.getState();
      const currentSession = store.sessions.find((t) => t.id === sessionId);
      if (!currentSession) return;

      // Ignore input while executing (except Ctrl+C)
      if (currentSession.isExecuting && data !== '\x03') return;

      // Ctrl+C cancels active stream or just prints ^C
      if (data === '\x03' && currentSession.isExecuting) {
        cancelStream(sessionId);
        currentLineRef.current = '';
        return;
      }

      if (data === '\r') {
        // Enter pressed
        term.write('\r\n');
        const command = currentLineRef.current.trim();
        currentLineRef.current = '';

        if (command) {
          addToHistory(sessionId, command);
          // Append the command line to output buffer for history
          appendOutput(sessionId, `\x1b[38;2;92;64;51m${currentSession.workspaceName}:~$ \x1b[0m${command}\n`);
          lastOutputLenRef.current = useTerminalStore.getState().sessions.find(
            (t) => t.id === sessionId
          )?.outputBuffer.length ?? lastOutputLenRef.current;

          if (command.startsWith('&')) {
            // Background exec
            executeBackground(sessionId, command.slice(1).trim());
          } else {
            executeCommand(sessionId, command);
          }
        } else {
          writePrompt();
        }
      } else if (data === '\x7f' || data === '\b') {
        // Backspace
        if (currentLineRef.current.length > 0) {
          currentLineRef.current = currentLineRef.current.slice(0, -1);
          term.write('\b \b');
        }
      } else if (data === '\x03') {
        // Ctrl+C
        currentLineRef.current = '';
        term.write('^C\r\n');
        writePrompt();
      } else if (data === '\x1b[A') {
        // Up arrow - navigate history
        const history = currentSession.history;
        if (history.length === 0) return;
        let idx = currentSession.historyIndex;
        if (idx === -1) {
          idx = history.length - 1;
        } else if (idx > 0) {
          idx--;
        }
        setHistoryIndex(sessionId, idx);
        // Clear current line
        const clearLen = currentLineRef.current.length;
        term.write('\b \b'.repeat(clearLen));
        currentLineRef.current = history[idx];
        term.write(history[idx]);
      } else if (data === '\x1b[B') {
        // Down arrow - navigate history
        const history = currentSession.history;
        let idx = currentSession.historyIndex;
        if (idx === -1) return;
        const clearLen = currentLineRef.current.length;
        term.write('\b \b'.repeat(clearLen));

        if (idx >= history.length - 1) {
          idx = -1;
          currentLineRef.current = '';
          setHistoryIndex(sessionId, -1);
        } else {
          idx++;
          currentLineRef.current = history[idx];
          term.write(history[idx]);
          setHistoryIndex(sessionId, idx);
        }
      } else if (data === '\x15') {
        // Ctrl+U - clear line
        const clearLen = currentLineRef.current.length;
        term.write('\b \b'.repeat(clearLen));
        currentLineRef.current = '';
      } else if (data === '\x0c') {
        // Ctrl+L - clear screen
        term.clear();
        writePrompt();
        term.write(currentLineRef.current);
      } else if (data.length === 1 && data >= ' ') {
        // Printable character
        currentLineRef.current += data;
        term.write(data);
      }
    });

    return () => disposable.dispose();
  }, [sessionId, session, addToHistory, setHistoryIndex, appendOutput, executeCommand, executeBackground, cancelStream, writePrompt]);

  // Global keyboard shortcuts (Ctrl+F search, Ctrl+V paste, font size)
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      const ctrlOrMeta = e.ctrlKey || e.metaKey;

      // Ctrl+F -- search
      if (ctrlOrMeta && e.key === 'f') {
        e.preventDefault();
        e.stopPropagation();
        if (searchOpen) closeSearch();
        else openSearch();
        return;
      }

      // Ctrl+V -- paste
      if (ctrlOrMeta && e.key === 'v') {
        e.preventDefault();
        e.stopPropagation();
        handlePaste();
        return;
      }

      // Font size: Ctrl+= increase, Ctrl+- decrease, Ctrl+0 reset
      if (ctrlOrMeta && (e.key === '=' || e.key === '+')) {
        e.preventDefault();
        e.stopPropagation();
        changeFontSize(1);
        return;
      }
      if (ctrlOrMeta && e.key === '-') {
        e.preventDefault();
        e.stopPropagation();
        changeFontSize(-1);
        return;
      }
      if (ctrlOrMeta && e.key === '0') {
        e.preventDefault();
        e.stopPropagation();
        changeFontSize(null);
        return;
      }
    };

    container.addEventListener('keydown', handleKeyDown, true);
    return () => container.removeEventListener('keydown', handleKeyDown, true);
  }, [searchOpen, closeSearch, openSearch, handlePaste, changeFontSize]);

  // Live search as user types
  useEffect(() => {
    if (searchOpen && searchQuery) {
      searchAddonRef.current?.findNext(searchQuery);
    } else if (!searchQuery) {
      searchAddonRef.current?.clearDecorations();
    }
  }, [searchQuery, searchOpen]);

  if (!session) {
    return (
      <div className="flex items-center justify-center h-full bg-[#0A0A0A] text-[var(--text-muted)] text-sm font-mono">
        Terminal session not found
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      className="h-full w-full bg-[#0A0A0A] overflow-hidden relative group/pane"
      role="application"
      aria-label={`Terminal - ${session.workspaceName}`}
      onContextMenu={(e) => {
        e.preventDefault();
        const rect = containerRef.current?.getBoundingClientRect();
        const x = rect ? e.clientX - rect.left : e.clientX;
        const y = rect ? e.clientY - rect.top : e.clientY;
        const hasSelection = !!(xtermRef.current?.getSelection());
        setContextMenu({ visible: true, x, y, hasSelection });
      }}
    >
      <div ref={termRef} className="h-full w-full" />

      {/* Quick toolbar -- top-right floating */}
      <div className="absolute top-1 right-1 flex items-center gap-0.5 opacity-0 group-hover/pane:opacity-100 transition-opacity z-10">
        <button
          onClick={openSearch}
          className="w-6 h-6 flex items-center justify-center rounded bg-[#1E1A17] text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033]/15 transition-colors"
          aria-label="Search"
          title="Search (Ctrl+F)"
        >
          <Search size={12} />
        </button>
        <button
          onClick={handleClear}
          className="w-6 h-6 flex items-center justify-center rounded bg-[#1E1A17] text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033]/15 transition-colors"
          aria-label="Clear"
          title="Clear"
        >
          <Trash2 size={12} />
        </button>
        <button
          onClick={handleCopyAllOutput}
          className="w-6 h-6 flex items-center justify-center rounded bg-[#1E1A17] text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033]/15 transition-colors"
          aria-label="Copy output"
          title="Copy output"
        >
          <ClipboardCopy size={12} />
        </button>
        <button
          onClick={handleCopyLastOutput}
          className="w-6 h-6 flex items-center justify-center rounded bg-[#1E1A17] text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033]/15 transition-colors"
          aria-label="Copy last output"
          title="Copy last output"
        >
          <Clipboard size={12} />
        </button>
        {onMaximize && (
          <button
            onClick={onMaximize}
            className="w-6 h-6 flex items-center justify-center rounded bg-[#1E1A17] text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033]/15 transition-colors"
            aria-label="Toggle fullscreen"
            title="Toggle fullscreen"
          >
            <Maximize2 size={12} />
          </button>
        )}
      </div>

      {/* Search bar -- top-right overlay */}
      {searchOpen && (
        <div className="absolute top-1 right-10 flex items-center gap-1 bg-[#1E1A17] border border-[#252018] rounded px-2 py-1 z-20 shadow-lg">
          <input
            ref={searchInputRef}
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                closeSearch();
              } else if (e.key === 'Enter') {
                if (e.shiftKey) doSearchPrev();
                else doSearchNext();
              }
            }}
            placeholder="Search..."
            className="bg-transparent text-[#DCD5CC] text-xs outline-none w-36 font-mono placeholder:text-[#6B6258]"
            autoFocus
          />
          <button
            onClick={doSearchPrev}
            className="w-5 h-5 flex items-center justify-center rounded text-[#DCD5CC] hover:bg-[#252018] transition-colors"
            title="Previous (Shift+Enter)"
          >
            <ChevronUp size={12} />
          </button>
          <button
            onClick={doSearchNext}
            className="w-5 h-5 flex items-center justify-center rounded text-[#DCD5CC] hover:bg-[#252018] transition-colors"
            title="Next (Enter)"
          >
            <ChevronDown size={12} />
          </button>
          <button
            onClick={closeSearch}
            className="w-5 h-5 flex items-center justify-center rounded text-[#DCD5CC] hover:bg-[#252018] transition-colors"
            title="Close (Escape)"
          >
            <X size={12} />
          </button>
        </div>
      )}

      {/* Paste confirmation tooltip */}
      {pasteConfirm && (
        <div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 bg-[#1E1A17] border border-[#252018] rounded-md px-4 py-3 z-30 shadow-xl text-center">
          <p className="text-xs text-[#DCD5CC] font-mono mb-2">
            Paste {pasteConfirm.lineCount} lines?
          </p>
          <div className="flex items-center justify-center gap-2">
            <button
              onClick={confirmPaste}
              className="px-3 py-1 rounded text-xs bg-[#5C4033] text-[#DCD5CC] hover:bg-[#6D5040] transition-colors font-mono"
              autoFocus
            >
              Yes (Y)
            </button>
            <button
              onClick={cancelPaste}
              className="px-3 py-1 rounded text-xs bg-[#252018] text-[#DCD5CC] hover:bg-[#3E3830] transition-colors font-mono"
            >
              No (N)
            </button>
          </div>
        </div>
      )}

      {/* Paste confirmation keyboard handler */}
      {pasteConfirm && (
        <PasteConfirmKeyHandler onConfirm={confirmPaste} onCancel={cancelPaste} />
      )}

      {/* Right-click context menu */}
      {contextMenu.visible && (
        <div
          className="absolute bg-[#1E1A17] border border-[#252018] rounded shadow-lg z-40 min-w-[160px] py-1 overflow-hidden"
          style={{ left: contextMenu.x, top: contextMenu.y }}
          onClick={(e) => e.stopPropagation()}
        >
          {contextMenu.hasSelection && (
            <ContextMenuItem icon={<Copy size={13} />} label="Copy" shortcut="Ctrl+C" onClick={handleCopy} />
          )}
          <ContextMenuItem
            icon={<ClipboardPaste size={13} />}
            label="Paste"
            shortcut="Ctrl+V"
            onClick={() => {
              setContextMenu((s) => ({ ...s, visible: false }));
              handlePaste();
            }}
          />
          <ContextMenuItem
            icon={<Eraser size={13} />}
            label="Clear"
            onClick={handleClear}
          />
          <ContextMenuItem
            icon={<SquareDashedMousePointer size={13} />}
            label="Select All"
            onClick={handleSelectAll}
          />
          <div className="my-1 border-t border-[#252018]" />
          <ContextMenuItem
            icon={<Search size={13} />}
            label="Search"
            shortcut="Ctrl+F"
            onClick={() => {
              setContextMenu((s) => ({ ...s, visible: false }));
              openSearch();
            }}
          />
        </div>
      )}
    </div>
  );
}

function ContextMenuItem({
  icon,
  label,
  shortcut,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  shortcut?: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-[#DCD5CC] hover:bg-[#5C4033] transition-colors font-mono text-left"
    >
      <span className="text-[#8A7E72]">{icon}</span>
      <span className="flex-1">{label}</span>
      {shortcut && <span className="text-[#6B6258] text-[10px]">{shortcut}</span>}
    </button>
  );
}

function PasteConfirmKeyHandler({ onConfirm, onCancel }: { onConfirm: () => void; onCancel: () => void }) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'y' || e.key === 'Y' || e.key === 'Enter') {
        e.preventDefault();
        onConfirm();
      } else if (e.key === 'n' || e.key === 'N' || e.key === 'Escape') {
        e.preventDefault();
        onCancel();
      }
    };
    document.addEventListener('keydown', handler, true);
    return () => document.removeEventListener('keydown', handler, true);
  }, [onConfirm, onCancel]);
  return null;
}
