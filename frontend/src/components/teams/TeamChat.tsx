import { useState, useEffect, useRef, useCallback } from 'react';
import { Send } from 'lucide-react';
import { useTeamStore } from '../../stores/teams';
import type { TeamMessage } from '../../stores/teams';
import { sendTeamMessage } from '../../api/teams';

// Hash a string to pick a color from a palette
const SENDER_COLORS = [
  '#7C6F5B', '#5B7C6F', '#6F5B7C', '#7C5B5B', '#5B6F7C',
  '#8B7B3A', '#3A8B7B', '#7B3A8B', '#4A7C59', '#4A6B8B',
];

function hashColor(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i++) {
    hash = ((hash << 5) - hash + name.charCodeAt(i)) | 0;
  }
  return SENDER_COLORS[Math.abs(hash) % SENDER_COLORS.length];
}

function relativeTime(timestamp: string): string {
  const now = Date.now();
  const then = new Date(timestamp).getTime();
  const diffSec = Math.floor((now - then) / 1000);
  if (diffSec < 10) return 'just now';
  if (diffSec < 60) return `${diffSec}s ago`;
  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) return `${diffMin}m ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h ago`;
  const diffDay = Math.floor(diffHr / 24);
  return `${diffDay}d ago`;
}

interface MessageGroup {
  sender: string;
  messages: TeamMessage[];
}

function groupMessages(messages: TeamMessage[]): MessageGroup[] {
  const groups: MessageGroup[] = [];
  for (const msg of messages) {
    const last = groups[groups.length - 1];
    if (last && last.sender === msg.from) {
      last.messages.push(msg);
    } else {
      groups.push({ sender: msg.from, messages: [msg] });
    }
  }
  return groups;
}

interface TeamChatProps {
  teamName: string;
}

