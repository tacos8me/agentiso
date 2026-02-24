import { useState, useRef, useEffect, useCallback } from "react";
import { createPortal } from "react-dom";
import { Plus, X, ChevronDown } from "lucide-react";
import { useUiStore, type CustomBoard, type CustomBoardColumn } from "../../stores/ui";

import { WORKSPACE_COLUMNS, TASK_COLUMNS } from "../../constants/board";

interface BoardSelectorProps {
  activeCustomBoardId?: string | null;
  onSelectCustomBoard?: (board: CustomBoard) => void;
  onClearCustomBoard?: () => void;
}

function CreateBoardDialog({ onClose, onCreate }: {
  onClose: () => void;
  onCreate: (board: CustomBoard) => void;
}) {
  const [name, setName] = useState("");
  const [boardType, setBoardType] = useState<"workspaces" | "tasks">("workspaces");
  const [selectedValues, setSelectedValues] = useState<Set<string>>(new Set());
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const availableOptions = (boardType === "workspaces" ? WORKSPACE_COLUMNS : TASK_COLUMNS).map(
    (c) => ({ value: c.id, label: c.label, color: c.color })
  );

  const toggleValue = (val: string) => {
    setSelectedValues((prev) => {
      const next = new Set(prev);
      if (next.has(val)) next.delete(val);
      else next.add(val);
      return next;
    });
  };

  const handleCreate = () => {
    if (!name.trim() || selectedValues.size === 0) return;
    const columns: CustomBoardColumn[] = availableOptions
      .filter((opt) => selectedValues.has(opt.value))
      .map((opt) => ({
        id: opt.value,
        label: opt.label,
        color: opt.color,
        values: [opt.value],
      }));

    onCreate({
      id: `custom-${Date.now()}`,
      name: name.trim(),
      type: boardType,
      columns,
    });
    onClose();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-[#161210] border border-[#252018] rounded-lg p-5 w-[420px] shadow-2xl animate-fade-in"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Create custom board"
      >
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-sm font-semibold text-[#DCD5CC]">Create Custom Board</h3>
          <button onClick={onClose} className="text-[#6B6258] hover:text-[#DCD5CC]" aria-label="Close">
            <X size={16} />
          </button>
        </div>

        <div className="space-y-4">
          {/* Name */}
          <div>
            <label htmlFor="board-name" className="block text-xs text-[#6B6258] mb-1">Board Name</label>
            <input
              id="board-name"
              ref={inputRef}
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              className="w-full bg-[#0A0A0A] text-sm text-[#DCD5CC] border border-[#252018] rounded px-3 py-1.5 outline-none focus:border-[#5C4033] transition-colors"
              placeholder="My Board"
              onKeyDown={(e) => { if (e.key === 'Enter') handleCreate(); }}
            />
          </div>

          {/* Type */}
          <div>
            <label className="block text-xs text-[#6B6258] mb-1">Board Type</label>
            <div className="flex gap-2">
              {(["workspaces", "tasks"] as const).map((t) => (
                <button
                  key={t}
                  onClick={() => { setBoardType(t); setSelectedValues(new Set()); }}
                  className={`px-3 py-1.5 text-xs rounded transition-colors ${
                    boardType === t
                      ? "bg-[#5C4033] text-[#DCD5CC]"
                      : "bg-[#1E1A16] text-[#6B6258] hover:text-[#DCD5CC]"
                  }`}
                >
                  {t === "workspaces" ? "Workspaces" : "Tasks"}
                </button>
              ))}
            </div>
          </div>

          {/* Column selection */}
          <div>
            <label className="block text-xs text-[#6B6258] mb-1">Columns</label>
            <div className="flex flex-wrap gap-2">
              {availableOptions.map((opt) => {
                const selected = selectedValues.has(opt.value);
                return (
                  <button
                    key={opt.value}
                    onClick={() => toggleValue(opt.value)}
                    className={`flex items-center gap-1.5 px-2.5 py-1 text-xs rounded-full border transition-colors ${
                      selected
                        ? "border-[#5C4033] bg-[#5C4033]/20 text-[#DCD5CC]"
                        : "border-[#252018] text-[#6B6258] hover:text-[#DCD5CC] hover:border-[#352C22]"
                    }`}
                    aria-pressed={selected}
                  >
                    <span
                      className="w-2 h-2 rounded-full"
                      style={{ backgroundColor: opt.color }}
                    />
                    {opt.label}
                  </button>
                );
              })}
            </div>
          </div>
        </div>

        <div className="flex justify-end gap-2 mt-5">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-xs text-[#6B6258] rounded cursor-pointer hover:bg-[#1E1A16] transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleCreate}
            disabled={!name.trim() || selectedValues.size === 0}
            className="px-3 py-1.5 text-xs text-[#DCD5CC] bg-[#5C4033] rounded cursor-pointer hover:bg-[#7A5A4A] transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Create Board
          </button>
        </div>
      </div>
    </div>
  );
}

export function BoardSelector({ activeCustomBoardId, onSelectCustomBoard, onClearCustomBoard }: BoardSelectorProps) {
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [showCustomMenu, setShowCustomMenu] = useState(false);
  const customBoards = useUiStore((s) => s.customBoards);
  const addCustomBoard = useUiStore((s) => s.addCustomBoard);
  const removeCustomBoard = useUiStore((s) => s.removeCustomBoard);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [dropdownPos, setDropdownPos] = useState<{ top: number; right: number }>({ top: 0, right: 0 });

  const updateDropdownPosition = useCallback(() => {
    if (!buttonRef.current) return;
    const rect = buttonRef.current.getBoundingClientRect();
    setDropdownPos({
      top: rect.bottom + 4,
      right: window.innerWidth - rect.right,
    });
  }, []);

  // Calculate position FIRST, then open — avoids one-frame render at (0, 0)
  const handleToggleCustomMenu = useCallback(() => {
    if (!showCustomMenu && buttonRef.current) {
      const rect = buttonRef.current.getBoundingClientRect();
      setDropdownPos({
        top: rect.bottom + 4,
        right: window.innerWidth - rect.right,
      });
    }
    setShowCustomMenu((prev) => !prev);
  }, [showCustomMenu]);

  useEffect(() => {
    if (!showCustomMenu) return;
    function handleClick(e: MouseEvent) {
      const target = e.target as Node;
      if (
        buttonRef.current && !buttonRef.current.contains(target) &&
        dropdownRef.current && !dropdownRef.current.contains(target)
      ) {
        setShowCustomMenu(false);
      }
    }
    function handleScrollOrResize() {
      updateDropdownPosition();
    }
    document.addEventListener("mousedown", handleClick);
    window.addEventListener("scroll", handleScrollOrResize, true);
    window.addEventListener("resize", handleScrollOrResize);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      window.removeEventListener("scroll", handleScrollOrResize, true);
      window.removeEventListener("resize", handleScrollOrResize);
    };
  }, [showCustomMenu, updateDropdownPosition]);

  // Only render if there are custom boards or the user might want to create one
  return (
    <div className="flex items-center gap-1 h-10 border-b border-[#252018] px-4" aria-label="Board options">
      {activeCustomBoardId && (
        <button
          onClick={() => onClearCustomBoard?.()}
          className="px-3 py-1.5 text-xs text-[#6B6258] hover:text-[#DCD5CC] rounded transition-colors cursor-pointer"
        >
          Default View
        </button>
      )}

      {/* Custom boards dropdown */}
      <div className="relative ml-auto">
        <button
          ref={buttonRef}
          onClick={handleToggleCustomMenu}
          className="flex items-center gap-1 px-2 py-2 text-xs text-[#6B6258] hover:text-[#DCD5CC] rounded hover:bg-[#1E1A16] transition-colors cursor-pointer"
          aria-label="Custom boards"
          aria-haspopup="true"
          aria-expanded={showCustomMenu}
        >
          <ChevronDown size={14} />
          <span className="max-[639px]:hidden">Boards</span>
        </button>

        {showCustomMenu && dropdownPos.top > 0 && createPortal(
          <div
            ref={dropdownRef}
            className="fixed z-[9999] min-w-[200px] bg-[#161210] border border-[#252018] rounded-md shadow-xl overflow-hidden animate-fade-in"
            style={{ top: dropdownPos.top, right: dropdownPos.right }}
          >
            {customBoards.length > 0 && (
              <>
                <div className="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-[#4A4238]">
                  Custom Boards
                </div>
                {customBoards.map((board) => {
                  const isActive = activeCustomBoardId === board.id;
                  return (
                    <div
                      key={board.id}
                      className={`flex items-center justify-between px-3 py-1.5 text-xs text-[#DCD5CC] hover:bg-[#1E1A16] transition-colors cursor-pointer ${
                        isActive ? 'border-l-2 border-[#5C4033] bg-[#1E1A16]' : ''
                      }`}
                      onClick={() => { onSelectCustomBoard?.(board); setShowCustomMenu(false); }}
                      role="menuitem"
                    >
                      <span className="truncate">{board.name}</span>
                      <button
                        onClick={(e) => { e.stopPropagation(); removeCustomBoard(board.id); }}
                        className="text-[#6B6258] hover:text-[#8B4A4A] ml-2 p-0.5"
                        aria-label={`Delete ${board.name} board`}
                      >
                        <X size={10} />
                      </button>
                    </div>
                  );
                })}
                <div className="border-t border-[#252018]" />
              </>
            )}
            <button
              onClick={() => { setShowCustomMenu(false); setShowCreateDialog(true); }}
              className="flex items-center gap-2 w-full px-3 py-2 text-xs text-[#DCD5CC] hover:bg-[#5C4033] transition-colors"
            >
              <Plus size={14} />
              <span>Create Board</span>
            </button>
          </div>,
          document.body,
        )}
      </div>

      {showCreateDialog && (
        <CreateBoardDialog
          onClose={() => setShowCreateDialog(false)}
          onCreate={addCustomBoard}
        />
      )}
    </div>
  );
}
