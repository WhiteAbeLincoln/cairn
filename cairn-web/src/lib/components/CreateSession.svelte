<!--
  Create-session form: primary fields (name, command, workdir) always visible,
  advanced fields (env, scrollback, idle timeout, tty/stdin) collapsed by
  default — mirrors the v1 spec's "Create Session" view, mapped onto the full
  `SessionSpec`.
-->
<script lang="ts">
import { goto } from '$app/navigation';
import { buildSessionSpec, DEFAULT_SCROLLBACK_LINES } from '$lib/sessionSpecForm';
import { getClient } from '$lib/stores/connection.svelte';

let name = $state('');
let command = $state('');
let workdir = $state('');
let showAdvanced = $state(false);
let envInherit = $state(true);
let envPairs = $state('');
// Number inputs bind as `number | null` (`null` when cleared or invalid);
// buildSessionSpec handles both shapes.
let scrollbackLines = $state<number | null>(DEFAULT_SCROLLBACK_LINES);
let idleTimeout = $state<number | null>(null);
let tty = $state(true);
let stdin = $state(true);
let submitting = $state(false);
let error = $state<string | undefined>(undefined);

async function handleSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (!command.trim() || submitting) return;

    const client = getClient();
    if (!client) {
        error = 'Not connected to the daemon yet.';
        return;
    }

    submitting = true;
    error = undefined;
    try {
        const spec = buildSessionSpec({
            name,
            command,
            workdir,
            envText: envPairs,
            envInherit,
            scrollbackLines,
            idleTimeoutSecs: idleTimeout,
            tty,
            stdin,
        });
        const info = await client.create(spec);
        await goto(`/sessions/${info.id}`);
    } catch (err) {
        error = err instanceof Error ? err.message : String(err);
    } finally {
        submitting = false;
    }
}
</script>

<form onsubmit={handleSubmit}>
    {#if error}
        <p class="banner-error">{error}</p>
    {/if}

    <label class="field">
        <span class="field-label">Name <span class="optional">(optional)</span></span>
        <input type="text" bind:value={name} placeholder="my-session" autocomplete="off" />
    </label>

    <label class="field">
        <span class="field-label">Command</span>
        <input type="text" bind:value={command} placeholder="bash" autocomplete="off" required />
    </label>

    <label class="field">
        <span class="field-label">Working directory <span class="optional">(optional)</span></span>
        <input type="text" bind:value={workdir} placeholder="/home/user/project" autocomplete="off" />
    </label>

    <button
        type="button"
        class="advanced-toggle"
        aria-expanded={showAdvanced}
        onclick={() => {
            showAdvanced = !showAdvanced;
        }}
    >
        {showAdvanced ? '▾' : '▸'} Advanced
    </button>

    {#if showAdvanced}
        <div class="advanced">
            <label class="field">
                <span class="field-label">Environment variables</span>
                <textarea bind:value={envPairs} placeholder="KEY=value&#10;ANOTHER=value" rows="3"
                ></textarea>
            </label>

            <label class="checkbox-field">
                <input type="checkbox" bind:checked={envInherit} />
                <span>Inherit environment</span>
            </label>

            <label class="field">
                <span class="field-label">Scrollback lines</span>
                <input type="number" bind:value={scrollbackLines} min="0" max="1000000" />
            </label>

            <label class="field">
                <span class="field-label">Idle timeout <span class="optional">(seconds, optional)</span></span>
                <input type="number" bind:value={idleTimeout} min="0" placeholder="300" />
            </label>

            <label class="checkbox-field">
                <input type="checkbox" bind:checked={tty} />
                <span>Allocate TTY</span>
            </label>

            <label class="checkbox-field">
                <input type="checkbox" bind:checked={stdin} />
                <span>Enable stdin</span>
            </label>
        </div>
    {/if}

    <div class="form-actions">
        <a href="/sessions" class="btn">Cancel</a>
        <button type="submit" class="btn btn-primary" disabled={submitting || !command.trim()}>
            {submitting ? 'Creating…' : 'Create'}
        </button>
    </div>
</form>

<style>
    form {
        max-width: 32rem;
    }

    .field {
        display: block;
        margin-bottom: 1rem;
    }

    .field-label {
        display: block;
        font-size: 0.875rem;
        font-weight: 500;
        margin-bottom: 0.25rem;
    }

    .optional {
        color: var(--color-text-muted);
        font-weight: 400;
    }

    input[type='text'],
    input[type='number'],
    textarea {
        width: 100%;
        padding: 0.625rem 0.75rem;
        min-height: 2.5rem;
        background: var(--color-surface);
        border: 1px solid var(--color-border);
        border-radius: var(--radius);
        font-family: var(--font-mono);
        font-size: 0.9rem;
    }

    input:focus,
    textarea:focus {
        outline: none;
        border-color: var(--color-accent);
    }

    textarea {
        resize: vertical;
    }

    .checkbox-field {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        margin-bottom: 0.875rem;
        font-size: 0.9rem;
        min-height: 2rem;
        cursor: pointer;
    }

    .checkbox-field input {
        width: 1.25rem;
        height: 1.25rem;
    }

    .advanced-toggle {
        background: none;
        border: none;
        color: var(--color-text-muted);
        font-size: 0.875rem;
        padding: 0.375rem 0;
        margin-bottom: 0.75rem;
        cursor: pointer;
        min-height: 2.25rem;
    }
    .advanced-toggle:hover {
        color: var(--color-text);
    }

    .advanced {
        padding-left: 1rem;
        border-left: 2px solid var(--color-border);
        margin-bottom: 1rem;
    }

    .form-actions {
        display: flex;
        justify-content: flex-end;
        gap: 0.75rem;
        margin-top: 1.5rem;
    }

    @media (max-width: 480px) {
        .form-actions {
            flex-direction: column-reverse;
        }
        .form-actions .btn {
            width: 100%;
        }
    }
</style>
