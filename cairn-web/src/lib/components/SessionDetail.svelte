<!--
  Session detail: header (back / name / Rename / Kill), the live terminal
  filling the viewport, and a compact metadata bar. A session that was already
  exited when the page loaded shows a static exit panel instead of attaching;
  one that exits (or evicts us) while attached is handled by the terminal's own
  overlay. Reconnect recovery is bridged in: a connection `connected` transition
  bumps `reattachSignal`, which the terminal acts on only when disconnected.
-->
<script lang="ts">
import { onMount, untrack } from 'svelte';
import Terminal from '$lib/components/Terminal.svelte';
import { commandBasename, describeExit, relativeTime } from '$lib/format';
import type { SessionInfo } from '$lib/protocol';
import { getClient, onConnectionStatusChange } from '$lib/stores/connection.svelte';
import { refreshSessions } from '$lib/stores/sessions.svelte';
import type { AttachPhase } from '$lib/terminal/attachController';

interface Props {
    session: SessionInfo;
    onUpdated: (info: SessionInfo) => void;
}

const { session, onUpdated }: Props = $props();

const client = $derived(getClient());

// Latched at mount (the view is keyed by session id, so this is per session):
// only a session that was already exited when opened shows the static panel. A
// live session that exits later intentionally keeps the terminal and its exit
// overlay, so we read the initial value once via `untrack`.
const startedExited = untrack(() => session.exit !== undefined);

let killing = $state(false);
let renaming = $state(false);
// Seeded only when rename mode is entered (see the button below) rather than
// at declaration time, so it doesn't capture a stale snapshot of the `session`
// prop if the session is renamed elsewhere and re-fetched.
let renameValue = $state('');
let editingName = $state(false);
let actionError = $state<string | undefined>(undefined);
let reattachSignal = $state(0);

// A connection `connected` transition (initial connect or reconnect recovery)
// nudges the terminal to reattach if it dropped. Transitions only — the
// reconnect controller already de-dupes steady-state re-probes.
onMount(() =>
    onConnectionStatusChange((s) => {
        if (s.state === 'connected') reattachSignal += 1;
    }),
);

async function refreshInfo(): Promise<void> {
    const c = getClient();
    if (!c) return;
    const fresh = await c.inspect(session.id);
    onUpdated(fresh);
    refreshSessions();
}

// Keep the metadata bar honest across the attach lifecycle: (re)attaching or
// eviction changes the attached-client count, and an exit needs the exit tag.
// Transient states (connecting/disconnected) carry no new server-side info.
function handlePhase(phase: AttachPhase): void {
    if (phase.kind === 'attached' || phase.kind === 'exited' || phase.kind === 'error') {
        void refreshInfo();
    }
}

async function handleKill(): Promise<void> {
    const client = getClient();
    if (!client || killing) return;
    killing = true;
    actionError = undefined;
    try {
        await client.kill(session.id, { tag: 'named', val: 'term' });
        await refreshInfo();
    } catch (err) {
        actionError = err instanceof Error ? err.message : String(err);
    } finally {
        killing = false;
    }
}

async function handleRename(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    const client = getClient();
    const trimmed = renameValue.trim();
    if (!client || !trimmed || renaming) return;
    renaming = true;
    actionError = undefined;
    try {
        await client.rename(session.id, trimmed);
        await refreshInfo();
        editingName = false;
    } catch (err) {
        actionError = err instanceof Error ? err.message : String(err);
    } finally {
        renaming = false;
    }
}
</script>

<div class="detail">
    <div class="header">
        <a href="/sessions" class="back">&larr; Sessions</a>
        {#if editingName}
            <form class="rename-form" onsubmit={handleRename}>
                <input type="text" bind:value={renameValue} autocomplete="off" aria-label="Session name" />
                <button type="submit" class="btn btn-primary" disabled={renaming || !renameValue.trim()}>
                    Save
                </button>
                <button
                    type="button"
                    class="btn"
                    onclick={() => {
                        editingName = false;
                        renameValue = session.name ?? '';
                    }}
                >
                    Cancel
                </button>
            </form>
        {:else}
            <h1>{session.name ?? commandBasename(session.spec.command)}</h1>
            <button
                type="button"
                class="btn"
                onclick={() => {
                    renameValue = session.name ?? '';
                    editingName = true;
                }}
            >
                Rename
            </button>
        {/if}
        <button
            type="button"
            class="btn btn-danger"
            disabled={killing || !!session.exit}
            onclick={handleKill}
        >
            {killing ? 'Killing…' : 'Kill'}
        </button>
    </div>

    {#if actionError}
        <p class="banner-error">{actionError}</p>
    {/if}

    <div class="terminal-area">
        {#if startedExited}
            <div class="exited-panel">
                <p class="exited-title">Session exited</p>
                <p class="muted">{session.exit ? describeExit(session.exit) : ''}</p>
            </div>
        {:else if client}
            <Terminal {client} sessionId={session.id} {reattachSignal} onPhase={handlePhase} />
        {:else}
            <div class="exited-panel"><p class="muted">Disconnected</p></div>
        {/if}
    </div>

    <div class="meta-bar">
        <span class="mono" title={session.spec.command.join(' ')}>
            {commandBasename(session.spec.command)}
        </span>
        <span>{session.cols}×{session.rows}</span>
        <span>created {relativeTime(session.createdAtUnixMs)}</span>
        <span>{session.attachedClients.length} attached</span>
        {#if session.pid !== undefined}
            <span>pid {session.pid}</span>
        {/if}
        {#if session.exit}
            <span class="exit-tag">exited — {describeExit(session.exit)}</span>
        {/if}
    </div>
</div>

<style>
    .detail {
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
        /* Fill the flex-column main area so the terminal can take the viewport. */
        flex: 1;
        min-height: 0;
    }

    .header {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.75rem;
        flex-shrink: 0;
    }

    .header h1 {
        flex: 1;
        font-size: 1.15rem;
        margin: 0;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }

    .back {
        font-size: 0.875rem;
        flex-shrink: 0;
    }

    .rename-form {
        display: flex;
        flex: 1;
        gap: 0.5rem;
        min-width: 12rem;
    }

    .rename-form input {
        flex: 1;
        min-width: 0;
        padding: 0.5rem 0.625rem;
        background: var(--color-surface);
        border: 1px solid var(--color-border);
        border-radius: var(--radius);
        font-family: var(--font-mono);
    }

    .terminal-area {
        flex: 1;
        min-height: 0;
        /* Positioning context + definite size for the terminal, which fills it. */
        position: relative;
    }

    .exited-panel {
        position: absolute;
        inset: 0;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: 0.5rem;
        border: 1px solid var(--color-border);
        border-radius: var(--radius);
        background: var(--color-surface);
    }

    .exited-title {
        margin: 0;
        font-weight: 600;
    }

    .meta-bar {
        flex-shrink: 0;
        display: flex;
        flex-wrap: wrap;
        gap: 0.375rem 1rem;
        padding-top: 0.25rem;
        font-size: 0.8rem;
        color: var(--color-text-muted);
    }

    .meta-bar .mono {
        font-family: var(--font-mono);
        color: var(--color-text);
        max-width: 100%;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }

    .exit-tag {
        color: var(--color-warning);
    }
</style>
