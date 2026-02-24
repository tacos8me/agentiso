import { create } from 'zustand';
import { api } from '../api/client';
import { execStream } from '../api/workspaces';

export interface TerminalSession {
  id: string;
  workspaceId: string;
  workspaceName: string;
  history: string[];
  historyIndex: number;
  outputBuffer: string;
  isExecuting: boolean;
  /** Active SSE abort handle — allows Ctrl+C to cancel streaming exec */
  _streamAbort?: { abort: () => void } | null;
}

interface BackgroundExecResponse {
  job_id: string;
}

interface BackgroundPollResponse {
  status: string;
  stdout?: string;
  stderr?: string;
  exit_code?: number;
}

interface TerminalStore {
  sessions: TerminalSession[];

  createTerminal: (workspaceId: string, workspaceName: string) => string;
  destroyTerminal: (id: string) => void;
  destroyTerminalsForWorkspace: (workspaceId: string) => void;
  getSessionsForWorkspace: (workspaceId: string) => TerminalSession[];
  addToHistory: (id: string, command: string) => void;
  setHistoryIndex: (id: string, index: number) => void;
  appendOutput: (id: string, text: string) => void;
  setExecuting: (id: string, executing: boolean) => void;
  executeCommand: (id: string, command: string) => void;
  executeBackground: (id: string, command: string) => Promise<void>;
  cancelStream: (id: string) => void;
}

const HISTORY_STORAGE_KEY = 'agentiso-terminal-history';

function loadHistory(workspaceId: string): string[] {
  try {
    const stored = localStorage.getItem(`${HISTORY_STORAGE_KEY}:${workspaceId}`);
    if (stored) return JSON.parse(stored);
  } catch { /* ignore */ }
  return [];
}

function saveHistory(workspaceId: string, history: string[]) {
  try {
    // Keep last 200 commands
    const trimmed = history.slice(-200);
    localStorage.setItem(`${HISTORY_STORAGE_KEY}:${workspaceId}`, JSON.stringify(trimmed));
  } catch { /* ignore */ }
}

let nextTerminalId = 1;

