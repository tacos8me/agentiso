import { useEffect, useRef, useCallback } from "react";

interface ConfirmDialogProps {
  title: string;
  message: string;
  confirmLabel: string;
  confirmVariant?: "danger" | "default";
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  title,
  message,
  confirmLabel,
  confirmVariant = "default",
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    confirmRef.current?.focus();
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [onCancel]);

  const handleBackdropClick = useCallback(
    (e: React.MouseEvent) => {
      if (dialogRef.current && !dialogRef.current.contains(e.target as Node)) {
        onCancel();
      }
    },
    [onCancel],
  );

  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center"
      onClick={handleBackdropClick}
    >
      <div className="absolute inset-0 bg-black/60" />
      <div
        ref={dialogRef}
        className="relative bg-[#161210] border border-[#252018] rounded-lg shadow-xl w-[360px] p-5 space-y-4"
      >
        <h3 className="text-sm font-semibold text-[#DCD5CC]">{title}</h3>
        <p className="text-sm text-[#6B6258]">{message}</p>
        <div className="flex items-center justify-end gap-2 pt-1">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-sm text-[#6B6258] hover:text-[#DCD5CC] rounded-md hover:bg-[#1E1A16] transition-colors cursor-pointer"
          >
            Cancel
          </button>
          <button
            ref={confirmRef}
            onClick={onConfirm}
            className={`
              px-3 py-1.5 text-sm font-medium rounded-md transition-colors cursor-pointer
              ${
                confirmVariant === "danger"
                  ? "text-red-300 bg-red-900/50 hover:bg-red-800/50"
                  : "text-[#DCD5CC] bg-[#5C4033] hover:bg-[#6D5040]"
              }
            `}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
