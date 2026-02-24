// StatusDot accepts any status string and maps it to a color.
// Supports workspace states (running, idle, creating, stopped, etc.)
// and task statuses (pending, claimed, in_progress, completed, failed).

const colorMap: Record<string, string> = {
  running: "bg-[#4A7C59]",
  creating: "bg-[#8B7B3A]",
  idle: "bg-[#4A6B8B]",
  stopped: "bg-[#8B4A4A]",
  suspended: "bg-[#4A6B8B]",
  destroying: "bg-[#8B4A4A]",
  failed: "bg-[#8B4A4A]",
  pending: "bg-[#5A524A]",
  claimed: "bg-[#8B7B3A]",
  in_progress: "bg-[#4A6B8B]",
  completed: "bg-[#4A7C59]",
};

const pulseStatuses = new Set(["running", "creating", "in_progress", "destroying"]);

interface StatusDotProps {
  status: string;
  size?: "sm" | "md";
  className?: string;
}

export function StatusDot({ status, size = "sm", className = "" }: StatusDotProps) {
  const sizeClass = size === "sm" ? "w-2 h-2" : "w-2.5 h-2.5";
  const color = colorMap[status] || "bg-[#5A524A]";
  const shouldPulse = pulseStatuses.has(status);

  return (
    <span className={`relative inline-flex ${className}`}>
      {shouldPulse && (
        <span
          className={`absolute inline-flex h-full w-full rounded-full opacity-40 animate-ping ${color}`}
        />
      )}
      <span className={`relative inline-flex rounded-full ${sizeClass} ${color}`} />
    </span>
  );
}
