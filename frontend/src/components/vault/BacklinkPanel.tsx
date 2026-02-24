import { useState, useMemo } from "react";
import { ChevronDown, ChevronRight, Link2, FileText } from "lucide-react";
import { useVaultStore } from "../../stores/vault";

interface BacklinkPanelProps {
  notePath: string;
  onNavigate: (notePath: string) => void;
}

export function BacklinkPanel({ notePath, onNavigate }: BacklinkPanelProps) {
  const [collapsed, setCollapsed] = useState(false);
  const graphData = useVaultStore((s) => s.graphData);

  // Derive backlinks from graph data: find all edges where target matches notePath
  // or target matches the note's basename (wikilink style)
  const backlinks = useMemo(() => {
    if (!graphData) return [];

    const noteName = notePath.split('/').pop()?.replace(/\.md$/, '') || notePath;
    const results: Array<{ path: string; name: string; context: string }> = [];

    for (const edge of graphData.links) {
      const sourceStr = typeof edge.source === 'string' ? edge.source : (edge.source as { id?: string })?.id || '';
      const targetStr = typeof edge.target === 'string' ? edge.target : (edge.target as { id?: string })?.id || '';

      if (targetStr === notePath || targetStr === noteName) {
        const srcName = sourceStr.split('/').pop()?.replace(/\.md$/, '') || sourceStr;
        // Avoid duplicate entries
        if (!results.some(r => r.path === sourceStr)) {
          results.push({
            path: sourceStr,
            name: srcName,
            context: `Links to [[${noteName}]]`,
          });
        }
      }
    }

    return results;
  }, [notePath, graphData]);

  return (
    <div className="border-t border-[#252018]" style={{ background: "#0A0A0A" }}>
      <button
        className="flex items-center w-full px-4 py-2 text-left hover:bg-[#161210]"
        onClick={() => setCollapsed(!collapsed)}
      >
        <span className="mr-1.5 text-[#6B6258]">
          {collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
        </span>
        <Link2 size={14} className="mr-2 text-[#5C4033]" />
        <span className="text-xs font-medium text-[#DCD5CC]">
          {backlinks.length} backlink{backlinks.length !== 1 ? "s" : ""}
        </span>
      </button>

      {!collapsed && (
        <div className="px-4 pb-3">
          {backlinks.length === 0 ? (
            <div className="text-[#4A4238] text-xs py-2 pl-6">
              No backlinks found
            </div>
          ) : (
            <div className="space-y-1">
              {backlinks.map((bl) => (
                <button
                  key={bl.path}
                  className="flex flex-col w-full text-left px-3 py-2 rounded hover:bg-[#1E1A16] group"
                  onClick={() => onNavigate(bl.path)}
                >
                  <span className="flex items-center text-xs text-[#DCD5CC] group-hover:text-[#7A5A4A]">
                    <FileText size={12} className="mr-1.5 flex-shrink-0 text-[#6B6258]" />
                    {bl.name}
                  </span>
                  <span className="text-[11px] text-[#6B6258] mt-0.5 pl-5 truncate">
                    {bl.context}
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
