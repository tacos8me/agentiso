import { useState, useEffect, useRef, type ReactNode } from "react";
import {
  Play,
  Check,
  X,
  RotateCcw,
  ArrowRight,
  MoreHorizontal,
} from "lucide-react";
import { StatusDot } from "../common/StatusDot";
import { SparkLine } from "../common/SparkLine";
import { ConfirmDialog } from "../common/ConfirmDialog";
import { useTeamStore } from "../../stores/teams";
import type { Workspace, BoardTask } from "../../types/workspace";

function formatUptime(seconds: number | null): string {
  if (seconds === null || seconds <= 0) return "stopped";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

const priorityColors: Record<string, string> = {
  critical: "#8B4A4A",
  high: "#8B7B3A",
  medium: "#4A6B8B",
  low: "#5A524A",
};

const stateAccentColors: Record<string, string> = {
  running: "#4A7C59",
  stopped: "#8B4A4A",
  creating: "#8B7B3A",
  idle: "#4A6B8B",
};

interface ActionButtonProps {
  label: string;
  icon: ReactNode;
  onClick: (e: React.MouseEvent) => void;
  title: string;
}

function ActionButton({ label, icon, onClick, title }: ActionButtonProps) {
  return (
    <div className="group relative">
      <button
        onClick={(e) => {
          e.stopPropagation();
          onClick(e);
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            e.stopPropagation();
            onClick(e as unknown as React.MouseEvent);
          }
        }}
        title={title}
        tabIndex={0}
        className="
          sm:w-8 sm:h-8 w-10 h-10 flex items-center justify-center rounded cursor-pointer
          text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033]
          transition-colors duration-150
        "
        aria-label={label}
      >
        {icon}
      </button>
      <span className="absolute -top-7 left-1/2 -translate-x-1/2 text-[10px] bg-[#262220]
        px-1.5 py-0.5 rounded opacity-0 group-hover:opacity-100 pointer-events-none
        transition-opacity whitespace-nowrap z-10">
        {title}
      </span>
    </div>
  );
}

// ============================================================
// Workspace Card Overflow Menu
// ============================================================

