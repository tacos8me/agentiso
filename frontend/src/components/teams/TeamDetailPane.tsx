import { useState, useEffect } from 'react';
import { Plus, Play, Check, X, RotateCcw, MessageSquare, Users as UsersIcon, ListTodo, MessageCircle } from 'lucide-react';
import { useTeamStore } from '../../stores/teams';
import { useUiStore } from '../../stores/ui';
import { StatusDot } from '../common/StatusDot';
import { CreateTaskDialog } from './CreateTaskDialog';
import type { BoardTask, TaskStatus } from '../../types/workspace';

const stateColors: Record<string, string> = {
  Creating: '#8B7B3A',
  Ready: '#4A6B8B',
  Working: '#4A7C59',
  Completing: '#8B7B3A',
  Destroyed: '#8B4A4A',
};

const statusDotMap: Record<string, string> = {
  idle: 'idle',
  busy: 'running',
  offline: 'stopped',
};

const MINI_COLUMNS: { id: TaskStatus; label: string; color: string }[] = [
  { id: 'pending', label: 'Pending', color: '#5A524A' },
  { id: 'claimed', label: 'Claimed', color: '#8B7B3A' },
  { id: 'in_progress', label: 'In Progress', color: '#4A6B8B' },
  { id: 'completed', label: 'Done', color: '#4A7C59' },
  { id: 'failed', label: 'Failed', color: '#8B4A4A' },
];

interface TeamDetailPaneProps {
  teamName: string;
}

