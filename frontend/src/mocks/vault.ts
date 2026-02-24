import type { VaultNote, VaultFolder, VaultTreeNode, VaultGraphData, VaultGraphNode, VaultGraphLink } from "../types/vault";

export const mockVaultNotes: VaultNote[] = [
  {
    path: "analysis/prd.md",
    name: "prd",
    content: `---
status: approved
tags: [planning, requirements]
priority: high
created: "2026-02-18"
updated: "2026-02-21"
---

# Product Requirements Document

## Overview

agentiso is a QEMU microvm workspace manager for AI agents, exposed via MCP tools.
The system manages isolated VM workspaces with ZFS-backed storage, vsock communication,
and team coordination capabilities.

## Key Requirements

### Core Platform
- Workspace lifecycle management (create, start, stop, destroy)
- ZFS zvol storage with snapshots and clones
- QEMU microvm with host kernel boot
- Guest agent communication via vsock

### Team Coordination
- Multi-agent team creation with role assignment
- Inter-agent messaging via \`[[message-relay]]\`
- Task board with dependency resolution
- Nested sub-teams with budget inheritance

### Security
- Network isolation via nftables
- Token-based session management
- Rate limiting per operation category
- Guest agent hardening (see \`[[security-model]]\`)

## Milestones

| Phase | Description | Status |
|-------|-------------|--------|
| Core | VM + Storage + MCP | Complete |
| Teams | Multi-agent coordination | Complete |
| Vault | Knowledge base integration | Complete |
| Dashboard | Frontend UI | In Progress |

## Related
- \`[[api-spec]]\` for REST endpoint design
- \`[[architecture]]\` for system overview
- \`[[security-model]]\` for threat model
`,
    frontmatter: {
      status: "approved",
      tags: ["planning", "requirements"],
      priority: "high",
      created: "2026-02-18",
      updated: "2026-02-21",
    },
    backlinks: ["analysis/api-spec.md", "analysis/architecture.md"],
    outlinks: ["teams/message-relay.md", "analysis/security-model.md", "analysis/api-spec.md", "analysis/architecture.md"],
    tags: ["planning", "requirements"],
    modified_at: "2026-02-21T10:30:00Z",
  },
  {
    path: "analysis/api-spec.md",
    name: "api-spec",
    content: `---
status: draft
tags: [api, rest, design]
priority: high
created: "2026-02-20"
---

# API Specification

REST API for the agentiso dashboard, as defined in the \`[[prd]]\`.

## Base URL

\`\`\`
http://127.0.0.1:8080/api
\`\`\`

## Endpoints

### Workspaces

\`\`\`typescript
GET    /api/workspaces          // List all workspaces
POST   /api/workspaces          // Create workspace
GET    /api/workspaces/:id      // Get workspace details
DELETE /api/workspaces/:id      // Destroy workspace
POST   /api/workspaces/:id/exec // Execute command
\`\`\`

### Teams

\`\`\`typescript
GET    /api/teams               // List teams
POST   /api/teams               // Create team
GET    /api/teams/:name         // Get team details
DELETE /api/teams/:name         // Destroy team
POST   /api/teams/:name/message // Send message
\`\`\`

### Vault

\`\`\`typescript
GET    /api/vault/notes         // List notes
GET    /api/vault/notes/:path   // Read note
PUT    /api/vault/notes/:path   // Write note
GET    /api/vault/search        // Full-text search
GET    /api/vault/graph         // Graph data
\`\`\`

## Response Envelope

All responses use a standard envelope:

\`\`\`json
{
  "data": { ... },
  "meta": { "request_id": "uuid" }
}
\`\`\`

See \`[[architecture]]\` for WebSocket protocol.
`,
    frontmatter: {
      status: "draft",
      tags: ["api", "rest", "design"],
      priority: "high",
      created: "2026-02-20",
    },
    backlinks: ["analysis/prd.md"],
    outlinks: ["analysis/prd.md", "analysis/architecture.md"],
    tags: ["api", "rest", "design"],
    modified_at: "2026-02-20T16:00:00Z",
  },
  {
    path: "analysis/architecture.md",
    name: "architecture",
    content: `---
status: approved
tags: [architecture, design]
priority: high
created: "2026-02-16"
updated: "2026-02-21"
---

# System Architecture

## Component Overview

\`\`\`
                    MCP Client (Claude/OpenCode)
                         |
                    MCP Server (stdio)
                         |
              +----------+----------+
              |          |          |
          Workspace   Storage    Network
          Manager     (ZFS)     (TAP/nft)
              |          |          |
              +----VM Manager------+
                    |
              QEMU microvm
                    |
              Guest Agent (vsock)
\`\`\`

## Key Design Decisions

1. **vsock over SSH**: Lower latency, no key management, kernel-level security
2. **ZFS zvols**: Copy-on-write snapshots enable instant workspace forking
3. **microvm**: Minimal attack surface, sub-second boot times
4. **MCP protocol**: Native integration with AI agent frameworks

## Storage Layout

\`\`\`
agentiso/agentiso/
  base/              # Base images
  workspaces/        # Active workspace zvols
  forks/             # Forked workspace zvols
\`\`\`

## Network Architecture

Each VM gets a TAP device on \`br-agentiso\` (10.99.0.0/16).
nftables rules enforce isolation between non-team workspaces.

See \`[[prd]]\` for requirements and \`[[security-model]]\` for threat analysis.
`,
    frontmatter: {
      status: "approved",
      tags: ["architecture", "design"],
      priority: "high",
      created: "2026-02-16",
      updated: "2026-02-21",
    },
    backlinks: ["analysis/prd.md", "analysis/api-spec.md"],
    outlinks: ["analysis/prd.md", "analysis/security-model.md"],
    tags: ["architecture", "design"],
    modified_at: "2026-02-21T08:00:00Z",
  },
  {
    path: "analysis/security-model.md",
    name: "security-model",
    content: `---
status: approved
tags: [security, threat-model]
priority: critical
created: "2026-02-19"
---

# Security Model

## Threat Categories

### Guest Escape
- **Mitigation**: KVM hardware isolation, minimal microvm attack surface
- **Monitoring**: QEMU stderr logging, crash detection

### Network Lateral Movement
- **Mitigation**: Per-workspace nftables isolation
- **Exception**: Team members get bidirectional rules
- **Default**: Internet disabled (\`default_allow_internet = false\`)

### Resource Exhaustion
- **Mitigation**: ZFS volsize quotas, cgroup v2 memory+CPU limits
- **Rate limiting**: Token-bucket per operation category

### Credential Theft
- **Mitigation**: \`SetEnv\` via vsock (never written to disk)
- **Redaction**: Git push credential scrubbing

## Guest Agent Hardening

\`\`\`rust
// ENV/BASH_ENV blocklist prevents env injection
const BLOCKED_ENV_VARS: &[&str] = &["ENV", "BASH_ENV"];

// Output truncation prevents memory exhaustion
const MAX_OUTPUT_SIZE: usize = 32 * 1024 * 1024; // 32 MiB
\`\`\`

See \`[[prd]]\` for full requirements.
`,
    frontmatter: {
      status: "approved",
      tags: ["security", "threat-model"],
      priority: "critical",
      created: "2026-02-19",
    },
    backlinks: ["analysis/prd.md", "analysis/architecture.md"],
    outlinks: ["analysis/prd.md"],
    tags: ["security", "threat-model"],
    modified_at: "2026-02-19T14:00:00Z",
  },
  {
    path: "analysis/performance-notes.md",
    name: "performance-notes",
    content: `---
status: active
tags: [performance, benchmarks]
created: "2026-02-20"
---

# Performance Notes

## Boot Latency

Current microvm boot time: ~800ms from QEMU start to guest agent ready.

Breakdown:
- QEMU startup: ~200ms
- Kernel boot: ~400ms
- initrd + guest agent: ~200ms

## Warm Pool

Pre-warmed VMs reduce perceived latency to near-zero for workspace creation.
Default pool size: 2, auto-replenish on claim.

## Fork Performance

ZFS clone is O(1) — instant regardless of workspace size.
Bottleneck is VM boot, not storage.

## Exec Throughput

With fresh vsock connections per operation (no shared mutex):
- Simple commands: ~10ms round-trip
- Build operations: limited by CPU/IO in guest

See \`[[architecture]]\` for system design context.
`,
    frontmatter: {
      status: "active",
      tags: ["performance", "benchmarks"],
      created: "2026-02-20",
    },
    backlinks: [],
    outlinks: ["analysis/architecture.md"],
    tags: ["performance", "benchmarks"],
    modified_at: "2026-02-20T11:00:00Z",
  },
  {
    path: "teams/message-relay.md",
    name: "message-relay",
    content: `---
status: implemented
tags: [teams, messaging, vsock]
created: "2026-02-19"
---

# Message Relay

## Overview

The MessageRelay is the host-side message router for inter-agent communication.
Each agent has a bounded inbox (VecDeque, capacity 100) keyed by team+agent name.

## Architecture

\`\`\`
Agent A (VM) --vsock:5001--> Host MessageRelay --vsock:5001--> Agent B (VM)
\`\`\`

## Protocol

Messages use the \`TeamMessageEnvelope\` type:

\`\`\`rust
struct TeamMessageEnvelope {
    from: String,
    to: MessageTarget, // Direct(name) | Broadcast
    content: String,
    timestamp: DateTime<Utc>,
}
\`\`\`

## Rate Limiting

- Burst: 50 messages
- Sustained: 300/min per agent
- Content size: 256 KiB max

## Guest Side

The guest agent runs an axum HTTP API on port 8080:
- \`GET /health\` — health check
- \`GET /messages\` — receive pending messages
- \`POST /messages\` — send message (forwarded to host relay)

Related: \`[[team-workflow]]\`, \`[[daemon-design]]\`
`,
    frontmatter: {
      status: "implemented",
      tags: ["teams", "messaging", "vsock"],
      created: "2026-02-19",
    },
    backlinks: ["analysis/prd.md"],
    outlinks: ["teams/team-workflow.md", "teams/daemon-design.md"],
    tags: ["teams", "messaging", "vsock"],
    modified_at: "2026-02-19T18:00:00Z",
  },
  {
    path: "teams/team-workflow.md",
    name: "team-workflow",
    content: `---
status: active
tags: [teams, workflow, orchestration]
created: "2026-02-20"
---

# Team Workflow

## Lifecycle

1. **Create team** with role definitions and golden workspace
2. **Fork workspaces** for each team member from golden snapshot
3. **Inject environment** (API keys, config) via SetEnv
4. **Assign tasks** via TaskBoard
5. **Execute work** — agents claim and work on tasks
6. **Collect results** via \`[[message-relay]]\`
7. **Merge changes** using \`workspace_merge\`
8. **Destroy team** — cascade cleanup

## Task Board

Tasks use YAML frontmatter markdown files in the vault:

\`\`\`yaml
---
id: task-001
title: Implement feature X
status: pending
owner: null
priority: high
depends_on: []
---
\`\`\`

Dependency resolution uses Kahn's topological sort with cycle detection.

## Merge Strategies

| Strategy | Use Case |
|----------|----------|
| \`sequential\` | Linear patch series (git format-patch/am) |
| \`branch-per-source\` | Parallel work (branch + merge) |
| \`cherry-pick\` | Selective commits |

See \`[[prd]]\` for full team requirements.
`,
    frontmatter: {
      status: "active",
      tags: ["teams", "workflow", "orchestration"],
      created: "2026-02-20",
    },
    backlinks: ["teams/message-relay.md"],
    outlinks: ["teams/message-relay.md", "analysis/prd.md"],
    tags: ["teams", "workflow", "orchestration"],
    modified_at: "2026-02-20T14:00:00Z",
  },
  {
    path: "teams/daemon-design.md",
    name: "daemon-design",
    content: `---
status: implemented
tags: [daemon, a2a, autonomous]
created: "2026-02-20"
---

# A2A Agent Daemon

## Overview

The daemon runs inside each guest VM, autonomously executing tasks pushed
via the relay vsock channel (port 5001).

## Architecture

\`\`\`
Host (TeamManager) --push task--> Relay vsock:5001 --> Guest Daemon
                                                        |
                                                    sh -c <command>
                                                        |
Guest Daemon --store result--> RESULT_OUTBOX
Host --PollDaemonResults via vsock:5000--> Collect results
\`\`\`

## Concurrency

- Semaphore-gated: max 4 concurrent tasks
- Each task runs in its own process group for clean kill

## Error Handling

- Malformed task_assignment: produces error result (exit_code=-2)
- Execution timeout: process group kill + error result
- UTF-8 safe truncation on output

See \`[[message-relay]]\` for the transport layer.
`,
    frontmatter: {
      status: "implemented",
      tags: ["daemon", "a2a", "autonomous"],
      created: "2026-02-20",
    },
    backlinks: ["teams/message-relay.md"],
    outlinks: ["teams/message-relay.md"],
    tags: ["daemon", "a2a", "autonomous"],
    modified_at: "2026-02-20T18:00:00Z",
  },
  {
    path: "swarm-test/run-001.md",
    name: "run-001",
    content: `---
status: completed
tags: [swarm, test-run, results]
created: "2026-02-21"
---

# Swarm Test Run 001

## Configuration

- Workers: 3
- Golden workspace: \`golden\`
- Strategy: \`branch-per-source\`
- Shared context: project README

## Results

| Worker | Task | Exit Code | Duration |
|--------|------|-----------|----------|
| fork-1 | Implement auth module | 0 | 45.2s |
| fork-2 | Write unit tests | 0 | 32.1s |
| fork-3 | Update API docs | 0 | 12.8s |

## Merge Result

All 3 branches merged successfully into target workspace.
No conflicts detected.

## Metrics

- Total wall time: 52.3s (parallel execution)
- Peak memory: 1.2 GiB across all workers
- ZFS snapshot size: 3.4 GiB total

See \`[[run-002]]\` for the follow-up run with more workers.
`,
    frontmatter: {
      status: "completed",
      tags: ["swarm", "test-run", "results"],
      created: "2026-02-21",
    },
    backlinks: ["swarm-test/run-002.md"],
    outlinks: ["swarm-test/run-002.md"],
    tags: ["swarm", "test-run", "results"],
    modified_at: "2026-02-21T09:00:00Z",
  },
  {
    path: "swarm-test/run-002.md",
    name: "run-002",
    content: `---
status: completed
tags: [swarm, test-run, results]
created: "2026-02-21"
---

# Swarm Test Run 002

## Configuration

- Workers: 5
- Golden workspace: \`golden\`
- Strategy: \`sequential\`
- Vault context: \`[[prd]]\`, \`[[api-spec]]\`

## Results

| Worker | Task | Exit Code | Duration |
|--------|------|-----------|----------|
| fork-1 | Refactor storage module | 0 | 67.4s |
| fork-2 | Add metrics endpoint | 0 | 28.9s |
| fork-3 | Fix nftables rules | 0 | 15.3s |
| fork-4 | Implement rate limiting | 0 | 41.2s |
| fork-5 | Update integration tests | 1 | 89.1s |

## Issues

Worker fork-5 failed with test compilation error.
Patch from fork-5 was skipped during sequential merge.

## Follow-up

Filed \`[[run-003-plan]]\` to re-run fork-5 task with fixed dependencies.

See \`[[run-001]]\` for the initial test run.
`,
    frontmatter: {
      status: "completed",
      tags: ["swarm", "test-run", "results"],
      created: "2026-02-21",
    },
    backlinks: ["swarm-test/run-001.md"],
    outlinks: ["analysis/prd.md", "analysis/api-spec.md", "swarm-test/run-003-plan.md", "swarm-test/run-001.md"],
    tags: ["swarm", "test-run", "results"],
    modified_at: "2026-02-21T11:00:00Z",
  },
  {
    path: "swarm-test/run-003-plan.md",
    name: "run-003-plan",
    content: `---
status: planned
tags: [swarm, planning]
created: "2026-02-21"
---

# Run 003 Plan

Re-run the failed integration test task from \`[[run-002]]\`.

## Changes

- Pin dependency versions before forking
- Increase exec timeout to 180s
- Add \`shared_context\` with test fixtures path

## Expected Workers

1. Fix compilation error from run-002
2. Re-run full test suite
3. Validate merge result

## Blocked By

Waiting for \`[[run-002]]\` post-mortem analysis.
`,
    frontmatter: {
      status: "planned",
      tags: ["swarm", "planning"],
      created: "2026-02-21",
    },
    backlinks: ["swarm-test/run-002.md"],
    outlinks: ["swarm-test/run-002.md"],
    tags: ["swarm", "planning"],
    modified_at: "2026-02-21T11:30:00Z",
  },
  {
    path: "r3f-farmstead/project-overview.md",
    name: "project-overview",
    content: `---
status: active
tags: [r3f, react, 3d, project]
created: "2026-02-15"
---

# R3F Farmstead Project

## Overview

A React Three Fiber (R3F) based 3D visualization dashboard that renders
workspace VMs as buildings in an isometric farmstead scene.

## Tech Stack

- React Three Fiber + Drei
- Three.js r170
- Zustand for state management
- React Spring for animations

## Scene Layout

\`\`\`
  [Barn: Team Hub]    [Silo: Storage]
       |                    |
  [Field: Active VMs]  [Workshop: Build]
       |
  [Farmhouse: Dashboard]
\`\`\`

Each running VM is represented as a building with:
- Smoke animation when active
- Color-coded roof by team
- Floating status label

## Integration

Reads workspace state from the same REST API as the main dashboard.
Could be embedded as an alternative view in \`[[prd]]\`.
`,
    frontmatter: {
      status: "active",
      tags: ["r3f", "react", "3d", "project"],
      created: "2026-02-15",
    },
    backlinks: [],
    outlinks: ["analysis/prd.md"],
    tags: ["r3f", "react", "3d", "project"],
    modified_at: "2026-02-15T20:00:00Z",
  },
  {
    path: "r3f-farmstead/shader-notes.md",
    name: "shader-notes",
    content: `---
status: draft
tags: [r3f, shaders, graphics]
created: "2026-02-16"
---

# Shader Notes

## Toon Shading

Using a custom toon shader for the farmstead aesthetic:

\`\`\`glsl
uniform vec3 uColor;
uniform vec3 uLightDirection;
varying vec3 vNormal;

void main() {
  float intensity = dot(normalize(vNormal), normalize(uLightDirection));
  float step = smoothstep(0.3, 0.35, intensity);
  vec3 color = mix(uColor * 0.6, uColor, step);
  gl_FragColor = vec4(color, 1.0);
}
\`\`\`

## Smoke Particle System

Using instanced mesh with custom vertex displacement for chimney smoke.
Performance target: 60fps with 20 active buildings.

See \`[[project-overview]]\` for the full project context.
`,
    frontmatter: {
      status: "draft",
      tags: ["r3f", "shaders", "graphics"],
      created: "2026-02-16",
    },
    backlinks: [],
    outlinks: ["r3f-farmstead/project-overview.md"],
    tags: ["r3f", "shaders", "graphics"],
    modified_at: "2026-02-16T15:00:00Z",
  },
  {
    path: "inbox.md",
    name: "inbox",
    content: `---
tags: [inbox, scratch]
---

# Inbox

Quick capture for unprocessed thoughts.

- Look into WebSocket binary frames for exec streaming
- Consider adding \`workspace_health\` endpoint
- The \`[[architecture]]\` doc needs a networking diagram
- Check if \`react-force-graph-2d\` supports WebGL rendering
- Team DAG visualization could reuse the graph component
`,
    frontmatter: {
      tags: ["inbox", "scratch"],
    },
    backlinks: [],
    outlinks: ["analysis/architecture.md"],
    tags: ["inbox", "scratch"],
    modified_at: "2026-02-21T12:00:00Z",
  },
  {
    path: "daily/2026-02-21.md",
    name: "2026-02-21",
    content: `---
tags: [daily, log]
created: "2026-02-21"
---

# Daily Log — 2026-02-21

## Done

- Fixed swarm_run merge ownership failure
- Fixed merge_sequential diff fallback
- Added golden_workspace param to team create
- Flipped default_allow_internet to true
- Started frontend dashboard sprint
- Scaffolded React + Vite + TailwindCSS project

## In Progress

- Frontend kanban board implementation
- Vault browser + Tiptap editor
- REST API backend (axum)

## Blocked

- Nothing currently blocked

## Notes

- 859 unit tests passing (up from 815 after recent fixes)
- Swarm of 5 agents working on frontend sprint
- Using \`[[prd]]\` as primary reference for feature scope
`,
    frontmatter: {
      tags: ["daily", "log"],
      created: "2026-02-21",
    },
    backlinks: [],
    outlinks: ["analysis/prd.md"],
    tags: ["daily", "log"],
    modified_at: "2026-02-21T13:00:00Z",
  },
];

