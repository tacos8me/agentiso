import { X, Terminal, Users, BookOpen, Monitor, AlertCircle } from 'lucide-react';
import { useNotificationStore } from '../../stores/notifications';
import type { Notification } from '../../types';

function toastIcon(type: Notification['type']) {
  switch (type) {
    case 'exec': return <Terminal size={14} className="text-[var(--success)]" />;
    case 'team': return <Users size={14} className="text-[var(--info)]" />;
    case 'vault': return <BookOpen size={14} className="text-[#8B6BA0]" />;
    case 'workspace': return <Monitor size={14} className="text-[var(--success)]" />;
    case 'system': return <AlertCircle size={14} className="text-[var(--text-muted)]" />;
    default: return <Monitor size={14} className="text-[var(--text-muted)]" />;
  }
}

function toastBorderColor(type: Notification['type']): string {
  switch (type) {
    case 'exec': return 'border-l-[var(--success)]';
    case 'team': return 'border-l-[var(--info)]';
    case 'vault': return 'border-l-[#8B6BA0]';
    case 'workspace': return 'border-l-[var(--success)]';
    case 'system': return 'border-l-[var(--text-muted)]';
    default: return 'border-l-[var(--text-muted)]';
  }
}

export function ToastContainer() {
  const toastQueue = useNotificationStore((s) => s.toastQueue);
  const dismissToast = useNotificationStore((s) => s.dismissToast);

  if (toastQueue.length === 0) return null;

  return (
    <div className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 pointer-events-none">
      {toastQueue.slice(-3).map((toast) => (
        <div
          key={toast.id}
          className={`pointer-events-auto flex items-start gap-2 w-[300px] px-3 py-2.5 bg-[var(--surface)] border border-[var(--border)] border-l-2 ${toastBorderColor(toast.type)} rounded-lg shadow-2xl animate-[slideInRight_0.2s_ease-out]`}
        >
          <div className="mt-0.5 shrink-0">
            {toastIcon(toast.type)}
          </div>
          <div className="flex-1 min-w-0">
            <div className="text-xs font-medium text-[var(--text)] truncate">
              {toast.title}
            </div>
            <div className="text-[11px] text-[var(--text-muted)] truncate mt-0.5">
              {toast.message}
            </div>
          </div>
          <button
            onClick={() => dismissToast(toast.id)}
            className="w-5 h-5 flex items-center justify-center rounded hover:bg-[var(--surface-2)] text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors shrink-0"
          >
            <X size={11} />
          </button>
        </div>
      ))}
    </div>
  );
}
