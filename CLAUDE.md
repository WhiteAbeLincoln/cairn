# ccmux

Session log viewer for Claude Code. Rust/Dioxus fullstack app.

## Development

```
cd crates/ccmux-app && dx serve   # fullstack dev server
```

Run `cargo clippy --workspace` and `cargo fmt --all` to lint and format after every change.
Fix any warnings or errors before committing.

## Browser Testing with Rodney

[Rodney](https://github.com/simonw/rodney) is available in the nix dev shell for browser automation. Use it to visually verify frontend changes.

Quick start:

```bash
rodney start                        # launch headless Chrome
rodney open http://localhost:5173   # navigate to the app
rodney screenshot shot.png          # capture the page
rodney click 'a[href*="/raw"]'      # click an element
rodney text '.some-selector'        # read text content
rodney js 'document.title'          # run JS and get result
rodney stop                         # shut down Chrome
```

Use an IIFE for a `js` command that needs to run multiple statements:

```bash
rodney js '(() => { const el = document.querySelector("[data-role=bash]"); el.scrollIntoView() })()'
```

Use `rodney --help` for the full command reference. Key capabilities:

- **Navigation**: `open`, `back`, `forward`, `reload`
- **Interaction**: `click`, `input`, `select`, `submit`, `hover`
- **Inspection**: `text`, `html`, `attr`, `url`, `title`, `screenshot`, `pdf`
- **JavaScript**: `js <expression>` — expressions auto-wrap as arrow functions, results print as JSON
- **Assertions**: `exists`, `visible`, `assert`, `count` — exit code 0 = pass, 1 = fail
- **Accessibility**: `ax-tree`, `ax-find`, `ax-node` — query the accessibility tree
- **Viewport**: `viewport` (get), `viewport 390 844` (set mobile), `viewport --reset` (restore default) — persists across commands
- **Waits**: `wait <selector>`, `waitload`, `waitstable`, `waitidle`

## Known Issues

- **Task list rendering**: TaskCreate/TaskUpdate runs that are split across multiple groups (separated by non-task tool calls) each only show the tasks from their own events. Later groups with TaskUpdate-only events resolve descriptions from the global toolUseMap, but the task list doesn't carry forward the full cumulative state from prior groups. Needs rework to maintain a running task snapshot across all groups.
