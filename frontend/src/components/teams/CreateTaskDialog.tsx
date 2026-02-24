import { useState, useEffect, useRef } from 'react';
import { X } from 'lucide-react';
import { useTeamStore } from '../../stores/teams';
import type { BoardTask } from '../../types/workspace';

interface CreateTaskDialogProps {
  teamName: string;
  existingTasks: BoardTask[];
  onClose: () => void;
}

export function CreateTaskDialog({ teamName, existingTasks, onClose }: CreateTaskDialogProps) {
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [priority, setPriority] = useState<'low' | 'medium' | 'high' | 'critical'>('medium');
  const [dependsOn, setDependsOn] = useState<string[]>([]);
  const [submitting, setSubmitting] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => { inputRef.current?.focus(); }, []);

  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (e.key === 'Escape') onClose();
    }
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [onClose]);

  const handleSubmit = async () => {
    if (!title.trim() || submitting) return;
    setSubmitting(true);
    try {
      await useTeamStore.getState().createTask(teamName, {
        title: title.trim(),
        description: description.trim() || undefined,
        priority,
        depends_on: dependsOn.length > 0 ? dependsOn : undefined,
      });
      onClose();
    } catch {
      setSubmitting(false);
    }
  };

  const toggleDep = (taskId: string) => {
    setDependsOn((prev) =>
      prev.includes(taskId) ? prev.filter((id) => id !== taskId) : [...prev, taskId],
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-[#161210] border border-[#252018] rounded-lg p-5 w-[420px] shadow-2xl animate-fade-in"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Create task"
      >
        <div className="flex items-center justify-between mb-4">
          <h3 className="text-sm font-semibold text-[#DCD5CC]">
            Create Task <span className="text-[#6B6258] font-normal">in {teamName}</span>
          </h3>
          <button onClick={onClose} className="text-[#6B6258] hover:text-[#DCD5CC] cursor-pointer" aria-label="Close">
            <X size={14} />
          </button>
        </div>

        <div className="space-y-3">
          {/* Title */}
          <div>
            <label htmlFor="task-title" className="block text-xs text-[#6B6258] mb-1">Title</label>
            <input
              id="task-title"
              ref={inputRef}
              type="text"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              className="w-full bg-[#0A0A0A] text-sm text-[#DCD5CC] border border-[#252018] rounded px-3 py-1.5 outline-none focus:border-[#5C4033] transition-colors"
              placeholder="Task title"
              onKeyDown={(e) => { if (e.key === 'Enter') handleSubmit(); }}
            />
          </div>

          {/* Description */}
          <div>
            <label htmlFor="task-desc" className="block text-xs text-[#6B6258] mb-1">Description</label>
            <textarea
              id="task-desc"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={3}
              className="w-full bg-[#0A0A0A] text-sm text-[#DCD5CC] border border-[#252018] rounded px-3 py-1.5 outline-none focus:border-[#5C4033] transition-colors resize-none"
              placeholder="Optional description"
            />
          </div>

          {/* Priority */}
          <div>
            <label htmlFor="task-priority" className="block text-xs text-[#6B6258] mb-1">Priority</label>
            <select
              id="task-priority"
              value={priority}
              onChange={(e) => setPriority(e.target.value as typeof priority)}
              className="w-full bg-[#0A0A0A] text-sm text-[#DCD5CC] border border-[#252018] rounded px-3 py-1.5 outline-none focus:border-[#5C4033] transition-colors cursor-pointer"
            >
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
              <option value="critical">Critical</option>
            </select>
          </div>

          {/* Dependencies */}
          {existingTasks.length > 0 && (
            <div>
              <span className="block text-xs text-[#6B6258] mb-1">Depends on</span>
              <div className="max-h-[120px] overflow-y-auto space-y-1 bg-[#0A0A0A] border border-[#252018] rounded p-2">
                {existingTasks.map((task) => (
                  <label
                    key={task.id}
                    className="flex items-center gap-2 px-1 py-0.5 rounded hover:bg-[#1E1A16] cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={dependsOn.includes(task.id)}
                      onChange={() => toggleDep(task.id)}
                      className="accent-[#5C4033]"
                    />
                    <span className="text-xs text-[#DCD5CC] truncate">{task.title}</span>
                    <span className="text-[10px] text-[#4A4238] ml-auto shrink-0">{task.status}</span>
                  </label>
                ))}
              </div>
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 mt-4">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-xs rounded cursor-pointer text-[#6B6258] hover:text-[#DCD5CC] transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={!title.trim() || submitting}
            className="px-3 py-1.5 text-xs rounded cursor-pointer bg-[#5C4033] text-[#DCD5CC] hover:bg-[#7A5A4A] disabled:opacity-50 transition-colors"
          >
            {submitting ? 'Creating...' : 'Create'}
          </button>
        </div>
      </div>
    </div>
  );
}
