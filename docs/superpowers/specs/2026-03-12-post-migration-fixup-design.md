# Post-Migration UI/UX Fixup Design

Addresses the regressions and missing features documented in ISSUES.md, bringing the Dioxus 0.7 app to functional and visual parity with the pre-migration SolidJS app (commit `5e14bad`).

## Old Source Reference

All implementation work should refer to the old SolidJS source at commit `5e14bad` as the source of truth. Use `git show 5e14bad:<path>` to read files. Key files:

| Area | Old Source Path |
|------|----------------|
| Session list | `web/src/components/SessionList.tsx`, `web/src/components/SessionList.module.css` |
| Session view | `web/src/components/SessionView.tsx`, `web/src/components/SessionView.module.css` |
| Tool formatting | `web/src/components/blocks/ToolUseBlockView.tsx`, `web/src/components/blocks/ToolUseBlockView.module.css` |
| Tool extra labels | `web/src/components/blocks/ToolUseBlockView.tsx` (`toolExtraLabel` function) |
| Display item rendering | `web/src/components/blocks/DisplayItemView.tsx` |
| Display item types & grouping | `web/src/lib/display-item.ts` |
| Message block wrappers | `web/src/components/blocks/MessageBlock.tsx`, `web/src/components/blocks/CollapsibleBlock.module.css` |
| Collapsible block styling | `web/src/components/blocks/CollapsibleBlock.module.css` |
| Text utilities | `web/src/lib/format.ts` (`stripReadLineNumbers`, `stripAnsi`, `truncate`, `parseToolResultParts`) |
| Grep parser | `web/src/lib/grep-parse.ts` |
| Syntax highlighting | `web/src/lib/highlight.ts` |
| Theme toggle | `web/src/components/ThemeToggle.tsx`, `web/src/components/ThemeToggle.module.css` |
| Global CSS & themes | `web/src/app.css` |
| Agent block | `web/src/components/blocks/AgentBlockView.module.css` |
| Edit block | `web/src/components/blocks/EditBlockView.module.css` |
| Task list | `web/src/components/blocks/TaskListView.module.css` |

Screenshots of the old app are in `_assets/` (session-list.png, session-view.png, bash-blocks.png, question-block.png, read-image-preview.png, raw-logs.png).

## Scope

**In scope:** Everything in ISSUES.md except items explicitly marked as out-of-scope.

**Out of scope:**
- Raw Log View page (intentional omission)
- "events" vs "msgs" label (keeping "events")
- Live toggle button (replaced by always-on streaming + jump-to-bottom FAB in Section 1.6)

## Approach: Three-Phase Foundation → Features → Visual

- **Phase 1 — Foundation:** Backend metadata extraction + shared frontend infrastructure
- **Phase 2 — Features:** Tool formatters, grouped blocks, session list fixes, streaming FAB
- **Phase 3 — Visual:** Theme system, color scheme, typography, syntax highlighting

---

## Phase 1: Foundation

### 1.1 Backend: Metadata on DisplayItem

Add per-item metadata extracted from assistant events so the frontend can display model, tokens, and UUID without parsing raw JSON.

