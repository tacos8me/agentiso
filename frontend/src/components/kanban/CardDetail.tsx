import { useState, useEffect, useCallback, useRef, type ReactNode } from "react";
import {
  Play,
  Square,
  Trash2,
  Terminal,
  GitFork,
  Camera,
  FileText,
  RotateCw,
  Copy,
  Check,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { StatusDot } from "../common/StatusDot";
import { SparkLine } from "../common/SparkLine";
import { ConfirmDialog } from "../common/ConfirmDialog";
import type { Workspace } from "../../types/workspace";
import { useWorkspaceStore } from "../../stores/workspaces";

// --- State badge colors ---

const stateColors: Record<string, string> = {
  running: "#4A7C59",
  stopped: "#8B4A4A",
  creating: "#8B7B3A",
  idle: "#4A6B8B",
};

// --- Copy button ---

function CopyButton({ text, size = 12 }: { text: string; size?: number }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
    navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }, [text]);

  return (
    <button
      onClick={handleCopy}
      className="ml-1 inline-flex items-center text-[#6B6258] hover:text-[#DCD5CC] transition-colors"
      aria-label="Copy to clipboard"
      title="Copy"
    >
      {copied ? <Check size={size} /> : <Copy size={size} />}
    </button>
  );
}

// --- Resource bar ---

interface ResourceBarProps {
  label: string;
  used: number;
  total: number;
  unit: string;
  color: string;
}

function ResourceBar({ label, used, total, unit, color }: ResourceBarProps) {
  const pct = total > 0 ? Math.min((used / total) * 100, 100) : 0;
  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between text-xs">
        <span className="text-[#6B6258]">{label}</span>
        <span className="font-mono text-[#DCD5CC]">
          {used} / {total} {unit}
        </span>
      </div>
      <div className="h-1.5 bg-[#1E1A16] rounded-full overflow-hidden">
        <div
          className="h-full rounded-full transition-all duration-150"
          style={{ width: `${pct}%`, backgroundColor: color }}
        />
      </div>
    </div>
  );
}

// --- Detail Action Button ---

interface DetailActionProps {
  label: string;
  icon: ReactNode;
  onClick: () => void;
  variant?: "default" | "danger";
  loading?: boolean;
  disabled?: boolean;
}

function DetailAction({ label, icon, onClick, variant = "default", loading = false, disabled = false }: DetailActionProps) {
  return (
    <button
      onClick={onClick}
      disabled={disabled || loading}
      aria-label={label}
      className={`
        flex items-center gap-2 px-3 py-2 rounded-lg bg-[#1E1A17] text-sm font-medium
        transition-colors duration-150
        ${disabled || loading ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}
        ${
          variant === "danger"
            ? "text-[#8B4A4A] hover:bg-[#8B4A4A]/10"
            : "text-[#DCD5CC] hover:bg-[#5C4033]/15"
        }
      `}
    >
      {loading ? (
        <span className="animate-spin"><RotateCw size={16} /></span>
      ) : (
        icon
      )}
      <span>{label}</span>
    </button>
  );
}

// --- Format helpers ---

function formatUptime(seconds: number): string {
  if (seconds <= 0) return "stopped";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  const parts: string[] = [];
  if (h > 0) parts.push(`${h}h`);
  if (m > 0) parts.push(`${m}m`);
  parts.push(`${s}s`);
  return parts.join(" ");
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

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = (ms / 1000).toFixed(1);
  return `${s}s`;
}

