import { useState, useCallback } from "react";
import {
  DragDropContext,
  Draggable,
  type DropResult,
} from "@hello-pangea/dnd";
import { Plus, Package, ListTodo, LayoutList, Kanban } from "lucide-react";
import { BoardSelector } from "./BoardSelector";
import { Column } from "./Column";
import { TableView } from "./TableView";
import { WorkspaceCard, TaskCard } from "./Card";
import { CardDetail } from "./CardDetail";
import { CreateTaskDialog } from "../teams/CreateTaskDialog";
import { useTeamStore } from "../../stores/teams";
import { useUiStore } from "../../stores/ui";
import type {
  Workspace,
  WorkspaceState,
  BoardTask,
  BoardType,
  TaskStatus,
} from "../../types/workspace";
import type { CustomBoard } from "../../stores/ui";
import { WORKSPACE_COLUMNS, TASK_COLUMNS } from "../../constants/board";

function SkeletonCard() {
  return (
    <div className="rounded-lg border border-[#252018] bg-[#262220] animate-pulse">
      <div className="px-3 pt-3 pb-1 flex items-center gap-2">
        <div className="w-2 h-2 rounded-full bg-[#3E3830]" />
        <div className="h-3.5 w-24 bg-[#3E3830] rounded" />
      </div>
      <div className="px-3 pb-2 space-y-1.5">
        <div className="h-3 w-20 bg-[#3E3830] rounded" />
        <div className="h-3 w-28 bg-[#3E3830] rounded" />
      </div>
      <div className="px-2 pb-2 pt-1 border-t border-[#252018] flex gap-1">
        <div className="w-8 h-8 bg-[#3E3830] rounded" />
        <div className="w-8 h-8 bg-[#3E3830] rounded" />
      </div>
    </div>
  );
}

function EmptyState({ type }: { type: "workspaces" | "tasks" }) {
  const Icon = type === "workspaces" ? Package : ListTodo;
  return (
    <div className="flex flex-col items-center justify-center h-full py-20 text-center">
      <Icon size={24} className="text-[#4A4238] mb-2" />
      <p className="text-sm text-[#6B6258]">
        {type === "workspaces" ? "No workspaces yet" : "No tasks yet"}
      </p>
      <button className="mt-3 text-xs text-[#7A5A4A] hover:text-[#8B6B5A]">
        {type === "workspaces"
          ? "Create a workspace from the sidebar"
          : "Create a team with tasks via MCP tools"}
      </button>
    </div>
  );
}

// Announce DnD actions to screen readers
function announce(message: string) {
  const el = document.getElementById("a11y-announcer");
  if (el) el.textContent = message;
}

interface BoardProps {
  workspaces: Workspace[];
  tasks: BoardTask[];
  boardType?: BoardType;
  loading?: boolean;
  activeCustomBoard?: CustomBoard | null;
  onActiveCustomBoardChange?: (id: string | null) => void;
  onWorkspaceStateChange?: (workspaceId: string, newState: WorkspaceState) => void;
  onTaskStatusChange?: (taskId: string, newStatus: TaskStatus) => void;
  onWorkspaceAction?: (workspaceId: string, action: string) => void;
}

