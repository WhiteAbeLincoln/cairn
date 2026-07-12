<!--
  Stub session detail: metadata + Kill/Rename actions, no terminal yet (Task
  8). Deliberately thin per the Task 7 brief — just enough to make navigation
  from the list coherent and to exercise the kill/rename round-trip.
-->
<script lang="ts">
import { commandBasename, relativeTime } from '$lib/format';
import type { SessionInfo } from '$lib/protocol';
import { getClient } from '$lib/stores/connection.svelte';
import { refreshSessions } from '$lib/stores/sessions.svelte';

interface Props {
    session: SessionInfo;
    onUpdated: (info: SessionInfo) => void;
}

const { session, onUpdated }: Props = $props();

let killing = $state(false);
let renaming = $state(false);
// Seeded only when rename mode is entered (see the button below) rather than
// at declaration time, so it doesn't capture a stale snapshot of the `session`
// prop if the session is renamed elsewhere and re-fetched.
let renameValue = $state('');
let editingName = $state(false);
let actionError = $state<string | undefined>(undefined);

function exitSummary(info: SessionInfo): string {
    if (!info.exit) return '';
    const parts: string[] = [];
    if (info.exit.code !== undefined) parts.push(`code ${info.exit.code}`);
    if (info.exit.signal !== undefined) parts.push(`signal ${info.exit.signal}`);
    if (info.exit.reason) parts.push(info.exit.reason);
    return parts.join(', ') || 'exited';
}

async function refreshInfo(): Promise<void> {
    const client = getClient();
    if (!client) return;
    const fresh = await client.inspect(session.id);
    onUpdated(fresh);
    refreshSessions();
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

    {#if session.exit}
        <p class="exit-banner">Exited — {exitSummary(session)}</p>
    {/if}

    <div class="terminal-placeholder muted">Terminal attach arrives in a later task.</div>

    <dl class="meta">
        <div><dt>ID</dt><dd class="mono">{session.id}</dd></div>
        <div><dt>Command</dt><dd class="mono">{session.spec.command.join(' ')}</dd></div>
        {#if session.pid !== undefined}
            <div><dt>PID</dt><dd>{session.pid}</dd></div>
        {/if}
        <div><dt>Size</dt><dd>{session.cols}×{session.rows}</dd></div>
        <div><dt>Created</dt><dd>{relativeTime(session.createdAtUnixMs)}</dd></div>
        <div><dt>Attached clients</dt><dd>{session.attachedClients.length}</dd></div>
        {#if session.spec.workdir}
            <div><dt>Workdir</dt><dd class="mono">{session.spec.workdir}</dd></div>
        {/if}
    </dl>
</div>

<style>
    .header {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.75rem;
        margin-bottom: 1rem;
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

    .exit-banner {
        padding: 0.625rem 0.875rem;
        background: var(--color-surface);
        border: 1px solid var(--color-border);
        border-radius: var(--radius);
        margin-bottom: 1rem;
        font-size: 0.875rem;
    }

    .terminal-placeholder {
        display: flex;
        align-items: center;
        justify-content: center;
        min-height: 12rem;
        border: 1px dashed var(--color-border);
        border-radius: var(--radius);
        margin-bottom: 1.25rem;
        font-size: 0.875rem;
    }

    .meta {
        display: grid;
        grid-template-columns: 1fr;
        gap: 0.5rem 1.5rem;
        margin: 0;
    }

    .meta > div {
        display: flex;
        justify-content: space-between;
        gap: 1rem;
        padding: 0.5rem 0;
        border-bottom: 1px solid var(--color-border);
        font-size: 0.875rem;
    }

    .meta dt {
        color: var(--color-text-muted);
    }

    .meta dd {
        margin: 0;
        text-align: right;
        overflow-wrap: anywhere;
    }

    .mono {
        font-family: var(--font-mono);
    }

    @media (min-width: 640px) {
        .meta {
            grid-template-columns: 1fr 1fr;
        }
    }
</style>
