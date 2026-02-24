import { useState, useCallback, useRef, useEffect } from "react";
import { ChevronRight, ChevronDown, FileText, Folder, FolderOpen, Plus, Trash2, Pencil, Loader2 } from "lucide-react";
import type { VaultTreeNode, VaultFolder, VaultNote } from "../../types/vault";
import { useVaultStore } from "../../stores/vault";

interface VaultTreeProps {
  onNoteSelect: (note: VaultNote) => void;
  selectedPath?: string;
}

interface ContextMenu {
  x: number;
  y: number;
  node: VaultTreeNode;
}

function countNotes(nodes: VaultTreeNode[]): number {
  let count = 0;
  for (const node of nodes) {
    if (node.type === "note") count++;
    else count += countNotes(node.folder.children);
  }
  return count;
}

function FolderItem({
  folder,
  depth,
  onNoteSelect,
  selectedPath,
  onContextMenu,
}: {
  folder: VaultFolder;
  depth: number;
  onNoteSelect: (note: VaultNote) => void;
  selectedPath?: string;
  onContextMenu: (e: React.MouseEvent, node: VaultTreeNode) => void;
}) {
  const [expanded, setExpanded] = useState(true);
  const noteCount = countNotes(folder.children);

  return (
    <div>
      <button
        className="flex items-center w-full text-left px-2 py-0.5 hover:bg-[#1E1A16] rounded group"
        style={{ paddingLeft: `${depth * 12 + 8}px`, height: "28px", fontSize: "13px" }}
        onClick={() => setExpanded(!expanded)}
        onContextMenu={(e) => onContextMenu(e, { type: "folder", folder })}
      >
        <span className="mr-1 text-[#6B6258] flex-shrink-0">
          {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </span>
        <span className="mr-1.5 text-[#5C4033] flex-shrink-0">
          {expanded ? <FolderOpen size={14} /> : <Folder size={14} />}
        </span>
        <span className="text-[#DCD5CC] truncate flex-1">{folder.name}</span>
        <span className="text-[#4A4238] text-xs ml-auto opacity-0 group-hover:opacity-100">
          {noteCount}
        </span>
      </button>
      {expanded && (
        <div>
          {folder.children.map((child) => (
            <TreeNode
              key={child.type === "folder" ? child.folder.path : child.note.path}
              node={child}
              depth={depth + 1}
              onNoteSelect={onNoteSelect}
              selectedPath={selectedPath}
              onContextMenu={onContextMenu}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function NoteItem({
  note,
  depth,
  isSelected,
  onNoteSelect,
  onContextMenu,
}: {
  note: VaultNote;
  depth: number;
  isSelected: boolean;
  onNoteSelect: (note: VaultNote) => void;
  onContextMenu: (e: React.MouseEvent, node: VaultTreeNode) => void;
}) {
  return (
    <button
      className={`flex items-center w-full text-left px-2 py-0.5 rounded ${
        isSelected
          ? "bg-[#5C4033]/20 text-[#DCD5CC]"
          : "hover:bg-[#1E1A16] text-[#6B6258]"
      }`}
      style={{ paddingLeft: `${depth * 12 + 8}px`, height: "28px", fontSize: "13px" }}
      onClick={() => onNoteSelect(note)}
      onContextMenu={(e) => onContextMenu(e, { type: "note", note })}
    >
      <span className="mr-1.5 flex-shrink-0">
        <FileText size={14} className={isSelected ? "text-[#5C4033]" : ""} />
      </span>
      <span className="truncate">{note.name}</span>
      {note.tags.length > 0 && (
        <span className="ml-auto text-[#4A4238] text-[10px]">
          {note.tags.length}
        </span>
      )}
    </button>
  );
}

function TreeNode({
  node,
  depth,
  onNoteSelect,
  selectedPath,
  onContextMenu,
}: {
  node: VaultTreeNode;
  depth: number;
  onNoteSelect: (note: VaultNote) => void;
  selectedPath?: string;
  onContextMenu: (e: React.MouseEvent, node: VaultTreeNode) => void;
}) {
  if (node.type === "folder") {
    return (
      <FolderItem
        folder={node.folder}
        depth={depth}
        onNoteSelect={onNoteSelect}
        selectedPath={selectedPath}
        onContextMenu={onContextMenu}
      />
    );
  }
  return (
    <NoteItem
      note={node.note}
      depth={depth}
      isSelected={node.note.path === selectedPath}
      onNoteSelect={onNoteSelect}
      onContextMenu={onContextMenu}
    />
  );
}

export function VaultTree({ onNoteSelect, selectedPath }: VaultTreeProps) {
  const tree = useVaultStore((s) => s.tree);
  const treeLoading = useVaultStore((s) => s.treeLoading);
  const loadTree = useVaultStore((s) => s.loadTree);
  const createNote = useVaultStore((s) => s.createNote);
  const deleteNoteAction = useVaultStore((s) => s.deleteNoteAction);
  const renameNote = useVaultStore((s) => s.renameNote);

  const [contextMenu, setContextMenu] = useState<ContextMenu | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const contextMenuRef = useRef<HTMLDivElement>(null);
  const renameInputRef = useRef<HTMLInputElement>(null);

  // Load tree on mount
  useEffect(() => {
    loadTree();
  }, [loadTree]);

  const handleContextMenu = useCallback((e: React.MouseEvent, node: VaultTreeNode) => {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY, node });
  }, []);

  useEffect(() => {
    function handleClick(e: MouseEvent) {
      if (contextMenuRef.current && !contextMenuRef.current.contains(e.target as Node)) {
        setContextMenu(null);
      }
    }
    if (contextMenu) {
      document.addEventListener("mousedown", handleClick);
      return () => document.removeEventListener("mousedown", handleClick);
    }
  }, [contextMenu]);

  // Focus rename input when it appears
  useEffect(() => {
    if (renaming && renameInputRef.current) {
      renameInputRef.current.focus();
      renameInputRef.current.select();
    }
  }, [renaming]);

  const handleNewNote = useCallback((parentFolder?: string) => {
    const name = prompt("Note name (e.g. my-note.md):");
    if (!name) return;
    const fullPath = parentFolder ? `${parentFolder}/${name}` : name;
    const path = fullPath.endsWith('.md') ? fullPath : `${fullPath}.md`;
    createNote(path);
    setContextMenu(null);
  }, [createNote]);

  const handleDelete = useCallback((path: string) => {
    if (confirm(`Delete "${path}"?`)) {
      deleteNoteAction(path);
    }
    setContextMenu(null);
  }, [deleteNoteAction]);

  const handleRenameStart = useCallback((path: string) => {
    setRenaming(path);
    setRenameValue(path);
    setContextMenu(null);
  }, []);

  const handleRenameSubmit = useCallback(() => {
    if (renaming && renameValue && renaming !== renameValue) {
      const to = renameValue.endsWith('.md') ? renameValue : `${renameValue}.md`;
      renameNote(renaming, to);
    }
    setRenaming(null);
  }, [renaming, renameValue, renameNote]);

  const menuItems = (() => {
    if (!contextMenu) return [];
    const node = contextMenu.node;
    if (node.type === "folder") {
      return [
        { label: "New Note", icon: Plus, action: () => handleNewNote(node.folder.path) },
        { label: "Rename", icon: Pencil, action: () => handleRenameStart(node.folder.path) },
      ];
    }
    return [
      { label: "Rename", icon: Pencil, action: () => handleRenameStart(node.note.path) },
      { label: "Delete", icon: Trash2, action: () => handleDelete(node.note.path) },
    ];
  })();

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-3 py-2 border-b border-[#252018]">
        <span className="text-xs font-medium text-[#6B6258] uppercase tracking-wider">
          Vault
        </span>
        <button
          className="text-[#6B6258] hover:text-[#DCD5CC] p-0.5 rounded hover:bg-[#1E1A16]"
          title="New Note"
          onClick={() => handleNewNote()}
        >
          <Plus size={14} />
        </button>
      </div>
      <div className="flex-1 overflow-y-auto py-1">
        {treeLoading ? (
          <div className="flex items-center justify-center py-8 text-[#6B6258]">
            <Loader2 size={16} className="animate-spin mr-2" />
            <span className="text-xs">Loading vault...</span>
          </div>
        ) : tree.length === 0 ? (
          <div className="text-center py-8 text-[#4A4238] text-xs">
            No notes in vault
          </div>
        ) : (
          tree.map((node) => (
            <TreeNode
              key={node.type === "folder" ? node.folder.path : node.note.path}
              node={node}
              depth={0}
              onNoteSelect={onNoteSelect}
              selectedPath={selectedPath}
              onContextMenu={handleContextMenu}
            />
          ))
        )}
      </div>

      {/* Rename dialog */}
      {renaming && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
          <div className="bg-[#1E1A16] border border-[#252018] rounded-lg p-4 w-80">
            <h3 className="text-sm text-[#DCD5CC] mb-3">Rename</h3>
            <input
              ref={renameInputRef}
              className="w-full bg-[#0A0A0A] text-sm text-[#DCD5CC] border border-[#252018] rounded px-2 py-1.5 outline-none focus:border-[#5C4033]"
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleRenameSubmit();
                if (e.key === 'Escape') setRenaming(null);
              }}
            />
            <div className="flex justify-end gap-2 mt-3">
              <button className="text-xs text-[#6B6258] px-3 py-1 rounded hover:bg-[#262220]" onClick={() => setRenaming(null)}>Cancel</button>
              <button className="text-xs text-[#DCD5CC] bg-[#5C4033] px-3 py-1 rounded hover:bg-[#7A5A4A]" onClick={handleRenameSubmit}>Rename</button>
            </div>
          </div>
        </div>
      )}

      {contextMenu && (
        <div
          ref={contextMenuRef}
          className="fixed z-50 bg-[#1E1A16] border border-[#252018] rounded-md shadow-lg py-1 min-w-[160px]"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          {menuItems.map((item) => (
            <button
              key={item.label}
              className="flex items-center w-full px-3 py-1.5 text-sm text-[#DCD5CC] hover:bg-[#262220]"
              onClick={item.action}
            >
              <item.icon size={14} className="mr-2 text-[#6B6258]" />
              {item.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
