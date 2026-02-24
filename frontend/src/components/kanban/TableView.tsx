import { useState, useCallback, useRef, useEffect } from "react";
import { createPortal } from "react-dom";
import {
  Terminal,
  FileText,
  GitFork,
  Camera,
  Square,
  Trash2,
  MoreHorizontal,
  ChevronUp,
  ChevronDown,
} from "lucide-react";
import { StatusDot } from "../common/StatusDot";
import { useUiStore } from "../../stores/ui";
// board constants available if needed
import type { Workspace, BoardTask } from "../../types/workspace";

// ----- helpers -----

function formatUptime(seconds: number): string {
  if (seconds <= 0) return "--";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

/** Compute live uptime: use uptime_seconds if nonzero, else derive from created_at for running workspaces */
function computeUptime(workspace: Workspace): number {
  if (workspace.uptime_seconds > 0) return workspace.uptime_seconds;
  if (workspace.state === "running" || workspace.state === "idle") {
    const elapsed = Math.floor((Date.now() - new Date(workspace.created_at).getTime()) / 1000);
    return Math.max(elapsed, 0);
  }
  return 0;
}

const stateColors: Record<string, { bg: string; text: string }> = {
  creating: { bg: "#8B7B3A30", text: "#8B7B3A" },
  running: { bg: "#4A7C5930", text: "#4A7C59" },
  idle: { bg: "#4A6B8B30", text: "#4A6B8B" },
  stopped: { bg: "#8B4A4A30", text: "#8B4A4A" },
};

const taskStatusColors: Record<string, { bg: string; text: string }> = {
  pending: { bg: "#5A524A30", text: "#5A524A" },
  claimed: { bg: "#8B7B3A30", text: "#8B7B3A" },
  in_progress: { bg: "#4A6B8B30", text: "#4A6B8B" },
  completed: { bg: "#4A7C5930", text: "#4A7C59" },
  failed: { bg: "#8B4A4A30", text: "#8B4A4A" },
};

const priorityColors: Record<string, { bg: string; text: string }> = {
  critical: { bg: "#8B4A4A30", text: "#8B4A4A" },
  high: { bg: "#8B7B3A30", text: "#8B7B3A" },
  medium: { bg: "#4A6B8B30", text: "#4A6B8B" },
  low: { bg: "#5A524A30", text: "#5A524A" },
};

// ----- sort helpers -----

type SortDir = "asc" | "desc";

function compare<T>(a: T, b: T, dir: SortDir): number {
  if (a == null && b == null) return 0;
  if (a == null) return dir === "asc" ? -1 : 1;
  if (b == null) return dir === "asc" ? 1 : -1;
  if (typeof a === "number" && typeof b === "number") return dir === "asc" ? a - b : b - a;
  const sa = String(a).toLowerCase();
  const sb = String(b).toLowerCase();
  return dir === "asc" ? sa.localeCompare(sb) : sb.localeCompare(sa);
}

// ----- sort icon -----

function SortIcon({ field, active, dir }: { field: string; active: string; dir: SortDir }) {
  if (field !== active) return null;
  return dir === "asc" ? <ChevronUp size={12} className="inline ml-0.5" /> : <ChevronDown size={12} className="inline ml-0.5" />;
}

// ----- overflow menu -----

function OverflowMenu({ onAction }: { onAction: (action: string) => void }) {
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ top: number; left: number }>({ top: 0, left: 0 });

  const updatePosition = useCallback(() => {
    if (!buttonRef.current) return;
    const rect = buttonRef.current.getBoundingClientRect();
    setPos({
      top: rect.bottom + 4,
      left: rect.right,
    });
  }, []);

  // Calculate position FIRST, then open — avoids one-frame render at (0, 0)
  const handleToggle = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
    if (!open && buttonRef.current) {
      const rect = buttonRef.current.getBoundingClientRect();
      setPos({ top: rect.bottom + 4, left: rect.right });
    }
    setOpen((prev) => !prev);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function handleClick(e: MouseEvent) {
      const target = e.target as Node;
      if (
        buttonRef.current && !buttonRef.current.contains(target) &&
        dropdownRef.current && !dropdownRef.current.contains(target)
      ) {
        setOpen(false);
      }
    }
    function handleScrollOrResize() {
      updatePosition();
    }
    document.addEventListener("mousedown", handleClick);
    window.addEventListener("scroll", handleScrollOrResize, true);
    window.addEventListener("resize", handleScrollOrResize);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      window.removeEventListener("scroll", handleScrollOrResize, true);
      window.removeEventListener("resize", handleScrollOrResize);
    };
  }, [open, updatePosition]);

  const items = [
    { label: "Terminal", action: "terminal", icon: <Terminal size={12} /> },
    { label: "View Logs", action: "logs", icon: <FileText size={12} /> },
    { label: "Fork", action: "fork", icon: <GitFork size={12} /> },
    { label: "Snapshot", action: "snapshot", icon: <Camera size={12} /> },
    { label: "Stop", action: "stop", icon: <Square size={12} /> },
    { label: "Destroy", action: "destroy", icon: <Trash2 size={12} />, danger: true },
  ];

  return (
    <div className="relative">
      <button
        ref={buttonRef}
        onClick={handleToggle}
        className="w-7 h-7 flex items-center justify-center rounded cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#5C4033] transition-colors"
        aria-label="More actions"
      >
        <MoreHorizontal size={14} />
      </button>
      {open && pos.top > 0 && createPortal(
        <div
          ref={dropdownRef}
          className="fixed z-[9999] min-w-[130px] bg-[#1E1A16] border border-[#252018] rounded-lg shadow-xl py-1"
          style={{ top: pos.top, left: pos.left, transform: "translateX(-100%)" }}
        >
          {items.map((item) => (
            <button
              key={item.action}
              onClick={(e) => { e.stopPropagation(); onAction(item.action); setOpen(false); }}
              className={`w-full text-left px-3 py-1.5 text-xs flex items-center gap-2 transition-colors ${
                item.danger
                  ? "text-[#8B4A4A] hover:bg-[#8B4A4A20]"
                  : "text-[#DCD5CC] hover:bg-[#252018]"
              }`}
            >
              {item.icon}
              {item.label}
            </button>
          ))}
        </div>,
        document.body,
      )}
    </div>
  );
}

