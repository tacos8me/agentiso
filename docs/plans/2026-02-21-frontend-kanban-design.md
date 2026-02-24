# Frontend Kanban + Vault UI — Design Plan

**Date**: 2026-02-21 (updated 2026-02-23)
**Status**: Complete (all 3 phases shipped + v1-v3 post-ship polish)

## Current Issues (pre-pivot snapshot)

### Fixed this session
1. **swarm_run merge ownership failure** — `merge_workspaces_internal()` bypasses ownership checks for ephemeral workers
2. **merge_sequential diff fallback** — wrong stdout variable + wrong apply method (`git apply` vs `git am`)
3. **Team create blank workspaces** — added `golden_workspace` param to fork from snapshot
4. **Internet disabled by default** — flipped `default_allow_internet` to `true`
5. **Empty-tree SHA truncated** — fixed 37-char → 40-char SHA in diff fallback
6. **commits_applied always 0** — git-apply path now returns 1

### Known remaining issues
- Live `agentiso serve` must be restarted to pick up new binary
- Vault has stale placeholder entries from failed OpenCode exchange (`analysis/*.md`)
- Team create with golden_workspace not yet covered in integration test

### Test status
- 872 unit tests passing (775 agentiso + 37 guest + 60 protocol)
- 100 MCP integration test steps (136 assertions)

---

## Stack Decision

| Layer | Choice | Package |
|-------|--------|---------|
| Framework | React 19 + Vite | `react`, `vite` |
| Styling | TailwindCSS 4 | `tailwindcss` v4 |
| State | Zustand 5 | `zustand` |
| Kanban DnD | @hello-pangea/dnd | `@hello-pangea/dnd` |
| Markdown Editor | Tiptap (ProseMirror) | `@tiptap/react`, `tiptap-markdown` |
| Graph | react-force-graph-2d | `react-force-graph-2d` |
| Terminal | xterm.js 5 | `@xterm/xterm`, `@xterm/addon-webgl`, `@xterm/addon-fit`, `@xterm/addon-search` |
| Split Panes | react-resizable-panels | `react-resizable-panels` |
| Fonts | Inter + JetBrains Mono | `@fontsource/inter`, `@fontsource/jetbrains-mono` |
| Backend API | axum REST + WebSocket | existing `axum` dep + `tower-http` |
| Hosting | Embedded in agentiso binary | `tower-http::ServeDir` |

---

## Design Language

### Color Palette — Cool Espresso

```
Background:    #0A0A0A  (pure dark)
Surface:       #161210  (cool dark brown)
Surface-2:     #1E1A16  (elevated surface)
Surface-3:     #262220  (cards, panels)
Accent:        #5C4033  (espresso brown)
Accent-hover:  #7A5A4A  (lighter espresso)
Accent-active: #8B6B5A  (pressed state)
Text:          #DCD5CC  (cool cream)
Text-muted:    #5A524A  (grey-brown)
Text-dim:      #3E3830  (barely visible)
Border:        #252018  (barely there)
Border-hover:  #352C22  (on hover)
Success:       #4A7C59  (muted green)
Warning:       #8B7B3A  (muted amber)
Error:         #8B4A4A  (muted red)
Info:          #4A6B8B  (muted blue)
```

### Typography
- **UI text**: Inter, 14-16px base, weight 400/500/600
- **Code/data**: JetBrains Mono, 13px, weight 400
- **Headings**: Inter, weight 600, -0.02em letter-spacing
- **Loading**: Self-hosted WOFF2 via @fontsource (no CDN)

### Density
- Balanced: 14-16px base font, comfortable spacing
- Sidebar: 13px with 28px row height
- Kanban cards: 14px with 12px padding
- Terminal: 13px JetBrains Mono

---

## Layout Architecture

### Overall Structure
```
┌──────────────────────────────────────────────────────┐
│  ██ agentiso    [search Ctrl+K]    🔔 ⚙️   │ Top Bar
├──────┬───────────────────────────────────────────────┤
│      │                                               │
│ Side │           Mosaic Pane Area                     │
│ bar  │                                               │
│      │  ┌─────────────┬──────────────────┐           │
│ Tree │  │  Pane 1     │  Pane 2          │           │
│      │  │  (kanban)   │  (vault note)    │           │
│      │  │             │                  │           │
│      │  ├─────────────┼────────┬─────────┤           │
│      │  │  Pane 3     │ Pane 4 │ Pane 5  │           │
│      │  │  (terminal) │ (chat) │ (graph) │           │
│      │  └─────────────┴────────┴─────────┘           │
│      │                                               │
└──────┴───────────────────────────────────────────────┘
```