function OverflowMenu({
  workspace,
  onAction,
  onClose,
}: {
  workspace: Workspace;
  onAction: (action: string) => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [confirmDestroy, setConfirmDestroy] = useState(false);

  useEffect(() => {
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [onClose]);

  return (
    <>
      <div
        ref={ref}
        className="absolute right-0 top-full mt-1 z-50 min-w-[150px] bg-[#1E1A17] border border-[#252018] rounded-lg shadow-lg py-1"
      >
        <button
          onClick={(e) => { e.stopPropagation(); onAction("terminal"); onClose(); }}
          className="w-full text-left px-3 py-1.5 text-sm text-[#DCD5CC] hover:bg-[#5C4033]/15 cursor-pointer transition-colors"
        >
          Terminal
        </button>
        <button
          onClick={(e) => { e.stopPropagation(); onAction("logs"); onClose(); }}
          className="w-full text-left px-3 py-1.5 text-sm text-[#DCD5CC] hover:bg-[#5C4033]/15 cursor-pointer transition-colors"
        >
          View Logs
        </button>
        <button
          onClick={(e) => { e.stopPropagation(); onAction("fork"); onClose(); }}
          className="w-full text-left px-3 py-1.5 text-sm text-[#DCD5CC] hover:bg-[#5C4033]/15 cursor-pointer transition-colors"
        >
          Fork
        </button>
        <button
          onClick={(e) => { e.stopPropagation(); onAction("snapshot"); onClose(); }}
          className="w-full text-left px-3 py-1.5 text-sm text-[#DCD5CC] hover:bg-[#5C4033]/15 cursor-pointer transition-colors"
        >
          Snapshot
        </button>
        <div className="border-t border-[#252018] my-0.5" />
        <button
          onClick={(e) => {
            e.stopPropagation();
            onAction(workspace.state === "running" ? "stop" : "start");
            onClose();
          }}
          className="w-full text-left px-3 py-1.5 text-sm text-[#DCD5CC] hover:bg-[#5C4033]/15 cursor-pointer transition-colors"
        >
          {workspace.state === "running" ? "Stop" : "Start"}
        </button>
        <div className="border-t border-[#252018] my-0.5" />
        <button
          onClick={(e) => { e.stopPropagation(); setConfirmDestroy(true); }}
          className="w-full text-left px-3 py-1.5 text-sm text-red-400 hover:bg-[#5C4033]/15 cursor-pointer transition-colors"
        >
          Destroy
        </button>
      </div>
      {confirmDestroy && (
        <ConfirmDialog
          title="Destroy workspace"
          message={`Destroy workspace '${workspace.name}'? This cannot be undone.`}
          confirmLabel="Destroy"
          confirmVariant="danger"
          onConfirm={() => {
            setConfirmDestroy(false);
            onAction("destroy");
            onClose();
          }}
          onCancel={() => setConfirmDestroy(false)}
        />
      )}
    </>
  );
}

// ============================================================
// Workspace Card
// ============================================================

interface WorkspaceCardProps {
  workspace: Workspace;
  isDragging: boolean;
  onClick: () => void;
  onAction?: (action: string) => void;
}

export function WorkspaceCard({ workspace, isDragging, onClick, onAction }: WorkspaceCardProps) {
  const [uptimeOffset, setUptimeOffset] = useState(0);
  const [menuOpen, setMenuOpen] = useState(false);

  useEffect(() => {
    if (workspace.state !== "running") return;
    const interval = setInterval(() => setUptimeOffset((o) => o + 1), 1000);
    return () => clearInterval(interval);
  }, [workspace.state]);

  const liveUptime =
    workspace.uptime_seconds !== null ? workspace.uptime_seconds + uptimeOffset : null;
  const shortId = workspace.id.slice(0, 8);
  const teamName = workspace.team_name ?? workspace.team_id;
  const cpuHistory = workspace.cpu_history;

  const stateColor = stateAccentColors[workspace.state] ?? "#4A6B8B";

  return (
    <div
      onClick={onClick}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") onClick(); }}
      className={`
        group rounded-lg border cursor-pointer select-none relative overflow-visible
        transition-[border-color,box-shadow] duration-150
        ${
          isDragging
            ? "border-[#5C4033] shadow-lg shadow-black/40 bg-[#2A2520]"
            : "border-[#252018] bg-[#262220] shadow-[inset_0_1px_0_0_rgba(255,255,255,0.03)] hover:border-[#5C4033] hover:shadow-[0_0_0_1px_#5C4033,0_2px_8px_rgba(92,64,51,0.15)]"
        }
      `}
    >
      {/* Left accent stripe by state */}
      <div
        className="absolute left-0 top-0 bottom-0 w-1 rounded-l-lg"
        style={{ backgroundColor: stateColor }}
        aria-hidden="true"
      />

      {/* Overflow menu button */}
      <div className="absolute top-2 right-2 opacity-0 group-hover:opacity-100 transition-opacity z-10">
        <button
          onClick={(e) => { e.stopPropagation(); setMenuOpen(!menuOpen); }}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); setMenuOpen(!menuOpen); } }}
          className="w-6 h-6 flex items-center justify-center rounded cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033]/30 transition-colors"
          aria-label="Workspace actions"
          tabIndex={0}
        >
          <MoreHorizontal size={14} />
        </button>
        {menuOpen && onAction && (
          <OverflowMenu
            workspace={workspace}
            onAction={onAction}
            onClose={() => setMenuOpen(false)}
          />
        )}
      </div>

      {/* Header: status dot + name */}
      <div className="flex items-center gap-2 pl-3 pr-8 pt-3 pb-1">
        <StatusDot status={workspace.state} size="md" />
        <span className="text-sm font-semibold text-[#DCD5CC] truncate">
          {workspace.name}
        </span>
        <span className="ml-auto text-[11px] font-mono text-[#4A4238]">
          {shortId}
        </span>
      </div>

      {/* Info rows */}
      <div className="px-3 pb-3 space-y-0.5">
        <div className="text-[13px] font-mono text-[#6B6258]">
          {workspace.network.ip}
        </div>
        <div className="text-[13px] font-mono text-[#6B6258]">
          {workspace.resources.memory_mb} MB | {workspace.resources.vcpus} vCPU
        </div>
        {teamName && (
          <div className="flex items-center gap-1.5">
            <span className="text-xs text-[#6B6258]">team:</span>
            <span className="text-xs text-[#DCD5CC] bg-[#1E1A16] px-1.5 py-0.5 rounded">
              {teamName}
            </span>
          </div>
        )}
        {workspace.state === "running" && liveUptime !== null && (
          <div className="text-xs text-[#6B6258]">
            up {formatUptime(liveUptime)}
          </div>
        )}
        {workspace.last_exec && (
          <div className="text-xs">
            <span className="text-[#6B6258]">last: </span>
            <span className={workspace.last_exec.exit_code === 0 ? "text-[#4A7C59]" : "text-[#8B4A4A]"}>
              {workspace.last_exec.exit_code === 0 ? "\u2713" : "\u2717"} exit {workspace.last_exec.exit_code}
            </span>
          </div>
        )}
        {cpuHistory && cpuHistory.length >= 2 && (
          <div className="flex items-center gap-1.5 pt-0.5">
            <SparkLine
              data={cpuHistory}
              width={80}
              height={16}
              color={workspace.state === "running" ? "#4A7C59" : "#4A6B8B"}
            />
            <span className="text-[10px] text-[#4A4238] uppercase">CPU</span>
          </div>
        )}
      </div>
    </div>
  );
}

// ============================================================
// Assign Popover (used in TaskCard)
// ============================================================

