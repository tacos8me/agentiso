type SkeletonVariant = 'card' | 'editor' | 'terminal' | 'tree';

interface LoadingSkeletonProps {
  variant?: SkeletonVariant;
}

function PulseBar({ className, style }: { className?: string; style?: React.CSSProperties }) {
  return (
    <div
      className={`rounded animate-pulse ${className ?? ''}`}
      style={{ background: '#1E1A16', ...style }}
    />
  );
}

function CardSkeleton() {
  return (
    <div className="flex flex-col gap-3 p-4">
      <PulseBar className="h-4 w-1/3" />
      <PulseBar className="h-3 w-2/3" />
      <PulseBar className="h-3 w-1/2" />
      <div className="flex gap-2 mt-2">
        <PulseBar className="h-8 w-24 rounded-md" />
        <PulseBar className="h-8 w-24 rounded-md" />
      </div>
    </div>
  );
}

function EditorSkeleton() {
  return (
    <div className="flex flex-col h-full p-6 gap-4">
      <PulseBar className="h-5 w-2/5" />
      <PulseBar className="h-px w-full" />
      <PulseBar className="h-4 w-4/5" />
      <PulseBar className="h-4 w-3/5" />
      <PulseBar className="h-4 w-full" />
      <PulseBar className="h-4 w-2/3" />
      <div className="mt-2">
        <PulseBar className="h-20 w-full rounded-md" />
      </div>
      <PulseBar className="h-4 w-3/4" />
      <PulseBar className="h-4 w-1/2" />
    </div>
  );
}

function TerminalSkeleton() {
  return (
    <div className="flex flex-col h-full" style={{ background: '#0A0A0A' }}>
      <div className="h-8 border-b border-[#252018] flex items-center px-3" style={{ background: '#161210' }}>
        <PulseBar className="h-3 w-24" />
      </div>
      <div className="flex flex-col gap-2 p-4 font-mono">
        <PulseBar className="h-3 w-48" />
        <PulseBar className="h-3 w-32" />
        <PulseBar className="h-3 w-56" />
        <PulseBar className="h-3 w-20" />
      </div>
    </div>
  );
}

function TreeSkeleton() {
  return (
    <div className="flex flex-col gap-1 p-3">
      <PulseBar className="h-3 w-16 mb-2" />
      {[...Array(8)].map((_, i) => (
        <div key={i} className="flex items-center gap-2" style={{ paddingLeft: `${(i % 3) * 12 + 8}px` }}>
          <PulseBar className="h-3 w-3 rounded-sm" />
          <PulseBar className="h-3" style={{ width: `${60 + Math.random() * 80}px` }} />
        </div>
      ))}
    </div>
  );
}

export function LoadingSkeleton({ variant = 'card' }: LoadingSkeletonProps) {
  return (
    <div className="h-full w-full" style={{ background: '#0A0A0A' }}>
      {variant === 'card' && <CardSkeleton />}
      {variant === 'editor' && <EditorSkeleton />}
      {variant === 'terminal' && <TerminalSkeleton />}
      {variant === 'tree' && <TreeSkeleton />}
    </div>
  );
}