**New struct in `crates/ccmux-core/src/display/mod.rs`:**

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ItemMeta {
    pub uuid: Option<String>,
    pub model: Option<String>,
    pub tokens: Option<u64>,
}
```

**Changes to `DisplayItem`:**
- Add `meta: ItemMeta` to `UserMessage` (uuid only), `AssistantMessage`, `Thinking`, `ToolUse`, and `Group`
- `Group.meta` is an aggregate: sum of children's tokens, model from first child, no single UUID

**Changes to `pipeline.rs`:**
- In `assistant_event_items()`, extract `model` from `message.model`, `usage.output_tokens` from `message.usage`, and `uuid` from `CoreEventData.uuid`
- Attach `ItemMeta` to each emitted `DisplayItem`
- In `user_event_items()`, extract `uuid` from `CoreEventData.uuid`

**Changes to `flush_grouped()`:**
- Compute aggregate `ItemMeta` for the `Group` wrapper

**Streaming:** `StreamEvent::Append` and `StreamEvent::MergeWithPrevious` already carry `DisplayItem`, so metadata comes for free.

**Old source reference:** The old app extracted model from `event.message.model`, tokens from `event.message.usage.output_tokens`, and UUID from `event.uuid`. See `5e14bad:web/src/components/SessionView.tsx` and `5e14bad:web/src/components/blocks/DisplayItemView.tsx`.

### 1.2 Backend: Session List Fixes

All changes in `crates/ccmux-core/src/session/loader.rs` and `crates/ccmux-app/src/server_fns.rs`.

**Project path unescaping:** Replace dash-escaped project directory names with real paths. Use the `project_path` field (extracted from the first event's `cwd`). If unavailable, unescape by converting leading-dash-separated segments back to `/`-separated path.

**Sort order:** In `list_sessions()`, return projects sorted by most recent `updated_at` within each group (descending). Replace the frontend's `BTreeMap` grouping with server-side sorted `Vec<(String, Vec<SessionMeta>)>`.

**First message cleanup:** In `loader.rs` where `first_message` is extracted, walk events in order and select the first event matching all criteria as the `first_message`. If no event matches, leave `first_message` as `None`. Criteria:
1. Must be `user_type: "external"` with `role: "user"`
2. No `tool_use_result` or `source_tool_assistant_uuid`
3. Content must not start with `<local-command` or similar CLI-generated XML tags — these are generated by the Claude Code CLI and are not user messages
4. Strip any remaining XML tags from the selected message's content
5. Truncate to ~200 chars server-side

**Old source reference:** `5e14bad:web/src/components/SessionList.tsx` (grouping, sorting, display logic), `5e14bad:web/src/lib/session.ts` (message filtering).

### 1.3 Frontend: Block Header Rework — MessageBlock

Replace the current minimal `MessageBlock` with a component supporting two rendering modes:

**Full block mode:**
- Left colored border
- Header bar: left side has role/tool label + contextual extra label; right side has metadata (model, token count, short UUID) + action buttons
- Action buttons: raw `{}` toggle (visible on wide viewports), collapse `^`/`v` toggle (conditional — not shown on User/Assistant message blocks)
- Expandable body below header
- Below 768px: raw toggle and collapse move into a `⋮` kebab menu

**Minimal/collapsed row mode:**
- Single-line summary: tool name + extra label + metadata on right
- No body, no border
- Clicking expands to full block mode
- Used for tool items inside a collapsed group and for standalone collapsed items

**Old source reference:** `5e14bad:web/src/components/blocks/MessageBlock.tsx`, `5e14bad:web/src/components/blocks/MessageBlock.module.css`, `5e14bad:web/src/components/blocks/CollapsibleBlock.module.css`.

### 1.4 Frontend: Per-Block Raw JSON Toggle

- `{}` button on each block header toggles a `<pre>` with pretty-printed JSON from the `raw: Value` field
- Raw JSON renders below the normal content (not replacing it)
- Toggle state is per-block (local signal), but controlled by a shared "raw mode" signal (see below)
- On wide viewports: `{}` button visible in header action area
- On narrow viewports: "Raw JSON" item in kebab `⋮` menu

**Bulk raw toggle ("Raw" button):** A `Raw` button in the session view header (next to the back button / session title) toggles all blocks to raw mode simultaneously. This sets a shared signal that overrides each block's local raw state. Clicking `Raw` again turns all raw views off. Individual per-block `{}` toggles still work independently — they flip the local override for that block. This matches the old app's `Raw` button behavior.

**Old source reference:** `5e14bad:web/src/components/SessionView.tsx` (Raw button in header, raw toggle propagation to all blocks).

### 1.5 Frontend: Group Block

New `GroupBlock` component replacing the current flat `div.group-block`.

**Collapsed state (default):**
- Single summary row: `▸ Thinking · ToolSearch · Glob ×3 · Read /path/to/file`
- Dedup by (kind, name): count occurrences, display in first-seen order
- Single occurrences show no count suffix; multiples show `×N`
- Extra labels from the last occurrence of each type
- Aggregate metadata on the right: total tokens, model

**Expanded state:**
- Each child rendered as a minimal/collapsed row (MessageBlock minimal mode)
- Clicking an individual row expands to full block inline
- `▾` toggle to re-collapse the whole group

**Summary generation algorithm:**
1. Walk `Group.items`, track first-seen order of each `(kind, name)` pair
2. Count total occurrences of each
3. Render in first-seen order with `·` separators (use indexed iteration with conditional separator in RSX)
4. Extra label from the *last* occurrence of each type

**Old source reference:** `5e14bad:web/src/components/SessionView.tsx` (internal-group rendering, step summary generation), `5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` (`toolExtraLabel` function), `5e14bad:web/src/components/blocks/CollapsibleBlock.module.css`.

### 1.6 Frontend: Jump-to-Bottom FAB

- Floating action button (down-arrow) in the session view
- Appears when user scrolls up from the bottom (threshold: ~200px)
- Clicking smooth-scrolls to the bottom
- Disappears when at/near the bottom
- Scroll listener on the session items container

---

## Phase 2: Features

### 2.1 Session List Page Fixes

**Collapsible project groups:** Triangle toggle on each project header. Default expanded. Persist state in-session (signal, not localStorage).

**Session count badge:** Render `sessions.len()` next to project name in group header.

**Date format:** Switch to localized format (`3/12/2026, 12:23:42 AM`). Use `chrono` with a fixed US-style format on the server (e.g., `"%-m/%-d/%Y, %-I:%M:%S %p"`). If true browser-locale formatting is needed later, it can be added as a client-side progressive enhancement via `js_sys::Date`.

**Session card style:** Flat list rows with subtle dividers instead of rounded card boxes. Pure CSS change.

**Session title in session view:** Change from project name + item count to `Session <short-id>` (first 6 chars of UUID). Add a styled `← Back` button.

**Old source reference:** `5e14bad:web/src/components/SessionList.tsx`, `5e14bad:web/src/components/SessionList.module.css`.

### 2.2 Tool Formatters — Tier 1 (Bash, Read, Edit)

Replace generic raw JSON display with tool-specific formatting. The `ToolUseBlock` component dispatches on `name` to dedicated sub-components.

**Bash:**
- Label extra: `description` field from input
- Input: Syntax-highlighted command with `$ ` prefix
- Result: stdout/stderr separated, ANSI escape codes stripped, error badge on non-zero exit

**Read:**
- Label extra: `file_path` from input
- Result: Strip line-number prefixes (`    N\t` format), render images for image reads (detect base64 content blocks)

**Edit:**
- Label extra: `file_path` from input (+ "all" badge if `replace_all`)
- Input: Diff view showing old string → new string
- Result: Success/error status

**Old source reference:** `5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` (`BashView`, `ReadView`, `EditView`), `5e14bad:web/src/lib/format.ts` (`stripReadLineNumbers`, `stripAnsi`, `parseToolResultParts`).

### 2.3 Tool Formatters — Tier 2 (Grep, Write, Glob)

**Grep:**
- Label extra: `pattern` from input
- Result: Parse rg output into file-grouped sections with line numbers and context lines

**Write:**
- Label extra: `file_path` from input
- Result: File content display

**Glob:**
- Label extra: `pattern` from input
- Result: Matched file list

**Old source reference:** `5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` (`GrepView`, `WriteView`, `GlobView`), `5e14bad:web/src/lib/grep-parse.ts`.

### 2.4 Tool Formatters — Tier 3 (ToolSearch, WebSearch, Agent, AskUserQuestion)

**ToolSearch:**
- Label extra: `query` from input
- Result: Tool name badges with deferred count

**WebSearch:**
- Label extra: `query` from input
- Result: Links list + collapsible summary

**Agent:**
- Label extra: `subagent_type · description` from input
- Result: Link to subagent session + output rendered as prose

**AskUserQuestion:**
- Structured question display with user's answer from tool result

**Old source reference:** `5e14bad:web/src/components/blocks/ToolUseBlockView.tsx` (remaining view functions), `5e14bad:web/src/components/blocks/AgentBlockView.module.css`.

### 2.5 Collapsibility Defaults

- User and Assistant messages: **not collapsible** (no collapse toggle), always open. Raw `{}` toggle only.
- Bash and AskUserQuestion (full-mode tools): collapsible, default **open**
- Tool blocks inside an expanded group: default **closed** (minimal row), expand on click
- Thinking blocks: default **closed** with text preview
- Groups: default **closed** (summary row)

**Old source reference:** `5e14bad:web/src/components/SessionView.tsx` (expand/collapse defaults).

---

## Phase 3: Visual Polish

### 3.1 Theme System

System/light/dark theme support with selector dropdown in the nav bar.

- CSS custom properties for all colors in `:root` (light), `[data-theme="dark"]`, and `@media (prefers-color-scheme: dark)` for system mode
- **Light theme (default):** Warm beige/cream background (`#f4f2ed`), dark text (`#2c2a25`)
- **Dark theme:** Gray with brown tint (not current navy) — per screenshots in `_assets/`
- Persist preference to `localStorage`, read on load
- `ThemeToggle` component in nav bar, sets `data-theme` on `<html>`