// ----- pill badge -----

function Pill({ label, bg, text }: { label: string; bg: string; text: string }) {
  return (
    <span
      className="text-[11px] uppercase font-semibold px-1.5 py-0.5 rounded whitespace-nowrap"
      style={{ backgroundColor: bg, color: text }}
    >
      {label}
    </span>
  );
}

// ==================================================================
// Workspace Table
// ==================================================================

type WsSortField = "name" | "state" | "ip" | "memory" | "vcpus" | "uptime" | "team";

function getWsSortValue(ws: Workspace, field: WsSortField): string | number | null {
  switch (field) {
    case "name": return ws.name;
    case "state": return ws.state;
    case "ip": return ws.network.ip;
    case "memory": return ws.resources.memory_mb;
    case "vcpus": return ws.resources.vcpus;
    case "uptime": return computeUptime(ws);
    case "team": return ws.team_name;
  }
}

interface WorkspaceTableProps {
  workspaces: Workspace[];
  onAction?: (workspaceId: string, action: string) => void;
}

function WorkspaceTable({ workspaces, onAction }: WorkspaceTableProps) {
  const [sortField, setSortField] = useState<WsSortField>("name");
  const [sortDir, setSortDir] = useState<SortDir>("asc");
  const openInPane = useUiStore((s) => s.openInPane);

  const toggleSort = useCallback((field: WsSortField) => {
    setSortField((prev) => {
      if (prev === field) {
        setSortDir((d) => (d === "asc" ? "desc" : "asc"));
        return prev;
      }
      setSortDir("asc");
      return field;
    });
  }, []);

  const sorted = [...workspaces].sort((a, b) =>
    compare(getWsSortValue(a, sortField), getWsSortValue(b, sortField), sortDir),
  );

  const headerCls = "px-3 py-2 text-left text-xs uppercase tracking-wider font-medium text-[#6B6258] cursor-pointer select-none hover:text-[#DCD5CC] transition-colors";

  return (
    <div className="bg-[#0A0A0A] rounded-lg border border-[#252018] overflow-hidden">
      <div className="overflow-x-auto">
        <table className="w-full text-sm" role="table">
          <thead>
            <tr className="bg-[#161210]">
              <th className={headerCls} onClick={() => toggleSort("name")}>
                Name <SortIcon field="name" active={sortField} dir={sortDir} />
              </th>
              <th className={headerCls} onClick={() => toggleSort("state")}>
                State <SortIcon field="state" active={sortField} dir={sortDir} />
              </th>
              <th className={headerCls} onClick={() => toggleSort("ip")}>
                IP <SortIcon field="ip" active={sortField} dir={sortDir} />
              </th>
              <th className={`${headerCls} max-[499px]:hidden`} onClick={() => toggleSort("memory")}>
                Memory <SortIcon field="memory" active={sortField} dir={sortDir} />
              </th>
              <th className={`${headerCls} max-[639px]:hidden`} onClick={() => toggleSort("vcpus")}>
                vCPUs <SortIcon field="vcpus" active={sortField} dir={sortDir} />
              </th>
              <th className={headerCls} onClick={() => toggleSort("uptime")}>
                Uptime <SortIcon field="uptime" active={sortField} dir={sortDir} />
              </th>
              <th className={`${headerCls} max-[767px]:hidden`} onClick={() => toggleSort("team")}>
                Team <SortIcon field="team" active={sortField} dir={sortDir} />
              </th>
              <th className="px-3 py-2 text-right text-xs uppercase tracking-wider font-medium text-[#6B6258] w-12 sticky right-0 bg-[#161210]">
                {/* actions */}
              </th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((ws, i) => {
              const sc = stateColors[ws.state] ?? stateColors.stopped;
              const teamName = ws.team_name ?? ws.team_id ?? "--";
              return (
                <tr
                  key={ws.id}
                  onClick={() => openInPane("workspace-detail", ws.name, { workspaceId: ws.id })}
                  className={`h-10 cursor-pointer border-b border-[#252018] hover:bg-[#161210] transition-colors ${
                    i % 2 === 0 ? "bg-[#0A0A0A]" : "bg-[#0E0C0A]"
                  }`}
                  role="row"
                >
                  <td className="px-3 py-1.5">
                    <span className="flex items-center gap-2">
                      <StatusDot status={ws.state} size="sm" />
                      <span className="text-[#DCD5CC] font-medium truncate max-w-[200px]">{ws.name}</span>
                    </span>
                  </td>
                  <td className="px-3 py-1.5">
                    <Pill label={ws.state} bg={sc.bg} text={sc.text} />
                  </td>
                  <td className="px-3 py-1.5 font-mono text-[#6B6258] text-xs">{ws.network.ip || "--"}</td>
                  <td className="px-3 py-1.5 font-mono text-[#6B6258] text-xs max-[499px]:hidden">{ws.resources.memory_mb} MB</td>
                  <td className="px-3 py-1.5 font-mono text-[#6B6258] text-xs max-[639px]:hidden">{ws.resources.vcpus} vCPU</td>
                  <td className="px-3 py-1.5 font-mono text-[#6B6258] text-xs">{formatUptime(computeUptime(ws))}</td>
                  <td className="px-3 py-1.5 text-xs text-[#6B6258] max-[767px]:hidden">{teamName}</td>
                  <td className="px-3 py-1.5 text-right sticky right-0" style={{ backgroundColor: i % 2 === 0 ? '#0A0A0A' : '#0E0C0A' }} onClick={(e) => e.stopPropagation()}>
                    <OverflowMenu onAction={(action) => onAction?.(ws.id, action)} />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {sorted.length === 0 && (
        <div className="py-12 text-center text-sm text-[#6B6258]">No workspaces yet</div>
      )}
    </div>
  );
}

// ==================================================================
// Task Table
// ==================================================================

type TaskSortField = "title" | "status" | "priority" | "owner" | "team" | "created";

function getTaskSortValue(task: BoardTask, field: TaskSortField): string | number | null {
  switch (field) {
    case "title": return task.title;
    case "status": return task.status;
    case "priority": {
      const order: Record<string, number> = { critical: 0, high: 1, medium: 2, low: 3 };
      return order[task.priority] ?? 4;
    }
    case "owner": return task.owner;
    case "team": return task.team_name;
    case "created": return task.created_at;
  }
}

interface TaskTableProps {
  tasks: BoardTask[];
}

function TaskTable({ tasks }: TaskTableProps) {
  const [sortField, setSortField] = useState<TaskSortField>("title");
  const [sortDir, setSortDir] = useState<SortDir>("asc");
  const openInPane = useUiStore((s) => s.openInPane);

  const toggleSort = useCallback((field: TaskSortField) => {
    setSortField((prev) => {
      if (prev === field) {
        setSortDir((d) => (d === "asc" ? "desc" : "asc"));
        return prev;
      }
      setSortDir("asc");
      return field;
    });
  }, []);

  const sorted = [...tasks].sort((a, b) =>
    compare(getTaskSortValue(a, sortField), getTaskSortValue(b, sortField), sortDir),
  );

  const headerCls = "px-3 py-2 text-left text-xs uppercase tracking-wider font-medium text-[#6B6258] cursor-pointer select-none hover:text-[#DCD5CC] transition-colors";

  return (
    <div className="bg-[#0A0A0A] rounded-lg border border-[#252018] overflow-hidden">
      <div className="overflow-x-auto">
        <table className="w-full text-sm" role="table">
          <thead>
            <tr className="bg-[#161210]">
              <th className={headerCls} onClick={() => toggleSort("title")}>
                Title <SortIcon field="title" active={sortField} dir={sortDir} />
              </th>
              <th className={headerCls} onClick={() => toggleSort("status")}>
                Status <SortIcon field="status" active={sortField} dir={sortDir} />
              </th>
              <th className={headerCls} onClick={() => toggleSort("priority")}>
                Priority <SortIcon field="priority" active={sortField} dir={sortDir} />
              </th>
              <th className={headerCls} onClick={() => toggleSort("owner")}>
                Owner <SortIcon field="owner" active={sortField} dir={sortDir} />
              </th>
              <th className={headerCls} onClick={() => toggleSort("team")}>
                Team <SortIcon field="team" active={sortField} dir={sortDir} />
              </th>
              <th className={headerCls} onClick={() => toggleSort("created")}>
                Created <SortIcon field="created" active={sortField} dir={sortDir} />
              </th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((task, i) => {
              const sc = taskStatusColors[task.status] ?? taskStatusColors.pending;
              const pc = priorityColors[task.priority] ?? priorityColors.low;
              return (
                <tr
                  key={task.id}
                  onClick={() => openInPane("team-detail", task.title, { taskId: task.id, teamName: task.team_name })}
                  className={`h-10 cursor-pointer border-b border-[#252018] hover:bg-[#161210] transition-colors ${
                    i % 2 === 0 ? "bg-[#0A0A0A]" : "bg-[#0E0C0A]"
                  }`}
                  role="row"
                >
                  <td className="px-3 py-1.5">
                    <span className="text-[#DCD5CC] font-medium truncate max-w-[280px] block">{task.title}</span>
                  </td>
                  <td className="px-3 py-1.5">
                    <Pill label={task.status.replace("_", " ")} bg={sc.bg} text={sc.text} />
                  </td>
                  <td className="px-3 py-1.5">
                    <Pill label={task.priority} bg={pc.bg} text={pc.text} />
                  </td>
                  <td className="px-3 py-1.5 text-xs text-[#6B6258]">{task.owner ?? "--"}</td>
                  <td className="px-3 py-1.5 text-xs text-[#6B6258]">{task.team_name}</td>
                  <td className="px-3 py-1.5 text-xs font-mono text-[#6B6258]">
                    {task.created_at ? new Date(task.created_at).toLocaleDateString() : "--"}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {sorted.length === 0 && (
        <div className="py-12 text-center text-sm text-[#6B6258]">No tasks yet</div>
      )}
    </div>
  );
}

// ==================================================================
// Main TableView
// ==================================================================

interface TableViewProps {
  workspaces: Workspace[];
  tasks: BoardTask[];
  activeBoard: "workspaces" | "tasks";
  onWorkspaceAction?: (workspaceId: string, action: string) => void;
}

export function TableView({ workspaces, tasks, activeBoard, onWorkspaceAction }: TableViewProps) {
  if (activeBoard === "workspaces") {
    return (
      <div className="flex-1 p-5 overflow-auto">
        <WorkspaceTable workspaces={workspaces} onAction={onWorkspaceAction} />
      </div>
    );
  }

  return (
    <div className="flex-1 p-5 overflow-auto">
      <TaskTable tasks={tasks} />
    </div>
  );
}