export function Board({
  workspaces,
  tasks,
  boardType: boardTypeProp = "workspaces",
  loading = false,
  activeCustomBoard,
  onActiveCustomBoardChange,
  onWorkspaceStateChange,
  onTaskStatusChange,
  onWorkspaceAction,
}: BoardProps) {
  const [selectedWorkspace, setSelectedWorkspace] = useState<Workspace | null>(null);
  const [showCreateTask, setShowCreateTask] = useState(false);
  const teams = useTeamStore((s) => s.teams);
  const viewMode = useUiStore((s) => s.viewMode);
  const setViewMode = useUiStore((s) => s.setViewMode);

  // Board type is driven by sidebar section (prop), custom board overrides it
  const activeBoard = boardTypeProp;
  const effectiveBoardType = activeCustomBoard ? activeCustomBoard.type : activeBoard;
  const effectiveColumns = activeCustomBoard
    ? activeCustomBoard.columns.map((c) => ({ id: c.id, label: c.label, color: c.color, values: c.values }))
    : null;

  const workspacesByState = WORKSPACE_COLUMNS.reduce(
    (acc, col) => {
      acc[col.id] = workspaces.filter((w) => w.state === col.id);
      return acc;
    },
    {} as Record<WorkspaceState, Workspace[]>,
  );

  const tasksByStatus = TASK_COLUMNS.reduce(
    (acc, col) => {
      acc[col.id] = tasks.filter((t) => t.status === col.id);
      return acc;
    },
    {} as Record<TaskStatus, BoardTask[]>,
  );

  const handleDragEnd = useCallback(
    (result: DropResult) => {
      if (!result.destination) return;
      if (result.source.droppableId === result.destination.droppableId) return;

      const destCol = effectiveBoardType === "workspaces"
        ? WORKSPACE_COLUMNS.find((c) => c.id === result.destination!.droppableId)
        : TASK_COLUMNS.find((c) => c.id === result.destination!.droppableId);

      if (effectiveBoardType === "workspaces") {
        const ws = workspaces.find((w) => w.id === result.draggableId);
        announce(`Moved ${ws?.name ?? 'workspace'} to ${destCol?.label ?? result.destination.droppableId}`);
        onWorkspaceStateChange?.(
          result.draggableId,
          result.destination.droppableId as WorkspaceState,
        );
      } else if (effectiveBoardType === "tasks") {
        const task = tasks.find((t) => t.id === result.draggableId);
        announce(`Moved ${task?.title ?? 'task'} to ${destCol?.label ?? result.destination.droppableId}`);
        onTaskStatusChange?.(
          result.draggableId,
          result.destination.droppableId as TaskStatus,
        );
      }
    },
    [effectiveBoardType, workspaces, tasks, onWorkspaceStateChange, onTaskStatusChange],
  );

  const handleCardClick = useCallback((workspace: Workspace) => {
    setSelectedWorkspace(workspace);
  }, []);

  const handleCloseDetail = useCallback(() => {
    setSelectedWorkspace(null);
  }, []);

  const resolvedSelected = selectedWorkspace
    ? workspaces.find((w) => w.id === selectedWorkspace.id) ?? selectedWorkspace
    : null;

  return (
    <div className="flex flex-col h-full bg-[#0A0A0A]">
      <div className="flex items-center flex-wrap gap-y-1">
        <BoardSelector
          activeCustomBoardId={activeCustomBoard?.id ?? null}
          onSelectCustomBoard={(board) => onActiveCustomBoardChange?.(board.id)}
          onClearCustomBoard={() => onActiveCustomBoardChange?.(null)}
        />
        {effectiveBoardType === "tasks" && teams.length > 0 && (
          <button
            onClick={() => setShowCreateTask(true)}
            className="flex items-center gap-1 mr-4 px-2.5 py-1 text-xs rounded cursor-pointer bg-[#5C4033] text-[#DCD5CC] hover:bg-[#7A5A4A] transition-colors shrink-0"
            title="Create new task"
            aria-label="Create new task"
          >
            <Plus size={12} />
            New Task
          </button>
        )}

        {/* View mode toggle */}
        <div className="flex items-center gap-0.5 ml-auto mr-4">
          <button
            onClick={() => setViewMode("table")}
            className={`w-7 h-7 flex items-center justify-center rounded cursor-pointer transition-colors ${
              viewMode === "table"
                ? "bg-[#5C4033] text-[#DCD5CC]"
                : "text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#252018]"
            }`}
            title="Table view"
            aria-label="Switch to table view"
            aria-pressed={viewMode === "table"}
          >
            <LayoutList size={14} />
          </button>
          <button
            onClick={() => setViewMode("board")}
            className={`w-7 h-7 flex items-center justify-center rounded cursor-pointer transition-colors ${
              viewMode === "board"
                ? "bg-[#5C4033] text-[#DCD5CC]"
                : "text-[#6B6258] hover:text-[#DCD5CC] hover:bg-[#252018]"
            }`}
            title="Board view"
            aria-label="Switch to board view"
            aria-pressed={viewMode === "board"}
          >
            <Kanban size={14} />
          </button>
        </div>
      </div>

      {viewMode === "table" ? (
        <TableView
          workspaces={workspaces}
          tasks={tasks}
          activeBoard={effectiveBoardType}
          onWorkspaceAction={onWorkspaceAction}
        />
      ) : (
        <DragDropContext onDragEnd={handleDragEnd}>
          <div
            className="flex-1 flex gap-5 p-5 overflow-x-auto overflow-y-hidden max-[767px]:flex-col max-[767px]:overflow-y-auto max-[767px]:overflow-x-hidden max-[767px]:pb-20"
            role="region"
            aria-label={`${effectiveBoardType === "workspaces" ? "Workspace" : "Task"} kanban board`}
          >
            {/* Loading skeletons */}
            {loading && effectiveBoardType === "workspaces" && !activeCustomBoard &&
              WORKSPACE_COLUMNS.map((col) => (
                <Column
                  key={col.id}
                  columnId={col.id}
                  title={col.label}
                  count={0}
                  accentColor={col.color}
                >
                  {col.id === "running" ? (
                    <>
                      <SkeletonCard />
                      <SkeletonCard />
                    </>
                  ) : (
                    <SkeletonCard />
                  )}
                </Column>
              ))}

            {/* Custom board columns */}
            {!loading && activeCustomBoard && effectiveColumns && effectiveBoardType === "workspaces" && (
              workspaces.length === 0 ? (
                <EmptyState type="workspaces" />
              ) : (
                effectiveColumns.map((col) => {
                  const items = workspaces.filter((w) => col.values.includes(w.state));
                  return (
                    <Column
                      key={col.id}
                      columnId={col.id}
                      title={col.label}
                      count={items.length}
                      accentColor={col.color}
                    >
                      {items.map((ws, index) => (
                        <Draggable key={ws.id} draggableId={ws.id} index={index}>
                          {(provided, snapshot) => (
                            <div
                              ref={provided.innerRef}
                              {...provided.draggableProps}
                              {...provided.dragHandleProps}
                              style={provided.draggableProps.style}
                              className={`
                                transition-transform duration-150
                                ${snapshot.isDragging ? "scale-[1.03] z-50" : ""}
                              `}
                              role="listitem"
                              aria-grabbed={snapshot.isDragging}
                              aria-label={`${ws.name}, ${ws.state}`}
                            >
                              <WorkspaceCard
                                workspace={ws}
                                isDragging={snapshot.isDragging}
                                onClick={() => handleCardClick(ws)}
                                onAction={(action) => onWorkspaceAction?.(ws.id, action)}
                              />
                            </div>
                          )}
                        </Draggable>
                      ))}
                    </Column>
                  );
                })
              )
            )}

            {!loading && activeCustomBoard && effectiveColumns && effectiveBoardType === "tasks" && (
              tasks.length === 0 ? (
                <EmptyState type="tasks" />
              ) : (
                effectiveColumns.map((col) => {
                  const items = tasks.filter((t) => col.values.includes(t.status));
                  return (
                    <Column
                      key={col.id}
                      columnId={col.id}
                      title={col.label}
                      count={items.length}
                      accentColor={col.color}
                    >
                      {items.map((task, index) => (
                        <Draggable key={task.id} draggableId={task.id} index={index}>
                          {(provided, snapshot) => (
                            <div
                              ref={provided.innerRef}
                              {...provided.draggableProps}
                              {...provided.dragHandleProps}
                              style={provided.draggableProps.style}
                              className={`
                                transition-transform duration-150
                                ${snapshot.isDragging ? "scale-[1.03] z-50" : ""}
                              `}
                              role="listitem"
                              aria-grabbed={snapshot.isDragging}
                              aria-label={`${task.title}, ${task.status}`}
                            >
                              <TaskCard
                                task={task}
                                isDragging={snapshot.isDragging}
                              />
                            </div>
                          )}
                        </Draggable>
                      ))}
                    </Column>
                  );
                })
              )
            )}

            {/* Default workspace board */}
            {!loading && !activeCustomBoard && activeBoard === "workspaces" && workspaces.length === 0 && (
              <EmptyState type="workspaces" />
            )}

            {!loading && !activeCustomBoard && activeBoard === "workspaces" && workspaces.length > 0 &&
              WORKSPACE_COLUMNS.map((col) => {
                const items = workspacesByState[col.id] || [];
                return (
                  <Column
                    key={col.id}
                    columnId={col.id}
                    title={col.label}
                    count={items.length}
                    accentColor={col.color}
                  >
                    {items.map((ws, index) => (
                      <Draggable key={ws.id} draggableId={ws.id} index={index}>
                        {(provided, snapshot) => (
                          <div
                            ref={provided.innerRef}
                            {...provided.draggableProps}
                            {...provided.dragHandleProps}
                            style={provided.draggableProps.style}
                            className={`
                              transition-transform duration-150
                              ${snapshot.isDragging ? "scale-[1.03] z-50" : ""}
                            `}
                            role="listitem"
                            aria-grabbed={snapshot.isDragging}
                            aria-label={`${ws.name}, ${ws.state}`}
                          >
                            <WorkspaceCard
                              workspace={ws}
                              isDragging={snapshot.isDragging}
                              onClick={() => handleCardClick(ws)}
                              onAction={(action) => onWorkspaceAction?.(ws.id, action)}
                            />
                          </div>
                        )}
                      </Draggable>
                    ))}
                  </Column>
                );
              })}

            {/* Default task board */}
            {!activeCustomBoard && activeBoard === "tasks" && tasks.length === 0 && (
              <EmptyState type="tasks" />
            )}

            {!activeCustomBoard && activeBoard === "tasks" && tasks.length > 0 &&
              TASK_COLUMNS.map((col) => {
                const items = tasksByStatus[col.id] || [];
                return (
                  <Column
                    key={col.id}
                    columnId={col.id}
                    title={col.label}
                    count={items.length}
                    accentColor={col.color}
                  >
                    {items.map((task, index) => (
                      <Draggable key={task.id} draggableId={task.id} index={index}>
                        {(provided, snapshot) => (
                          <div
                            ref={provided.innerRef}
                            {...provided.draggableProps}
                            {...provided.dragHandleProps}
                            style={provided.draggableProps.style}
                            className={`
                              transition-transform duration-150
                              ${snapshot.isDragging ? "scale-[1.03] z-50" : ""}
                            `}
                            role="listitem"
                            aria-grabbed={snapshot.isDragging}
                            aria-label={`${task.title}, ${task.status}`}
                          >
                            <TaskCard
                              task={task}
                              isDragging={snapshot.isDragging}
                            />
                          </div>
                        )}
                      </Draggable>
                    ))}
                  </Column>
                );
              })}
          </div>
        </DragDropContext>
      )}

      {resolvedSelected && (
        <CardDetail
          workspace={resolvedSelected}
          onClose={handleCloseDetail}
          onAction={(action) => onWorkspaceAction?.(resolvedSelected.id, action)}
        />
      )}

      {showCreateTask && teams.length > 0 && (
        <CreateTaskDialog
          teamName={teams[0].name}
          existingTasks={tasks}
          onClose={() => setShowCreateTask(false)}
        />
      )}
    </div>
  );
}
