import { useConnectionStatus } from '../../hooks/useConnectionStatus';

const statusConfig = {
  connected: {
    color: 'bg-[var(--success)]',
    pulse: false,
    label: 'Connected',
    tooltip: 'Connected to WebSocket',
  },
  disconnected: {
    color: 'bg-[var(--error)]',
    pulse: false,
    label: 'Disconnected',
    tooltip: 'WebSocket disconnected',
  },
  reconnecting: {
    color: 'bg-[var(--warning)]',
    pulse: true,
    label: 'Reconnecting',
    tooltip: 'Reconnecting to WebSocket...',
  },
} as const;

export function ConnectionStatus() {
  const status = useConnectionStatus();
  const config = statusConfig[status];

  return (
    <div className="relative group" title={config.tooltip}>
      {config.pulse && (
        <span
          className={`absolute inline-flex h-full w-full rounded-full opacity-40 animate-ping ${config.color}`}
        />
      )}
      <span className={`relative inline-flex w-2 h-2 rounded-full ${config.color}`} />

      {/* Tooltip on hover */}
      <div className="absolute left-1/2 -translate-x-1/2 top-5 hidden group-hover:block z-50">
        <div className="bg-[var(--surface-3)] border border-[var(--border)] rounded px-2 py-1 text-[10px] text-[var(--text-muted)] whitespace-nowrap shadow-lg">
          {config.label}
        </div>
      </div>
    </div>
  );
}
