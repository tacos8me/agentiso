import React from "react";
import { Droppable } from "@hello-pangea/dnd";

interface ColumnProps {
  columnId: string;
  title: string;
  count: number;
  accentColor?: string;
  children: React.ReactNode;
}

export function Column({
  columnId,
  title,
  count,
  accentColor = "#5A524A",
  children,
}: ColumnProps) {
  const isEmpty = React.Children.count(children) === 0;

  return (
    <div className="flex flex-col min-w-[290px] w-[310px] max-h-full max-[767px]:w-full max-[767px]:min-w-0">
      {/* S1: Background strip on column header */}
      <div className="flex items-center gap-2 bg-[var(--surface)] rounded-t-lg px-3 py-2.5 mb-1 sticky top-0 z-10">
        <h3 className="text-sm font-semibold text-[#DCD5CC] uppercase tracking-wide">
          {title}
        </h3>
        {/* S1: Colored count pill matching column status color */}
        <span
          className="text-xs font-mono px-2 py-0.5 rounded-full ml-auto transition-all duration-150"
          style={{ backgroundColor: `${accentColor}20`, color: accentColor }}
        >
          {count}
        </span>
      </div>

      <Droppable droppableId={columnId}>
        {(provided, snapshot) => (
          <div
            ref={provided.innerRef}
            {...provided.droppableProps}
            role="list"
            aria-label={`${title} workspaces`}
            className={`
              flex-1 flex flex-col gap-2 px-2 py-2 rounded-lg overflow-y-auto
              transition-colors duration-150
              ${
                snapshot.isDraggingOver
                  ? "bg-[#1E1A16] ring-1 ring-[#5C4033]/40"
                  : "bg-transparent"
              }
            `}
            style={{ minHeight: 80 }}
          >
            {children}
            {provided.placeholder}
            {/* S1: Empty column drop zone */}
            {isEmpty && !snapshot.isDraggingOver && (
              <div className="flex flex-col items-center justify-center py-6 text-center">
                <div className="empty-state-shape-lines mb-2" aria-hidden="true" />
                <p className="text-xs text-[var(--text-dim)]">No items</p>
              </div>
            )}
            {isEmpty && snapshot.isDraggingOver && (
              <div className="min-h-[120px] border-2 border-dashed border-[#5C4033]/40 rounded-lg flex items-center justify-center">
                <span className="text-xs text-[#5C4033] select-none">Drop here</span>
              </div>
            )}
          </div>
        )}
      </Droppable>
    </div>
  );
}