**Old source reference:** `5e14bad:web/src/components/ThemeToggle.tsx`, `5e14bad:web/src/components/ThemeToggle.module.css`, `5e14bad:web/src/app.css`.

### 3.2 Color Scheme

- **Borders:** Green left border for User and Assistant blocks, blue for groups
- **Backgrounds:** Subtle fill on blocks, not heavy rounded borders
- **Badges:** Error in red, success in green
- **Session list:** Flat rows with subtle dividers

**Old source reference:** `5e14bad:web/src/app.css`, `5e14bad:web/src/components/SessionView.module.css`.

### 3.3 Typography

- **Headings:** Serif font for page titles ("Sessions")
- **Body:** Sans-serif
- **Code/paths:** Monospace for tool names, file paths, code blocks
- **Labels:** Title case ("User", "Assistant") not uppercase

**Old source reference:** `5e14bad:web/src/app.css`.

### 3.4 Markdown & Code Highlighting

Add syntax highlighting to code blocks in the `Prose` component using **syntect**.

- Integrate syntect **server-side** to avoid bloating the WASM binary (~2-5MB for syntax definitions). Highlighting can happen via a server function that returns HTML with inline styles, or during `DisplayItem` construction in the pipeline
- Detect language from code fence info strings
- Theme-aware: light/dark syntax themes matching the app theme
- Use `fileExtToLang` equivalent for tool-specific code (Bash commands, file content in Read/Write)

**Old source reference:** `5e14bad:web/src/lib/highlight.ts` (language detection, theme selection).

### 3.5 Block Styling

- Remove heavy rounded borders and dark card fill
- Subtle background fill, colored left border, clean spacing
- Group blocks: minimal chrome, dot separators between summary items
- Tool rows inside groups: compact with hover highlight

**Old source reference:** `5e14bad:web/src/components/SessionView.module.css`, `5e14bad:web/src/components/blocks/CollapsibleBlock.module.css`, `5e14bad:web/src/components/blocks/ToolUseBlockView.module.css`.
