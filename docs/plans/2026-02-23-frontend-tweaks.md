## Frontend Dashboard — Styling Addendum

**Date**: 2026-02-23
**Companion to**: `2026-02-23-frontend-dashboard-bugs-and-tasks.md`
**Based on**: 30+ Playwright screenshots (desktop/tablet/mobile), full theme.css + 28 component source review
**Current state**: Espresso dark theme (#0A0A0A bg, #5C4033 accent), Inter + JetBrains Mono fonts

---

### Executive Summary

The dashboard is structurally solid — layout, accessibility, responsive design, and code splitting are all well-executed. But visually it reads as a **first-pass wireframe** rather than a finished product. The main issues are: (1) insufficient visual hierarchy between empty space and content, (2) cards and panels that feel flat despite having borders, (3) the vast majority of the viewport is empty #0A0A0A black with no texture, and (4) action buttons use bare lucide icons at 12px that are hard to see and hard to hit.

The espresso theme palette is genuinely good — warm browns + muted text on near-black is distinctive and comfortable for long sessions. The improvements below preserve the palette entirely and focus on **depth, polish, and information density**.

---

### S1. Column Headers — Add Weight and Separation

**Current**: Column headers (CREATING, RUNNING, IDLE, STOPPED) are 14px uppercase text with a small colored dot and a count badge floating at the right edge. They sit directly on the #0A0A0A background with no container, making them feel detached from their columns.

**Problem**: At 1920px, the four columns spread across the full viewport width with nothing but the dot + label + badge. Empty columns are indistinguishable from background. When there's only 1 workspace (common during development), 3 of 4 columns are completely blank.

**Recommendations**:

1. **Add a subtle background strip** to column headers:
   ```css
   bg-[var(--surface)] rounded-t-lg px-3 py-2.5 mb-1
   ```

2. **Replace the bare count badge** with a colored pill that matches the column's status color at 15% opacity:
   ```tsx
   <span
     className="text-xs font-mono px-2 py-0.5 rounded-full"
     style={{ backgroundColor: `${accentColor}20`, color: accentColor }}
   >
     {count}
   </span>
   ```

3. **Empty column placeholder**: Instead of just blank space, add a dashed-border drop zone hint:
   ```css
   min-h-[120px] border-2 border-dashed border-[var(--border)] rounded-lg
   flex items-center justify-center
   ```
   With subtle text: "Drop here" or "No workspaces".

---

### S2. Workspace Cards — Add Depth and Visual Richness

**Current**: Cards have `rounded-lg border border-[#252018] bg-[#262220]` — a barely-visible 1px border on a surface that's only 4 shades lighter than the background. The hover effect (`scale(1.01)`) is good but the resting state is too flat.

**Recommendations**:

1. **Add a subtle inner shadow** for depth:
   ```css
   shadow-[inset_0_1px_0_0_rgba(255,255,255,0.03)]
   ```

2. **Left accent stripe** based on workspace state (like TaskCard already has for priority). Running = green stripe, Stopped = red stripe:
   ```tsx
   <div className="h-full w-1 rounded-l-lg absolute left-0 top-0 bottom-0"
     style={{ backgroundColor: stateColor }} />
   ```

3. **Card footer action buttons** — add tooltip on hover:
   ```tsx
   <span className="absolute -top-7 left-1/2 -translate-x-1/2 text-[10px] bg-[var(--surface-3)]
     px-1.5 py-0.5 rounded opacity-0 group-hover:opacity-100 pointer-events-none
     transition-opacity whitespace-nowrap">
     {title}
   </span>
   ```

4. **Stronger hover state**: Replace subtle `scale(1.01)` with visible border glow:
   ```css
   .card-hover:hover {
     border-color: var(--accent);
     box-shadow: 0 0 0 1px var(--accent), 0 2px 8px rgba(92, 64, 51, 0.15);
   }
   ```

---

### S3. TopBar — Warmer, More Grounded

1. **Logo upgrade**: Replace plain brown square with gradient:
   ```tsx
   style={{ background: 'linear-gradient(135deg, #5C4033 0%, #7A5A4A 100%)' }}
   ```

2. **Search bar**: More prominent with inner shadow + focus ring:
   ```css
   h-9 shadow-[inset_0_1px_2px_rgba(0,0,0,0.3)] focus-within:ring-1 focus-within:ring-[var(--accent)]/50
   ```

---

### S4. Sidebar — Tighter Spacing, Active State Polish

1. **Active tab — filled pill instead of underline**:
   ```tsx
   className={sidebarSection === id
     ? 'text-[var(--text)] bg-[var(--accent)]/15 rounded-md'
     : 'text-[var(--text-muted)] hover:text-[var(--text)]'
   }
   ```

2. **Tree item selected state** — highlight when open in pane:
   ```css
   bg-[var(--accent)]/10 text-[var(--text)] border-r-2 border-[var(--accent)]
   ```

3. **Empty vault message**: Use the existing decorative CSS shapes (defined in theme.css but currently unused):
   ```tsx
   <div className="empty-state-shape-circle mb-3" />
   <p className="text-xs text-[var(--text-dim)]">No notes yet</p>
   <button className="mt-2 text-xs text-[var(--accent-hover)]">Create your first note</button>
   ```

---

### S5. Detail Drawer — Richer Sections

1. **Section cards**: Wrap each section in a subtle container:
   ```css
   bg-[var(--surface-2)] rounded-lg p-3
   ```

2. **IP address**: Add a copy button on hover. **State**: Show as colored badge pill. **Uptime**: Running timer with monospace digits.

3. **Action buttons**: Add subtle background:
   ```css
   rounded-lg bg-[var(--surface-2)] hover:bg-[var(--accent)]/15
   ```

---

### S6. Terminal — More Polished Chrome

1. **Connection status**: Compact pill instead of text:
   ```tsx
   <span className="w-1.5 h-1.5 rounded-full bg-[var(--success)]" />
   <span className="text-[var(--text-muted)]">farts</span>
   ```

2. **Toolbar** (planned): `[ A- ] [ A+ ] [ Search ] [ Clear ] [ Copy ]` — same icon button style as card actions.

3. **Terminal background**: Match xterm.js bg to `#0A0A0A` instead of pure `#000000`.

---

### S7. Team Detail — Needs Structure

1. **Team header**: Large icon + name + member count + status badge:
   ```tsx
   <div className="w-10 h-10 rounded-lg bg-[var(--accent)]/20 flex items-center justify-center">
     <Users size={20} className="text-[var(--accent-hover)]" />
   </div>
   ```

2. **Empty states**: Use the existing decorative CSS shapes from theme.css (`empty-state-shape-lines`, `empty-state-shape-circle`).

3. **Messages**: Style as left-aligned chat bubbles when messages exist.

---

### S8. Notification Center — Minor Tweaks

- Unread dot: `w-2 h-2` (currently tiny)
- Category pills: Add `bg-[var(--surface-2)]` background, active = `bg-[var(--accent)]/20`
- Panel width: Widen from ~200px to 280px

---

### S9. Create Workspace Dialog

- Name input: Add `font-mono` class (workspace names are identifiers)
- Dialog: Add top accent line `border-t-2 border-[var(--accent)]`
- Create button: Spinner during submission

---

### S10. Command Palette Results (once bug is fixed)

- Result items: icon + label + category badge
- Active highlight: `bg-[var(--accent)]/10 border-l-2 border-[var(--accent)]`
- Keyboard hint footer: `↑↓ Navigate  ↵ Open  esc Close`

---

### S11. Mobile View

- Action buttons: Increase to 16px icons, `w-10 h-10` touch targets
- Column headers: Sticky on scroll
- Bottom padding: `pb-20` for thumb reach

---

### S12. Global Polish

1. Loading skeleton shimmer during initial data fetch
2. Smooth count badge transitions
3. Proper favicon (accent brown)
4. Dynamic page title: `agentiso — 3 running, 1 stopped`
5. Subtle background noise texture at 1.5% opacity (CSS-only, optional)
6. Toast bounce animation
7. Scrollbar thumb width 7px → 8px

---

### Priority Order

| Priority | Item | Effort | Impact |
|----------|------|--------|--------|
| 1 | S2 — Card depth (inner shadow + state stripe + hover glow) | 30 min | High |
| 2 | S1 — Column headers (background strip + empty state) | 20 min | High |
| 3 | S5 — Detail drawer sections | 20 min | Medium |
| 4 | S4 — Sidebar active states | 20 min | Medium |
| 5 | S3 — TopBar polish | 15 min | Medium |
| 6 | S7 — Team detail structure | 30 min | Medium |
| 7 | S6 — Terminal chrome | 30 min | Medium |
| 8 | S12 — Global polish | 30 min | Low-Medium |
| 9-12 | S8-S11 | 50 min | Low |

**Total**: ~4 hours. **S1-S4 alone (~90 min) produce the biggest visual transformation.**

All changes are frontend-only CSS/Tailwind, zero new dependencies, preserve the espresso palette, maintain 143KB gzip bundle.
