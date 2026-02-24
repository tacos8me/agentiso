import type { WorkspaceState, TaskStatus } from "../types/workspace";

export const WORKSPACE_COLUMNS: { id: WorkspaceState; label: string; color: string }[] = [
  { id: "creating", label: "Creating", color: "#8B7B3A" },
  { id: "running", label: "Running", color: "#4A7C59" },
  { id: "idle", label: "Idle", color: "#4A6B8B" },
  { id: "stopped", label: "Stopped", color: "#8B4A4A" },
];

export const TASK_COLUMNS: { id: TaskStatus; label: string; color: string }[] = [
  { id: "pending", label: "Pending", color: "#5A524A" },
  { id: "claimed", label: "Claimed", color: "#8B7B3A" },
  { id: "in_progress", label: "In Progress", color: "#4A6B8B" },
  { id: "completed", label: "Completed", color: "#4A7C59" },
  { id: "failed", label: "Failed", color: "#8B4A4A" },
];
