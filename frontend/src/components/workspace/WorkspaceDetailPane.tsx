import { useState, useEffect, useCallback, lazy, Suspense } from 'react';
import {
  Play,
  Square,
  Trash2,
  GitFork,
  Camera,
  Globe,
  Copy,
} from 'lucide-react';
import { StatusDot } from '../common/StatusDot';
import { SparkLine } from '../common/SparkLine';
import { LoadingSkeleton } from '../common/LoadingSkeleton';
import { useWorkspaceStore } from '../../stores/workspaces';

const TerminalTabs = lazy(() =>
  import('../terminal/TerminalTabs').then((m) => ({ default: m.TerminalTabs }))
);

// --- Resource bar (matches CardDetail pattern) ---

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

// --- Format helpers ---

function formatUptime(seconds: number): string {
  if (seconds <= 0) return 'stopped';
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  const parts: string[] = [];
  if (h > 0) parts.push(`${h}h`);
  if (m > 0) parts.push(`${m}m`);
  parts.push(`${s}s`);
  return parts.join(' ');
}

/** Compute live uptime: use uptime_seconds if nonzero, else derive from created_at for running workspaces */
function computeUptimeFromWorkspace(ws: { uptime_seconds: number; state: string; created_at: string }): number {
  if (ws.uptime_seconds > 0) return ws.uptime_seconds;
  if (ws.state === 'running' || ws.state === 'idle') {
    const elapsed = Math.floor((Date.now() - new Date(ws.created_at).getTime()) / 1000);
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
  return d.toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

// --- Action button ---

interface ActionBtnProps {
  label: string;
  icon: React.ReactNode;
  onClick: () => void;
  variant?: 'default' | 'danger';
  loading?: boolean;
  disabled?: boolean;
}

function ActionBtn({
  label,
  icon,
  onClick,
  variant = 'default',
  loading = false,
  disabled = false,
}: ActionBtnProps) {
  return (
    <button
      onClick={onClick}
      disabled={disabled || loading}
      title={label}
      className={`
        flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-xs font-medium
        transition-colors duration-150
        ${disabled || loading ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'}
        ${
          variant === 'danger'
            ? 'text-[#8B4A4A] hover:bg-[#8B4A4A]/10'
            : 'text-[#DCD5CC] hover:bg-[#5C4033]/30'
        }
      `}
    >
      {loading ? (
        <span className="animate-spin">&#x21BB;</span>
      ) : (
        icon
      )}
      <span>{label}</span>
    </button>
  );
}

// --- Main component ---

interface WorkspaceDetailPaneProps {
  workspaceId: string;
}

export function WorkspaceDetailPane({ workspaceId }: WorkspaceDetailPaneProps) {
  const workspace = useWorkspaceStore((s) =>
    s.workspaces.find((w) => w.id === workspaceId)
  );
  const actionLoading = useWorkspaceStore((s) => s.actionLoading);
  const startPolling = useWorkspaceStore((s) => s.startPolling);
  const apiStartWorkspace = useWorkspaceStore((s) => s.apiStartWorkspace);
  const apiStopWorkspace = useWorkspaceStore((s) => s.apiStopWorkspace);
  const apiDestroyWorkspace = useWorkspaceStore((s) => s.apiDestroyWorkspace);
  const apiCreateSnapshot = useWorkspaceStore((s) => s.apiCreateSnapshot);
  const apiForkWorkspace = useWorkspaceStore((s) => s.apiForkWorkspace);
  const apiUpdateNetworkPolicy = useWorkspaceStore((s) => s.apiUpdateNetworkPolicy);

  const [confirmDestroy, setConfirmDestroy] = useState(false);

  useEffect(() => {
    const stop = startPolling();
    return stop;
  }, [startPolling]);

  // Reset confirm when workspace changes
  useEffect(() => {
    setConfirmDestroy(false);
  }, [workspaceId]);

  const isLoading = actionLoading[workspaceId] ?? false;

  const handleDestroy = useCallback(() => {
    if (!confirmDestroy) {
      setConfirmDestroy(true);
      return;
    }
    apiDestroyWorkspace(workspaceId);
    setConfirmDestroy(false);
  }, [confirmDestroy, apiDestroyWorkspace, workspaceId]);

  if (!workspace) {
    return (
      <div className="flex items-center justify-center h-full bg-[#0A0A0A] text-[#6B6258] text-sm">
        Workspace not found
      </div>
    );
  }

  const teamName = workspace.team_name ?? workspace.team_id;

  return (
    <div className="flex h-full bg-[#0A0A0A]">
      {/* Left: Info panel */}
      <div className="w-[40%] min-w-[280px] max-w-[420px] h-full border-r border-[#252018] overflow-y-auto">
        <div className="px-4 py-4 space-y-5">
          {/* Header */}
          <div>
            <div className="flex items-center gap-2">
              <StatusDot status={workspace.state} size="md" />
              <h2 className="text-lg font-semibold text-[#DCD5CC] truncate">
                {workspace.name}
              </h2>
              {isLoading && (
                <span className="text-xs text-[#8B7B3A] animate-pulse">
                  updating...
                </span>
              )}
            </div>
            <div className="flex items-center gap-2 mt-1">
              <span className="text-xs font-mono text-[#4A4238] truncate">
                {workspace.id}
              </span>
              <button
                onClick={() => navigator.clipboard.writeText(workspace.id)}
                className="text-[#4A4238] hover:text-[#6B6258] transition-colors"
                title="Copy ID"
              >
                <Copy size={10} />
              </button>
            </div>
            <div className="flex items-center gap-3 mt-2 text-xs text-[#6B6258]">
              <span className="capitalize">{workspace.state}</span>
              <span className="text-[#4A4238]">&middot;</span>
              <span>{formatUptime(computeUptimeFromWorkspace(workspace))}</span>
            </div>
          </div>

          {/* Actions */}
          <section>
            <div className="flex flex-wrap gap-1.5">
              {workspace.state === 'stopped' ? (
                <ActionBtn
                  label="Start"
                  icon={<Play size={13} />}
                  onClick={() => apiStartWorkspace(workspaceId)}
                  loading={isLoading}
                  disabled={isLoading}
                />
              ) : workspace.state === 'running' ? (
                <ActionBtn
                  label="Stop"
                  icon={<Square size={13} />}
                  onClick={() => apiStopWorkspace(workspaceId)}
                  loading={isLoading}
                  disabled={isLoading}
                />
              ) : null}
              <ActionBtn
                label="Fork"
                icon={<GitFork size={13} />}
                onClick={() => apiForkWorkspace(workspaceId)}
                loading={isLoading}
                disabled={isLoading}
              />
              <ActionBtn
                label="Snapshot"
                icon={<Camera size={13} />}
                onClick={() =>
                  apiCreateSnapshot(workspaceId, `snap-${Date.now()}`)
                }
                loading={isLoading}
                disabled={isLoading}
              />
              <ActionBtn
                label={
                  workspace.network.internet_enabled
                    ? 'Disable Internet'
                    : 'Enable Internet'
                }
                icon={<Globe size={13} />}
                onClick={() =>
                  apiUpdateNetworkPolicy(
                    workspaceId,
                    !workspace.network.internet_enabled
                  )
                }
                loading={isLoading}
                disabled={isLoading}
              />
              {confirmDestroy ? (
                <div className="flex items-center gap-1.5">
                  <button
                    onClick={handleDestroy}
                    className="px-2.5 py-1.5 rounded-md text-xs font-medium text-[#DCD5CC] bg-[#8B4A4A] hover:bg-[#8B4A4A]/80 transition-colors cursor-pointer"
                  >
                    Confirm Destroy
                  </button>
                  <button
                    onClick={() => setConfirmDestroy(false)}
                    className="px-2.5 py-1.5 rounded-md text-xs font-medium text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#1E1A16] transition-colors cursor-pointer"
                  >
                    Cancel
                  </button>
                </div>
              ) : (
                <ActionBtn
                  label="Destroy"
                  icon={<Trash2 size={13} />}
                  onClick={handleDestroy}
                  variant="danger"
                  loading={isLoading}
                  disabled={isLoading}
                />
              )}
            </div>
          </section>

          {/* Resources */}
          <section className="space-y-3">
            <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">
              Resources
            </h3>
            <div className="grid grid-cols-2 gap-3 text-sm">
              <div>
                <span className="text-[#6B6258]">vCPUs</span>
                <div className="text-[#DCD5CC] font-mono">
                  {workspace.resources.vcpus}
                </div>
              </div>
              <div>
                <span className="text-[#6B6258]">Memory</span>
                <div className="text-[#DCD5CC] font-mono">
                  {workspace.resources.memory_mb} MB
                </div>
              </div>
            </div>
            {workspace.resources.disk_used_mb > 0 && (
              <ResourceBar
                label="Disk"
                used={workspace.resources.disk_used_mb}
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
                  width={240}
                  height={32}
                  color="#4A7C59"
                />
              </div>
            )}
          </section>

          {/* Network */}
          <section className="space-y-2">
            <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">
              Network
            </h3>
            <div className="grid grid-cols-2 gap-2 text-sm">
              <div>
                <span className="text-[#6B6258]">IP</span>
                <div className="text-[#DCD5CC] font-mono text-[13px]">
                  {workspace.network.ip}
                </div>
              </div>
              <div>
                <span className="text-[#6B6258]">Internet</span>
                <div
                  className={
                    workspace.network.internet_enabled
                      ? 'text-[#4A7C59]'
                      : 'text-[#8B4A4A]'
                  }
                >
                  {workspace.network.internet_enabled ? 'Enabled' : 'Disabled'}
                </div>
              </div>
            </div>
          </section>

          {/* Metadata */}
          <section className="space-y-2">
            <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">
              Metadata
            </h3>
            <div className="grid grid-cols-2 gap-2 text-sm">
              <div>
                <span className="text-[#6B6258]">Created</span>
                <div className="text-[#DCD5CC] text-[13px]">
                  {formatTimestamp(workspace.created_at)}
                </div>
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
                  <div className="text-[#DCD5CC] font-mono text-[13px] truncate">
                    {workspace.forked_from}
                  </div>
                </div>
              )}
              {workspace.vsock_cid !== null && (
                <div>
                  <span className="text-[#6B6258]">Vsock CID</span>
                  <div className="text-[#DCD5CC] font-mono text-[13px]">
                    {workspace.vsock_cid}
                  </div>
                </div>
              )}
              {workspace.qemu_pid !== null && (
                <div>
                  <span className="text-[#6B6258]">QEMU PID</span>
                  <div className="text-[#DCD5CC] font-mono text-[13px]">
                    {workspace.qemu_pid}
                  </div>
                </div>
              )}
            </div>
          </section>

          {/* Last Exec */}
          {workspace.last_exec && (
            <section className="space-y-2">
              <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">
                Last Exec
              </h3>
              <div className="bg-[#1E1A16] rounded-lg p-3 space-y-2">
                <div className="flex items-center justify-between">
                  <code className="text-[13px] font-mono text-[#DCD5CC] truncate max-w-[180px]">
                    {workspace.last_exec.command}
                  </code>
                  <span
                    className={`text-xs font-mono ${
                      workspace.last_exec.exit_code === 0
                        ? 'text-[#4A7C59]'
                        : 'text-[#8B4A4A]'
                    }`}
                  >
                    exit {workspace.last_exec.exit_code}
                  </span>
                </div>
                <div className="flex items-center gap-3 text-[11px] text-[#6B6258]">
                  <span>{formatDuration(workspace.last_exec.duration_ms)}</span>
                  <span>
                    {formatTimestamp(workspace.last_exec.timestamp)}
                  </span>
                </div>
              </div>
            </section>
          )}

          {/* Snapshots */}
          <section className="space-y-2 pb-4">
            <div className="flex items-center justify-between">
              <h3 className="text-xs font-semibold text-[#6B6258] uppercase tracking-wide">
                Snapshots
              </h3>
              <span className="text-xs text-[#4A4238]">
                {workspace.snapshots.length}
              </span>
            </div>
            {workspace.snapshots.length === 0 ? (
              <div className="text-xs text-[#4A4238] py-1">No snapshots</div>
            ) : (
              <div className="space-y-1">
                {workspace.snapshots.map((snap) => (
                  <div
                    key={snap.name}
                    className="flex items-center justify-between bg-[#1E1A16] rounded-md px-3 py-2"
                  >
                    <div>
                      <span className="text-sm text-[#DCD5CC]">
                        {snap.name}
                      </span>
                      <span className="ml-2 text-[10px] text-[#4A4238]">
                        {snap.size_mb}MB
                      </span>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>
        </div>
      </div>

      {/* Right: Embedded terminal */}
      <div className="flex-1 h-full overflow-hidden">
        <Suspense fallback={<LoadingSkeleton variant="terminal" />}>
          <TerminalTabs workspaceId={workspaceId} />
        </Suspense>
      </div>
    </div>
  );
}
