import { create } from 'zustand';
import type { Notification } from '../types';

interface NotificationStore {
  notifications: Notification[];
  toastQueue: Notification[];

  addNotification: (notification: Omit<Notification, 'id' | 'timestamp' | 'read'>) => void;
  markRead: (id: string) => void;
  markAllRead: () => void;
  dismissToast: (id: string) => void;
  clearAll: () => void;
  getUnreadCount: () => number;
}

const mockNotifications: Notification[] = [
  {
    id: 'notif-001',
    type: 'workspace',
    title: 'Workspace started',
    message: 'worker-1 is now running at 10.99.0.2',
    timestamp: '2026-02-21T10:00:05Z',
    read: true,
  },
  {
    id: 'notif-002',
    type: 'exec',
    title: 'Exec completed',
    message: 'worker-2: exit code 1 (3.4s)',
    timestamp: '2026-02-21T10:20:00Z',
    read: false,
  },
  {
    id: 'notif-003',
    type: 'team',
    title: 'New message',
    message: 'researcher: Schema analysis complete',
    timestamp: '2026-02-21T10:15:00Z',
    read: false,
  },
];

export const useNotificationStore = create<NotificationStore>((set, get) => ({
  notifications: mockNotifications,
  toastQueue: [],

  addNotification: (notification) => {
    const full: Notification = {
      ...notification,
      id: `notif-${Date.now()}`,
      timestamp: new Date().toISOString(),
      read: false,
    };
    set((s) => ({
      notifications: [full, ...s.notifications],
      toastQueue: [...s.toastQueue, full].slice(-3),
    }));
    // Auto-dismiss toast after 5s
    setTimeout(() => {
      set((s) => ({
        toastQueue: s.toastQueue.filter((t) => t.id !== full.id),
      }));
    }, 5000);
  },

  markRead: (id) =>
    set((s) => ({
      notifications: s.notifications.map((n) =>
        n.id === id ? { ...n, read: true } : n
      ),
    })),

  markAllRead: () =>
    set((s) => ({
      notifications: s.notifications.map((n) => ({ ...n, read: true })),
    })),

  dismissToast: (id) =>
    set((s) => ({
      toastQueue: s.toastQueue.filter((t) => t.id !== id),
    })),

  clearAll: () => set({ notifications: [], toastQueue: [] }),

  getUnreadCount: () => get().notifications.filter((n) => !n.read).length,
}));