function AssignPopover({
  teamName,
  onAssign,
  onClose,
}: {
  teamName: string;
  onAssign: (agent: string | null) => void;
  onClose: () => void;
}) {
  const teams = useTeamStore((s) => s.teams);
  const ref = useRef<HTMLDivElement>(null);
  const team = teams.find((t) => t.name === teamName);
  const members = team?.members ?? [];

  useEffect(() => {
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [onClose]);

  return (
    <div
      ref={ref}
      className="absolute right-0 top-full mt-1 z-50 min-w-[140px] bg-[#1E1A16] border border-[#252018] rounded-lg shadow-xl py-1"
    >
      {members.map((m) => (
        <button
          key={m.name}
          onClick={(e) => { e.stopPropagation(); onAssign(m.name); }}
          className="w-full text-left px-3 py-1.5 text-xs text-[#DCD5CC] hover:bg-[#252018] transition-colors"
        >
          {m.name}
        </button>
      ))}
      <div className="border-t border-[#252018] my-0.5" />
      <button
        onClick={(e) => { e.stopPropagation(); onAssign(null); }}
        className="w-full text-left px-3 py-1.5 text-xs text-[#6B6258] hover:bg-[#252018] transition-colors"
      >
        Unassign
      </button>
    </div>
  );
}

// ============================================================
// Task Card
// ============================================================

interface TaskCardProps {
  task: BoardTask;
  isDragging: boolean;
}

export function TaskCard({ task, isDragging }: TaskCardProps) {
  const [showAssign, setShowAssign] = useState(false);
  const claimTask = useTeamStore((s) => s.claimTask);
  const startTask = useTeamStore((s) => s.startTask);
  const completeTask = useTeamStore((s) => s.completeTask);
  const failTask = useTeamStore((s) => s.failTask);
  const releaseTask = useTeamStore((s) => s.releaseTask);
  const depCount = task.depends_on.length;
  const teamLabel = task.team_name;

  return (
    <div
      className={`
        rounded-lg border overflow-hidden select-none card-hover
        ${
          isDragging
            ? "border-[#5C4033] shadow-lg shadow-black/40 bg-[#2A2520]"
            : "border-[#252018] hover:border-[#352C22] bg-[#262220]"
        }
      `}
    >
      {/* Priority color stripe */}
      <div
        className="h-1"
        style={{ backgroundColor: priorityColors[task.priority] || "#5A524A" }}
        aria-hidden="true"
      />

      <div className="px-3 py-2.5 space-y-1.5">
        <div className="flex items-center gap-1">
          <div className="text-sm font-medium text-[#DCD5CC] leading-tight flex-1 truncate">
            {task.title}
          </div>
          <div className="relative shrink-0">
            <button
              onClick={(e) => { e.stopPropagation(); setShowAssign(!showAssign); }}
              onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); setShowAssign(!showAssign); } }}
              title="Assign to member"
              tabIndex={0}
              className="w-6 h-6 flex items-center justify-center rounded cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033] transition-colors text-xs"
              aria-label="Assign task"
            >
              <ArrowRight size={12} />
            </button>
            {showAssign && (
              <AssignPopover
                teamName={task.team_name}
                onAssign={(agent) => {
                  claimTask(task.team_name, task.id, agent);
                  setShowAssign(false);
                }}
                onClose={() => setShowAssign(false)}
              />
            )}
          </div>
        </div>
        <div className="flex items-center gap-2">
          <StatusDot status={task.status} />
          <span className="text-xs text-[#6B6258] capitalize">
            {task.status.replace("_", " ")}
          </span>
          {task.owner && (
            <>
              <span className="text-[#4A4238]">&middot;</span>
              <span className="text-xs text-[#DCD5CC]">{task.owner}</span>
            </>
          )}
        </div>
        <div className="flex items-center gap-2">
          <span
            className="text-[11px] uppercase font-semibold px-1.5 py-0.5 rounded"
            style={{
              backgroundColor: (priorityColors[task.priority] || "#5A524A") + "30",
              color: priorityColors[task.priority] || "#5A524A",
            }}
          >
            {task.priority}
          </span>
          {depCount > 0 && (
            <span className="text-[11px] text-[#6B6258]">
              {depCount} dep{depCount !== 1 ? "s" : ""}
            </span>
          )}
          <span className="ml-auto text-[11px] text-[#4A4238]">{teamLabel}</span>
        </div>

        {/* Lifecycle action buttons */}
        {(task.status === "claimed" || task.status === "in_progress") && (
          <div className="flex items-center gap-1 pt-1.5 border-t border-[#252018]">
            {task.status === "claimed" && (
              <ActionButton
                label={`Start ${task.title}`}
                icon={<Play size={12} />}
                onClick={() => startTask(task.team_name, task.id)}
                title="Start task"
              />
            )}
            {task.status === "in_progress" && (
              <>
                <ActionButton
                  label={`Complete ${task.title}`}
                  icon={<Check size={12} />}
                  onClick={() => completeTask(task.team_name, task.id)}
                  title="Complete task"
                />
                <ActionButton
                  label={`Fail ${task.title}`}
                  icon={<X size={12} />}
                  onClick={() => failTask(task.team_name, task.id)}
                  title="Fail task"
                />
              </>
            )}
            <ActionButton
              label={`Release ${task.title}`}
              icon={<RotateCcw size={12} />}
              onClick={() => releaseTask(task.team_name, task.id)}
              title="Release task"
            />
          </div>
        )}
      </div>
    </div>
  );
}
