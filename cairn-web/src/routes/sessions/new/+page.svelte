<script lang="ts">
import CreateSession from '$lib/components/CreateSession.svelte';
</script>

<div class="page-header">
    <a href="/sessions" class="back">&larr; Sessions</a>
    <h1>New session</h1>
</div>

<CreateSession />

<style>
    .page-header {
        display: flex;
        align-items: center;
        gap: 1rem;
        margin-bottom: 1.25rem;
    }

    .back {
        font-size: 0.875rem;
    }

    .page-header h1 {
        font-size: 1.25rem;
        margin: 0;
    }
</style>