function buildTreeFromNotes(notes: VaultNote[]): VaultTreeNode[] {
  const folders = new Map<string, VaultFolder>();
  const rootChildren: VaultTreeNode[] = [];

  // Collect all folder paths
  for (const note of notes) {
    const parts = note.path.split("/");
    if (parts.length > 1) {
      const folderPath = parts.slice(0, -1).join("/");
      const folderName = parts[parts.length - 2];
      if (!folders.has(folderPath)) {
        folders.set(folderPath, { name: folderName, path: folderPath, children: [] });
      }
    }
  }

  // Assign notes to folders or root
  for (const note of notes) {
    const parts = note.path.split("/");
    if (parts.length > 1) {
      const folderPath = parts.slice(0, -1).join("/");
      const folder = folders.get(folderPath)!;
      folder.children.push({ type: "note", note });
    } else {
      rootChildren.push({ type: "note", note });
    }
  }

  // Build folder nodes
  const folderNodes: VaultTreeNode[] = [];
  for (const folder of folders.values()) {
    folder.children.sort((a, b) => {
      const nameA = a.type === "folder" ? a.folder.name : a.note.name;
      const nameB = b.type === "folder" ? b.folder.name : b.note.name;
      return nameA.localeCompare(nameB);
    });
    folderNodes.push({ type: "folder", folder });
  }

  // Sort: folders first, then notes
  folderNodes.sort((a, b) => {
    if (a.type !== b.type) return a.type === "folder" ? -1 : 1;
    const nameA = a.type === "folder" ? a.folder.name : a.note.name;
    const nameB = b.type === "folder" ? b.folder.name : b.note.name;
    return nameA.localeCompare(nameB);
  });

  return [...folderNodes, ...rootChildren];
}