### Sidebar — Obsidian-style File Tree
- Collapsible sections: Workspaces, Vault, Teams
- 13px Inter, 28px row height
- Status dots (green/yellow/red) for workspace state
- Folder expand/collapse with chevron icons
- Click to open in new pane or focus existing
- Right-click context menu (create, delete, rename)

### Mosaic Pane System
- Full react-resizable-panels mosaic layout
- Drag tabs between panes to rearrange
- Drag pane edges to resize
- Each pane has a tab bar (multiple tabs per pane)
- Pane types: Kanban, Vault Note, Terminal (tabs/grid), Team Chat, Graph, Workspace Detail (info+terminal), Team Detail
- Keyboard: Ctrl+\ to split vertical, Ctrl+- to split horizontal

### Command Palette (Ctrl+K)
- Fuzzy search across: workspaces, vault notes, teams, tasks, actions
- Actions: create workspace, exec command, search vault, open terminal
- Recent items at top
- Keyboard-driven (arrow keys, enter to select)

---

## Views

### 1. Kanban Board (Home View)

Multi-board with board selector tabs:
- **Workspaces Board**: columns = Creating | Running | Idle | Stopped
- **Tasks Board** (per-team): columns = Pending | Claimed | In Progress | Completed | Failed
- **Custom Boards**: user-defined columns

**Card Design — Rich Mini Dashboard:**
```
┌──────────────────────┐
│ ● worker-2           │  status dot + name
│   10.99.0.5          │  IP address
│   256MB │ 2 vCPU     │  resources
│   team: analysis     │  team membership
│   up 4m 32s          │  uptime
│   last: ✓ exit 0     │  last exec result
│   ▁▂▃▅▃▂▁ CPU        │  spark line
│   [▶] [■] [⌨]       │  Lucide icon action buttons
└──────────────────────┘
```

Action buttons on cards: Start/Stop, Destroy, Open Terminal

**Card click** → Slide-out drawer from right:
- Full workspace details, resource usage
- Recent exec history
- Snapshot list
- Network policy
- Buttons: Terminal, Logs, Fork, Snapshot

**Drag-and-drop**: Move workspace cards between columns to change state (drag to Stopped → stops VM).

### 2. Vault Browser

**File tree** in sidebar, **note content** in main pane.

**Editor**: Tiptap with live preview (Obsidian Reading View style):
- Rendered markdown by default
- Click to edit inline, seamless read/write transition
- Custom extensions for `[[wikilinks]]`, `#tags`, YAML frontmatter
- Syntax highlighting for code blocks (via lowlight)
- Espresso-themed editor chrome

**Backlinks panel** (bottom or side of note):
- List of notes that link to current note
- Click to navigate
- Shows surrounding context snippet

**Graph view** (opens as pane tab on demand):
- Force-directed graph via react-force-graph-2d
- Nodes = notes, edges = `[[wikilink]]` references
- Click node to navigate to note
- Hover for preview tooltip
- Filter by folder, tag, or search
- Color-coded by folder/tag cluster

### 3. Interactive Terminal

**xterm.js** embedded terminal panes:
- One auto-created shell per running workspace
- "New Terminal" button for additional sessions
- Full ANSI color support, 256-color + truecolor
- WebGL renderer for performance
- Auto-fit to pane size on resize
- Terminal tabs show workspace name + shell number

**Connection**: WebSocket → REST exec endpoint (Phase 1), future: chunked vsock streaming (Phase 2)

**All spawned headless VMs accessible** — terminal selector shows every running workspace.

### 4. Team View

**Chat-style message thread** (main area):
- Slack-like conversation view per team
- Sender name + avatar/role icon + timestamp
- Broadcast vs DM distinction (DMs slightly indented, different bg)
- Message content rendered as markdown

**Activity sidebar** (collapsible right panel):
- Chronological event log: task claims, state changes, workspace events
- Filterable by event type
- Compact format with timestamps

**Task board** embedded as a sub-tab within team view.

### 5. Dashboard Status (Top Bar / Collapsible)

Quick stats in top bar or collapsible panel above kanban:
- Active workspaces count + status breakdown
- Running teams
- Vault note count
- System metrics (CPU, memory, ZFS pool usage)
- Warm pool status

### 6. Notifications

**Toasts** (bottom-right):
- Exec completed, merge done, team message received
- Auto-dismiss after 5s, click to navigate
- Stacked max 3 visible

