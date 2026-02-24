// Re-export workspace types consumed via '../types' path
export type { WorkspaceState } from './types/workspace';

// API response envelope
export interface ApiResponse<T> {
  data: T;
  meta: { request_id: string };
}

export interface ApiError {
  error: { code: string; message: string };
  meta: { request_id: string };
}

// Pane system types
export type PaneType = 'kanban' | 'vault-note' | 'terminal' | 'team-chat' | 'graph' | 'workspace-detail' | 'team-detail';

export interface PaneTab {
  id: string;
  type: PaneType;
  title: string;
  data?: Record<string, unknown>;
}

export interface PaneConfig {
  id: string;
  tabs: PaneTab[];
  activeTabId: string;
}

// WebSocket message types
export interface WsClientMessage {
  type: 'subscribe' | 'unsubscribe' | 'exec_input';
  topic: string;
  data?: unknown;
}

export interface WsServerMessage {
  type: 'event' | 'exec_output' | 'error';
  topic: string;
  data: unknown;
  timestamp: string;
}

// Notification types
export interface Notification {
  id: string;
  type: 'exec' | 'team' | 'vault' | 'system' | 'workspace';
  title: string;
  message: string;
  timestamp: string;
  read: boolean;
  action_url?: string;
}
