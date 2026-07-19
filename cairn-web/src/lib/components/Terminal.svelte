<!--
  The terminal: the ONLY module that imports wterm. It measures its container,
  attaches to a session with the right initial size (so the PTY is correct from
  the first byte), pipes snapshot/output bytes into the ghostty-backed grid,
  forwards keystrokes/resizes back through an `AttachController`, and renders the
  exit / kicked / disconnected overlays. Task 9 compiles this file to the
  `<cairn-terminal>` web component, so wterm stays fully encapsulated here.
-->
<script lang="ts">
import { WTerm } from '@wterm/dom';
import '@wterm/dom/css';
import { GhosttyCore } from '@wterm/ghostty';
import { onMount, untrack } from 'svelte';
import { describeExit } from '$lib/format';
import type { DaemonClient } from '$lib/protocol';
import { AttachController, type AttachPhase } from '$lib/terminal/attachController';

interface Props {
    client: DaemonClient;
    sessionId: string;
    /** Optional font overrides (also the future `font-size` / `font-family` web-component attrs). */
    fontSize?: number;
    fontFamily?: string;
    /**
     * A monotonically increasing signal from the parent (bumped on a connection
     * `connected` transition). Each increment triggers a reattach *if* the
     * terminal is currently disconnected/kicked — this is how reconnect recovery
     * integrates without the terminal reaching into the connection store.
     */
    reattachSignal?: number;
    onPhase?: (phase: AttachPhase) => void;
}

let { client, sessionId, fontSize, fontFamily, reattachSignal = 0, onPhase }: Props = $props();

let host: HTMLDivElement;
let phase = $state<AttachPhase>({ kind: 'connecting' });

let term: WTerm | undefined;
let controller: AttachController | undefined;
let loadError = $state<string | undefined>(undefined);
let termCorrupted = false;
const encoder = new TextEncoder();

const hostStyle = $derived(
    [
        fontSize ? `--term-font-size:${fontSize}px` : '',
        fontFamily ? `--term-font-family:${fontFamily}` : '',
    ]
        .filter(Boolean)
        .join(';'),
);

/**
 * Measure the grid the same way wterm does — a hidden `.term-row > span` probe
 * under a `.wterm` context so the terminal CSS applies — and divide the host's
 * content box by the cell size. wterm's own ResizeObserver settles on the same
 * numbers, so attaching with these dimensions right-sizes the PTY immediately.
 */
function measureGrid(hostEl: HTMLElement): { cols: number; rows: number } {
    const probe = document.createElement('div');
    probe.className = 'wterm';
    probe.style.cssText = 'position:absolute;left:-9999px;top:0;visibility:hidden;padding:0';
    if (fontSize) probe.style.setProperty('--term-font-size', `${fontSize}px`);
    if (fontFamily) probe.style.setProperty('--term-font-family', fontFamily);
    const grid = document.createElement('div');
    grid.className = 'term-grid';
    const row = document.createElement('div');
    row.className = 'term-row';
    const span = document.createElement('span');
    span.textContent = 'W';
    row.appendChild(span);
    grid.appendChild(row);
    probe.appendChild(grid);
    document.body.appendChild(probe);
    const charWidth = span.getBoundingClientRect().width;
    const rowHeight = row.getBoundingClientRect().height;
    probe.remove();

    const cs = getComputedStyle(hostEl);
    const padX = (parseFloat(cs.paddingLeft) || 0) + (parseFloat(cs.paddingRight) || 0);
    const padY = (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0);
    const contentW = Math.max(0, hostEl.clientWidth - padX);
    const contentH = Math.max(0, hostEl.clientHeight - padY);
    const cols = charWidth > 0 ? Math.max(1, Math.floor(contentW / charWidth)) : 80;
    const rows = rowHeight > 0 ? Math.max(1, Math.floor(contentH / rowHeight)) : 24;
    return { cols, rows };
}

function writeToTerm(bytes: Uint8Array): void {
    try {
        term?.write(bytes);
    } catch (e) {
        termCorrupted = true;
        throw e;
    }
}

function createTerm(core: GhosttyCore): WTerm {
    return new WTerm(host, {
        core,
        autoResize: true,
        onData: (data: string) => controller?.write(encoder.encode(data)),
        onResize: (c: number, r: number) => controller?.resize(c, r),
    });
}

function startAttach(cols: number, rows: number): void {
    controller = new AttachController(client, {
        onSnapshot: writeToTerm,
        onOutput: writeToTerm,
        onPhase: (p) => {
            phase = p;
            onPhase?.(p);
        },
    });
    controller.start(sessionId, { cols, rows, noStdin: false });
}