export const useTerminalStore = create<TerminalStore>((set, get) => ({
  sessions: [],

  createTerminal: (workspaceId, workspaceName) => {
    const id = `term-${nextTerminalId++}`;
    const history = loadHistory(workspaceId);
    const session: TerminalSession = {
      id,
      workspaceId,
      workspaceName,
      history,
      historyIndex: -1,
      outputBuffer: '',
      isExecuting: false,
      _streamAbort: null,
    };
    set((s) => ({ sessions: [...s.sessions, session] }));
    return id;
  },

  destroyTerminal: (id) =>
    set((s) => ({ sessions: s.sessions.filter((t) => t.id !== id) })),

  destroyTerminalsForWorkspace: (workspaceId) =>
    set((s) => ({ sessions: s.sessions.filter((t) => t.workspaceId !== workspaceId) })),

  getSessionsForWorkspace: (workspaceId) =>
    get().sessions.filter((t) => t.workspaceId === workspaceId),

  addToHistory: (id, command) =>
    set((s) => ({
      sessions: s.sessions.map((t) => {
        if (t.id !== id) return t;
        const history = [...t.history, command];
        saveHistory(t.workspaceId, history);
        return { ...t, history, historyIndex: -1 };
      }),
    })),

  setHistoryIndex: (id, index) =>
    set((s) => ({
      sessions: s.sessions.map((t) =>
        t.id === id ? { ...t, historyIndex: index } : t
      ),
    })),

  appendOutput: (id, text) =>
    set((s) => ({
      sessions: s.sessions.map((t) =>
        t.id === id ? { ...t, outputBuffer: t.outputBuffer + text } : t
      ),
    })),

  setExecuting: (id, executing) =>
    set((s) => ({
      sessions: s.sessions.map((t) =>
        t.id === id ? { ...t, isExecuting: executing } : t
      ),
    })),

  /**
   * Execute a command using SSE streaming for live output.
   * Falls back to regular exec if the stream endpoint fails to connect.
   */
  executeCommand: (id, command) => {
    const session = get().sessions.find((t) => t.id === id);
    if (!session) return;

    get().setExecuting(id, true);

    const handle = execStream(
      session.workspaceId,
      command,
      (event) => {
        switch (event.type) {
          case 'stdout':
            get().appendOutput(id, event.data);
            break;
          case 'stderr':
            get().appendOutput(id, `\x1b[33m${event.data}\x1b[0m`);
            break;
          case 'exit': {
            const exitCode = event.exit_code ?? -1;
            const exitColor = exitCode === 0 ? '\x1b[32m' : '\x1b[31m';
            get().appendOutput(id, `${exitColor}[exit ${exitCode}]\x1b[0m\n`);
            get().setExecuting(id, false);
            // Clear stream abort handle
            set((s) => ({
              sessions: s.sessions.map((t) =>
                t.id === id ? { ...t, _streamAbort: null } : t
              ),
            }));
            break;
          }
          case 'error':
            get().appendOutput(id, `\x1b[31mError: ${event.data}\x1b[0m\n`);
            get().setExecuting(id, false);
            set((s) => ({
              sessions: s.sessions.map((t) =>
                t.id === id ? { ...t, _streamAbort: null } : t
              ),
            }));
            break;
          case 'started':
            // Stream connected, job started
            break;
        }
      },
      { timeout_secs: 120 },
    );

    // Store the abort handle so Ctrl+C can cancel
    set((s) => ({
      sessions: s.sessions.map((t) =>
        t.id === id ? { ...t, _streamAbort: handle } : t
      ),
    }));
  },

  cancelStream: (id) => {
    const session = get().sessions.find((t) => t.id === id);
    if (session?._streamAbort) {
      session._streamAbort.abort();
      get().appendOutput(id, '\x1b[33m^C (stream cancelled)\x1b[0m\n');
      get().setExecuting(id, false);
      set((s) => ({
        sessions: s.sessions.map((t) =>
          t.id === id ? { ...t, _streamAbort: null } : t
        ),
      }));
    }
  },

  executeBackground: async (id, command) => {
    const session = get().sessions.find((t) => t.id === id);
    if (!session) return;

    get().setExecuting(id, true);

    try {
      const bgResult = await api.post<BackgroundExecResponse>(
        `/workspaces/${session.workspaceId}/exec/background`,
        { command }
      );

      get().appendOutput(id, `\x1b[34m[background job ${bgResult.job_id}]\x1b[0m\n`);

      // Poll for completion
      const pollInterval = 1000;
      const maxPolls = 300; // 5 min max
      let polls = 0;

      const poll = async () => {
        while (polls < maxPolls) {
          polls++;
          await new Promise((r) => setTimeout(r, pollInterval));

          try {
            const pollResult = await api.get<BackgroundPollResponse>(
              `/workspaces/${session.workspaceId}/exec/background/${bgResult.job_id}`
            );

            if (pollResult.status === 'completed' || pollResult.status === 'failed') {
              let output = '';
              if (pollResult.stdout) output += pollResult.stdout;
              if (pollResult.stderr) output += pollResult.stderr;
              if (!output.endsWith('\n') && output.length > 0) output += '\n';

              if (pollResult.exit_code !== undefined) {
                const exitColor = pollResult.exit_code === 0 ? '\x1b[32m' : '\x1b[31m';
                output += `${exitColor}[bg ${bgResult.job_id} exit ${pollResult.exit_code}]\x1b[0m\n`;
              }

              get().appendOutput(id, output);
              break;
            }
          } catch {
            get().appendOutput(id, `\x1b[31m[bg ${bgResult.job_id} poll error]\x1b[0m\n`);
            break;
          }
        }
      };

      // Fire and forget the polling
      poll().finally(() => get().setExecuting(id, false));
      // Return early so the user can keep typing
      return;
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      get().appendOutput(id, `\x1b[31mError: ${msg}\x1b[0m\n`);
      get().setExecuting(id, false);
    }
  },
}));