function formatTimestamp(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

// ============================================================
// Advanced Section (collapsible)
// ============================================================

function AdvancedSection({
  workspace,
  onAction,
  isLoading,
}: {
  workspace: Workspace;
  onAction?: (action: string) => void;
  isLoading: boolean;
}) {
  const [open, setOpen] = useState(false);

  return (
    <section className="space-y-2">
      <button
        onClick={() => setOpen(!open)}
        className="flex items-center gap-1 text-xs font-semibold text-[#6B6258] uppercase tracking-wide hover:text-[#DCD5CC] transition-colors cursor-pointer"
      >
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        Advanced
      </button>
      {open && (
        <div className="flex items-center justify-between p-3 bg-[#1E1A17] rounded-lg">
          <span className="text-sm text-[#DCD5CC]">Internet Access</span>
          <button
            onClick={() => onAction?.(workspace.network.internet_enabled ? "disable_internet" : "enable_internet")}
            disabled={isLoading}
            className={`
              relative w-10 h-5 rounded-full transition-colors duration-150
              ${isLoading ? "opacity-50 cursor-not-allowed" : ""}
              ${workspace.network.internet_enabled ? "bg-[#4A7C59]" : "bg-[#3E3830]"}
            `}
            role="switch"
            aria-checked={workspace.network.internet_enabled}
            aria-label="Toggle internet access"
          >
            <span
              className={`
                absolute top-0.5 w-4 h-4 rounded-full bg-[#DCD5CC] transition-transform duration-150
                ${workspace.network.internet_enabled ? "translate-x-5" : "translate-x-0.5"}
              `}
            />
          </button>
        </div>
      )}
    </section>
  );
}

// ============================================================
// CardDetail -- slide-out drawer
// ============================================================

interface CardDetailProps {
  workspace: Workspace;
  onClose: () => void;
  onAction?: (action: string) => void;
}

export function CardDetail({ workspace, onClose, onAction }: CardDetailProps) {
  const drawerRef = useRef<HTMLDivElement>(null);
  const actionLoading = useWorkspaceStore((s) => s.actionLoading);
  const isLoading = actionLoading[workspace.id] ?? false;
  const [confirmDestroy, setConfirmDestroy] = useState(false);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [onClose]);

  const handleBackdropClick = useCallback(
    (e: React.MouseEvent) => {
      if (drawerRef.current && !drawerRef.current.contains(e.target as Node)) {
        onClose();
      }
    },
    [onClose],
  );

  const teamName = workspace.team_name ?? workspace.team_id;
  const diskUsed = workspace.resources.disk_used_mb;

  return (
    <div
      className="fixed inset-0 z-50 flex justify-end"
      onClick={handleBackdropClick}
    >
      <div className="absolute inset-0 bg-black/50 animate-fade-in" />
      <div
        ref={drawerRef}
        className="relative w-[400px] h-full bg-[#161210] border-l border-[#252018] overflow-y-auto animate-slide-in"
      >
        {/* Header */}
        <div className="sticky top-0 z-10 bg-[#161210] border-b border-[#252018] px-4 py-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <StatusDot status={workspace.state} size="md" />
              <h2 className="text-lg font-semibold text-[#DCD5CC]">{workspace.name}</h2>
              <CopyButton text={workspace.id} size={12} />
              {isLoading && (
                <span className="text-xs text-[#8B7B3A] animate-pulse">updating...</span>
              )}
            </div>
            <button
              onClick={onClose}
              className="w-8 h-8 flex items-center justify-center rounded-md cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#1E1A16] transition-colors"
              aria-label="Close detail drawer"
            >
              &times;
            </button>
          </div>
        </div>

        <div className="px-4 py-4 space-y-5">
          {/* Info section */}
          <section className="space-y-2">
            <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">Details</h3>
            <div className="bg-[#1E1A17] rounded-lg p-3 space-y-2">
              <div className="grid grid-cols-2 gap-2 text-sm">
                <div>
                  <span className="text-[#6B6258]">State</span>
                  <div>
                    <span
                      className="inline-block text-xs px-2 py-0.5 rounded-full font-medium capitalize"
                      style={{
                        backgroundColor: `${stateColors[workspace.state] ?? "#5A524A"}20`,
                        color: stateColors[workspace.state] ?? "#5A524A",
                      }}
                    >
                      {workspace.state}
                    </span>
                  </div>
                </div>
                <div className="group">
                  <span className="text-[#6B6258]">IP</span>
                  <div className="flex items-center">
                    <span className="text-[#DCD5CC] font-mono text-[13px]">{workspace.network.ip}</span>
                    {workspace.network.ip && <CopyButton text={workspace.network.ip} />}
                  </div>
                </div>
                <div>
                  <span className="text-[#6B6258]">Uptime</span>
                  <div className="text-[#DCD5CC] font-mono">{formatUptime(computeUptime(workspace))}</div>
                </div>
                {teamName && (
                  <div>
                    <span className="text-[#6B6258]">Team</span>
                    <div className="text-[#DCD5CC]">{teamName}</div>
                  </div>
                )}
                {workspace.forked_from && (
                  <div>
                    <span className="text-[#6B6258]">Forked from</span>
                    <div className="text-[#DCD5CC] font-mono text-[13px]">{workspace.forked_from}</div>
                  </div>
                )}
                {workspace.vsock_cid !== null && (
                  <div>
                    <span className="text-[#6B6258]">Vsock CID</span>
                    <div className="text-[#DCD5CC] font-mono text-[13px]">{workspace.vsock_cid}</div>
                  </div>
                )}
                {workspace.qemu_pid !== null && (
                  <div>
                    <span className="text-[#6B6258]">QEMU PID</span>
                    <div className="text-[#DCD5CC] font-mono text-[13px]">{workspace.qemu_pid}</div>
                  </div>
                )}
              </div>
            </div>
          </section>

          {/* Resources */}
          <section className="space-y-2">
            <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">Resources</h3>
            <div className="bg-[#1E1A17] rounded-lg p-3 space-y-2">
              <div className="grid grid-cols-2 gap-3 text-sm">
                <div>
                  <span className="text-[#6B6258]">Memory</span>
                  <div className="text-[#DCD5CC] font-mono">{workspace.resources.memory_mb} MB</div>
                </div>
                <div>
                  <span className="text-[#6B6258]">vCPUs</span>
                  <div className="text-[#DCD5CC] font-mono">{workspace.resources.vcpus}</div>
                </div>
              </div>
              {diskUsed > 0 && (
                <ResourceBar
                  label="Disk"
                  used={diskUsed}
                  total={workspace.resources.disk_mb}
                  unit="MB"
                  color="#4A6B8B"
                />
              )}
              {workspace.cpu_history.length >= 2 && (
                <div className="pt-1">
                  <div className="flex items-center justify-between mb-1">
                    <span className="text-xs text-[#6B6258]">CPU History</span>
                    <span className="text-xs font-mono text-[#DCD5CC]">
                      {workspace.cpu_history[workspace.cpu_history.length - 1]}%
                    </span>
                  </div>
                  <SparkLine
                    data={workspace.cpu_history}
                    width={352}
                    height={32}
                    color="#4A7C59"
                  />
                </div>
              )}
            </div>
          </section>

          {/* Advanced (collapsible) */}
          <AdvancedSection workspace={workspace} onAction={onAction} isLoading={isLoading} />

          {/* Last Exec */}
          {workspace.last_exec && (
            <section className="space-y-2">
              <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">
                Last Exec
              </h3>
              <div className="bg-[#1E1A16] rounded-lg p-3 space-y-2">
                <div className="flex items-center justify-between">
                  <code className="text-[13px] font-mono text-[#DCD5CC] truncate max-w-[240px]">
                    {workspace.last_exec.command}
                  </code>
                  <span
                    className={`text-xs font-mono ${
                      workspace.last_exec.exit_code === 0 ? "text-[#4A7C59]" : "text-[#8B4A4A]"
                    }`}
                  >
                    exit {workspace.last_exec.exit_code}
                  </span>
                </div>
                <div className="flex items-center gap-3 text-[11px] text-[#6B6258]">
                  <span>{formatDuration(workspace.last_exec.duration_ms)}</span>
                  <span>{formatTimestamp(workspace.last_exec.timestamp)}</span>
                </div>
              </div>
            </section>
          )}

          {/* Snapshots */}
          <section className="space-y-2">
            <div className="flex items-center justify-between">
              <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">
                Snapshots
              </h3>
              <span className="text-xs text-[#4A4238]">{workspace.snapshots.length}</span>
            </div>
            <div className="bg-[#1E1A17] rounded-lg p-3 space-y-2">
              {workspace.snapshots.length === 0 ? (
                <div className="text-xs text-[#4A4238] py-1">No snapshots</div>
              ) : (
                <div className="space-y-1">
                  {workspace.snapshots.map((snap) => (
                    <div
                      key={snap.name}
                      className="flex items-center justify-between bg-[#161210] rounded-md px-3 py-2"
                    >
                      <div>
                        <span className="text-sm text-[#DCD5CC]">{snap.name}</span>
                        <span className="ml-2 text-[10px] text-[#4A4238]">{snap.size_mb}MB</span>
                      </div>
                      <div className="flex gap-1">
                        <button
                          onClick={() => onAction?.(`restore:${snap.name}`)}
                          disabled={isLoading}
                          className="text-xs text-[#4A6B8B] hover:text-[#DCD5CC] px-2 py-1 rounded hover:bg-[#262220] transition-colors disabled:opacity-50"
                          title="Restore snapshot"
                        >
                          Restore
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </section>

          {/* Action buttons — grouped */}
          <section className="space-y-2 pb-4">
            <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">Actions</h3>
            <div className="bg-[#1E1A17] rounded-lg p-3 space-y-2">
              {/* Primary */}
              <div className="grid grid-cols-2 gap-2">
                <DetailAction label="Terminal" icon={<Terminal size={16} />} onClick={() => onAction?.("terminal")} />
                <DetailAction label="View Logs" icon={<FileText size={16} />} onClick={() => onAction?.("logs")} />
              </div>
              {/* Management */}
              <div className="grid grid-cols-2 gap-2">
                <DetailAction
                  label="Fork"
                  icon={<GitFork size={16} />}
                  onClick={() => onAction?.("fork")}
                  loading={isLoading}
                  disabled={isLoading}
                />
                <DetailAction
                  label="Snapshot"
                  icon={<Camera size={16} />}
                  onClick={() => onAction?.("snapshot")}
                  loading={isLoading}
                  disabled={isLoading}
                />
              </div>
              {/* Dangerous — separated */}
              <div className="border-t border-[#252018] pt-2 grid grid-cols-2 gap-2">
                {workspace.state === "stopped" ? (
                  <DetailAction
                    label="Start"
                    icon={<Play size={16} />}
                    onClick={() => onAction?.("start")}
                    loading={isLoading}
                    disabled={isLoading}
                  />
                ) : workspace.state === "running" ? (
                  <DetailAction
                    label="Stop"
                    icon={<Square size={16} />}
                    onClick={() => onAction?.("stop")}
                    loading={isLoading}
                    disabled={isLoading}
                  />
                ) : null}
                <DetailAction
                  label="Destroy"
                  icon={<Trash2 size={16} />}
                  onClick={() => setConfirmDestroy(true)}
                  variant="danger"
                  disabled={isLoading}
                />
              </div>
            </div>
          </section>

          {confirmDestroy && (
            <ConfirmDialog
              title="Destroy workspace"
              message={`Destroy workspace '${workspace.name}'? This cannot be undone.`}
              confirmLabel="Destroy"
              confirmVariant="danger"
              onConfirm={() => {
                setConfirmDestroy(false);
                onAction?.("destroy");
              }}
              onCancel={() => setConfirmDestroy(false)}
            />
          )}
        </div>
      </div>
    </div>
  );
}
