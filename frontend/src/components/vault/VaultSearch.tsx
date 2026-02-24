import { useState, useCallback, useRef, useEffect } from "react";
import { Search, ToggleLeft, ToggleRight, FileText, Hash, Loader2 } from "lucide-react";
import { useVaultStore } from "../../stores/vault";

interface VaultSearchProps {
  onNoteSelect: (notePath: string) => void;
}

export function VaultSearch({ onNoteSelect }: VaultSearchProps) {
  const [query, setQuery] = useState("");
  const [useRegex, setUseRegex] = useState(false);
  const [searching, setSearching] = useState(false);
  const searchResults = useVaultStore((s) => s.searchResults);
  const search = useVaultStore((s) => s.search);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Debounced search (300ms)
  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (!query.trim()) {
      useVaultStore.getState().setSearchResults([]);
      setSearching(false);
      return;
    }
    setSearching(true);
    debounceRef.current = setTimeout(async () => {
      await search(query);
      setSearching(false);
    }, 300);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [query, useRegex, search]);

  const highlightMatch = useCallback(
    (text: string) => {
      if (!query.trim()) return text;
      try {
        const re = useRegex ? new RegExp(query, "gi") : new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi");
        const parts = text.split(re);
        const matches = text.match(re);
        if (!matches) return text;

        const elements: Array<string | { match: string; idx: number }> = [];
        for (let i = 0; i < parts.length; i++) {
          if (parts[i]) elements.push(parts[i]);
          if (matches[i]) elements.push({ match: matches[i], idx: i });
        }

        return (
          <span>
            {elements.map((el, i) =>
              typeof el === "string" ? (
                <span key={i}>{el}</span>
              ) : (
                <span key={`m${i}`} className="bg-[#5C4033]/40 text-[#DCD5CC] rounded px-0.5">
                  {el.match}
                </span>
              )
            )}
          </span>
        );
      } catch {
        return text;
      }
    },
    [query, useRegex]
  );

  // Group results by note
  const groupedResults = new Map<string, Array<{ line: number; context: string }>>();
  for (const r of searchResults) {
    const key = r.path;
    if (!groupedResults.has(key)) groupedResults.set(key, []);
    groupedResults.get(key)!.push({ line: r.line, context: r.context });
  }

  return (
    <div className="flex flex-col h-full" style={{ background: "#0A0A0A" }}>
      {/* Search input */}
      <div className="px-3 py-2 border-b border-[#252018]">
        <div className="flex items-center bg-[#161210] rounded px-2.5 py-1.5 border border-[#252018] focus-within:border-[#5C4033]">
          {searching ? (
            <Loader2 size={14} className="text-[#6B6258] mr-2 flex-shrink-0 animate-spin" />
          ) : (
            <Search size={14} className="text-[#6B6258] mr-2 flex-shrink-0" />
          )}
          <input
            type="text"
            placeholder="Search vault..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="bg-transparent text-sm text-[#DCD5CC] outline-none w-full placeholder-[#3E3830] font-mono"
            autoFocus
          />
          <button
            onClick={() => setUseRegex(!useRegex)}
            className={`flex items-center gap-1 text-[10px] px-1.5 py-0.5 rounded flex-shrink-0 cursor-pointer ${
              useRegex
                ? "text-[#DCD5CC] bg-[#5C4033]/30"
                : "text-[#6B6258] hover:text-[#DCD5CC]"
            }`}
            title={useRegex ? "Regex mode on" : "Regex mode off"}
          >
            {useRegex ? <ToggleRight size={14} /> : <ToggleLeft size={14} />}
            <span>.*</span>
          </button>
        </div>
        {query.trim() && !searching && (
          <div className="text-[11px] text-[#4A4238] mt-1.5 px-0.5">
            {searchResults.length} match{searchResults.length !== 1 ? "es" : ""} in{" "}
            {groupedResults.size} note{groupedResults.size !== 1 ? "s" : ""}
          </div>
        )}
      </div>

      {/* Results list */}
      <div className="flex-1 overflow-y-auto">
        {!query.trim() && (
          <div className="flex flex-col items-center justify-center h-full text-[#4A4238]">
            <Search size={32} className="mb-3 opacity-40" />
            <span className="text-xs">Type to search across vault notes</span>
          </div>
        )}

        {query.trim() && !searching && searchResults.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-[#4A4238]">
            <span className="text-xs">No matches found</span>
          </div>
        )}

        {Array.from(groupedResults.entries()).map(([path, matches]) => (
          <div key={path} className="border-b border-[#161210]">
            <button
              className="flex items-center w-full px-3 py-1.5 text-left cursor-pointer hover:bg-[#161210]"
              onClick={() => onNoteSelect(path)}
            >
              <FileText size={13} className="mr-2 text-[#5C4033] flex-shrink-0" />
              <span className="text-xs text-[#DCD5CC] font-medium truncate">
                {path}
              </span>
              <span className="ml-auto text-[10px] text-[#4A4238] flex-shrink-0">
                {matches.length}
              </span>
            </button>
            <div className="pl-8 pr-3 pb-1.5">
              {matches.slice(0, 5).map((m, i) => (
                <button
                  key={i}
                  className="flex items-start w-full text-left py-0.5 cursor-pointer hover:bg-[#1E1A16] rounded px-1"
                  onClick={() => onNoteSelect(path)}
                >
                  <Hash size={10} className="text-[#4A4238] mt-0.5 mr-1 flex-shrink-0" />
                  <span className="text-[11px] text-[#6B6258] mr-2 flex-shrink-0 font-mono w-6 text-right">
                    {m.line}
                  </span>
                  <span className="text-[11px] text-[#6B6258] truncate">
                    {highlightMatch(m.context)}
                  </span>
                </button>
              ))}
              {matches.length > 5 && (
                <div className="text-[10px] text-[#4A4238] pl-7 py-0.5">
                  +{matches.length - 5} more matches
                </div>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