async function reattach(): Promise<void> {
    controller?.stop();
    phase = { kind: 'connecting' };
    if (termCorrupted) {
        const cols = term?.cols ?? 80;
        const rows = term?.rows ?? 24;
        term?.destroy();
        term = undefined;
        termCorrupted = false;
        try {
            const core = await GhosttyCore.load();
            const t = createTerm(core);
            t.cols = cols;
            t.rows = rows;
            await t.init();
            term = t;
        } catch (err) {
            loadError = err instanceof Error ? err.message : String(err);
            return;
        }
    }
    startAttach(term?.cols ?? 80, term?.rows ?? 24);
    term?.focus();
}

onMount(() => {
    let disposed = false;

    void (async () => {
        try {
            const core = await GhosttyCore.load();
            if (disposed) return;

            // Construct first (adds the `.wterm` class + padding), then size from
            // the measured grid before init so the first render is at the real
            // size and the PTY is right-sized from the first byte.
            const t = createTerm(core);
            const { cols, rows } = measureGrid(host);
            t.cols = cols;
            t.rows = rows;
            await t.init();
            if (disposed) {
                t.destroy();
                return;
            }
            term = t;
            startAttach(cols, rows);
            t.focus();
        } catch (err) {
            if (!disposed) loadError = err instanceof Error ? err.message : String(err);
        }
    })();

    return () => {
        disposed = true;
        // detach-then-close is the normal teardown; wterm cleanup follows.
        controller?.stop();
        term?.destroy();
    };
});

// Reconnect recovery: when the parent bumps `reattachSignal` (a connection
// `connected` transition) and we're currently in a recoverable failure state,
// reattach. `phase` is read untracked so this effect fires only on the signal,
// never re-looping on its own phase writes.
$effect(() => {
    const signal = reattachSignal;
    if (signal === 0) return; // initial value: nothing to recover yet
    untrack(() => {
        if (phase.kind === 'disconnected' || phase.kind === 'error') reattach();
    });
});
</script>

<div class="terminal-root">
    <div class="term-host" class:hidden={!!loadError} bind:this={host} style={hostStyle}></div>

    {#if loadError}
        <div class="overlay">
            <div class="overlay-card">
                <p class="overlay-title">Terminal failed to load</p>
                <p class="overlay-msg">{loadError}</p>
            </div>
        </div>
    {:else if phase.kind === 'exited'}
        <div class="overlay">
            <div class="overlay-card">
                <p class="overlay-title">Session exited</p>
                <p class="overlay-msg">{describeExit(phase.status)}</p>
                <a class="btn" href="/sessions">Back to sessions</a>
            </div>
        </div>
    {:else if phase.kind === 'error'}
        <div class="overlay">
            <div class="overlay-card">
                <p class="overlay-title">Detached</p>
                <p class="overlay-msg">{phase.message} ({phase.code})</p>
                <button type="button" class="btn btn-primary" onclick={reattach}>Reattach</button>
            </div>
        </div>
    {:else if phase.kind === 'disconnected'}
        <div class="overlay">
            <div class="overlay-card">
                <p class="overlay-title">Connection lost</p>
                {#if phase.message}<p class="overlay-msg">{phase.message}</p>{/if}
                <button type="button" class="btn btn-primary" onclick={reattach}>Reattach</button>
            </div>
        </div>
    {:else if phase.kind === 'connecting'}
        <div class="overlay overlay-transient">
            <div class="overlay-card"><p class="overlay-msg">Attaching…</p></div>
        </div>
    {/if}
</div>

<style>
    /* Fill the container by absolute positioning rather than a percentage-height
       chain (which doesn't resolve through block flex items). The container just
       needs to be positioned and sized — the contract for the future
       `<cairn-terminal>` web component too. */
    .terminal-root {
        position: absolute;
        inset: 0;
        display: flex;
        flex-direction: column;
        overflow: hidden;
        border-radius: var(--radius);
    }

    .term-host {
        flex: 1;
        min-height: 0;
        width: 100%;
    }

    .term-host.hidden {
        visibility: hidden;
    }

    .overlay {
        position: absolute;
        inset: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 1rem;
        background: color-mix(in srgb, var(--color-bg) 55%, transparent);
        z-index: 5;
    }

    /* The "attaching" flash shouldn't dim the terminal or block clicks. */
    .overlay-transient {
        background: transparent;
        pointer-events: none;
    }

    .overlay-card {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 0.75rem;
        padding: 1.25rem 1.5rem;
        max-width: min(28rem, 100%);
        text-align: center;
        background: var(--color-surface);
        border: 1px solid var(--color-border);
        border-radius: var(--radius);
        box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
    }

    .overlay-title {
        margin: 0;
        font-weight: 600;
    }

    .overlay-msg {
        margin: 0;
        font-size: 0.875rem;
        color: var(--color-text-muted);
        overflow-wrap: anywhere;
    }
</style>