export const mockVaultTree: VaultTreeNode[] = buildTreeFromNotes(mockVaultNotes);

export function buildGraphData(notes: VaultNote[]): VaultGraphData {
  const nodeMap = new Map<string, VaultGraphNode>();
  const links: VaultGraphLink[] = [];

  for (const note of notes) {
    const folder = note.path.includes("/") ? note.path.split("/")[0] : "";
    nodeMap.set(note.path, {
      id: note.path,
      name: note.name,
      folder,
      tags: note.tags,
      val: 1 + note.outlinks.length + note.backlinks.length,
    });
  }

  const notePaths = new Set(notes.map((n) => n.path));

  for (const note of notes) {
    for (const target of note.outlinks) {
      if (notePaths.has(target)) {
        links.push({ source: note.path, target });
      }
    }
  }

  return { nodes: Array.from(nodeMap.values()), links };
}

export const mockGraphData: VaultGraphData = buildGraphData(mockVaultNotes);

export function findNoteByPath(path: string): VaultNote | undefined {
  return mockVaultNotes.find((n) => n.path === path);
}

export function findNoteByName(name: string): VaultNote | undefined {
  return mockVaultNotes.find((n) => n.name === name);
}

export function searchNotes(query: string, useRegex: boolean): Array<{ path: string; name: string; line: number; context: string }> {
  const results: Array<{ path: string; name: string; line: number; context: string }> = [];
  let matcher: (line: string) => boolean;

  if (useRegex) {
    try {
      const re = new RegExp(query, "i");
      matcher = (line) => re.test(line);
    } catch {
      return results;
    }
  } else {
    const lowerQuery = query.toLowerCase();
    matcher = (line) => line.toLowerCase().includes(lowerQuery);
  }

  for (const note of mockVaultNotes) {
    const lines = note.content.split("\n");
    for (let i = 0; i < lines.length; i++) {
      if (matcher(lines[i])) {
        results.push({
          path: note.path,
          name: note.name,
          line: i + 1,
          context: lines[i].trim(),
        });
      }
    }
  }

  return results;
}

export function getBacklinks(notePath: string): Array<{ path: string; name: string; context: string }> {
  const note = findNoteByPath(notePath);
  if (!note) return [];

  const results: Array<{ path: string; name: string; context: string }> = [];
  const noteName = note.name;

  for (const other of mockVaultNotes) {
    if (other.path === notePath) continue;
    const lines = other.content.split("\n");
    for (const line of lines) {
      if (line.includes(`[[${noteName}]]`)) {
        results.push({
          path: other.path,
          name: other.name,
          context: line.trim(),
        });
        break;
      }
    }
  }

  return results;
}

export function getAllTags(): string[] {
  const tagSet = new Set<string>();
  for (const note of mockVaultNotes) {
    for (const tag of note.tags) {
      tagSet.add(tag);
    }
  }
  return Array.from(tagSet).sort();
}

export function getAllNotePaths(): string[] {
  return mockVaultNotes.map((n) => n.path);
}

export function getAllNoteNames(): string[] {
  return mockVaultNotes.map((n) => n.name);
}