**Notification center** (bell icon → dropdown):
- Full history of all events
- Mark as read / mark all read
- Filter by type (exec, team, vault, system)
- Badge count on bell icon

---

## REST API Design

### Server Setup
- New axum server on configurable port (default `0.0.0.0:7070`)
- Separate from MCP bridge (10.99.0.1:3100) and metrics server
- Auth: admin token required when binding to non-localhost, no auth for localhost

### Config
```toml
[dashboard]
enabled = true
bind_addr = "0.0.0.0"
port = 7070
admin_token = ""        # empty = no auth (localhost-only safe)
static_dir = ""         # empty = embedded, or path to React build
```

### Endpoints (60 REST + 1 WebSocket)

#### Workspaces — `/api/workspaces` (26 endpoints)
- `GET /api/workspaces` — list all (filterable by state)
- `POST /api/workspaces` — create
- `GET /api/workspaces/:id` — get details (name or UUID)
- `DELETE /api/workspaces/:id` — destroy
- `POST /api/workspaces/:id/start|stop|suspend|resume` — lifecycle
- `POST /api/workspaces/:id/exec` — execute command
- `POST /api/workspaces/:id/exec/background` — background exec
- `GET /api/workspaces/:id/exec/background/:job_id` — poll job
- `DELETE /api/workspaces/:id/exec/background/:job_id` — kill job
- `GET|PUT /api/workspaces/:id/files` — file read/write
- `GET /api/workspaces/:id/files/list` — directory listing
- `GET /api/workspaces/:id/logs` — console/stderr logs
- `POST /api/workspaces/:id/snapshots` — create snapshot
- `GET /api/workspaces/:id/snapshots` — list snapshots
- `POST /api/workspaces/:id/fork` — fork from snapshot
- `POST /api/workspaces/:id/network-policy` — update rules
- `POST /api/workspaces/:id/port-forward` — add port forward
- `POST /api/workspaces/:id/env` — set environment variables
- `POST /api/workspaces/:id/adopt` — adopt ownership

#### Git — `/api/workspaces/:id/git` (5 endpoints)
- clone, status, commit, push, diff

#### Teams — `/api/teams` (16 endpoints)
- CRUD + members + messages + task board full lifecycle

#### Vault — `/api/vault` (15 endpoints)
- Notes CRUD + search + frontmatter + tags + graph data

#### System — `/api/system` (4 endpoints)
- health, metrics (prometheus + JSON), config (read-only)

#### Batch — `/api/batch` (3 endpoints)
- exec_parallel, swarm_run, workspace_prepare

### Response Envelope
```json
{ "data": <payload>, "meta": { "request_id": "uuid" } }
{ "error": { "code": "NOT_FOUND", "message": "..." }, "meta": { "request_id": "uuid" } }
```

---

## WebSocket Protocol

Single multiplexed WebSocket at `GET /api/ws`

### Message Format
```typescript
// Client → Server
{ type: "subscribe" | "unsubscribe" | "exec_input", topic: string, data?: any }

// Server → Client
{ type: "event" | "exec_output" | "error", topic: string, data: any, timestamp: string }
```

### Topics
| Topic | Events |
|-------|--------|
| `workspaces` | state changes, created, destroyed |
| `workspace:{id}` | per-workspace events, exec output |
| `teams` | created, destroyed, member changes |
| `team:{name}:messages` | real-time message delivery |
| `team:{name}:tasks` | task board changes |
| `metrics` | periodic system metrics (every 2-5s) |
| `exec:{ws_id}:{job_id}` | streaming exec output |

### Phased Implementation
1. **Phase 1**: Polling-based — background task diffs state every 2s, pushes via WebSocket
2. **Phase 2**: Event-driven — broadcast channels on manager state transitions
3. **Phase 3**: Exec streaming — chunked vsock output for live terminal

---

## Data Model (Frontend Types)

### Core Entities
- **Workspace**: id, name, state (stopped|running|suspended), network, resources, snapshots, team_id, forked_from
- **Team**: name, state (Creating|Ready|Working|Completing|Destroyed), members, max_vms, parent_team
- **BoardTask**: id, title, status (pending|claimed|inprogress|completed|failed), owner, priority, depends_on
- **VaultNote**: path, content, frontmatter (YAML), backlinks
- **AgentCard**: name, role, skills, status, endpoints (ip, vsock_cid)
- **Message**: id, from, to, content, type, timestamp

