<!-- Root route: the app has no standalone landing page, so send visitors straight to the session list. -->
<script lang="ts">
import { onMount } from 'svelte';
import { goto } from '$app/navigation';

onMount(() => {
    goto('/sessions', { replaceState: true });
});
</script>