export function TeamChat({ teamName }: TeamChatProps) {
  const messages = useTeamStore((s) => s.messages);
  const teams = useTeamStore((s) => s.teams);
  const fetchMessages = useTeamStore((s) => s.fetchMessages);
  const fetchTeamDetail = useTeamStore((s) => s.fetchTeamDetail);

  const [fromAgent, setFromAgent] = useState('');
  const [toAgent, setToAgent] = useState('all');
  const [text, setText] = useState('');
  const [sending, setSending] = useState(false);

  const listRef = useRef<HTMLDivElement>(null);
  const prevCountRef = useRef(messages.length);

  const team = teams.find((t) => t.name === teamName);
  const members = team?.members ?? [];

  // Set default "from" once members load
  useEffect(() => {
    if (members.length > 0 && !fromAgent) {
      setFromAgent(members[0].name);
    }
  }, [members, fromAgent]);

  // Fetch team detail + messages on mount, poll every 3s
  useEffect(() => {
    fetchTeamDetail(teamName);
    fetchMessages(teamName);
    const interval = setInterval(() => {
      fetchMessages(teamName);
    }, 3000);
    return () => clearInterval(interval);
  }, [teamName, fetchTeamDetail, fetchMessages]);

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    if (messages.length > prevCountRef.current && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
    prevCountRef.current = messages.length;
  }, [messages.length]);

  // Also scroll on initial render
  useEffect(() => {
    if (listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, []);

  const handleSend = useCallback(async () => {
    const trimmed = text.trim();
    if (!trimmed || !fromAgent || sending) return;
    setSending(true);
    try {
      const isBroadcast = toAgent === 'all';
      await sendTeamMessage(teamName, fromAgent, trimmed, {
        to: isBroadcast ? undefined : toAgent,
        broadcast: isBroadcast,
      });
      setText('');
      // Refresh messages immediately after sending
      fetchMessages(teamName);
    } finally {
      setSending(false);
    }
  }, [text, fromAgent, toAgent, sending, teamName, fetchMessages]);

  const groups = groupMessages(messages);

  return (
    <div className="flex flex-col h-full bg-[#0A0A0A]">
      {/* Header */}
      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-[#252018] shrink-0 bg-[#161210]">
        <span className="text-sm font-semibold text-[#DCD5CC]">
          Chat: {teamName}
        </span>
        <span className="text-xs text-[#6B6258]">
          {members.length} member{members.length !== 1 ? 's' : ''}
        </span>
        <span className="text-xs text-[#6B6258] ml-auto">
          {messages.length} message{messages.length !== 1 ? 's' : ''}
        </span>
      </div>

      {/* Message list */}
      <div
        ref={listRef}
        className="flex-1 overflow-y-auto px-4 py-3 space-y-3"
      >
        {messages.length === 0 ? (
          <div className="flex items-center justify-center h-full text-sm text-[#6B6258]">
            No messages yet. Send one to get started.
          </div>
        ) : (
          groups.map((group) => {
            const color = hashColor(group.sender);
            const firstMsg = group.messages[0];
            return (
              <div key={firstMsg.id} className="space-y-0.5">
                {/* Group header: sender badge + timestamp of first message */}
                <div className="flex items-center gap-2 mb-1">
                  <span
                    className="text-[11px] font-semibold px-1.5 py-0.5 rounded"
                    style={{
                      backgroundColor: color + '25',
                      color: color,
                    }}
                  >
                    {group.sender}
                  </span>
                  <span className="text-[10px] text-[#4A4238]">
                    {relativeTime(firstMsg.timestamp)}
                  </span>
                </div>

                {/* Individual messages in the group */}
                {group.messages.map((msg) => {
                  const isBroadcast = msg.type === 'broadcast';
                  return (
                    <div
                      key={msg.id}
                      className={`py-1.5 px-3 rounded text-xs ${
                        isBroadcast
                          ? 'bg-[#252018] border border-[#5C4033]/25'
                          : 'bg-[#161210]'
                      }`}
                    >
                      <div className="flex items-center gap-1.5 mb-0.5">
                        {msg.to && (
                          <span className="text-[10px] text-[#6B6258]">
                            {'\u2192'} {msg.to}
                          </span>
                        )}
                        {isBroadcast && (
                          <span className="text-[10px] text-[#5C4033] uppercase font-semibold">
                            broadcast
                          </span>
                        )}
                        {/* Show relative time on non-first messages */}
                        {msg.id !== firstMsg.id && (
                          <span className="text-[10px] text-[#4A4238] ml-auto">
                            {relativeTime(msg.timestamp)}
                          </span>
                        )}
                      </div>
                      <div className="text-[#DCD5CC]/85 leading-relaxed">
                        {msg.content}
                      </div>
                    </div>
                  );
                })}
              </div>
            );
          })
        )}
      </div>

      {/* Input bar */}
      <div className="shrink-0 border-t border-[#252018] bg-[#161210] px-3 py-2.5">
        <div className="flex items-center gap-2">
          {/* From dropdown */}
          <select
            value={fromAgent}
            onChange={(e) => setFromAgent(e.target.value)}
            className="text-xs bg-[#0A0A0A] text-[#DCD5CC] border border-[#252018] rounded px-2 py-1.5 outline-none focus:border-[#5C4033] transition-colors min-w-0 max-w-[140px]"
            aria-label="Send as"
          >
            {members.length === 0 && (
              <option value="">No members</option>
            )}
            {members.map((m) => (
              <option key={m.name} value={m.name}>
                {m.name}
              </option>
            ))}
          </select>

          {/* To dropdown */}
          <select
            value={toAgent}
            onChange={(e) => setToAgent(e.target.value)}
            className="text-xs bg-[#0A0A0A] text-[#DCD5CC] border border-[#252018] rounded px-2 py-1.5 outline-none focus:border-[#5C4033] transition-colors min-w-0 max-w-[140px]"
            aria-label="Send to"
          >
            <option value="all">All (broadcast)</option>
            {members.map((m) => (
              <option key={m.name} value={m.name}>
                {m.name}
              </option>
            ))}
          </select>

          {/* Text input */}
          <input
            type="text"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                handleSend();
              }
            }}
            placeholder="Type a message..."
            className="flex-1 text-xs bg-[#0A0A0A] text-[#DCD5CC] border border-[#252018] rounded px-3 py-1.5 outline-none focus:border-[#5C4033] transition-colors placeholder-[#3E3830] min-w-0"
            disabled={sending || members.length === 0}
          />

          {/* Send button */}
          <button
            onClick={handleSend}
            disabled={sending || !text.trim() || !fromAgent}
            className="w-8 h-8 flex items-center justify-center rounded bg-[#5C4033] text-[#DCD5CC] hover:bg-[#6D4E3D] disabled:opacity-30 disabled:cursor-not-allowed transition-colors cursor-pointer shrink-0"
            aria-label="Send message"
          >
            <Send size={14} />
          </button>
        </div>
      </div>
    </div>
  );
}