### Real-time Fields (WebSocket updates)
- Workspace.state, qemu_pid
- Team.state, member statuses
- Task.status, owner
- Message inbox
- Metrics counters

### Static Fields (set at creation)
- Workspace: id, name, base_image, network.ip, resources, vsock_cid
- Team: name, max_vms, created_at
- Task: id, title, depends_on

---

## Implementation Phases

### Phase 1: Foundation (MVP) — COMPLETE
- [x] React 19 + Vite 7 + TypeScript project in `frontend/` (30+ files)
- [x] TailwindCSS 4 with espresso theme CSS variables (`src/theme.css`)
- [x] Inter + JetBrains Mono font setup via @fontsource
- [x] Zustand store slices: workspaces, teams, vault, ui, notifications
- [x] axum dashboard server + DashboardConfig (9 Rust files in `src/dashboard/web/`)
- [x] 60 REST endpoints: workspaces (29), teams (14), vault (11), system (3), batch (2), WebSocket (1)
- [x] WebSocket at `/api/ws` with BroadcastHub + 2s polling + topic subscriptions
- [x] Static file serving (ServeDir + SPA fallback) + CORS
- [x] Sidebar: Obsidian-style file tree (Workspaces, Vault, Teams sections)
- [x] TopBar: logo, Ctrl+K trigger, notification bell, settings
- [x] Mosaic pane manager (react-resizable-panels) with tab bar + pane type registry
- [x] Command palette (Ctrl+K) with fuzzy search + keyboard nav
- [x] Multi-board kanban (@hello-pangea/dnd): workspace + task boards
- [x] Rich kanban cards: StatusDot, IP, resources, uptime, sparkline, action buttons
- [x] Slide-out detail drawer (400px, ESC/click-outside close)
- [x] Tiptap markdown editor with live preview + starter-kit + lowlight
- [x] Custom extensions: WikiLink ([[links]]), FrontmatterBlock, TagMark (#tags)
- [x] BacklinkPanel: note references with context snippets
- [x] GraphView: react-force-graph-2d with folder-color nodes, search/filter
- [x] VaultSearch: full-text with regex toggle, grouped results
- [x] VaultTree: folder tree with context menus
- [x] StatusDot + SparkLine common components
- [x] API client (fetch wrapper + auth) + WebSocket manager (reconnect + topics)
- [x] Toast notification system with auto-dismiss
- [x] Unified types across all components
- [x] Mock data: 10 workspaces, 15 tasks, 15 vault notes
- [x] Vite build: 375KB JS → 116KB gzip, builds in 1.6s
- [x] Rust tests passing at time of Phase 1 completion

### Phase 2: Integration + Live Data — COMPLETE
- [x] Wire kanban board to Zustand stores → REST API (replace mock data)
- [x] Wire vault browser to REST API (notes CRUD, search, tags)
- [x] Wire sidebar file tree to live workspace/vault/team data
- [x] Wire WebSocket to Zustand stores (real-time state updates)
- [x] Wire command palette to live search across all entities
- [x] Wire slide-out drawer actions to REST API (start, stop, fork, snapshot)
- [x] Wire team chat to REST messages endpoint + WebSocket delivery
- [x] Toast notifications from WebSocket events
- [x] xterm.js terminal panes with exec endpoint connection
- [x] Auto shell per running workspace + on-demand extras
- [x] Notification center (bell dropdown, mark read, filter by type)
- [x] Full keyboard navigation (tab/arrow through cards, vim optional)
- [x] Error handling: API errors → toasts, retry logic, loading states
- [x] Empty states for all views (no workspaces, no notes, no teams)
- [x] Vite build: 1.68MB JS → 516KB gzip, tsc clean, builds in 4.5s
- [x] Rust tests passing at time of Phase 2 completion

### Phase 3: Polish + Production — COMPLETE
- [x] WebSocket event-driven (replace 2s polling with event-driven broadcasts, 10s fallback)
- [x] Exec streaming via SSE (live terminal output with Ctrl+C cancellation)
- [x] Responsive layout (sidebar rail <1200px, mobile overlay <768px, vertical stacking <1024px)
- [x] Embed React build in agentiso binary (rust-embed with compression, MIME types, cache headers, SPA fallback)
- [x] Code splitting: 14 chunks, ~114KB gzip initial load (was 516KB, 78% reduction)
- [x] Vault graph filtering (by folder, tag, search highlight)
- [x] Custom kanban boards (user-defined columns, persisted to localStorage)
- [x] Virtualized lists infrastructure (@tanstack/react-virtual)
- [x] Accessibility: full ARIA audit, skip-to-content, focus rings, DnD announcements, combobox/dialog/tab patterns
- [x] Empty states for all views (CSS-only geometric shapes)
- [x] Loading skeletons for lazy-loaded views (espresso-themed shimmer)
- [x] 872 Rust tests passing (775 agentiso + 37 guest + 60 protocol), 4 ignored, 0 failures

### Post-Ship Polish (v1-v3) — COMPLETE
- [x] **v1**: Custom board selection fix, sidebar "+" button (create workspace/note dialogs)
- [x] **v1**: Task API wiring (7 endpoints: create/claim/start/complete/fail/release/fetch), CreateTaskDialog, task lifecycle buttons on cards
- [x] **v1**: Terminal grid view (tabs/grid toggle, 1-4 in CSS grid, click-to-focus, double-click maximize)
- [x] **v1**: Terminal UX: scrollback search (Ctrl+F via @xterm/addon-search), paste handling (multi-line confirmation), right-click context menu, floating toolbar, font size control (Ctrl+=/Ctrl+-/Ctrl+0, 10-20px, localStorage)
- [x] **v2**: +15% scale pass (base font 14→16px, all component sizes scaled proportionally)
- [x] **v2**: Deduplicated BoardType, column constants (→ constants/board.ts), CreateWorkspaceDialog
- [x] **v2**: TeamDetailPane (mini task board + member cards + message log + "Open Chat" button)
- [x] **v2**: Consistency pass (150ms transitions, cursor-pointer + tabIndex + aria-labels on all interactive elements)
- [x] **v2**: Empty state guidance text (sidebar sections + board views)
- [x] **v3**: WorkspaceDetailPane — full split view (status/resources/network/metadata + embedded TerminalTabs), replaces stub
- [x] **v3**: TeamChat — real-time messaging wired to team API (send/receive/broadcast), grouped messages with color-coded sender badges, relative timestamps, from/to dropdowns, 3s polling
- [x] **v3**: Lucide icons — all Unicode action buttons replaced with proper Lucide React icons (Play, Square, Trash2, GitFork, Camera, Globe, Terminal, etc.)
- [x] **v3**: ConfirmDialog — reusable confirmation dialog on all destructive actions (workspace destroy in Card + CardDetail)
- [x] Only remaining stub: Settings button ("Settings coming soon!" toast)

---

## File Structure (as built)

```
frontend/
├── index.html
├── package.json
├── vite.config.ts
├── tsconfig.json / tsconfig.app.json / tsconfig.node.json
├── eslint.config.js
├── public/
└── src/                        # 65+ source files
    ├── main.tsx                # React entry
    ├── App.tsx                 # Router + layout + global dialogs
    ├── theme.css               # Espresso CSS variables + @fontsource imports (16px base)
    ├── types.ts                # Shared type definitions
    ├── api/
    │   ├── client.ts           # fetch wrapper with auth + error handling
    │   ├── workspaces.ts       # workspace API (exec, streaming, background jobs)
    │   ├── teams.ts            # team API (CRUD, messages, 7 task endpoints)
    │   ├── vault.ts            # vault API (notes CRUD, search, tags)
    │   ├── websocket.ts        # WebSocket connection + topic subscription
    │   └── realtimeSync.ts     # Real-time sync coordination
    ├── stores/
    │   ├── workspaces.ts       # Zustand workspace slice
    │   ├── teams.ts            # Zustand team slice (7 task actions + messaging)
    │   ├── vault.ts            # Zustand vault slice
    │   ├── ui.ts               # Zustand UI state (sidebar, panes, modals, custom boards)
    │   ├── notifications.ts    # Zustand notification slice
    │   ├── metrics.ts          # System metrics store
    │   └── terminals.ts        # Terminal session store
    ├── constants/
    │   └── board.ts            # Shared column constants (WORKSPACE_COLUMNS, TASK_COLUMNS)
    ├── types/
    │   ├── index.ts            # Type re-exports + PaneType union
    │   ├── vault.ts            # Vault-specific types
    │   └── workspace.ts        # Workspace + BoardTask + TaskStatus + TaskPriority types
    ├── mocks/
    │   ├── index.ts            # Mock data exports
    │   ├── workspaces.ts       # Mock workspace data
    │   └── vault.ts            # Mock vault data
    ├── components/
    │   ├── layout/
    │   │   ├── TopBar.tsx          # Logo, search, notifications, settings
    │   │   ├── Sidebar.tsx         # Obsidian-style tree + create dialogs
    │   │   ├── PaneManager.tsx     # Mosaic pane orchestration (7 pane types)
    │   │   ├── CommandPalette.tsx   # Ctrl+K fuzzy search + actions
    │   │   └── index.ts
    │   ├── kanban/
    │   │   ├── Board.tsx           # Multi-board kanban + custom boards
    │   │   ├── Column.tsx          # DnD column wrapper
    │   │   ├── Card.tsx            # WorkspaceCard + TaskCard with Lucide icons
    │   │   ├── CardDetail.tsx      # Slide-out drawer with Lucide icons
    │   │   ├── BoardSelector.tsx   # Board tabs + custom board selector
    │   │   ├── KanbanDemo.tsx      # Demo/empty state
    │   │   └── index.ts
    │   ├── vault/
    │   │   ├── NoteEditor.tsx      # Tiptap live preview editor
    │   │   ├── BacklinkPanel.tsx
    │   │   ├── GraphView.tsx       # react-force-graph-2d
    │   │   ├── VaultSearch.tsx     # Full-text + regex search
    │   │   ├── VaultTree.tsx       # Folder tree with context menus
    │   │   ├── index.ts
    │   │   └── extensions/
    │   │       ├── WikiLink.ts
    │   │       ├── FrontmatterBlock.ts
    │   │       └── TagMark.ts
    │   ├── terminal/
    │   │   ├── TerminalPane.tsx    # xterm.js + search + paste + context menu + toolbar
    │   │   └── TerminalTabs.tsx    # Tab/grid view toggle, GridCellHeader
    │   ├── teams/
    │   │   ├── TeamDetailPane.tsx  # Full team view (task board + members + messages)
    │   │   ├── TeamChat.tsx        # Real-time team messaging
    │   │   └── CreateTaskDialog.tsx # Task creation form
    │   ├── workspace/
    │   │   └── WorkspaceDetailPane.tsx  # Split info+terminal view
    │   └── common/
    │       ├── StatusDot.tsx
    │       ├── SparkLine.tsx
    │       ├── Toast.tsx
    │       ├── NotificationCenter.tsx
    │       ├── ConnectionStatus.tsx
    │       ├── LoadingSkeleton.tsx
    │       ├── ConfirmDialog.tsx   # Reusable destroy confirmation
    │       └── index.ts
    └── hooks/
        ├── useKeyboard.ts
        ├── useConnectionStatus.ts
        └── useResponsive.ts

# Backend additions
agentiso/src/dashboard/
├── mod.rs              # DashboardState (TUI dashboard)
├── data.rs             # TUI data types
├── ui.rs               # TUI rendering
└── web/                # Web dashboard (9 files)
    ├── mod.rs          # DashboardState, router setup, server startup
    ├── auth.rs         # Admin token middleware
    ├── embedded.rs     # rust-embed static file serving
    ├── workspaces.rs   # 29 workspace REST handlers + SSE
    ├── teams.rs        # 14 team REST handlers
    ├── vault.rs        # 11 vault REST handlers
    ├── system.rs       # 3 system REST handlers
    ├── batch.rs        # 2 batch REST handlers
    └── ws.rs           # WebSocket handler + BroadcastHub
```

---

## Dependencies

### Frontend (package.json)
```json
{
  "dependencies": {
    "react": "^19",
    "react-dom": "^19",
    "@hello-pangea/dnd": "latest",
    "@tiptap/react": "latest",
    "@tiptap/starter-kit": "latest",
    "@tiptap/extension-link": "latest",
    "@tiptap/extension-code-block-lowlight": "latest",
    "tiptap-markdown": "latest",
    "react-force-graph-2d": "latest",
    "@xterm/xterm": "^5",
    "@xterm/addon-webgl": "^0.18",
    "@xterm/addon-fit": "^0.10",
    "@xterm/addon-search": "^0.16.0",
    "lucide-react": "latest",
    "react-resizable-panels": "latest",
    "zustand": "^5",
    "@fontsource/inter": "latest",
    "@fontsource/jetbrains-mono": "latest"
  },
  "devDependencies": {
    "vite": "^6",
    "@vitejs/plugin-react": "latest",
    "tailwindcss": "^4",
    "typescript": "^5"
  }
}
```

### Backend (Cargo.toml additions)
```toml
tower-http = { version = "0.6", features = ["fs", "cors"] }
# axum WebSocket support is built-in (no new dep)
```
