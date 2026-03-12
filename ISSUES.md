# Post-Migration UI/UX Differences

Comparison of pre-migration (SolidJS, commit 5e14bad) vs current (Dioxus 0.7).

## Session List Page

### Layout & Styling
- **Theme**: Old uses light (beige/cream) theme by default with system/light/dark selector dropdown. New is always dark with no theme selector.
- **Project labels**: Old shows real paths (`/Users/abe/Projects/ccmux`). New shows dash-escaped form (`-Users-abe-Projects-ccmux`).
- **Session cards**: Old uses flat list rows with subtle dividers. New uses rounded card boxes with heavier borders.
- **Session count**: Old shows count badge on right side of collapsible project header (e.g. `23`). New shows no count.
- **Sorting**: Old sorts projects by most recently updated session descending. New sorts alphabetically (BTreeMap).
- **Collapsibility**: Old project groups are collapsible with triangle toggle. New groups are static (not collapsible).
- **Date format**: Old shows localized dates with time (`3/12/2026, 12:23:42 AM`). New shows ISO-ish format without time precision (`2026-03-11 22:15`).
- **Event count label**: Old says "msgs". New says "events". <!-- USER NOTE: this can stay actually, "events" is more accurate -->
- **First message preview**: Old shows full first user message text and handles XML tags. New shows raw content including `<local-command-caveat>` XML tags. <!-- USER NOTE: Old frontend had some good logic for determining messages from the Human user from other messages that used the "user" type. Reuse this in the backend. -->

## Session View Page

### Header & Navigation
- **Back button**: Old has a styled `← Back` button. New has a `ccmux` link in the nav bar (always present).
- **Session title**: Old shows `Session <short-id>` with short UUID prefix. New shows project-escaped-name with `<n> items` count.
- **Live button**: Old has a `Live` toggle button for real-time streaming. New has no live button (streaming is background).
- **Raw button**: Old has a `Raw` toggle button that switches to raw JSONL log view. New has no raw view at all. <!-- USER NOTE: this toggle was separate from the /raw page - effectively it set the `{}` toggle on every message in the regular session view -->

### Message Blocks
- **User messages**: Old shows as a block with green left border, "User" label, and `{}` raw toggle button. New shows blue left border, "USER" uppercase label, and `^` collapse toggle.
- **Assistant messages**: Old shows green left border, "Assistant" label, model name (e.g. `claude-opus-4-6`), token count (e.g. `2 tok`), and `{}` raw toggle. New shows blue left border, "ASSISTANT" uppercase label, collapse toggle, no model/token info.
- **Message metadata**: Old shows per-block metadata: model name, total token count, `{}` raw JSON toggle on every message. New shows none of these.
- **Collapsibility**: Old: Full blocks (User, Assistant) are NOT collapsible — only grouped and individual tool blocks are. New: Every block has a collapse toggle including User and Assistant messages, which should not be collapsible.

### Grouped Tool Blocks
- **Collapsed display**: Old shows a single row: `▸ Thinking · ToolSearch · Glob ×3 · Read` — a summary of tool names with repeat counts, collapsed by default. New shows each tool as a separate row inside a group container, all visible (not collapsed).
- **Expanded display**: Old expands to show individual tool rows, each with model + token count + raw toggle. New has no expand/collapse — group items are always visible.
- **Summary labels**: Old generates smart summaries (tool name + key input like file path or pattern). New shows only tool name + generic "result" badge.
- **Repeat deduplication**: Old deduplicates repeated tools in summary (`Glob ×3`). New shows each individually.

### Tool-Specific Formatting
- **Bash**: Old shows expanded by default with syntax-highlighted command, structured output. New shows collapsed by default with raw JSON input dump.
- **Read**: Old strips line number prefixes, shows clean file content, renders images for image reads. New shows raw content with line number prefixes intact, no image rendering.
- **Edit**: Old shows diff-formatted view (old/new string comparison). New shows raw JSON with `old_string`/`new_string` fields.
- **Grep**: Old parses rg output into structured groups (file headers, line numbers, context). New shows raw rg output with file prefixes.
- **Write**: Old shows file path and content with syntax highlighting. New shows raw JSON input.
- **Glob**: Old shows pattern and matched files. New shows raw JSON.
- **ToolSearch**: Old shows query. New shows raw JSON.
- **Tool labels**: Old shows tool name + key input as description (e.g. `Read /Users/abe/Projects/ccmux/web/src/lib/json-tree.tsx`). New shows only tool name.
- **Tool results**: Old renders structured result content. New shows raw "result" badge with no content visible unless expanded.

### Raw Log View
- **Entire feature missing**: Old has a dedicated Raw view toggled by the `Raw` button, showing every JSONL event in a table: colored event type badge, short UUID, truncated raw JSON preview, timestamp. New has no equivalent. <!-- USER NOTE: This page was an intentional omission during the migration -->

### Per-Item Raw JSON Toggle
- **Missing**: Old has a `{}` button on every message/tool block that toggles inline raw JSON display. New has no per-item raw toggle.

## Styling & Visual Design

### Color Scheme
- **Old**: Light theme (beige/cream background, dark text) as default. System/Light/Dark selector. Dark theme should be gray with a brown tint, see screenshots in _assets.
- **New**: Dark only (dark navy background, light text). No theme selector.

### Typography
- **Old**: Serif heading font ("Sessions"), sans-serif body. Monospace for tool names/paths.
- **New**: Sans-serif throughout. Tool names in bold colored text.

### Block Borders
- **Old**: Colored left border (green for user, green for assistant, blue for groups). Blocks have subtle background fill.
- **New**: Colored left border (blue for user, blue for assistant, cyan for tools). Blocks have rounded borders with darker fill.

### Markdown Rendering
- **Old**: Full markdown rendering with syntax-highlighted code blocks (using highlight.js/shiki).
- **New**: Markdown rendering via pulldown-cmark (no syntax highlighting for code blocks).

## Missing Features (not in pre-migration either)

These are noted for completeness but were not regressions:
- Auto-scroll to bottom during live streaming
- SSE reconnect handling on disconnect
- Pagination for large session lists
