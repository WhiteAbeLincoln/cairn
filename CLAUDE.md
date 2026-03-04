# ccmux

Session log viewer for Claude Code. Rust/Axum GraphQL backend, SolidJS frontend.

## Development

```
cargo run          # backend on :3001
cd web && bun dev  # frontend on :5173
cd web && bun run check # typecheck frontend
```

## GraphQL Schema & TypeScript Codegen

Rust types are the source of truth. After changing GraphQL types in `src/graphql/types.rs`, run from the project root:

```bash
cargo run --bin export_schema > schema.graphql && bun run --cwd web codegen
```

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
- **Waits**: `wait <selector>`, `waitload`, `waitstable`, `waitidle`
