import { useState, useRef, useEffect } from 'react';
import { Bell, Check, CheckCheck, Terminal, Users, BookOpen, Monitor, X } from 'lucide-react';
import { useNotificationStore } from '../../stores/notifications';
import type { Notification } from '../../types';

type FilterTab = 'all' | 'exec' | 'team' | 'vault' | 'system';

const filterTabs: { id: FilterTab; label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'exec', label: 'Exec' },
  { id: 'team', label: 'Teams' },
  { id: 'vault', label: 'Vault' },
  { id: 'system', label: 'System' },
];

function typeIcon(type: Notification['type']) {
  switch (type) {
    case 'exec': return <Terminal size={13} className="text-[var(--success)]" />;
    case 'team': return <Users size={13} className="text-[var(--info)]" />;
    case 'vault': return <BookOpen size={13} className="text-[#8B6BA0]" />;
    case 'workspace': return <Monitor size={13} className="text-[var(--success)]" />;
    case 'system': return <Monitor size={13} className="text-[var(--text-muted)]" />;
    default: return <Monitor size={13} className="text-[var(--text-muted)]" />;
  }
}

function formatTime(ts: string): string {
  try {
    const d = new Date(ts);
    const now = new Date();
    const diffMs = now.getTime() - d.getTime();
    if (diffMs < 60_000) return 'just now';
    if (diffMs < 3_600_000) return `${Math.floor(diffMs / 60_000)}m ago`;
    if (diffMs < 86_400_000) return `${Math.floor(diffMs / 3_600_000)}h ago`;
    return d.toLocaleDateString();
  } catch {
    return '';
  }
}

export function NotificationCenter() {
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState<FilterTab>('all');
  const ref = useRef<HTMLDivElement>(null);
  const notifications = useNotificationStore((s) => s.notifications);
  const markRead = useNotificationStore((s) => s.markRead);
  const markAllRead = useNotificationStore((s) => s.markAllRead);
  const unreadCount = useNotificationStore((s) => s.getUnreadCount());

  // Close on click outside
  useEffect(() => {
    if (!open) return;
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [open]);

  // Close on Escape
  useEffect(() => {
    if (!open) return;
    function handleKey(e: KeyboardEvent) {
      if (e.key === 'Escape') setOpen(false);
    }
    document.addEventListener('keydown', handleKey);
    return () => document.removeEventListener('keydown', handleKey);
  }, [open]);

  const filtered = filter === 'all'
    ? notifications
    : notifications.filter((n) => {
        if (filter === 'system') return n.type === 'system' || n.type === 'workspace';
        return n.type === filter;
      });

  const bellLabel = unreadCount > 0
    ? `Notifications, ${unreadCount} unread`
    : 'Notifications';

  return (
    <div className="relative" ref={ref}>
      {/* Bell button */}
      <button
        onClick={() => setOpen(!open)}
        className="relative w-8 h-8 flex items-center justify-center rounded hover:bg-[var(--surface-2)] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors"
        aria-label={bellLabel}
        aria-haspopup="dialog"
        aria-expanded={open}
      >
        <Bell size={16} />
        {unreadCount > 0 && (
          <span
            className="absolute top-1 right-1 w-3.5 h-3.5 flex items-center justify-center rounded-full bg-[var(--error)] text-[8px] font-bold text-[var(--text)]"
            aria-hidden="true"
          >
            {unreadCount > 9 ? '9+' : unreadCount}
          </span>
        )}
      </button>

      {/* Dropdown panel */}
      {open && (
        <div
          className="absolute right-0 top-10 w-[320px] min-w-[280px] max-h-[420px] flex flex-col bg-[var(--surface)] border border-[var(--border)] rounded-lg shadow-2xl z-50 overflow-hidden animate-fade-in"
          role="dialog"
          aria-label="Notification center"
        >
          {/* Header */}
          <div className="flex items-center justify-between px-3 h-10 border-b border-[var(--border)] shrink-0">
            <span className="text-xs font-semibold text-[var(--text)]">Notifications</span>
            <div className="flex items-center gap-1">
              {unreadCount > 0 && (
                <button
                  onClick={markAllRead}
                  className="flex items-center gap-1 px-2 py-1 text-[10px] text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-[var(--surface-2)] rounded transition-colors"
                  aria-label="Mark all notifications as read"
                >
                  <CheckCheck size={11} />
                  <span>Mark all read</span>
                </button>
              )}
              <button
                onClick={() => setOpen(false)}
                className="w-5 h-5 flex items-center justify-center rounded hover:bg-[var(--surface-2)] text-[var(--text-muted)] hover:text-[var(--text)] transition-colors"
                aria-label="Close notifications"
              >
                <X size={12} />
              </button>
            </div>
          </div>

          {/* Filter tabs */}
          <div className="flex items-center gap-0.5 px-2 h-8 border-b border-[var(--border)] shrink-0" role="tablist" aria-label="Notification filters">
            {filterTabs.map((tab) => (
              <button
                key={tab.id}
                role="tab"
                aria-selected={filter === tab.id}
                onClick={() => setFilter(tab.id)}
                className={`px-2 py-1 text-[10px] rounded transition-colors ${
                  filter === tab.id
                    ? 'bg-[#5C4033]/20 text-[var(--text)]'
                    : 'bg-[#1E1A17] text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-[#5C4033]/10'
                }`}
              >
                {tab.label}
              </button>
            ))}
          </div>

          {/* Notification list */}
          <div className="flex-1 overflow-y-auto" role="list" aria-label="Notifications">
            {filtered.length === 0 ? (
              <div className="px-4 py-10 text-center text-xs text-[var(--text-dim)]">
                No notifications
              </div>
            ) : (
              filtered.map((n) => (
                <div
                  key={n.id}
                  role="listitem"
                  className={`flex items-start gap-2 px-3 py-2.5 border-b border-[var(--border)] cursor-pointer transition-colors hover:bg-[var(--surface-2)] ${
                    n.read ? 'bg-[var(--bg)]' : 'bg-[var(--surface-2)]'
                  }`}
                  onClick={() => markRead(n.id)}
                >
                  <div className="mt-0.5 shrink-0">
                    {typeIcon(n.type)}
                  </div>
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-1.5">
                      <span className={`text-xs truncate ${n.read ? 'text-[var(--text-muted)]' : 'text-[var(--text)] font-medium'}`}>
                        {n.title}
                      </span>
                      {!n.read && (
                        <span className="w-2 h-2 rounded-full bg-[var(--info)] shrink-0" aria-label="Unread" />
                      )}
                    </div>
                    <div className="text-[11px] text-[var(--text-muted)] truncate mt-0.5">
                      {n.message}
                    </div>
                    <div className="text-[10px] text-[var(--text-dim)] mt-0.5">
                      {formatTime(n.timestamp)}
                    </div>
                  </div>
                  {!n.read && (
                    <button
                      onClick={(e) => { e.stopPropagation(); markRead(n.id); }}
                      className="mt-0.5 w-5 h-5 flex items-center justify-center rounded hover:bg-[var(--surface-3)] text-[var(--text-dim)] hover:text-[var(--text-muted)] transition-colors shrink-0"
                      aria-label={`Mark "${n.title}" as read`}
                    >
                      <Check size={10} />
                    </button>
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}