export function TeamDetailPane({ teamName }: TeamDetailPaneProps) {
  const teams = useTeamStore((s) => s.teams);
  const tasks = useTeamStore((s) => s.tasks);
  const messages = useTeamStore((s) => s.messages);
  const fetchTeamDetail = useTeamStore((s) => s.fetchTeamDetail);
  const fetchTasks = useTeamStore((s) => s.fetchTasks);
  const fetchMessages = useTeamStore((s) => s.fetchMessages);
  const claimTask = useTeamStore((s) => s.claimTask);
  const startTask = useTeamStore((s) => s.startTask);
  const completeTask = useTeamStore((s) => s.completeTask);
  const failTask = useTeamStore((s) => s.failTask);
  const releaseTask = useTeamStore((s) => s.releaseTask);
  const openInPane = useUiStore((s) => s.openInPane);
  const [showCreateDialog, setShowCreateDialog] = useState(false);

  const team = teams.find((t) => t.name === teamName);

  useEffect(() => {
    fetchTeamDetail(teamName);
    fetchTasks(teamName);
    fetchMessages(teamName);
  }, [teamName, fetchTeamDetail, fetchTasks, fetchMessages]);

  if (!teamName) {
    return (
      <div className="flex flex-col items-center justify-center h-full gap-4 px-6">
        <div className="empty-state-shape-lines" aria-hidden="true" />
        <div className="text-center">
          <p className="text-sm text-[var(--text-muted)]">No team selected</p>
          <p className="text-xs text-[var(--text-dim)] mt-1">Select a team from the sidebar to view details.</p>
        </div>
      </div>
    );
  }

  if (!team) {
    return (
      <div className="flex flex-col items-center justify-center h-full gap-4 px-6">
        <div className="empty-state-shape-lines" aria-hidden="true" />
        <div className="text-center">
          <p className="text-sm text-[var(--text-muted)]">Team "{teamName}" not found</p>
          <p className="text-xs text-[var(--text-dim)] mt-1">The team may have been destroyed or doesn't exist yet.</p>
        </div>
      </div>
    );
  }

  const teamTasks = tasks.filter((t) => t.team_name === teamName);
  const tasksByStatus = MINI_COLUMNS.reduce(
    (acc, col) => {
      acc[col.id] = teamTasks.filter((t) => t.status === col.id);
      return acc;
    },
    {} as Record<TaskStatus, BoardTask[]>,
  );

  const createdDate = team.created_at
    ? new Date(team.created_at).toLocaleDateString(undefined, {
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      })
    : '';

  return (
    <div className="flex flex-col h-full bg-[#161210] overflow-hidden">
      {/* Header */}
      <div className="flex items-center gap-3 px-5 py-3 border-b border-[#252018] shrink-0">
        <div className="w-10 h-10 rounded-lg bg-[#5C4033]/20 flex items-center justify-center shrink-0">
          <UsersIcon size={20} className="text-[#7A5A4A]" />
        </div>
        <h3 className="text-base font-semibold text-[#DCD5CC]">{team.name}</h3>
        <span
          className="text-[10px] uppercase font-semibold px-2 py-0.5 rounded"
          style={{
            backgroundColor: (stateColors[team.state] ?? '#5A524A') + '30',
            color: stateColors[team.state] ?? '#5A524A',
          }}
        >
          {team.state}
        </span>
        <span className="text-xs text-[#6B6258]">
          {team.members.length} member{team.members.length !== 1 ? 's' : ''}
        </span>
        <button
          onClick={() => openInPane('team-chat', `Chat: ${teamName}`, { team: teamName })}
          className="ml-auto flex items-center gap-1 px-2 py-1 rounded text-[11px] text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033]/30 transition-colors cursor-pointer"
          title="Open team chat"
          aria-label={`Open chat for ${teamName}`}
        >
          <MessageSquare size={12} />
          <span>Chat</span>
        </button>
        {createdDate && (
          <span className="text-xs text-[#4A4238]">{createdDate}</span>
        )}
      </div>

      {/* Body: two-column on desktop, stacked on mobile */}
      <div className="flex-1 flex max-[767px]:flex-col overflow-hidden">
        {/* Left: Mini task board (60%) */}
        <div className="flex-[3] flex flex-col overflow-hidden border-r border-[#252018] max-[767px]:border-r-0 max-[767px]:border-b">
          <div className="flex items-center px-4 py-2 shrink-0">
            <span className="text-[11px] font-semibold uppercase tracking-wider text-[#6B6258]">
              Tasks ({teamTasks.length})
            </span>
            <button
              onClick={() => setShowCreateDialog(true)}
              className="ml-auto w-5 h-5 flex items-center justify-center rounded cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033] transition-colors"
              title="Create task"
              aria-label="Create task"
            >
              <Plus size={12} />
            </button>
          </div>
          <div className="flex-1 flex gap-2 px-3 pb-3 overflow-x-auto overflow-y-hidden">
            {MINI_COLUMNS.map((col) => {
              const items = tasksByStatus[col.id] ?? [];
              return (
                <div key={col.id} className="flex flex-col min-w-[140px] flex-1">
                  <div className="flex items-center gap-1.5 px-2 py-1.5 mb-1">
                    <div
                      className="w-1.5 h-1.5 rounded-full shrink-0"
                      style={{ backgroundColor: col.color }}
                    />
                    <span className="text-[10px] font-semibold uppercase text-[#6B6258]">
                      {col.label}
                    </span>
                    <span className="text-[10px] text-[#4A4238] ml-auto">{items.length}</span>
                  </div>
                  <div className="flex-1 overflow-y-auto space-y-1.5 px-0.5">
                    {items.length === 0 ? (
                      <div className="flex flex-col items-center justify-center py-8 text-center">
                        <ListTodo size={24} className="text-[#4A4238] mb-2" />
                        <p className="text-sm text-[#6B6258]">No tasks</p>
                      </div>
                    ) : (
                      items.map((task) => (
                        <MiniTaskCard
                          key={task.id}
                          task={task}
                          members={team.members.map((m) => m.name)}
                          onAssign={(agent) => claimTask(teamName, task.id, agent)}
                          onStart={() => startTask(teamName, task.id)}
                          onComplete={() => completeTask(teamName, task.id)}
                          onFail={() => failTask(teamName, task.id)}
                          onRelease={() => releaseTask(teamName, task.id)}
                        />
                      ))
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>

        {/* Right: Members + Messages (40%) */}
        <div className="flex-[2] flex flex-col overflow-hidden">
          {/* Members */}
          <div className="shrink-0 border-b border-[#252018]">
            <div className="px-4 py-2 text-[11px] font-semibold uppercase tracking-wider text-[#6B6258]">
              Members
            </div>
            <div className="px-3 pb-3 space-y-1.5">
              {team.members.map((m) => (
                <div
                  key={m.name}
                  className="flex items-center gap-2 px-3 py-2 bg-[#1E1A16] rounded-lg border border-[#252018]"
                >
                  <StatusDot
                    status={statusDotMap[m.status] as 'running' | 'idle' | 'stopped'}
                    size="sm"
                  />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm font-medium text-[#DCD5CC] truncate">
                      {m.name}
                    </div>
                    <div className="text-[11px] text-[#6B6258] truncate">{m.role}</div>
                  </div>
                  <div className="text-right shrink-0">
                    <div className="text-[10px] font-mono text-[#6B6258]">{m.ip}</div>
                    <div className="text-[10px] font-mono text-[#4A4238]">
                      {m.workspace_id.slice(0, 8)}
                    </div>
                  </div>
                </div>
              ))}
              {team.members.length === 0 && (
                <div className="flex flex-col items-center justify-center py-8 text-center">
                  <UsersIcon size={24} className="text-[#4A4238] mb-2" />
                  <p className="text-sm text-[#6B6258]">No members</p>
                </div>
              )}
            </div>
          </div>

          {/* Messages */}
          <div className="flex-1 flex flex-col overflow-hidden">
            <div className="px-4 py-2 text-[11px] font-semibold uppercase tracking-wider text-[#6B6258] shrink-0">
              Messages ({messages.length})
            </div>
            <div className="flex-1 overflow-y-auto px-3 pb-3 space-y-1">
              {messages.map((msg) => (
                <div
                  key={msg.id}
                  className="flex flex-col items-start max-w-[85%]"
                >
                  <div className="flex items-center gap-1.5 mb-0.5 px-1">
                    <span className="text-[10px] font-semibold text-[#6B6258]">{msg.from}</span>
                    {msg.to && (
                      <>
                        <span className="text-[#4A4238] text-[10px]">{'\u2192'}</span>
                        <span className="text-[10px] text-[#4A4238]">{msg.to}</span>
                      </>
                    )}
                    {msg.type === 'broadcast' && (
                      <span className="text-[9px] text-[#5C4033] uppercase font-semibold">
                        broadcast
                      </span>
                    )}
                    <span className="text-[10px] text-[#4A4238]">
                      {new Date(msg.timestamp).toLocaleTimeString(undefined, {
                        hour: '2-digit',
                        minute: '2-digit',
                      })}
                    </span>
                  </div>
                  <div
                    className={`px-3 py-2 rounded-lg rounded-tl-sm text-xs text-[#DCD5CC]/80 ${
                      msg.type === 'broadcast'
                        ? 'bg-[#252018] border border-[#5C4033]/30'
                        : 'bg-[#1E1A16]'
                    }`}
                  >
                    {msg.content}
                  </div>
                </div>
              ))}
              {messages.length === 0 && (
                <div className="flex flex-col items-center justify-center py-8 text-center">
                  <MessageCircle size={24} className="text-[#4A4238] mb-2" />
                  <p className="text-sm text-[#6B6258]">No messages yet</p>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>

      {showCreateDialog && (
        <CreateTaskDialog
          teamName={teamName}
          existingTasks={teamTasks}
          onClose={() => setShowCreateDialog(false)}
        />
      )}
    </div>
  );
}

// Compact task card for the mini board
function MiniTaskCard({
  task,
  members,
  onAssign,
  onStart,
  onComplete,
  onFail,
  onRelease,
}: {
  task: BoardTask;
  members: string[];
  onAssign: (agent: string | null) => void;
  onStart: () => void;
  onComplete: () => void;
  onFail: () => void;
  onRelease: () => void;
}) {
  return (
    <div className="bg-[#1E1A16] border border-[#252018] rounded px-2 py-1.5 group cursor-default">
      <div className="text-xs text-[#DCD5CC] leading-tight truncate">{task.title}</div>
      <div className="flex items-center gap-1 mt-1">
        {task.owner ? (
          <span className="text-[10px] text-[#6B6258] truncate">{task.owner}</span>
        ) : (
          <span className="text-[10px] text-[#4A4238] italic">unassigned</span>
        )}
        <div className="ml-auto flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity duration-150">
          {/* Lifecycle action buttons */}
          {task.status === 'claimed' && (
            <>
              <button
                onClick={onStart}
                className="w-5 h-5 flex items-center justify-center rounded cursor-pointer text-[#4A7C59] hover:bg-[#4A7C59]/20 transition-colors"
                title="Start task"
                aria-label={`Start ${task.title}`}
              >
                <Play size={10} />
              </button>
              <button
                onClick={onRelease}
                className="w-5 h-5 flex items-center justify-center rounded cursor-pointer text-[#6B6258] hover:bg-[#5A524A]/20 transition-colors"
                title="Release task"
                aria-label={`Release ${task.title}`}
              >
                <RotateCcw size={10} />
              </button>
            </>
          )}
          {task.status === 'in_progress' && (
            <>
              <button
                onClick={onComplete}
                className="w-5 h-5 flex items-center justify-center rounded cursor-pointer text-[#4A7C59] hover:bg-[#4A7C59]/20 transition-colors"
                title="Complete task"
                aria-label={`Complete ${task.title}`}
              >
                <Check size={10} />
              </button>
              <button
                onClick={onFail}
                className="w-5 h-5 flex items-center justify-center rounded cursor-pointer text-[#8B4A4A] hover:bg-[#8B4A4A]/20 transition-colors"
                title="Fail task"
                aria-label={`Fail ${task.title}`}
              >
                <X size={10} />
              </button>
              <button
                onClick={onRelease}
                className="w-5 h-5 flex items-center justify-center rounded cursor-pointer text-[#6B6258] hover:bg-[#5A524A]/20 transition-colors"
                title="Release task"
                aria-label={`Release ${task.title}`}
              >
                <RotateCcw size={10} />
              </button>
            </>
          )}
          {/* Assign dropdown */}
          <select
            className="text-[10px] bg-transparent text-[#6B6258] border-none outline-none cursor-pointer"
            value={task.owner ?? ''}
            onChange={(e) => onAssign(e.target.value || null)}
            onClick={(e) => e.stopPropagation()}
            aria-label={`Assign ${task.title}`}
            tabIndex={0}
          >
            <option value="">Unassign</option>
            {members.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
        </div>
      </div>
    </div>
  );
}
