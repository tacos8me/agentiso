import { useRef, useCallback, useState, useMemo, useEffect } from "react";
import ForceGraph2D from "react-force-graph-2d";
import { Search, Filter, Loader2, Tag, X } from "lucide-react";
import { useVaultStore } from "../../stores/vault";
import type { VaultGraphNode } from "../../types/vault";

interface GraphViewProps {
  currentNotePath?: string;
  onNoteSelect: (notePath: string) => void;
}

const FOLDER_COLORS: Record<string, string> = {
  analysis: "#7A5A4A",
  teams: "#5C4033",
  "swarm-test": "#8B7B3A",
  "r3f-farmstead": "#4A6B8B",
  "": "#5A524A",
  daily: "#4A7C59",
};

function getFolderColor(folder: string): string {
  return FOLDER_COLORS[folder] || "#5A524A";
}

type GraphNode = VaultGraphNode & {
  x?: number;
  y?: number;
  vx?: number;
  vy?: number;
};

type GraphLink = {
  source: string | GraphNode;
  target: string | GraphNode;
};

export function GraphView({ currentNotePath, onNoteSelect }: GraphViewProps) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const graphRef = useRef<any>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [dimensions, setDimensions] = useState({ width: 800, height: 600 });
  const [searchQuery, setSearchQuery] = useState("");
  const [folderFilter, setFolderFilter] = useState<string | null>(null);
  const [tagFilters, setTagFilters] = useState<string[]>([]);
  const [hoveredNode, setHoveredNode] = useState<string | null>(null);

  const graphData = useVaultStore((s) => s.graphData);
  const loadGraph = useVaultStore((s) => s.loadGraph);

  useEffect(() => {
    loadGraph();
  }, [loadGraph]);

  useEffect(() => {
    if (!containerRef.current) return;
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        setDimensions({
          width: entry.contentRect.width,
          height: entry.contentRect.height,
        });
      }
    });
    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, []);

  const folders = useMemo(() => {
    if (!graphData) return [];
    const set = new Set<string>();
    for (const node of graphData.nodes) {
      set.add(node.folder);
    }
    return Array.from(set).sort();
  }, [graphData]);

  // Collect all tags from graph nodes
  const allTags = useMemo(() => {
    if (!graphData) return [];
    const tagSet = new Set<string>();
    for (const node of graphData.nodes) {
      for (const tag of node.tags) {
        tagSet.add(tag);
      }
    }
    return Array.from(tagSet).sort();
  }, [graphData]);

  const toggleTag = useCallback((tag: string) => {
    setTagFilters((prev) =>
      prev.includes(tag) ? prev.filter((t) => t !== tag) : [...prev, tag]
    );
  }, []);

  // Track search matches for highlight
  const searchMatchIds = useMemo(() => {
    if (!graphData || !searchQuery) return new Set<string>();
    const lq = searchQuery.toLowerCase();
    return new Set(
      graphData.nodes.filter((n) => n.name.toLowerCase().includes(lq)).map((n) => n.id)
    );
  }, [graphData, searchQuery]);

  const filteredData = useMemo(() => {
    if (!graphData) return { nodes: [], links: [] };

    let nodes = graphData.nodes;
    let links = graphData.links;

    // Folder filter
    if (folderFilter) {
      const nodeIds = new Set(
        nodes.filter((n) => n.folder === folderFilter || n.folder.startsWith(folderFilter + "/")).map((n) => n.id)
      );
      nodes = nodes.filter((n) => nodeIds.has(n.id));
      links = links.filter(
        (l) => nodeIds.has(l.source as string) && nodeIds.has(l.target as string)
      );
    }

    // Tag filter: only show nodes that have ALL selected tags
    if (tagFilters.length > 0) {
      const nodeIds = new Set(
        nodes.filter((n) => tagFilters.every((t) => n.tags.includes(t))).map((n) => n.id)
      );
      // Include neighbors of matching nodes
      const withNeighbors = new Set(nodeIds);
      for (const link of links) {
        if (nodeIds.has(link.source as string)) withNeighbors.add(link.target as string);
        if (nodeIds.has(link.target as string)) withNeighbors.add(link.source as string);
      }
      nodes = nodes.filter((n) => withNeighbors.has(n.id));
      links = links.filter(
        (l) => withNeighbors.has(l.source as string) && withNeighbors.has(l.target as string)
      );
    }

    // Search filter
    if (searchQuery) {
      const lq = searchQuery.toLowerCase();
      const matchIds = new Set(
        nodes.filter((n) => n.name.toLowerCase().includes(lq)).map((n) => n.id)
      );
      const neighborIds = new Set(matchIds);
      for (const link of links) {
        if (matchIds.has(link.source as string)) neighborIds.add(link.target as string);
        if (matchIds.has(link.target as string)) neighborIds.add(link.source as string);
      }
      nodes = nodes.filter((n) => neighborIds.has(n.id));
      links = links.filter(
        (l) =>
          neighborIds.has(l.source as string) &&
          neighborIds.has(l.target as string)
      );
    }

    return { nodes: [...nodes], links: [...links] };
  }, [graphData, folderFilter, tagFilters, searchQuery]);

  const handleNodeClick = useCallback(
    (node: GraphNode) => {
      onNoteSelect(node.id);
    },
    [onNoteSelect]
  );

  const paintNode = useCallback(
    (node: GraphNode, ctx: CanvasRenderingContext2D, globalScale: number) => {
      const isCurrent = node.id === currentNotePath;
      const isHovered = node.id === hoveredNode;
      const isSearchMatch = searchMatchIds.has(node.id);
      const radius = Math.sqrt(node.val || 1) * 4;
      const x = node.x ?? 0;
      const y = node.y ?? 0;

      // Glow for search matches
      if (isSearchMatch) {
        ctx.beginPath();
        ctx.arc(x, y, radius + 6, 0, 2 * Math.PI);
        ctx.fillStyle = "#8B7B3A44";
        ctx.fill();
      }

      // Glow for current note
      if (isCurrent) {
        ctx.beginPath();
        ctx.arc(x, y, radius + 4, 0, 2 * Math.PI);
        ctx.fillStyle = `${getFolderColor(node.folder)}44`;
        ctx.fill();
      }

      // Node circle
      ctx.beginPath();
      ctx.arc(x, y, radius, 0, 2 * Math.PI);
      ctx.fillStyle = isSearchMatch
        ? "#8B7B3A"
        : isHovered
          ? "#8B6B5A"
          : isCurrent
            ? "#DCD5CC"
            : getFolderColor(node.folder);
      ctx.fill();

      // Border
      if (isCurrent || isHovered || isSearchMatch) {
        ctx.strokeStyle = isSearchMatch ? "#8B7B3A" : "#DCD5CC";
        ctx.lineWidth = 1.5 / globalScale;
        ctx.stroke();
      }

      // Label
      const fontSize = Math.max(11 / globalScale, 3);
      ctx.font = `${fontSize}px Inter, sans-serif`;
      ctx.textAlign = "center";
      ctx.textBaseline = "top";
      ctx.fillStyle = isCurrent || isHovered || isSearchMatch ? "#DCD5CC" : "#5A524A";
      ctx.fillText(node.name, x, y + radius + 2);
    },
    [currentNotePath, hoveredNode, searchMatchIds]
  );

  const paintLink = useCallback(
    (link: GraphLink, ctx: CanvasRenderingContext2D) => {
      const source = link.source as GraphNode;
      const target = link.target as GraphNode;
      if (!source?.x || !target?.x) return;

      ctx.beginPath();
      ctx.moveTo(source.x, source.y ?? 0);
      ctx.lineTo(target.x, target.y ?? 0);
      ctx.strokeStyle = "#252018";
      ctx.lineWidth = 0.5;
      ctx.stroke();
    },
    []
  );

  if (!graphData) {
    return (
      <div className="flex flex-col h-full items-center justify-center" style={{ background: "#0A0A0A" }}>
        <Loader2 size={24} className="animate-spin text-[#6B6258] mb-2" />
        <span className="text-xs text-[#4A4238]">Loading graph...</span>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full" style={{ background: "#0A0A0A" }}>
      {/* Controls bar */}
      <div className="flex flex-col gap-2 px-3 py-2 border-b border-[#252018]">
        {/* Row 1: search + folder + stats */}
        <div className="flex items-center gap-2">
          <div className="flex items-center flex-1 bg-[#161210] rounded px-2 py-1 border border-[#252018] focus-within:border-[#5C4033] transition-colors">
            <Search size={13} className="text-[#6B6258] mr-1.5" />
            <input
              type="text"
              placeholder="Filter notes..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="bg-transparent text-xs text-[#DCD5CC] outline-none w-full placeholder-[#3E3830]"
              aria-label="Search vault graph"
            />
            {searchQuery && (
              <button
                onClick={() => setSearchQuery("")}
                className="text-[#6B6258] hover:text-[#DCD5CC] ml-1"
                aria-label="Clear search"
              >
                <X size={12} />
              </button>
            )}
          </div>
          <div className="flex items-center">
            <Filter size={13} className="text-[#6B6258] mr-1" />
            <select
              value={folderFilter ?? ""}
              onChange={(e) => setFolderFilter(e.target.value || null)}
              className="bg-[#161210] text-xs text-[#DCD5CC] border border-[#252018] rounded px-2 py-1 outline-none focus:border-[#5C4033] transition-colors"
              aria-label="Filter by folder"
            >
              <option value="">All folders</option>
              {folders.map((f) => (
                <option key={f} value={f}>
                  {f || "(root)"}
                </option>
              ))}
            </select>
          </div>
          <div className="text-[11px] text-[#4A4238] shrink-0">
            {filteredData.nodes.length} nodes, {filteredData.links.length} edges
          </div>
        </div>

        {/* Row 2: tag filter chips */}
        {allTags.length > 0 && (
          <div className="flex items-center gap-1.5 overflow-x-auto" role="group" aria-label="Filter by tags">
            <Tag size={11} className="text-[#6B6258] shrink-0" />
            {allTags.map((tag) => {
              const isActive = tagFilters.includes(tag);
              return (
                <button
                  key={tag}
                  onClick={() => toggleTag(tag)}
                  className={`px-2 py-0.5 text-[11px] rounded-full border transition-colors shrink-0 ${
                    isActive
                      ? "border-[#5C4033] bg-[#5C4033]/20 text-[#DCD5CC]"
                      : "border-[#252018] text-[#6B6258] hover:text-[#DCD5CC] hover:border-[#352C22]"
                  }`}
                  aria-pressed={isActive}
                >
                  #{tag}
                  {isActive && <X size={9} className="inline ml-1 -mt-0.5" />}
                </button>
              );
            })}
            {tagFilters.length > 0 && (
              <button
                onClick={() => setTagFilters([])}
                className="text-[10px] text-[#6B6258] hover:text-[#DCD5CC] ml-1"
                aria-label="Clear all tag filters"
              >
                Clear
              </button>
            )}
          </div>
        )}
      </div>

      {/* Graph canvas */}
      <div ref={containerRef} className="flex-1 relative overflow-hidden">
        <ForceGraph2D
          ref={graphRef as React.MutableRefObject<never>}
          graphData={filteredData as never}
          width={dimensions.width}
          height={dimensions.height}
          backgroundColor="#0A0A0A"
          nodeId="id"
          nodeCanvasObject={paintNode}
          nodeCanvasObjectMode={() => "replace"}
          linkCanvasObject={paintLink}
          linkCanvasObjectMode={() => "replace"}
          onNodeClick={handleNodeClick}
          onNodeHover={(node) => setHoveredNode((node as GraphNode | null)?.id ?? null)}
          nodeLabel={(node) => {
            const n = node as GraphNode;
            return `${n.name} (${n.folder || "root"})`;
          }}
          cooldownTicks={100}
          enableNodeDrag={true}
          enableZoomInteraction={true}
          enablePanInteraction={true}
        />
      </div>

      {/* Legend */}
      <div className="flex items-center gap-4 px-3 py-1.5 border-t border-[#252018] overflow-x-auto">
        {folders.map((f) => (
          <button
            key={f}
            className={`flex items-center gap-1 text-[11px] shrink-0 transition-colors ${
              folderFilter === f ? "text-[#DCD5CC]" : "text-[#6B6258] hover:text-[#DCD5CC]"
            }`}
            onClick={() => setFolderFilter(folderFilter === f ? null : f)}
            aria-label={`Filter by folder: ${f || "root"}`}
            aria-pressed={folderFilter === f}
          >
            <span
              className="w-2.5 h-2.5 rounded-full inline-block"
              style={{ background: getFolderColor(f) }}
            />
            {f || "(root)"}
          </button>
        ))}
      </div>
    </div>
  );
}
