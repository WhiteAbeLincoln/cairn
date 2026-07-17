<!--
  Session list rows: status, name (or truncated command), command basename,
  attached count, relative created time, exit code. A single responsive grid
  handles both "table on wide screens" and "cards on narrow screens" — see the
  v1 spec's "Responsive Design"/"Session List" sections carried over by
  reference for this content.
-->
<script lang="ts">
import { commandBasename, relativeTime } from '$lib/format';
import type { SessionInfo } from '$lib/protocol';

interface Props {
    sessions: SessionInfo[];
    loading: boolean;
}

const { sessions, loading }: Props = $props();

// A signal-only exit (no exit code) is usually an intentional kill, not an
// error, so it gets the neutral exited styling rather than the red one.
function exitedCleanly(info: SessionInfo): boolean {
    return info.exit !== undefined && (info.exit.code === 0 || info.exit.code === undefined);
}

function statusClass(info: SessionInfo): string {
    if (!info.exit) return 'running';
    return exitedCleanly(info) ? 'exited-ok' : 'exited-err';
}

function statusLabel(info: SessionInfo): string {
    if (!info.exit) return 'Running';
    if (info.exit.code === undefined) return 'Killed';
    return info.exit.code === 0 ? 'Exited' : 'Failed';
}

function displayName(info: SessionInfo): string {
    return info.name ?? commandBasename(info.spec.command);
}

function exitLabel(info: SessionInfo): string {
    if (!info.exit) return '';
    if (info.exit.code !== undefined) return `exit ${info.exit.code}`;
    if (info.exit.signal !== undefined) return `signal ${info.exit.signal}`;
    return 'exited';
}
</script>

{#if loading && sessions.length === 0}
    <p class="muted">Loading sessions…</p>
{:else if sessions.length === 0}
    <p class="muted">No sessions yet. <a href="/sessions/new">Create one</a>.</p>
{:else}
    <div class="session-list" role="table" aria-label="Sessions">
        <div class="session-row session-row-head" role="row">
            <span role="columnheader"></span>
            <span role="columnheader">Name</span>
            <span role="columnheader">Command</span>
            <span role="columnheader">Clients</span>
            <span role="columnheader">Created</span>
            <span role="columnheader">Exit</span>
        </div>
        {#each sessions as session (session.id)}
            <a href="/sessions/{session.id}" class="session-row" role="row">
                <span class="cell-status" role="cell">
                    <span class="status-dot {statusClass(session)}" title={statusLabel(session)}></span>
                </span>
                <span class="cell-name" role="cell" data-label="Name">{displayName(session)}</span>
                <span class="cell-cmd" role="cell" data-label="Command"
                    >{commandBasename(session.spec.command)}</span
                >
                <span class="cell-clients" role="cell" data-label="Clients"
                    >{session.attachedClients.length}</span
                >
                <span class="cell-time" role="cell" data-label="Created"
                    >{relativeTime(session.createdAtUnixMs)}</span
                >
                <span class="cell-exit" role="cell">
                    {#if session.exit}
                        <span class="exit-badge" class:exit-ok={exitedCleanly(session)}>
                            {exitLabel(session)}
                        </span>
                    {/if}
                </span>
            </a>
        {/each}
    </div>
{/if}

<style>
    .session-list {
        display: flex;
        flex-direction: column;
        gap: 0.5rem;
    }

    .session-row {
        display: grid;
        grid-template-columns: auto 1fr auto;
        align-items: center;
        gap: 0.5rem 0.75rem;
        padding: 0.75rem 1rem;
        background: var(--color-surface);
        border: 1px solid var(--color-border);
        border-radius: var(--radius);
        color: var(--color-text);
    }

    a.session-row:hover {
        border-color: var(--color-accent);
        text-decoration: none;
        background: var(--color-surface-hover);
    }

    .session-row-head {
        display: none;
    }

    .cell-name {
        font-weight: 600;
        font-family: var(--font-mono);
        font-size: 0.875rem;
        grid-column: 2;
    }

    .cell-status {
        grid-row: 1 / 3;
    }

    .cell-exit {
        grid-column: 2 / 4;
    }

    .cell-cmd,
    .cell-clients,
    .cell-time {
        display: none;
    }

    .cell-cmd::before {
        content: 'cmd: ';
        color: var(--color-text-muted);
    }
    .cell-clients::before {
        content: attr(data-label) ': ';
        color: var(--color-text-muted);
    }

    .status-dot {
        display: inline-block;
        width: 10px;
        height: 10px;
        border-radius: 50%;
    }
    .status-dot.running {
        background: var(--color-success);
    }
    .status-dot.exited-ok {
        background: var(--color-text-muted);
    }
    .status-dot.exited-err {
        background: var(--color-error);
    }

    .exit-badge {
        font-size: 0.75rem;
        font-family: var(--font-mono);
        padding: 0.125rem 0.4rem;
        border-radius: 3px;
        color: var(--color-error);
        background: color-mix(in srgb, var(--color-error) 12%, transparent);
    }
    .exit-badge.exit-ok {
        color: var(--color-text-muted);
        background: color-mix(in srgb, var(--color-text-muted) 12%, transparent);
    }

    /* Table layout on wide screens: one row per grid line, all columns visible. */
    @media (min-width: 720px) {
        .session-list {
            gap: 0;
            border: 1px solid var(--color-border);
            border-radius: var(--radius);
            overflow: hidden;
        }

        .session-row {
            grid-template-columns: 1.5rem 2fr 1fr 5rem 7rem 6rem;
            border-radius: 0;
            border: none;
            border-bottom: 1px solid var(--color-border);
        }

        .session-row:last-child {
            border-bottom: none;
        }

        .session-row-head {
            display: grid;
            background: var(--color-surface-hover);
            font-size: 0.75rem;
            color: var(--color-text-muted);
            text-transform: uppercase;
            letter-spacing: 0.03em;
        }

        .cell-name,
        .cell-exit,
        .cell-status {
            grid-column: auto;
            grid-row: auto;
        }

        .cell-cmd,
        .cell-clients,
        .cell-time {
            display: block;
            font-size: 0.8rem;
            color: var(--color-text-muted);
        }

        .cell-cmd::before,
        .cell-clients::before {
            content: none;
        }
    }
</style>
